// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The brain: config, project, memory store, provider, model resolver, safety,
//! code graph, MCP, skills and the agent factory — built once per process and
//! shared by every agent. Moved from the Tauri app's `main.rs`; frontends pass
//! a [`HostNotifier`] for background startup events and a Tokio handle for the
//! background tasks the build spawns.

use std::sync::{Arc, RwLock};

use crate::agent::context::ContextManager;
use crate::agent::factory::AgentLoopFactory;
use crate::config::{global_config_dir, Config, SafetyMode};
use crate::host::index_progress::{
    clear_startup_index, ensure_startup_index_visible, mark_startup_index_active,
    record_startup_index_progress, startup_should_forward, IndexProgressEvent,
};
use crate::host::maintenance::{claim_busy, BusyGuard, ReconcileEvent};
use crate::host::notifier::{HostEvent, SharedNotifier};
use crate::host::startup::{
    project_flag_from_args, resolve_startup_provider, should_reembed_at_startup,
};
use crate::memory::embedder::HashEmbedder;
use crate::memory::MemoryStore;
use crate::workspace::{CODING_DIR_NAME, Workspace, ensure_minimal_project, find_coding_dir_ancestor};
use crate::provider::trace::LlmRequestLog;
use crate::provider::{LlmClient, ProviderKind};
use crate::safety_rules::SafetyRules;
use crate::tool::agent::sandbox::Sandbox;
use crate::provider::client_factory::{
    build_client, build_embedder, build_vision_client, embedder_startup_plan,
    EmbedderStartupPlan,
};
use crate::provider::openai::{OpenAiClient, OpenAiClientConfig};

/// The built brain — everything needed to run the agent.
///
/// `pub` with all fields `pub` so every host (the Tauri GUI, the console,
/// the headless API) can reuse the exact same construction path and wire
/// its own state from the pieces it needs: `build_brain` takes a
/// host-notifier + a runtime handle, never a Tauri app.
pub struct Brain {
    pub factory: Arc<AgentLoopFactory>,
    /// Concrete memory store for runtime embedder rewire (Settings → Memory).
    /// `pub(crate)` so console mode can start the deferred background embedder
    /// load (the GUI's setup hook does the same; console mode has no Tauri).
    pub memory_store: Option<Arc<MemoryStore>>,
    /// The resolved kernel workspace (2c.3 G3): the root + the state dir.
    /// The brain no longer holds the app's Project (the kernel facade must
    /// not import it) — the consumers build their own app-side Project.
    pub workspace: Arc<Workspace>,
    pub config: Arc<tokio::sync::Mutex<Config>>,
    pub sandbox: Arc<Sandbox>,
    pub safety_rules: Arc<SafetyRules>,
    pub safety_mode: Arc<RwLock<SafetyMode>>,
    /// The per-context model resolver (shared with the factory). The IPC layer
    /// pushes reloaded config into it after a Settings save so `[models]`
    /// overrides take effect on the next request (resolution runs per request,
    /// 2027-01-25).
    pub model_resolver: Arc<crate::model_resolver::ConfigModelResolver>,
    /// The shared LLM request/response trace log — wired into the default
    /// provider + every resolver-built provider and exposed to the IPC layer
    /// (Trace tab). One instance for the whole app.
    pub trace: Arc<LlmRequestLog>,
    /// The shared headless debug browser — the same `Arc` wired into the
    /// factory (agent tools) and the IPC state (Browser tab commands).
    pub browser: Arc<crate::browser::BrowserManager>,
    /// The shared embedder status — the same `Arc` held by `IpcState`, so
    /// the circuit breaker's transitions surface live to the UI.
    /// `pub(crate)` so console mode can drive the deferred embedder load.
    pub embedder_status: Arc<RwLock<crate::memory::embedder::EmbedderStatus>>,
    /// The shared classifier status — the same `Arc` held by `IpcState`, so
    /// the enabled/disabled/failed state surfaces live to Settings. `Disabled`
    /// unless the config enables Laya.
    pub classifier_status: Arc<RwLock<crate::memory::classifier::ClassifierStatus>>,
    /// The shared classifier SLOT (the live handle, backlog a147b63c):
    /// created in `build_brain_inner` and shared by the factory's
    /// `memory_write` tools (call-time read) and the IPC `RuntimeState`
    /// (rewire swaps it) — one Arc, no rebuilds. The auto-typing FLAG
    /// mirror lives in the factory's gate handle (Settings saves flip it
    /// via `set_auto_typing_enabled`).
    pub classifier_slot:
        Arc<RwLock<Option<Arc<dyn crate::memory::classifier::Classifier>>>>,
    /// A configured bundled embedding model that wasn't installed at startup
    /// (first run): the store started on the hash embedder and the setup hook
    /// must download this model in the background, swapping it into the live
    /// store + re-embedding rows on success.
    pub pending_model_download: Option<String>,
    /// A configured bundled embedding model that WAS installed at startup but
    /// whose ~110 MB ONNX load was deferred off the main thread (hang
    /// diagnostics, AppHangB1 2026-08-20): the store started on the hash
    /// embedder and the setup hook must load the model in the background and
    /// swap it into the live store (mirror of the download swap).
    /// `pub(crate)` so console mode starts the same background load.
    pub pending_model_load: Option<String>,
    /// The CodeGraph OS file watcher — held only to keep the watch alive for
    /// the app's lifetime (dropping it stops watching). `None` when codegraph
    /// is disabled, failed to open, or the watcher failed to start. The field
    /// is never read by name (it exists purely for its Drop semantics), so it
    /// is underscore-prefixed rather than `#[allow(dead_code)]`ed.
    pub _graph_watcher: Option<crate::codegraph::watcher::GraphWatcher>,
}

/// The outcome of [`build_brain`]: either the brain is ready to run, or no
/// project could be resolved and the app should show the project picker.
///
/// `NeedsProject` carries the loaded global `Config` (so the picker can list
/// the registered projects from `projects.toml`) without building any of the
/// project-scoped subsystems (provider, sandbox, memory, constitution…).
pub enum BrainOutcome {
    /// The brain built successfully — the app runs normally.
    Ready(Brain),
    /// No project resolved at startup — the frontend shows the project
    /// picker. The window is open but the agent can't run until a project is
    /// chosen (which restarts the app into the selected project).
    NeedsProject(Config),
}

/// Build the brain: load config, resolve project, wire provider + factory —
/// and always report how long it took.
///
/// The factory holds the shared, stateless deps (provider, constitution
/// source, memory store, sandbox, safety rules, safety mode, context manager,
/// plans dir). Each agent — including the first "main" agent — is built via
/// `factory.build()`, which gives it its own `Workflow` + `ToolRegistry`.
///
/// Returns [`BrainOutcome::NeedsProject`] when no project can be resolved
/// (the cwd is not inside a project, no `--project` flag was given, and no
/// pending-project marker from a prior switch is present). In that case the
/// frontend shows the project picker instead of the normal UI.
///
/// `build_brain` runs on the main thread (Tauri's sync `.setup` hook) BEFORE
/// the event loop starts pumping messages, so its duration is the app's
/// main-thread stall at startup. The measurement is printed permanently: it is
/// the baseline for hang diagnostics (AppHangB1, 2026-08-20) — if this number
/// ever grows to seconds, startup work must move off the main thread.
///
/// First measurement (2026-08-20): **537 ms total** — under the 1 s
/// restructure bar, and its single largest contributor (the bundled model's
/// ~110 MB ONNX init) is already deferred off the main thread
/// ([`EmbedderStartupPlan::UseHashThenLocalLoad`]); the rest is config load +
/// MemoryStore open + provider/factory wiring, all sub-100 ms pieces.
pub fn build_brain(notifier: Option<SharedNotifier>, rt: tokio::runtime::Handle) -> anyhow::Result<BrainOutcome> {
    let started = std::time::Instant::now();
    let outcome = build_brain_inner(notifier, rt);
    eprintln!(
        "mnemo: build_brain took {}ms on the main thread ({})",
        started.elapsed().as_millis(),
        match &outcome {
            Ok(BrainOutcome::Ready(_)) => "ready",
            Ok(BrainOutcome::NeedsProject(_)) => "needs-project",
            Err(_) => "error",
        }
    );
    outcome
}

/// The measured body of [`build_brain`] — see its doc comment for the timing
/// wrapper above.
fn build_brain_inner(notifier: Option<SharedNotifier>, rt: tokio::runtime::Handle) -> anyhow::Result<BrainOutcome> {
    let config_dir = global_config_dir();
    let config = Config::load(&config_dir)?;

    // The shared request/response trace log — created first so every provider
    // built below (default, dummy fallback, resolver) records into the same
    // instance the IPC layer hands to the Trace tab.
    let trace = Arc::new(LlmRequestLog::new());

    // Resolve the project. Resolution order:
    //   1. Pending-project marker — set by a prior `switch_project` call
    //      right before a restart. The marker is consumed here. The path must
    //      already be a project; if its `.coding/` is missing (e.g. the user
    //      deleted it), fall through to the picker rather than silently
    //      re-initializing.
    //   2. `--project <path>` flag — CLI override. Initializes the project if
    //      it isn't one yet (preserves the CLI workflow).
    //   3. Auto-detect — walk up from the cwd looking for `.coding/`.
    //   4. Otherwise — no project; return NeedsProject so the picker shows.
    //
    // The registry is deliberately NOT auto-picked: the picker lists its
    // entries but never opens one without an explicit choice (per the
    // "always show the dialog unless the cwd is a project" rule).
    //
    // `switched_open` records that THIS launch is the reload half of a
    // `switch_project` (a pending-project marker was consumed and valid):
    // the startup indexing pass uses it to skip the 1-second progress gate —
    // the user explicitly opened this project and the UI they clicked from
    // was just torn down for the restart, so the splash dialog must show
    // immediately rather than after a silent second.
    let mut switched_open = false;
    let workspace = if let Some(pending) = crate::config::take_pending_project() {
        if pending.join(CODING_DIR_NAME).is_dir() {
            switched_open = true;
            Arc::new(Workspace::new(&pending))
        } else {
            // Marker pointed at a non-project dir — show the picker, but say
            // WHY on stderr so a stale marker is diagnosable (backlog
            // 16e4a7f8: the silent fall-through re-showed the picker with no
            // explanation).
            eprintln!(
                "mnemo: pending-project marker pointed at '{}' which has no .coding/ — showing the picker",
                pending.display()
            );
            return Ok(BrainOutcome::NeedsProject(config));
        }
    } else if let Some(flagged) = project_flag_from_args(std::env::args()) {
        if flagged.join(CODING_DIR_NAME).exists() {
            Arc::new(Workspace::new(&flagged))
        } else {
            eprintln!(
                "No project found at '{}'. Initializing...",
                flagged.display()
            );
            // The kernel-minimal bootstrap in place of the app's Project::init
            // (G3): .coding/plans + the shipped skills + the agent.md template —
            // the kernel never writes .gitignore/.gitattributes.
            ensure_minimal_project(&flagged)?;
            Arc::new(Workspace::new(&flagged))
        }
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(found) = find_coding_dir_ancestor(&cwd) {
            Arc::new(Workspace::new(&found))
        } else {
            return Ok(BrainOutcome::NeedsProject(config));
        }
    };

    // Tell the trace log where to mirror records when the user opts in via the
    // Trace tab's "Log to file" checkbox. The path is configured here (where
    // coding_dir is known) but logging stays OFF by default — writing full
    // request/response bodies to disk is a deliberate opt-in.
    trace.set_log_file_path(workspace.state_dir.join("logs").join("traces.jsonl"));

    // Always-on provider-error log: every FAILED request (HTTP >= 400,
    // transport / stream errors) is appended as one compact JSON line to
    // .coding/logs/provider-errors.jsonl — independent of the traces.jsonl
    // opt-in checkbox — so provider errors are diagnosable after the fact
    // without full trace logging.
    trace.set_error_log_path(
        workspace
            .state_dir
            .join("logs")
            .join("provider-errors.jsonl"),
    );

    // Always-on user-cancel log (F3): every request the consumer drops
    // mid-stream (a user interrupt — the D1 classification) is appended as
    // one compact JSON line at stamping time, so a cancel survives the trace
    // ring's rotation. Independent of the traces.jsonl opt-in checkbox.
    trace.set_cancels_log_path(workspace.state_dir.join("logs").join("cancels.jsonl"));

    // Append-only terminal-record archive (F3): every record that reaches a
    // terminal state is appended once to .coding/logs/traces-history.jsonl
    // (rotated at 64 MiB, keeping the 4 newest archives), so a busy turn
    // cannot erase earlier records retrievably. Gated on the same opt-in as
    // the mirror — full request/response bodies are sensitive.
    trace.set_history_log_path(
        workspace
            .state_dir
            .join("logs")
            .join("traces-history.jsonl"),
    );

    // Apply the in-memory trace limits from `[trace]` (Advanced settings).
    // Clamped at config load; the fallback branches above keep the defaults
    // baked into LlmRequestLog (16 MiB budget / 256 KiB request cap).
    trace.set_memory_budget(config.general.trace.memory_budget_mb * 1024 * 1024);
    trace.set_request_body_cap(config.general.trace.request_body_cap_kb * 1024);

    // Build the provider.
    let (endpoint, model) = resolve_startup_provider(&config);

    // The DISPLAY-space twin of the startup effort (backlog 51dab4da):
    // recorded on the factory below so every loop it builds reports the
    // default model's effective effort (per-model override → endpoint
    // default → "max") from the very first turn. The dummy-provider branch
    // bakes "max" (see its config below).
    let startup_display_effort = match endpoint {
        Some(ep) => ep.display_reasoning_effort_for(Some(&model)),
        None => "max".to_string(),
    };

    let provider: Arc<dyn LlmClient> = if let Some(ep) = endpoint {
        // The shared construction path resolves the api key (stored key →
        // kind-appropriate env var → "dummy") and dispatches by endpoint kind
        // (OpenAI/Local → OpenAI-compatible client, Anthropic → native
        // Messages client), so the main provider can never drift from the
        // vision provider or the set_model runtime swap.
        build_client(
            &config,
            ep,
            &model,
            ep.multimodal_for(&model),
            ep.effective_reasoning_effort_for(Some(&model)),
            Some(trace.clone()),
        )
    } else {
        eprintln!("warning: no endpoint configured; using a dummy provider");
        Arc::new(OpenAiClient::new_with_trace(
            OpenAiClientConfig {
                base_url: "http://localhost/v1/".into(),
                api_key: "dummy".into(),
                model: model.clone(),
                kind: ProviderKind::Local,
                max_context: None,
                max_output_tokens: None,
                multimodal: false,
                reasoning_effort: Some("max".to_string()),
                provider: "dummy".into(),
                use_responses_api: false,
                ..Default::default()
            },
            Some(trace.clone()),
        ))
    };

    // The path sandbox — confines file operations to the project root.
    let sandbox = Arc::new(Sandbox::new(&workspace.root)?);

    // Memory store — uses the bundled in-process embedder when configured
    // (failure-protected: zero-vector fallback, never dies), or the built-in
    // deterministic hash embedder otherwise. The shared status Arc is the
    // same handle IpcState exposes to the UI, so circuit-breaker transitions
    // surface live.
    let embedder_status = Arc::new(RwLock::new(
        crate::memory::embedder::EmbedderStatus::Checking,
    ));
    // The bundled embedding model cache lives under the global config dir
    // (NOT .coding/, which is committed to git — model binaries are per-machine).
    let embedder_cache_dir = config_dir.join("models");
    // First-run plan: a configured-but-not-installed model must not block
    // startup on its ~100 MB download — build the hash embedder now and let
    // the setup hook download the model in the background (swapping it into
    // the live store + re-embedding rows on success). An INSTALLED model
    // takes the same shape (hash now + background LOCAL load + swap): its
    // ~110 MB ONNX init is the single largest main-thread block at startup
    // (AppHangB1 diagnostics, 2026-08-20), and the store already supports
    // swapping a late-arriving embedder.
    let (embedder, pending_download, pending_load) = match embedder_startup_plan(
        config.general.general.bundled_embedding_model.as_deref(),
        &embedder_cache_dir,
    ) {
        EmbedderStartupPlan::UseHashThenDownload(model) => {
            eprintln!(
                "info: bundled embedding model '{model}' not installed yet; starting with \
                 the built-in hash embedder and downloading in the background"
            );
            // Hash now, but advertise `Checking` (not Ready): the real
            // configured model is still pending — the download task flips
            // the status Downloading → Ready, so the UI never shows a
            // misleading "ready" before the progress bar.
            *embedder_status
                .write()
                .expect("embedder status lock poisoned") =
                crate::memory::embedder::EmbedderStatus::Checking;
            (
                Arc::new(HashEmbedder::new()) as Arc<dyn crate::memory::embedder::Embedder>,
                Some(model),
                None,
            )
        }
        EmbedderStartupPlan::UseHashThenLocalLoad(model) => {
            eprintln!(
                "info: bundled embedding model '{model}' is installed; starting with the \
                 built-in hash embedder and loading the model in the background \
                 (keeps the ONNX load off the main thread)"
            );
            // Hash now, status stays `Checking` — the load task flips it to
            // Ready on success (Failed on error), same honesty contract as
            // the download path.
            *embedder_status
                .write()
                .expect("embedder status lock poisoned") =
                crate::memory::embedder::EmbedderStatus::Checking;
            (
                Arc::new(HashEmbedder::new()) as Arc<dyn crate::memory::embedder::Embedder>,
                None,
                Some(model),
            )
        }
        // HashOptOut / HashDefault go through build_embedder (sentinel +
        // fallback respectively — nothing to load or download).
        _ => (
            build_embedder(&config, &embedder_cache_dir, embedder_status.clone()),
            None,
            None,
        ),
    };

    // The optional Laya classifier (opt-in; disabled by default). Built only
    // when [general.laya] enables it — otherwise `None` with
    // status Disabled and no HTTP client at all, so the app behaves exactly as
    // before. Constructing the client does no I/O: startup is never blocked.
    let classifier_status = Arc::new(RwLock::new(
        crate::memory::classifier::ClassifierStatus::Disabled,
    ));
    // The classifier itself is attached by the HOST after the build (the
    // managed runtime is host-owned — the app's attach_laya fills the
    // shared slot below before the first agent spawns), so the kernel
    // brain leaves it empty here.
    let classifier: Option<Arc<dyn crate::memory::classifier::Classifier>> = None;
    // The shared classifier SLOT + the auto-typing flag (backlog a147b63c):
    // one Arc pair feeds the factory's memory_write tools (call-time read)
    // and the IPC RuntimeState (rewire swaps the slot; Settings saves flip
    // the flag via `set_auto_typing_enabled`).
    let classifier_slot: Arc<
        RwLock<Option<Arc<dyn crate::memory::classifier::Classifier>>>,
    > = Arc::new(RwLock::new(classifier.clone()));
    let auto_typing_flag = Arc::new(std::sync::atomic::AtomicBool::new(
        config.general.general.laya.auto_type_memories,
    ));
    // The tool-choice flag (backlog e2c47d5f): the SAME classifier slot feeds
    // the search/search_read tools; Settings saves flip this flag via
    // `set_tool_choice_enabled`.
    let tool_choice_flag = Arc::new(std::sync::atomic::AtomicBool::new(
        config.general.general.laya.steer_tool_choice,
    ));
    let store = match MemoryStore::open(&workspace.state_dir.join("memory.db"), embedder) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("warning: failed to open memory store: {e}; using in-memory");
            Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap())
        }
    };
    let store_trait: Arc<dyn crate::memory::MemoryStoreTrait> = store.clone();

    // The failure-triage gate (backlog 1a4049c1): the SAME shared classifier
    // slot plus the `[general.laya] failure_triage` flag mirror. The factory
    // hands it to every loop it builds; the loop's dispatch site and both
    // provider retry layers read both at failure time, and a Settings save
    // flips the flag via `set_failure_triage_enabled` (no rebuild).
    //
    // The opt-in kNN overlay (item 4b, `failure_triage_knn`): a local
    // classifier over the failure-triage training log, consulted by the
    // gate BEFORE the shared slot while its flag is on. Its embedder
    // getter reads the store's LIVE shared embedder slot per
    // classification (a background bundled-model load or a
    // Settings-driven embedder swap flows in — the index re-embeds on
    // the model-id change), and its index tracks the training-log FILE,
    // so dispositions appended by any writer are picked up on the next
    // classification — genuinely online, no retraining, no `laya-serve`.
    // Lazy by construction: with the flag off nothing is ever read or
    // embedded. Constructed AFTER the store so the getter can capture a
    // store clone.
    let store_for_knn = store.clone();
    let failure_triage_handle = crate::agent::failure_triage::FailureTriageHandle::new(
        classifier_slot.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(
            config.general.general.laya.failure_triage,
        )),
    )
    .with_knn(
        Arc::new(crate::agent::failure_triage_knn::KnnClassifier::new(
            Arc::new(move || store_for_knn.embedder_handle()),
            crate::agent::failure_triage::training_log_path(),
        )),
        config.general.general.laya.failure_triage_knn,
    );

    // The pre-prompt routing gate (backlog 091e694d): the SAME shared
    // classifier slot. The `[general.laya] routing` opt-in and the
    // `[general.routing]` threshold / enforce are read from the live config
    // by the model resolver per turn, so this gate carries no flag mirror and
    // a Settings save needs no rewire.
    let routing_gate = crate::agent::model_routing::RoutingGate::new(classifier_slot.clone());

    // The compound-reflex handle (backlog a8495cc1) -- the escalation lane
    // ladder's classifier (backlog ad56c7bd): the SAME shared classifier slot
    // plus the `[general.laya] reflex` flag mirror, read at every plan-step
    // boundary. A Settings save flips the flag live via `set_reflex_enabled`
    // (no rebuild); the `[general.routing]` lane targets ride the live config
    // the resolver reads per request.
    let reflex_handle = crate::agent::reflex::ReflexHandle::new(
        classifier_slot.clone(),
        Arc::new(std::sync::atomic::AtomicBool::new(
            config.general.general.laya.reflex,
        )),
    );

    // Install the [memory] retrieval knobs (decay, caps, digest budgets) from
    // the loaded config — recall + memory_write read the store's snapshot per
    // call. Settings saves update it via rewire_vision_embedder_and_classifier.
    store.set_memory_search_config(config.general.memory.clone());

    // Startup reconciliation: check whether the semantic DB matches the
    // on-disk truth (plans, reviews, knowledge records, backlog) and rebuild
    // the derived index when it drifted. The staleness pre-check is cheap
    // (content hashes vs the state table) and keeps the run SILENT when
    // nothing changed — events + the progress dialog fire only when the
    // corpus actually differs (a git merge landed, a record was edited, or
    // memory.db was deleted and must be rebuilt from the files — the
    // delete-to-rebuild recovery path). Runs in the background so startup
    // never waits on it; the frontend shows the wait dialog while the
    // `memory://reconcile` events stream.
    //
    // The maintenance busy guard is claimed for the run (review LOW 3): a
    // manual "Rebuild derived index" clicked while the reconcile is mid-run
    // must not interleave delete_derived + index_state_clear with reconcile
    // writes — the guard serializes them (the manual command errors with
    // "already running" instead of corrupting the state table).
    {
        let notifier = notifier.clone();
        let store_for_index = store.clone();
        let plans_dir = workspace.state_dir.join("plans");
        let backlog_path = workspace.state_dir.join("backlog.jsonl");
        rt.spawn(async move {
            // Console mode (no app handle) has no frontend to notify — the
            // reconcile still runs, but silently (the events are dropped).
            let Some(notifier) = notifier else {
                let guard = match claim_busy() {
                    Ok(()) => Some(BusyGuard),
                    Err(e) => {
                        eprintln!(
                            "warning: derived-index reconcile skipped (maintenance busy): {}",
                            e.message
                        );
                        None
                    }
                };
                let Some(_guard) = guard else { return };
                let noop = |_: usize, _: usize| {};
                if let Err(e) = crate::memory::indexer::index_derived(
                    store_for_index.as_ref(),
                    &plans_dir,
                    &backlog_path,
                    &noop,
                )
                .await
                {
                    eprintln!("warning: derived-index reconcile failed: {e}");
                }
                return;
            };
            // Claim the single-op guard; skip (with a log) when a manual
            // maintenance op is already running — the manual op will index
            // the corpus anyway.
            let guard = match claim_busy() {
                Ok(()) => Some(BusyGuard),
                Err(e) => {
                    eprintln!(
                        "warning: derived-index reconcile skipped (maintenance busy): {}",
                        e.message
                    );
                    None
                }
            };
            let Some(guard) = guard else {
                return;
            };
            // Pre-existing authored typed rows (SPEC:/DECISION:/BUG:/HOW:
            // written before knowledge files existed) migrate to files — the
            // file becomes the truth, the row is deleted, and the derived
            // index rebuilds it below. Idempotent: a migrated row no longer
            // exists, so a re-run is a no-op. Only runs when a knowledge dir
            // is wired (a plans dir that implies no root).
            if let Some(knowledge_dir) = plans_dir
                .parent()
                .map(|p| p.join(crate::memory::knowledge::KNOWLEDGE_DIR_NAME))
            {
                let knowledge = crate::memory::KnowledgeStore::new(knowledge_dir);
                match crate::memory::indexer::migrate_authored_typed_rows(
                    store_for_index.as_ref(),
                    &knowledge,
                )
                .await
                {
                    Ok(report) if report.migrated > 0 => eprintln!(
                        "info: migrated {} authored typed row(s) to knowledge files{}",
                        report.migrated,
                        if report.skipped.is_empty() {
                            String::new()
                        } else {
                            format!(" ({} skipped)", report.skipped.len())
                        }
                    ),
                    Ok(_) => {}
                    Err(e) => eprintln!("warning: authored-row migration failed: {e}"),
                }
            }
            // The staleness pre-check decides whether the reconcile runs at
            // all. A failure here (store error, scan failure) degrades to
            // "run the index anyway" — the reconcile is the recovery path
            // for a deleted DB, so it must never skip on a pre-check error.
            let stale = match crate::memory::indexer::corpus_is_stale(
                store_for_index.as_ref(),
                &plans_dir,
                &backlog_path,
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "warning: derived-index staleness check failed ({e}); reconciling anyway"
                    );
                    true
                }
            };
            if !stale {
                return;
            }
            notifier.notify(HostEvent::Reconcile(ReconcileEvent::Started));
            // Progress events: one per completed source.
            let progress = |done: usize, total: usize| {
                notifier.notify(HostEvent::Reconcile(ReconcileEvent::Progress { done, total }));
            };
            match crate::memory::indexer::index_derived(
                store_for_index.as_ref(),
                &plans_dir,
                &backlog_path,
                &progress,
            )
            .await
            {
                Ok(report) => {
                    let summary = if report.indexed == 0 && report.removed == 0 {
                        "derived index in sync".to_string()
                    } else {
                        format!(
                            "derived index reconciled: {} indexed, {} skipped, {} removed",
                            report.indexed, report.skipped, report.removed
                        )
                    };
                    notifier.notify(HostEvent::Reconcile(ReconcileEvent::Done { summary }));
                }
                Err(e) => {
                    eprintln!("warning: derived-index reconcile failed: {e}");
                    notifier.notify(HostEvent::Reconcile(ReconcileEvent::Failed {
                        error: e.to_string(),
                    }));
                }
            }
            drop(guard);
        });
    }

    // Startup re-embed check: the memory DB is a gitignored per-machine cache
    // (.gitignore), so git never carries it across machines — but its stored
    // vectors can still mismatch the configured embedder: a Settings model
    // swap (a config save changes the model) re-wires the embedder under an
    // existing DB, a legacy/empty fingerprint set (rows embedded before
    // fingerprint tracking) has nothing to compare against, and only a
    // wholesale project-directory copy moves the DB between machines. If the
    // configured model's fingerprint (model_id + dim) is absent from the
    // stored fingerprints (or the set is empty/legacy-NULL), re-embed all rows
    // in the background so the stored vectors match the configured model. Fire-
    // and-forget: never blocks startup. Only fires when a bundled model is
    // configured AND loaded successfully (status Ready) — when the model fails
    // to load (status Failed, hash fallback), there's nothing useful to
    // re-embed with, and re-embedding would corrupt the fingerprint semantics.
    // Skipped on BOTH hash paths: the explicit "hash" opt-out (re-embedding
    // with hash would destroy stored semantic vectors) and the deferred
    // first-run download (the background download task owns the re-embed —
    // running it here with the interim hash embedder would destroy vectors).
    // Also skipped when a local model load is pending (the background load
    // task owns the re-embed the same way).
    let configured_bundled = config.general.general.bundled_embedding_model.as_deref();
    let status_snapshot = embedder_status
        .read()
        .expect("embedder status lock poisoned")
        .clone();
    if should_reembed_at_startup(
        configured_bundled,
        pending_download.is_some(),
        pending_load.is_some(),
        &status_snapshot,
    ) {
        let store_for_reembed = store.clone();
        let embedder_for_reembed = store.embedder_handle();
        rt.spawn(async move {
            crate::memory::reembed_if_needed(&store_for_reembed, &embedder_for_reembed).await;
        });
    }

    // Load constitutions. We keep a live source (file paths + mtimes) so the
    // agent loop can re-read agent.md from disk when it changes — edits take
    // effect on the next turn without restarting.
    let global_agent_md = config_dir.join("agent.md");
    let constitution_source =
        crate::project::ConstitutionSource::new(
                &global_agent_md,
                &workspace.root.join("agent.md"),
            )?;

    // Safety rules — regex-based auto-approve backed by `.coding/safety.toml`.
    // A missing file is fine (rules start empty); the store is mtime-checked
    // so edits in the Safety tab take effect without restarting.
    let safety_rules = Arc::new(match SafetyRules::new(&workspace.state_dir.join("safety.toml")) {
        Ok(sr) => sr,
        Err(e) => {
            eprintln!(
                "warning: failed to load safety rules from {}: {e}; starting empty",
                workspace.state_dir.join("safety.toml").display()
            );
            // Fall back to an empty store over the same path.
            SafetyRules::new(&workspace.state_dir.join("safety.toml"))
                .unwrap_or_else(|_| SafetyRules::new(std::path::Path::new(":memory:")).unwrap())
        }
    });

    // The shared safety-mode handle — toggled by the UI, read by every agent
    // loop on each tool call. Shared across all agents (a runtime toggle is a
    // global setting, not per-agent).
    let safety_mode = Arc::new(RwLock::new(config.general.general.safety));

    let context_manager = ContextManager::new(
        provider.capabilities().max_context,
        config.general.context.summarize_at_fill_rate,
    )
    .with_preflight(
        config.general.context.preflight_compact,
        config.general.context.compact_headroom_tokens,
    )
    .with_proxy_cache_ceiling(config.general.context.proxy_cache_ceiling_tokens);

    // Vision client from config (shared helper — also used by Settings rewire).
    // Still useful when the main provider is multimodal (describe_image tool
    // for on-disk image files). Warn if configured but endpoint is missing.
    let vision = build_vision_client(&config);
    if config.general.general.vision_model.is_some() && vision.is_none() {
        if let Some(vm) = &config.general.general.vision_model {
            eprintln!(
                "warning: vision_model endpoint '{}' not found in endpoints.toml; \
                 image-to-text fallback disabled",
                vm.endpoint
            );
        }
    }

    // Build the factory — holds the shared, stateless deps. Each agent is
    // built via factory.build(), getting its own Workflow + ToolRegistry.
    //
    // The per-context model resolver reads the `[models]` section each turn
    // to pick a model per workflow state / skill / subagent. It holds its own
    // sync handle to the config (separate from the async `state.project.config`
    // mutex) so the turn driver can read it without an await; the IPC layer
    // pushes reloaded config into it after a Settings save.
    let model_resolver = Arc::new(crate::model_resolver::ConfigModelResolver::new(
        Arc::new(RwLock::new(config.clone())),
        trace.clone(),
    ));
    // The shared headless debug browser — one instance for the agent tools and
    // the IPC layer (the right-panel Browser tab). Lazily spawned on first use.
    let browser = crate::browser::BrowserManager::new();
    // Agent-side bootstrap: browser_navigate can create the Browser tab's
    // child webview when the tab was never opened (plan 5ae26d22).
    // In a release build, enable the WebView2 CDP attach only when the user
    // opted in via `enable_browser_inspection` (the env var exposing the port
    // was already set above in `main`). Debug builds always have it on.
    if !cfg!(debug_assertions) && config.general.general.enable_browser_inspection {
        browser.set_webview_enabled(true);
    }
    // The shared, runtime-mutable git core-operations list — seeded from the
    // loaded `[git]` config section (default ["merge", "push"]). Every GitTool
    // built from the factory shares this Arc<RwLock<...>>, so a save_settings
    // call (which calls factory.set_core_operations) is observed live on the
    // next never_auto_for check without a registry rebuild.
    let core_operations = Arc::new(std::sync::RwLock::new(
        config.general.git.core_operations.clone(),
    ));
    // The shared, runtime-mutable shell-output filter config — seeded from the
    // loaded `[shell_filter]` config section (default: enabled, no overrides).
    // Every ShellTool built from the factory shares this Arc<RwLock<...>>, so
    // a save_settings call (which calls factory.set_shell_filter_config) is
    // observed live on the next command without a registry rebuild.
    let shell_filter = Arc::new(std::sync::RwLock::new(config.general.shell_filter.clone()));
    // Token-optimizer levers (backlog e4a50d22): live mirror of
    // `[general.optimizer]`, shared with every lever-bearing tool build.
    let optimizer_config = Arc::new(std::sync::RwLock::new(
        config.general.general.optimizer.clone(),
    ));
    // Harness-run deterministic verify (the cost-saving chain's verification
    // item): live mirror of `[general.verify]`, shared with every
    // StepVerifyHandle the factory builds (read at each plan-step boundary) —
    // a save_settings call flips it via factory.set_verify_config.
    let verify_config = Arc::new(std::sync::RwLock::new(config.general.general.verify.clone()));
    // The deterministic budget layer (backlog a25a5323): live mirror of
    // `[general.budget]`, shared with every loop the factory builds (read at
    // each plan-step gate) — a save_settings call flips it via
    // factory.set_budget_config.
    let budget_config = Arc::new(std::sync::RwLock::new(config.general.general.budget.clone()));
    // The per-project code knowledge graph (GitNexus-style). Opened here and
    // indexed on a background thread — startup must never block on parsing
    // the codebase. A DB-open failure disables the graph outright (the
    // `graph_*` tools are omitted, the Graph tab shows unavailable). A
    // mid-index error or panic keeps the handle live: the store lock is
    // poison-tolerant and the DB is a rebuildable cache, so the tools
    // degrade to per-call errors instead of being torn down.
    let codegraph: Option<Arc<crate::codegraph::CodeGraph>> = if config.general.general.codegraph {
        match crate::codegraph::CodeGraph::open(
            workspace.root.clone(),
            &workspace.state_dir.join("codegraph.db"),
        ) {
            Ok(graph) => {
                let graph = Arc::new(graph);
                let g = graph.clone();
                // The open-project overlay (IndexingOverlay.tsx) renders
                // this pass as a progress bar + "N/M files indexed"
                // counter. Console mode (no app handle) has no frontend
                // to notify — the pass runs silently there.
                let notifier = notifier.clone();
                rt.spawn(async move {
                    // index() owns the indexing flag: set on entry,
                    // cleared by a drop guard on every exit path (Ok,
                    // Err, panic unwind) — review M1.
                    //
                    // With a window: the pass streams IndexProgressEvent
                    // ticks on codegraph://index-progress. On a COLD
                    // start the 1s gate in startup_should_forward keeps
                    // an already-indexed project's sub-second pass
                    // silent (no overlay flash). On a POST-SWITCH
                    // startup (switched_open — a pending-project marker
                    // was consumed) the gate is skipped and Started is
                    // emitted immediately, so the splash dialog shows
                    // right away like the create-project seed pass.
                    // The terminal Done is always emitted (invisible
                    // when no bar is showing); Failed only when ≥1
                    // event was forwarded.
                    let result = match notifier {
                        Some(notifier) => {
                            // Mirror live progress into the module
                            // snapshot so an overlay that mounts after
                            // the pass began (the webview is recreated
                            // on every start AND every project switch)
                            // can catch up via get_index_progress. The
                            // snapshot is fetchable immediately only on
                            // a switched launch; a cold start publishes
                            // it when the first tick clears the 1s gate
                            // (a sub-second cold pass stays invisible —
                            // the no-flash contract).
                            mark_startup_index_active(switched_open);
                            // A post-switch startup announces itself
                            // immediately: the overlay appears before
                            // the first throttled tick would arrive.
                            let forwarded =
                                Arc::new(std::sync::atomic::AtomicBool::new(switched_open));
                            if switched_open {
                                notifier.notify(HostEvent::IndexProgress(IndexProgressEvent::Started));
                            }
                            let t0 = std::time::Instant::now();
                            let cb_forwarded = Arc::clone(&forwarded);
                            let cb_notifier = notifier.clone();
                            let progress = move |done: usize, total: usize| {
                                // Record EVERY tick before any gate — the
                                // snapshot must stay complete for ticks
                                // the throttle drops.
                                record_startup_index_progress(done, total);
                                if startup_should_forward(
                                    t0.elapsed(),
                                    done,
                                    total,
                                    switched_open,
                                ) {
                                    // The first FORWARDED tick publishes the
                                    // snapshot on a cold start — catch-up
                                    // visibility tracks the overlay stream
                                    // exactly (gated sub-second cold passes
                                    // never publish anything).
                                    ensure_startup_index_visible();
                                    cb_notifier.notify(HostEvent::IndexProgress(
                                        IndexProgressEvent::Progress { done, total },
                                    ));
                                    cb_forwarded.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                            };
                            let pass =
                                tokio::task::spawn_blocking(move || g.index(Some(&progress))).await;
                            // Clear the snapshot BEFORE the terminal event:
                            // a fetch that lands after this reports no pass
                            // rather than stale final counts.
                            clear_startup_index();
                            // Terminal events: Done is ALWAYS emitted — a
                            // lone Done folds to null in the overlay's
                            // reducer (invisible when nothing is showing),
                            // so cold starts stay flash-free, but any
                            // overlay that mounted late (snapshot
                            // catch-up) still gets a definitive end
                            // instead of waiting on a pass that will never
                            // tick again. Failed stays forwarded-gated:
                            // never pop a failure card for a silent
                            // sub-second cold pass.
                            match &pass {
                                Ok(Ok(stats)) => {
                                    notifier.notify(HostEvent::IndexProgress(
                                        IndexProgressEvent::Done {
                                            summary: format!(
                                                "{} files indexed · {} symbols · {} edges",
                                                stats.files_scanned, stats.symbols, stats.edges
                                            ),
                                        },
                                    ));
                                }
                                Ok(Err(e)) => {
                                    if forwarded.load(std::sync::atomic::Ordering::Relaxed) {
                                        notifier.notify(HostEvent::IndexProgress(
                                            IndexProgressEvent::Failed {
                                                error: e.to_string(),
                                            },
                                        ));
                                    }
                                }
                                Err(join) => {
                                    if forwarded.load(std::sync::atomic::Ordering::Relaxed) {
                                        notifier.notify(HostEvent::IndexProgress(
                                            IndexProgressEvent::Failed {
                                                error: format!("index task panicked: {join}"),
                                            },
                                        ));
                                    }
                                }
                            }
                            pass
                        }
                        None => tokio::task::spawn_blocking(move || g.index(None)).await,
                    };
                    match result {
                            Ok(Ok(stats)) => eprintln!(
                                "codegraph: indexed {} files ({} re-parsed, {} symbols, {} edges) in {}ms",
                                stats.files_scanned,
                                stats.files_reindexed,
                                stats.symbols,
                                stats.edges,
                                stats.elapsed_ms
                            ),
                            Ok(Err(e)) => eprintln!("codegraph: index failed: {e}"),
                            Err(e) => eprintln!("codegraph: index task panicked: {e}"),
                        }
                });
                Some(graph)
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to open codegraph at {}: {e}; graph disabled",
                    workspace.state_dir
                    .join("codegraph.db")
                    .display()
                );
                None
            }
        }
    } else {
        None
    };

    let factory = AgentLoopFactory::new(
        provider,
        constitution_source,
        Some(store_trait),
        sandbox.clone(),
        workspace.root.clone(),
        Some(safety_rules.clone()),
        safety_mode.clone(),
        context_manager,
        workspace.state_dir.join("plans"),
        vision,
    )
    // Wire the per-context model resolver so `[models]` overrides take effect
    // at turn time (skill > subagent > state > default).
    .with_model_resolver(model_resolver.clone())
    // Wire the shared git core-operations handle so a `[git]` config save is
    // observed live by every GitTool (no registry rebuild).
    .with_core_operations(core_operations)
    // Wire the shared shell-output filter config so a `[shell_filter]` config
    // save is observed live by every ShellTool (no registry rebuild).
    .with_shell_filter_config(shell_filter)
    // Wire the shared token-optimizer config (backlog e4a50d22) so the
    // `[general.optimizer]` flags are observed by every lever-bearing
    // tool build (read_files delta/skeleton re-reads, shell output
    // compression, …).
    .with_optimizer_config(optimizer_config)
    // Wire the shared harness-run verify config so `[general.verify]`'s
    // command + timeout are read at each plan-step boundary by every agent
    // built from this factory (a Settings save flips the mirror live).
    .with_verify_config(verify_config)
    // Wire the shared budget config so `[general.budget]`'s caps are read at
    // each plan-step gate by every agent built from this factory (a Settings
    // save flips the mirror live).
    .with_budget_config(budget_config)
    // Wire the shared Laya auto-typing gate (backlog a147b63c):
    // memory_write reads the classifier slot + flag at call time, and
    // Settings saves flip the flag via `set_auto_typing_enabled` — no
    // registry rebuild ever needed.
    .with_auto_typing(crate::tool::memory::AutoTypingHandle {
        classifier: classifier_slot.clone(),
        enabled: auto_typing_flag.clone(),
    })
    // Wire the shared Laya tool-choice gate (backlog e2c47d5f): the search /
    // search_read tools read the classifier slot + flag at call time, and a
    // Settings save flips the flag via `set_tool_choice_enabled` — no rebuild.
    .with_tool_choice(crate::tool::agent::tool_choice::ToolChoiceHandle {
        classifier: classifier_slot.clone(),
        enabled: tool_choice_flag.clone(),
        // The app logs to the default `~/.mnemo/laya/training/` home.
        log_path: None,
    })
    // Wire the shared Laya failure-triage gate (backlog 1a4049c1): every loop
    // the factory builds carries the classifier slot + flag, so the
    // tool-dispatch auto-retry and both provider retry layers act on
    // confident classifications — and a Settings save flips the flag live via
    // `set_failure_triage_enabled` (no rebuild).
    .with_failure_triage(failure_triage_handle)
    // Wire the shared Laya pre-prompt routing gate (backlog 091e694d): every
    // loop the factory builds reads the classifier slot at turn start; the
    // opt-in flag + the [general.routing] threshold/enforce ride the live
    // config the resolver reads per turn (no rebuild, no flag mirror).
    .with_routing_gate(routing_gate)
    // Wire the shared compound-reflex handle (backlog a8495cc1) -- the
    // escalation lane ladder's classifier (backlog ad56c7bd): every loop reads
    // the same classifier slot + the `[general.laya] reflex` flag mirror at
    // each plan-step boundary, and a Settings save flips the flag live via
    // `set_reflex_enabled` (no rebuild).
    .with_reflex(reflex_handle)
    // Share the headless debug browser with the IPC layer (Browser tab).
    .with_browser(browser.clone());
    // Record the startup default's DISPLAY effort (backlog 51dab4da) —
    // stamped onto every loop the factory builds (see build_inner).
    factory.set_default_display_effort(Some(startup_display_effort));
    // Seed the on-screen browser_* gate from the loaded config: those tools
    // attach over the WebView2 CDP endpoint, which only exists when the user
    // opted in, so with the flag off they stay out of the tools array
    // entirely rather than advertising a guaranteed failure.
    factory.set_browser_inspection(config.general.general.enable_browser_inspection);
    // Wire the code knowledge graph when enabled + opened successfully.
    let factory = match codegraph.clone() {
        Some(graph) => factory.with_codegraph(graph),
        None => factory,
    };
    // Wire the shared MCP manager: the GLOBAL set (config.mcp) union-merged
    // with this project's `.coding/mcp.toml` overrides (project wins by
    // name — same philosophy as skills). Inert by construction: connections
    // happen lazily on the first load_tools("mcp.<server>") reveal; the IPC
    // layer's Settings/Test commands reuse the same Arc.
    let project_mcp = crate::config::mcp::load_or_default(&workspace.state_dir.join("mcp.toml"))?;
    let mcp_servers = crate::config::mcp::merge_servers(&config.mcp, &project_mcp);
    let factory = factory.with_mcp(Arc::new(crate::mcp::McpManager::from_config(&mcp_servers)));
    let factory = Arc::new(factory);

    // Start the CodeGraph OS file watcher so the graph stays fresh on every
    // edit (agent tools, human editor, git ops) without a manual refresh. The
    // watcher debounces change bursts into incremental index() passes and is
    // best-effort: a start failure logs + disables auto-refresh, never fatal.
    // The handle MUST live on Brain — dropping it stops the watch.
    let graph_watcher = codegraph.clone().and_then(|graph| {
        match crate::codegraph::watcher::GraphWatcher::spawn(
            graph,
            workspace.root.clone(),
            std::time::Duration::from_millis(800),
        ) {
            Ok(w) => {
                eprintln!("codegraph: file watcher started (auto-refresh on edits)");
                Some(w)
            }
            Err(e) => {
                eprintln!("codegraph: file watcher failed to start: {e}; auto-refresh disabled");
                None
            }
        }
    });

    Ok(BrainOutcome::Ready(Brain {
        factory,
        memory_store: Some(store),
        workspace,
        config: Arc::new(tokio::sync::Mutex::new(config)),
        sandbox,
        safety_rules,
        safety_mode,
        model_resolver,
        trace,
        browser,
        embedder_status,
        classifier_status,
        classifier_slot,
        pending_model_download: pending_download,
        pending_model_load: pending_load,
        _graph_watcher: graph_watcher,
    }))
}
