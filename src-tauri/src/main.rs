// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo Tauri app — the native window shell.
//!
//! Wires the brain (config, project, provider, tools, workflow, runtime, agent
//! loop) to the Tauri IPC bridge and opens the webview frontend.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(warnings)]

use std::sync::{Arc, RwLock};

use mnemo::agent::context::ContextManager;
use mnemo::agent::factory::AgentLoopFactory;
use mnemo::config::{global_config_dir, Config, SafetyMode};
use mnemo::memory::embedder::HashEmbedder;
use mnemo::memory::MemoryStore;
use mnemo::project::Project;
use mnemo::provider::client_factory::{
    build_classifier, build_client, build_embedder, build_vision_client, embedder_startup_plan,
    EmbedderStartupPlan,
};
use mnemo::provider::openai::{OpenAiClient, OpenAiClientConfig};
use mnemo::provider::trace::LlmRequestLog;
use mnemo::provider::{LlmClient, ProviderKind};
use mnemo::runtime::AgentManager;
use mnemo::safety_rules::SafetyRules;
use mnemo::tool::agent::sandbox::Sandbox;
use tauri::{Emitter, Manager, PhysicalPosition, WebviewBuilder, WebviewUrl, WindowBuilder};

mod console;
mod ipc;
mod startup;
mod watchdog;
mod webview_udf;

use ipc::IpcState;
use ipc::PendingApprovals;
use ipc::PendingQuestions;

/// Read just the `enable_browser_inspection` flag from `config.toml`, without
/// loading the full [`Config`] (which reads four files). Used at the very top
/// of [`main`], before WebView2 is created, to decide whether to expose the
/// CDP debug port in a release build. Returns `false` on any read/parse error
/// (fail closed — never expose the unauthenticated port unless the opt-in is
/// clearly present). Debug builds ignore this and always expose CDP.
#[cfg(windows)]
fn browser_inspection_enabled() -> bool {
    let path = global_config_dir().join("config.toml");
    mnemo::config::GeneralConfig::load_or_default(&path)
        .map(|c| c.general.enable_browser_inspection)
        .unwrap_or(false)
}

/// Install the app-layer child-webview ensurer on a shared [`BrowserManager`]
/// (plan 5ae26d22): the agent's `browser_navigate` invokes this hook when no
/// child CDP target exists, creating the child webview (hidden) + emitting
/// `browser://reveal` so the frontend surfaces the Browser tab. The closure
/// clones the `AppHandle` per call and resolves `IpcState` lazily at ensure
/// time — the state is managed during setup, before any agent can run.
fn attach_child_ensurer(browser: &mnemo::browser::BrowserManager, app: tauri::AppHandle) {
    browser.set_child_ensurer(std::sync::Arc::new(move |url: String| {
        let app = app.clone();
        Box::pin(async move {
            let Some(state) = app.try_state::<IpcState>() else {
                return Err("app state not yet available".into());
            };
            ipc::browser_webview::ensure_for_agent_impl(&app, state.inner(), &url)
                .await
                .map_err(|e| e.message)
        })
    }));
}

/// Adopt the login shell's `$PATH` (Unix only).
///
/// macOS GUI apps launched from Finder/Dock do NOT inherit the user's shell
/// environment (no `.zshrc`/`.zprofile`), so `PATH` stays the bare system
/// default (`/usr/bin:/bin:/usr/sbin:/sbin`) — the agent's `sh -c` tool would
/// then fail to find `cargo`/`npm`/`node`/Homebrew `git` inside a shipped
/// `.app`. Run the user's login shell once at startup (before any child
/// process spawns) and adopt its resolved `PATH`. Best-effort: any failure
/// (no `$SHELL`, spawn error, non-adoptable output) keeps the inherited PATH.
/// Accepted trade-off: a dotfile that echoes to stdout pollutes the captured
/// value (the rc output shares stdout with the `printf`) — the
/// control-character check in [`is_adoptable_path`] rejects the obvious
/// cases; a same-line echo cannot be distinguished. In-tree equivalent of
/// the fix-path-env approach — implemented here dependency-free so the
/// macOS CI build has no extra crate-name risk.
#[cfg(unix)]
fn inherit_shell_path() {
    if let Some(shell) = std::env::var_os("SHELL") {
        if let Ok(output) = std::process::Command::new(&shell)
            .args(["-l", "-c", "printf %s \"$PATH\""])
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if is_adoptable_path(&path) {
                    std::env::set_var("PATH", path);
                } else {
                    eprintln!(
                        "mnemo: keeping the inherited PATH — the login shell's \
                         PATH was empty or contained control characters"
                    );
                }
            }
        }
    }
}

/// Whether a captured login-shell PATH value is safe to adopt.
///
/// Delegates to [`mnemo::shell_path::is_adoptable_path`], which lives in
/// platform-neutral library code so every platform's tests guard it (the
/// predicate was once `#[cfg(unix)]`-only here, which hid a whitespace-only
/// regression from the Windows leg — the macOS CI failure of 2026-09-21).
#[cfg(unix)]
fn is_adoptable_path(value: &str) -> bool {
    mnemo::shell_path::is_adoptable_path(value)
}

/// Non-Unix: nothing to do — Windows GUI apps inherit the user PATH.
#[cfg(not(unix))]
fn inherit_shell_path() {}

fn main() {
    // Console mode — run the brain as a terminal REPL, never touching
    // WebView2/Tauri. Checked FIRST, before anything else in main(): a
    // release build is a GUI-subsystem binary, so a terminal launch starts
    // with NULL std handles — console::attach_console() must AllocConsole
    // a window of its own and rewire stdin/stdout/stderr BEFORE the first
    // print anywhere in the process (Rust caches its stdio handles on
    // first use; the migration call below eprintln's). It deliberately
    // does NOT AttachConsole the parent: PowerShell/cmd keep reading a
    // shared console and steal keystrokes. The console path then skips
    // all GUI initialization (the WEBVIEW2 env var below, the builder,
    // the window). Accepts both `-console` and `--console`; project
    // resolution inside build_brain also honors `--project <path>` + the
    // cwd walk, same as the GUI. `--console` matches the `--project` flag
    // spelling (two dashes).
    if startup::console_mode_requested(std::env::args()) {
        console::attach_console();
        // Migrate the legacy pre-rename config dir (~/.myharness →
        // ~/.mnemo) before build_brain reads config — the same requirement
        // the GUI path below satisfies, ordered after the attach above
        // because it can eprintln. No-op on fresh installs and after a
        // successful migration.
        mnemo::config::migrate_legacy_config_dir();
        mnemo::app::install_panic_hook();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build the console tokio runtime");
        let code = rt.block_on(console::run_console());
        std::process::exit(code);
    }

    // Migrate the legacy pre-rename config dir (~/.myharness → ~/.mnemo)
    // BEFORE anything reads config (browser_inspection_enabled,
    // build_brain). No-op on fresh installs and after a successful
    // migration.
    mnemo::config::migrate_legacy_config_dir();

    // Unix (macOS): adopt the login shell's $PATH before any child process
    // spawns (the shell tool, git, the headless browser). No-op on Windows.
    inherit_shell_path();

    // Expose the WebView2 Chrome DevTools Protocol on the per-instance CDP
    // port (9222 when free — see `pick_cdp_port`) so the agent
    // can attach to the *live* WebView2 (the Browser tab's child webview —
    // the one the human browses/plays in)
    // and inspect its state. WebView2 reads this env var at creation time and
    // merges it with `additionalBrowserArgs` in tauri.conf.json.
    //
    // `--disable-extensions` is set unconditionally (hardening — was always-on
    // in the old tauri.conf.json window's `additionalBrowserArgs`). The CDP
    // port is appended only in debug builds, or release builds where the user
    // opted in via `enable_browser_inspection` — the endpoint is
    // unauthenticated, so any local process could run arbitrary JS / read the
    // DOM of the app's webview. The opt-in makes that risk explicit. The env
    // var MUST be set before WebView2 is created (it reads it at creation
    // time), so this runs before the Tauri builder.
    //
    // Windows-only block: `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` is a
    // WebView2 concept (ignored by wry on other platforms) and the flags
    // below address Windows-specific behavior. Gating the block also keeps
    // `browser_inspection_enabled` alive on Windows targets only, matching
    // its own `#[cfg(windows)]`.
    //
    // MNEMO_DISABLE_WIN_OCCLUSION=1 is an opt-in escape hatch for RDP
    // users (fix #4 of .coding/reviews/2026-08-18-rdp-freeze-diagnosis.md):
    // it appends --disable-features=CalculateNativeWinOcclusion so the
    // renderer stays unthrottled while the window is occluded (no
    // buffered-event catch-up burst) at the cost of background CPU/GPU while
    // hidden.
    #[cfg(windows)]
    {
        // Pick this instance's CDP port BEFORE the env var is built, and
        // record it so BrowserManager (constructed later, after the
        // builder) attaches to the same port the env var exposes — a
        // second instance whose WebView2 cannot bind 9222 gets its own
        // port instead of silently attaching to the first instance's
        // WebView2 (backlog 55ba23b1).
        let cdp_port = if cfg!(debug_assertions) || browser_inspection_enabled() {
            let port = mnemo::webview_args::pick_cdp_port();
            mnemo::webview_args::set_cdp_port(port);
            Some(port)
        } else {
            None
        };
        let args = mnemo::webview_args::build_webview2_args(
            cdp_port,
            mnemo::webview_args::env_flag_enabled(std::env::var_os("MNEMO_DISABLE_WIN_OCCLUSION")),
        );
        std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
    }

    mnemo::app::install_panic_hook();
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Sweep orphaned browser profile dirs left behind by crashed
            // prior runs (dead managers' stale markers, legacy >1h dirs) —
            // the in-session watchdog re-sweeps periodically; see
            // `BrowserManager::sweep_orphan_profiles`. Runs on every startup
            // path, even if this session never launches a browser.
            let _ = tauri::async_runtime::spawn_blocking(
                mnemo::browser::BrowserManager::sweep_orphan_profiles,
            );

            // Path (a): bare WindowBuilder + two ordered add_child webviews.
            // The window itself carries no webview; the agent-chat (React) and
            // browser-tab children are added below. Creating the agent-chat
            // webview first means its WebView2 env binds the CDP port;
            // the browser-tab child (created lazily by browser_webview.rs)
            // shares that env. A renderer crash in either child leaves the
            // other alive (proven by spike e9cd6fb1).
            let agent_chat_handle: Arc<std::sync::Mutex<Option<tauri::webview::Webview>>> =
                Arc::new(std::sync::Mutex::new(None));
            // A switch restart reloads in place of a window the user just
            // closed: `switch_project` writes the pending-project marker and
            // hard-exits (tauri::process::restart). Windows hands the
            // foreground to another app when the old window is destroyed, and
            // the relaunched process has no foreground right of its own (its
            // parent is dead) — without this the restarted window opens
            // BEHIND whatever took focus. set_focus (tao force_window_active:
            // SetForegroundWindow + the ALT-key fallback) brings it back to
            // the top, matching the window it replaces. Peeking does not
            // consume the marker — build_brain below still owns it.
            let switched_restart = mnemo::config::peek_pending_project().is_some();
            let window = WindowBuilder::new(app, "main")
                .title("Mnemo")
                .inner_size(1200.0, 720.0)
                .min_inner_size(800.0, 560.0)
                .center()
                .build()?;
            if switched_restart {
                if let Err(e) = window.set_focus() {
                    eprintln!("mnemo: failed to focus the restarted window: {e}");
                }
            }
            // Per-instance WebView2 user data folder: the FIRST instance
            // keeps WebView2's default profile; every ADDITIONAL instance
            // gets its own per-pid folder (see webview_udf.rs — without it a
            // second instance's session never initializes and its window is
            // a blank white frame, 2027-01-13). MUST run before the first
            // webview: WebView2 bakes the folder into its environment at
            // creation time.
            webview_udf::install_default_data_dir(app.handle(), std::process::id());

            // The agent-chat webview fills the whole window (the browser-tab
            // child is positioned over its placeholder rect by browser_webview.rs).
            let agent_chat_builder = webview_udf::apply(
                WebviewBuilder::new("agent-chat", WebviewUrl::App("index.html".into())),
                "agent-chat",
            );
            let agent_chat = window.add_child(
                agent_chat_builder,
                PhysicalPosition::new(0, 0),
                window.inner_size()?,
            )?;
            let _ = agent_chat.show();
            *agent_chat_handle
                .lock()
                .expect("agent_chat handle poisoned") = Some(agent_chat.clone());

            // Keep the agent-chat webview filling the window on resize (an
            // add_child webview does not auto-fill like a WebviewWindow does)
            // — but NEVER call `set_size` on the event loop: a WebView2
            // controller call can block on a stalled compositor when an RDP
            // session is switched (upstream WebView2Feedback #3581), which
            // froze the whole app (R1, .coding/reviews/2026-08-18-rdp-freeze-
            // diagnosis.md). Resizes go to a latest-wins worker thread
            // instead: intermediate sizes coalesce, and a wedged call parks
            // only that thread. The worker's clone keeps the webview handle
            // alive until process exit, so the RunEvent::Exit drop of the
            // state handle is no longer the last reference — the OS reaps the
            // HWND at process termination moments later.
            let resize_sink = agent_chat.clone();
            let resize_worker =
                mnemo::thread_util::spawn_latest_wins(move |size: tauri::PhysicalSize<u32>| {
                    let _ = resize_sink.set_size(size);
                });
            // `on_window_event` lives on `Window` (not `WindowBuilder`), so it
            // is attached after the window is built.
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::Resized(size) = event {
                    resize_worker.submit(*size);
                }
            });

            // Try to build the brain. If it fails, we still open the window —
            // the error is surfaced in the UI as a startup-error screen so the
            // user can fix the code and restart. This prevents the
            // chicken-and-egg problem of the app crashing when you're using it
            // to code on itself.
            let brain_result = build_brain(Some(app.handle().clone()));

            let pending_approvals = Arc::new(PendingApprovals::new());
            let pending_questions = Arc::new(PendingQuestions::new());

            // The backlog is UI state that must work even when the brain
            // failed to build, so resolve the project root independently of
            // the brain result (mirroring the Err path's cwd fallback) and
            // open the store before the match.
            let backlog_root = match &brain_result {
                Ok(BrainOutcome::Ready(brain)) => tauri::async_runtime::block_on(async {
                    brain.project.lock().await.root.clone()
                }),
                // NeedsProject + Err: no project root — fall back to cwd. The
                // backlog store tolerates a missing path (opens empty), and on
                // the NeedsProject path no agent can run so nothing is added.
                _ => std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            };

            // Same-project instance conflict + marker (2027-01-13):
            // `.coding/instance.json` in the project records which instance
            // launched last on it. The conflict is read BEFORE this
            // instance's marker overwrites the incumbent's, so a second
            // instance on a LIVE project can warn the user before opening.
            // Both are best-effort — a read-only project must still open
            // (the marker write is `let _ =`).
            let instance_conflict: Option<ipc::startup::InstanceConflict> =
                if matches!(&brain_result, Ok(BrainOutcome::Ready(_))) {
                    use mnemo::instance_marker::{conflict_for, InstanceMarker};

                    let conflict = conflict_for(
                        &backlog_root,
                        std::process::id(),
                        startup::instance_pid_alive,
                    );
                    let _ = InstanceMarker::new_self().write(&backlog_root);
                    conflict.map(|m| ipc::startup::InstanceConflict {
                        pid: m.pid,
                        started_at: m.started_at,
                    })
                } else {
                    None
                };

            // Set the OS window title to the product name + the loaded
            // project's folder name, so the user can tell which project is
            // open at a glance (e.g. "Mnemo — myproject"). The static
            // title in tauri.conf.json is the initial/fallback shown before
            // setup runs. On the NeedsProject path there is no project yet,
            // so the title is just the product name. `backlog_root` holds the
            // resolved project root in the Ready branch (and cwd otherwise),
            // and `join` below only borrows it, so reading its file name here
            // is safe.
            if let Some(window) = app.get_window("main") {
                let needs_project =
                    matches!(&brain_result, Ok(BrainOutcome::NeedsProject(_)));
                let _ = window.set_title(&startup::window_title(needs_project, &backlog_root));
            }

            let backlog = Arc::new(tokio::sync::Mutex::new(mnemo::backlog::BacklogStore::open(
                backlog_root
                    .join(Project::CODING_DIR_NAME)
                    .join("backlog.jsonl"),
            )));

            match &brain_result {
                Ok(BrainOutcome::Ready(brain)) => {
                    // Create the agent manager + per-agent loop map, wrapped in
                    // Arcs up front so the spawner can share them.
                    let mut mgr = AgentManager::new(256);

                    // Take the fan-in receiver from the bare manager before
                    // wrapping it in an Arc<Mutex> (the forwarder owns it).
                    let fanin_rx = mgr.take_fanin_rx().expect("fan-in receiver already taken");

                    let manager = Arc::new(tokio::sync::Mutex::new(mgr));
                    let agent_loops =
                        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

                    // Wire the spawn_agent tool's spawner into the factory BEFORE
                    // building the main agent, so every agent (including main) can
                    // start background agents. The spawner shares the manager +
                    // loop map + factory with the IPC commands, and holds the app
                    // handle so it can emit a PromptDispatched event for the
                    // spawned agent's task (showing it as the first prompt).
                    //
                    // The same `IpcSpawner` is wired as the DescendantTracker
                    // (it implements both traits) so the dispatch layer can gate
                    // workflow-state transitions on "no spawned subagents
                    // running". Build it once and hand clones to both setters.
                    let spawner = Arc::new(ipc::spawn::IpcSpawner::new(
                        brain.factory.clone(),
                        manager.clone(),
                        agent_loops.clone(),
                        app.handle().clone(),
                    ));
                    brain.factory.set_spawner(spawner.clone());
                    brain.factory.set_descendant_tracker(spawner);

                    // Share the ONE backlog store Arc (created above, before
                    // the match) between the IPC `backlog_*` commands
                    // (state.backlog.store) and the agent's `backlog_add`
                    // tool — both go through the same mutex, so agent adds
                    // and UI mutations stay consistent and can never clobber
                    // each other. Wired before the main agent is built (the
                    // tool is read per registry build). No backlog wiring on
                    // the NeedsProject/Err paths — no factory, no agent.
                    brain.factory.set_backlog(Arc::clone(&backlog));

                    // The UI seam for agent-side adds: the library crate has
                    // no Tauri dependency, so the factory's notifier callback
                    // is the bridge. After every successful `backlog_add`
                    // the closure re-emits the current backlog state on
                    // `backlog://changed` (the same event the IPC commands
                    // fire), so the Backlog tab updates live instead of
                    // waiting for a refresh/restart. Runs at tool-execution
                    // time, when IpcState is already managed (app.manage
                    // below) — the spawn is fire-and-forget on the Tauri
                    // async runtime.
                    {
                        let app_handle = app.handle().clone();
                        let notifier: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                            let app = app_handle.clone();
                            tauri::async_runtime::spawn(async move {
                                let state = app.state::<IpcState>();
                                crate::ipc::backlog_cmds::emit_backlog_changed(&app, &state).await;
                            });
                        });
                        brain.factory.set_backlog_notifier(notifier);
                    }

                    // Build the first ("main") agent via the factory so it gets
                    // its own workflow + tool registry (now including spawn_agent).
                    // This uses the SAME shared spawn path as the UI `spawn_agent`
                    // command and the IpcSpawner tool (Maint M3) — id allocation,
                    // channel setup, loop-map insert, and registration are
                    // identical. The setup closure is sync, so briefly block_on
                    // (the locks are uncontended at startup).
                    tauri::async_runtime::block_on(async {
                        ipc::spawn::spawn_agent_shared(
                            brain.factory.clone(),
                            manager.clone(),
                            agent_loops.clone(),
                            "main".to_string(),
                            None,
                            None,
                            None,  // no forced model — the main agent uses the default
                            None,  // no role — the main agent is unrestricted
                            false, // own_plans_dir — main keeps .coding/plans/
                            None,  // no worktree binding — the main agent works the main tree
                        )
                        .await
                        .expect("main agent spawn must succeed");
                    });

                    // Spawn the event forwarder — takes ownership of the
                    // fan-in receiver, a clone of the manager Arc, and a
                    // clone of the per-agent loops map (so it can remove dead
                    // agents' loops on Exited).
                    ipc::spawn_event_forwarder(
                        app.handle().clone(),
                        fanin_rx,
                        manager.clone(),
                        pending_approvals.clone(),
                        pending_questions.clone(),
                        agent_loops.clone(),
                    );

                    // Push live console events from the shared browser to the
                    // Browser tab as `browser://console`.
                    ipc::browser::spawn_console_forwarder(
                        app.handle().clone(),
                        brain.browser.clone(),
                    );

                    // Start the hang watchdog (main thread, after the brain
                    // is wired): from here on, any main-thread stall — agent
                    // runs, IPC, frontend event floods — writes an evidence
                    // report into .coding/logs/hang-*.txt.
                    let watchdog = watchdog::Watchdog::start(
                        app.handle().clone(),
                        backlog_root.join(Project::CODING_DIR_NAME).join("logs"),
                    );

                    app.manage(IpcState {
                        runtime: ipc::state::AgentRuntimeContext {
                            manager,
                            agent_loops,
                            context_usage: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            factory: Some(brain.factory.clone()),
                            memory_store: brain.memory_store.clone(),
                            safety_mode: brain.safety_mode.clone(),
                            safety_rules: Some(brain.safety_rules.clone()),
                            model_resolver: Some(brain.model_resolver.clone()),
                            browser: brain.browser.clone(),
                            startup_error: None,
                            needs_project: false,
                            embedder_status: brain.embedder_status.clone(),
                            classifier_status: brain.classifier_status.clone(),
                            classifier: Arc::new(RwLock::new(brain.classifier.clone())),
                            laya: brain.laya.clone(),
                            instance_conflict,
                        },
                        project: ipc::state::ProjectContext {
                            root: brain.project.clone(),
                            config: brain.config.clone(),
                            sandbox: brain.sandbox.clone(),
                        },
                        approvals: pending_approvals.clone(),
                        questions: pending_questions.clone(),
                        trace: brain.trace.clone(),
                        browser_webview: Arc::new(
                            crate::ipc::browser_webview::BrowserWebviewShared::new(),
                        ),
                        agent_chat_webview: agent_chat_handle.clone(),
                        watchdog: Some(watchdog),
                        backlog: ipc::state::BacklogContext {
                            store: backlog.clone(),
                            auto_feed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            parallel_run_all: Arc::new(std::sync::atomic::AtomicBool::new(
                                false,
                            )),
                            run_all: Arc::new(tokio::sync::Mutex::new(None)),
        run_completion_note: Arc::new(std::sync::Mutex::new(None)),
                            single_in_flight: Arc::new(std::sync::Mutex::new(None)),
                            user_intervention: Arc::new(std::sync::Mutex::new(None)),
                            compact_signal: Arc::new(tokio::sync::watch::channel(0).0),
                            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            run_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    });

                    // The embedder status was already determined by
                    // `build_embedder` during brain construction (Ready for a
                    // loaded bundled model or hash mode; Failed when a
                    // configured model fails to load). Emit it once so the
                    // frontend's startup poll + banner reflect the real state.
                    // (The legacy Ollama/cloud probe was removed with the
                    // remote-embedder path — the bundled path has no probe.)
                    {
                        let app_handle = app.handle().clone();
                        let status_lock = app.state::<IpcState>().runtime.embedder_status.clone();
                        let status = status_lock
                            .read()
                            .expect("embedder status lock poisoned")
                            .clone();
                        let _ = app_handle.emit("embedder://status", &status);
                    }

                    // The classifier status was already determined by
                    // `build_classifier` during brain construction (Disabled
                    // unless Laya is enabled with an endpoint, Ready
                    // otherwise). Emit it once so the Settings → Classifier
                    // section reflects the real state on mount.
                    {
                        let app_handle = app.handle().clone();
                        let status_lock = app.state::<IpcState>().runtime.classifier_status.clone();
                        let status = status_lock
                            .read()
                            .expect("classifier status lock poisoned")
                            .clone();
                        let _ = app_handle.emit("classifier://status", &status);
                    }

                    // First-run model download: the configured bundled model
                    // wasn't installed, so the store started on the hash
                    // embedder. Download in the background (progress via
                    // embedder://status); on success the task installs the
                    // real embedder into the live store + re-embeds rows, so
                    // semantic recall goes live without a restart.
                    if let Some(model) = &brain.pending_model_download {
                        if let Some(store) = &brain.memory_store {
                            ipc::embeddings::spawn_startup_download(
                                app.handle().clone(),
                                store.clone(),
                                brain.embedder_status.clone(),
                                model.clone(),
                            );
                        }
                    }

                    // Installed-model local load: the model is on disk, so no
                    // download — but its ONNX load was deferred off the main
                    // thread. Load in the background and swap into the live
                    // store + re-embed rows (the same install path as the
                    // download, minus the network).
                    if let Some(model) = &brain.pending_model_load {
                        if let Some(store) = &brain.memory_store {
                            ipc::embeddings::spawn_startup_load(
                                app.handle().clone(),
                                store.clone(),
                                brain.embedder_status.clone(),
                                model.clone(),
                            );
                        }
                    }

                    // Managed Laya: the classifier client already points at
                    // the pre-allocated loopback port (build_brain); spawn
                    // the sidecar now and drive the status to Ready/Failed
                    // (progress on classifier://status).
                    if let Some((checkpoint_id, port)) = &brain.pending_laya_start {
                        let app_handle = Some(app.handle().clone());
                        let manager = brain.laya.clone();
                        let checkpoint = ipc::laya::find_checkpoint(checkpoint_id);
                        let port = *port;
                        tauri::async_runtime::spawn(async move {
                            ipc::laya::start_managed_server(
                                &app_handle,
                                manager,
                                checkpoint,
                                port,
                            )
                            .await;
                        });
                    }
                }
                Ok(BrainOutcome::NeedsProject(config)) => {
                    // No project resolved at startup — show the project picker.
                    // The window is open and the config is loaded (so the
                    // picker can list registered projects), but no agent can
                    // run until a project is chosen. Build a minimal IpcState
                    // mirroring the startup-error fallback, but with
                    // needs_project=true and startup_error=None so the
                    // frontend shows the picker (not the error screen).
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let fallback_project = Project::from_root(&cwd);
                    let fallback_sandbox = Sandbox::new(&cwd)
                        .unwrap_or_else(|_| Sandbox::new(std::path::Path::new(".")).unwrap());
                    let fallback_safety = Arc::new(RwLock::new(SafetyMode::default()));
                    let mgr = AgentManager::new(256);
                    // A shared Disabled classifier status + manager pair —
                    // managed Laya stays inert without a brain, but the
                    // Settings catalog + setup still work.
                    let (fallback_classifier_status, laya_manager) =
                        ipc::laya::fresh_disabled();
                    let fallback_browser = mnemo::browser::BrowserManager::new();
                    attach_child_ensurer(&fallback_browser, app.handle().clone());
                    ipc::browser::spawn_console_forwarder(
                        app.handle().clone(),
                        fallback_browser.clone(),
                    );

                    // Hang watchdog with a temp-dir fallback: no project is
                    // open, so .coding/logs doesn't exist yet — but a hang in
                    // the picker still deserves an evidence report.
                    let watchdog = watchdog::Watchdog::start(
                        app.handle().clone(),
                        std::env::temp_dir().join("mnemo-hang-reports"),
                    );

                    app.manage(IpcState {
                        runtime: ipc::state::AgentRuntimeContext {
                            manager: Arc::new(tokio::sync::Mutex::new(mgr)),
                            agent_loops: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            context_usage: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            factory: None,
                            memory_store: None,
                            safety_mode: fallback_safety,
                            safety_rules: None,
                            model_resolver: None,
                            browser: fallback_browser,
                            startup_error: None,
                            needs_project: true,
                            instance_conflict: None,
                            embedder_status: Arc::new(RwLock::new(
                                mnemo::memory::embedder::EmbedderStatus::Ready,
                            )),
                            classifier_status: fallback_classifier_status,
                            classifier: Arc::new(RwLock::new(None)),
                            laya: laya_manager,
                        },
                        project: ipc::state::ProjectContext {
                            root: Arc::new(tokio::sync::Mutex::new(fallback_project)),
                            // `brain_result` is matched by reference (the Ready
                            // arm borrows `brain`), so `config` is a `&Config`
                            // here and must be cloned — it cannot be moved out.
                            config: Arc::new(tokio::sync::Mutex::new(config.clone())),
                            sandbox: Arc::new(fallback_sandbox),
                        },
                        approvals: pending_approvals.clone(),
                        questions: pending_questions.clone(),
                        trace: Arc::new(LlmRequestLog::new()),
                        browser_webview: Arc::new(
                            crate::ipc::browser_webview::BrowserWebviewShared::new(),
                        ),
                        agent_chat_webview: agent_chat_handle.clone(),
                        watchdog: Some(watchdog),
                        backlog: ipc::state::BacklogContext {
                            store: backlog.clone(),
                            auto_feed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            parallel_run_all: Arc::new(std::sync::atomic::AtomicBool::new(
                                false,
                            )),
                            run_all: Arc::new(tokio::sync::Mutex::new(None)),
        run_completion_note: Arc::new(std::sync::Mutex::new(None)),
                            single_in_flight: Arc::new(std::sync::Mutex::new(None)),
                            user_intervention: Arc::new(std::sync::Mutex::new(None)),
                            compact_signal: Arc::new(tokio::sync::watch::channel(0).0),
                            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            run_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    });
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    eprintln!("failed to build brain: {msg}");

                    // Construct a minimal fallback state so commands don't
                    // panic. The frontend will show the startup-error screen.
                    let cwd =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let fallback_project = Project::from_root(&cwd);
                    let fallback_sandbox = Sandbox::new(&cwd)
                        .unwrap_or_else(|_| Sandbox::new(std::path::Path::new(".")).unwrap());
                    let fallback_config = Arc::new(tokio::sync::Mutex::new(Config::default()));
                    // A shared Disabled classifier status + manager pair —
                    // managed Laya stays inert without a brain, but the
                    // Settings catalog + setup still work.
                    let (fallback_classifier_status, laya_manager) =
                        ipc::laya::fresh_disabled();
                    let fallback_safety = Arc::new(RwLock::new(SafetyMode::default()));
                    let mgr = AgentManager::new(256);
                    // The brain failed to build, so there is no shared
                    // browser from the factory — create a fresh manager so
                    // the Browser tab still works (it spawns its own
                    // Chromium on first use).
                    let fallback_browser = mnemo::browser::BrowserManager::new();
                    attach_child_ensurer(&fallback_browser, app.handle().clone());
                    ipc::browser::spawn_console_forwarder(
                        app.handle().clone(),
                        fallback_browser.clone(),
                    );

                    // Hang watchdog with a temp-dir fallback: the brain
                    // failed to build, so no project logs dir — but a hang on
                    // the startup-error screen still deserves evidence.
                    let watchdog = watchdog::Watchdog::start(
                        app.handle().clone(),
                        std::env::temp_dir().join("mnemo-hang-reports"),
                    );

                    app.manage(IpcState {
                        runtime: ipc::state::AgentRuntimeContext {
                            manager: Arc::new(tokio::sync::Mutex::new(mgr)),
                            agent_loops: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            context_usage: Arc::new(tokio::sync::Mutex::new(
                                std::collections::HashMap::new(),
                            )),
                            factory: None,
                            memory_store: None,
                            safety_mode: fallback_safety,
                            safety_rules: None,
                            model_resolver: None,
                            browser: fallback_browser,
                            startup_error: Some(msg),
                            needs_project: false,
                            instance_conflict: None,
                            embedder_status: Arc::new(RwLock::new(
                                mnemo::memory::embedder::EmbedderStatus::Ready,
                            )),
                            classifier_status: fallback_classifier_status,
                            classifier: Arc::new(RwLock::new(None)),
                            laya: laya_manager,
                        },
                        project: ipc::state::ProjectContext {
                            root: Arc::new(tokio::sync::Mutex::new(fallback_project)),
                            config: fallback_config,
                            sandbox: Arc::new(fallback_sandbox),
                        },
                        approvals: pending_approvals,
                        questions: pending_questions,
                        // No brain → no providers to capture, but the Trace tab
                        // still needs a live (empty) log to list/clear.
                        trace: Arc::new(LlmRequestLog::new()),
                        browser_webview: Arc::new(
                            crate::ipc::browser_webview::BrowserWebviewShared::new(),
                        ),
                        agent_chat_webview: agent_chat_handle.clone(),
                        watchdog: Some(watchdog),
                        backlog: ipc::state::BacklogContext {
                            store: backlog,
                            auto_feed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            parallel_run_all: Arc::new(std::sync::atomic::AtomicBool::new(
                                false,
                            )),
                            run_all: Arc::new(tokio::sync::Mutex::new(None)),
        run_completion_note: Arc::new(std::sync::Mutex::new(None)),
                            single_in_flight: Arc::new(std::sync::Mutex::new(None)),
                            user_intervention: Arc::new(std::sync::Mutex::new(None)),
                            compact_signal: Arc::new(tokio::sync::watch::channel(0).0),
                            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                            run_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        },
                    });
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::agent::send_prompt,
            ipc::agent::send_suggestion,
            ipc::agent::cancel_suggestion,
            ipc::agent::interrupt,
            ipc::agent::cancel,
            ipc::agent::compact,
            ipc::agent::clear_conversation,
            ipc::agent::approve,
            ipc::agent::answer_question,
            ipc::agent::set_safety_mode,
            ipc::agent::get_safety_mode,
            ipc::agent::get_safety_rules,
            ipc::agent::save_safety_rules,
            ipc::agent::add_safety_rule,
            ipc::agent::add_safety_rule_broad,
            ipc::agent::add_safety_rule_class,
            ipc::agent::get_startup_error,
            ipc::agent::list_agents,
            ipc::agent::ui_diag,
            ipc::agent::context_caps,
            ipc::startup::startup_snapshot,
            ipc::spawn::spawn_agent,
            ipc::agent::set_model,
            ipc::agent::get_workflow_state,
            ipc::agent::get_plan,
            ipc::agent::enter_skill,
            ipc::settings::get_settings,
            ipc::settings::save_settings,
            ipc::settings::get_embedder_status,
            ipc::settings::get_classifier_status,
            ipc::embeddings::list_bundled_embedding_models,
            ipc::embeddings::download_bundled_model,
            ipc::laya::laya_catalog,
            ipc::laya::laya_setup,
            ipc::keys::get_api_keys,
            ipc::mcp::mcp_list_servers,
            ipc::mcp::mcp_save_servers,
            ipc::mcp::mcp_oauth_start,
            ipc::mcp::mcp_status,
            ipc::mcp::mcp_test,
            ipc::models::list_models,
            ipc::models::list_vision_models,
            ipc::settings::save_endpoints,
            ipc::projects::list_projects,
            ipc::projects::create_project,
            ipc::projects::switch_project,
            ipc::projects::remove_project,
            ipc::projects::pick_directory,
            ipc::projects::get_needs_project,
            ipc::trace::list_llm_requests,
            ipc::trace::get_llm_request,
            ipc::trace::clear_llm_requests,
            ipc::trace::get_trace_logging,
            ipc::trace::set_trace_logging,
            ipc::trace::get_steering_stats,
            ipc::memory_debug::memory_debug_overview,
            ipc::memory_debug::memory_debug_list,
            ipc::memory_debug::memory_debug_recall,
            ipc::memory_debug::memory_access_log,
            ipc::memory_debug::memory_debug_resolve_link,
            ipc::memory_debug::memory_debug_backlinks,
            ipc::memory_maintenance::memory_cleanup,
            ipc::memory_maintenance::memory_rebuild_search,
            ipc::memory_maintenance::memory_rebuild_index,
            ipc::memory_maintenance::memory_index_status,
            ipc::codegraph_cmds::codegraph_status,
            ipc::codegraph_cmds::codegraph_refresh,
            ipc::codegraph_cmds::codegraph_graph,
            ipc::codegraph_cmds::get_index_progress,
            ipc::codegraph_cmds::codegraph_rebuild_index,
            ipc::agent::get_session_stats,
            ipc::agent::get_project_stats,
            ipc::agent::get_session_list,
            ipc::files::read_file,
            ipc::files::read_image_data_url,
            ipc::files::write_file,
            ipc::files::list_files,
            ipc::files::git_diff_head,
            ipc::files::git_init,
            ipc::files::git_history,
            ipc::files::browse_markdown_file,
            ipc::files::get_git_branch,
            ipc::files::save_conversation,
            ipc::files::load_conversation,
            ipc::browser::browser_pages,
            ipc::browser::browser_screenshot_latest,
            ipc::browser::browser_console,
            ipc::browser::browser_open,
            ipc::browser::browser_normalize_url,
            ipc::browser_webview::browser_webview_supported,
            ipc::browser_webview::browser_webview_ensure,
            ipc::browser_webview::browser_webview_ensure_for_agent,
            ipc::browser_webview::browser_webview_set_rect,
            ipc::browser_webview::browser_webview_navigate,
            ipc::browser_webview::browser_webview_stop,
            ipc::browser_webview::browser_webview_reload,
            ipc::browser_webview::browser_webview_set_tab_visible,
            ipc::browser_webview::browser_webview_overlay_enter,
            ipc::browser_webview::browser_webview_overlay_exit,
            ipc::browser_webview::browser_webview_destroy,
            ipc::backlog_cmds::backlog_add,
            ipc::backlog_cmds::backlog_list,
            ipc::backlog_cmds::backlog_remove,
            ipc::backlog_cmds::backlog_reorder,
            ipc::backlog_cmds::backlog_clear_finished,
            ipc::backlog_cmds::backlog_retry,
            ipc::backlog_cmds::backlog_edit,
            ipc::backlog_cmds::backlog_set_deferred,
            ipc::backlog_cmds::backlog_set_auto_feed,
            ipc::backlog_cmds::backlog_set_parallel_run_all,
            ipc::backlog_cmds::backlog_dispatch_next,
            ipc::backlog_cmds::backlog_dispatch_item,
            ipc::backlog_cmds::backlog_run_all,
            ipc::backlog_cmds::backlog_stop_all,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Mnemo app")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                // Stop the hang watchdog FIRST: the exit teardown below
                // deliberately blocks the main thread for up to 10 s, which
                // the watchdog would otherwise (correctly) report as a stall.
                if let Some(watchdog) = &app_handle.state::<IpcState>().watchdog {
                    watchdog.stop();
                }

                // Stop the managed laya-serve sidecar — it must never
                // outlive the app (the LayaManager Drop is the backstop).
                app_handle.state::<IpcState>().runtime.laya.stop();

                // Drain any queued trace/provider-error log writes before the
                // process exits — mirroring is asynchronous (background writer
                // thread), so without this the last moments of a session's
                // log lines could be lost. Kept OUTSIDE the timed teardown
                // below: it does not touch WebView2 and must always run.
                app_handle.state::<IpcState>().trace.flush_file_writes();

                // The remaining teardown touches the headless browser /
                // WebView2, whose calls can block on a wedged compositor
                // (RDP session switch — R5 of .coding/reviews/2026-08-18-
                // rdp-freeze-diagnosis.md). Run them on a separate thread
                // with a hard timeout. The bound must exceed
                // BrowserManager::close()'s NORMAL worst case — up to ~5 s of
                // Arc::try_unwrap retries + the kill wait, plus any time
                // parked behind a state lock held by an in-flight browser
                // launch (review F1) — so only a genuine wedge hits it; a
                // spurious timeout would orphan the headless Chromium child
                // (chromiumoxide never sets kill_on_drop, and Windows does
                // not reap children on parent exit).
                let browser = app_handle.state::<IpcState>().runtime.browser.clone();
                let browser_webview = app_handle.state::<IpcState>().browser_webview.clone();
                let agent_chat = app_handle.state::<IpcState>().agent_chat_webview.clone();
                let teardown = move || {
                    // Kill the shared headless debug browser before the process
                    // exits: chromiumoxide 0.7 never sets kill_on_drop, so a bare
                    // drop at teardown would leave the Chromium child alive
                    // holding locks on its throwaway profile dir (Review L2).
                    let _ = tauri::async_runtime::block_on(browser.close());
                    // Destroy the child WebView2 (drop the handle) so no native
                    // HWND leaks on exit. Mirrors the headless-browser close above.
                    let _ = tauri::async_runtime::block_on(async move {
                        let mut bw = browser_webview.state.lock().await;
                        // Dropping the Webview handle tears down the platform
                        // webview (wry closes the child on drop).
                        bw.take_webview();
                    });
                    // Drop the agent-chat (React) child webview handle too, so
                    // no native HWND leaks on exit. try_lock, not lock: a
                    // wedged holder of this std::sync::Mutex must never block
                    // exit (defense in depth — today the resize worker holds
                    // its own Webview clone and this mutex is only ever held
                    // briefly, but that invariant is cheap to keep).
                    if let Ok(mut h) = agent_chat.try_lock() {
                        *h = None;
                    }
                };
                if !mnemo::thread_util::run_with_timeout(
                    std::time::Duration::from_secs(10),
                    teardown,
                ) {
                    eprintln!(
                        "mnemo: exit teardown exceeded 10s (wedged compositor?) — \
                         proceeding to exit; WebView2 HWNDs are reaped by process \
                         termination, but the headless Chromium child may survive"
                    );
                }
            }
        });
}

/// The built brain — everything needed to run the agent.
///
/// `pub(crate)` (with the four fields the console runtime needs also
/// `pub(crate)`) so `-console` mode can reuse the exact same construction
/// path without a Tauri app: `build_brain` is AppHandle-free by design.
pub(crate) struct Brain {
    pub(crate) factory: Arc<AgentLoopFactory>,
    /// Concrete memory store for runtime embedder rewire (Settings → Memory).
    /// `pub(crate)` so console mode can start the deferred background embedder
    /// load (the GUI's setup hook does the same; console mode has no Tauri).
    pub(crate) memory_store: Option<Arc<MemoryStore>>,
    pub(crate) project: Arc<tokio::sync::Mutex<Project>>,
    pub(crate) config: Arc<tokio::sync::Mutex<Config>>,
    sandbox: Arc<Sandbox>,
    safety_rules: Arc<SafetyRules>,
    safety_mode: Arc<RwLock<SafetyMode>>,
    /// The per-context model resolver (shared with the factory). The IPC layer
    /// pushes reloaded config into it after a Settings save so `[models]`
    /// overrides take effect on the next turn.
    model_resolver: Arc<mnemo::model_resolver::ConfigModelResolver>,
    /// The shared LLM request/response trace log — wired into the default
    /// provider + every resolver-built provider and exposed to the IPC layer
    /// (Trace tab). One instance for the whole app.
    pub(crate) trace: Arc<LlmRequestLog>,
    /// The shared headless debug browser — the same `Arc` wired into the
    /// factory (agent tools) and the IPC state (Browser tab commands).
    pub(crate) browser: Arc<mnemo::browser::BrowserManager>,
    /// The shared embedder status — the same `Arc` held by `IpcState`, so
    /// the circuit breaker's transitions surface live to the UI.
    /// `pub(crate)` so console mode can drive the deferred embedder load.
    pub(crate) embedder_status: Arc<RwLock<mnemo::memory::embedder::EmbedderStatus>>,
    /// The shared classifier status — the same `Arc` held by `IpcState`, so
    /// the enabled/disabled/failed state surfaces live to Settings. `Disabled`
    /// unless the config enables Laya with an endpoint.
    pub(crate) classifier_status: Arc<RwLock<mnemo::memory::classifier::ClassifierStatus>>,
    /// The built Laya classifier when the config enables it with an endpoint —
    /// `None` while disabled (no client is built and no call is ever made).
    /// Moved into the `IpcState` slot at startup; items 2-5 consume it there.
    pub(crate) classifier: Option<Arc<dyn mnemo::memory::classifier::Classifier>>,
    /// The managed Laya runtime owner — the same `Arc` held by `IpcState`,
    /// so the startup auto-start hook, the Settings rewire, and app exit
    /// all steer one sidecar. `pub(crate)` so those paths can reach it.
    pub(crate) laya: Arc<ipc::laya::LayaManager>,
    /// Managed Laya start work the startup hooks owe: the checkpoint id +
    /// the pre-allocated loopback port the classifier client was built
    /// against. `pub(crate)` so the GUI + console hooks can spawn the
    /// sidecar (the mirror of the deferred embedder fields above).
    pub(crate) pending_laya_start: Option<(String, u16)>,
    /// A configured bundled embedding model that wasn't installed at startup
    /// (first run): the store started on the hash embedder and the setup hook
    /// must download this model in the background, swapping it into the live
    /// store + re-embedding rows on success.
    pending_model_download: Option<String>,
    /// A configured bundled embedding model that WAS installed at startup but
    /// whose ~110 MB ONNX load was deferred off the main thread (hang
    /// diagnostics, AppHangB1 2026-08-20): the store started on the hash
    /// embedder and the setup hook must load the model in the background and
    /// swap it into the live store (mirror of the download swap).
    /// `pub(crate)` so console mode starts the same background load.
    pub(crate) pending_model_load: Option<String>,
    /// The CodeGraph OS file watcher — held only to keep the watch alive for
    /// the app's lifetime (dropping it stops watching). `None` when codegraph
    /// is disabled, failed to open, or the watcher failed to start. The field
    /// is never read by name (it exists purely for its Drop semantics), so it
    /// is underscore-prefixed rather than `#[allow(dead_code)]`ed.
    _graph_watcher: Option<mnemo::codegraph::watcher::GraphWatcher>,
}

/// The outcome of [`build_brain`]: either the brain is ready to run, or no
/// project could be resolved and the app should show the project picker.
///
/// `NeedsProject` carries the loaded global `Config` (so the picker can list
/// the registered projects from `projects.toml`) without building any of the
/// project-scoped subsystems (provider, sandbox, memory, constitution…).
pub(crate) enum BrainOutcome {
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
pub(crate) fn build_brain(app: Option<tauri::AppHandle>) -> anyhow::Result<BrainOutcome> {
    let started = std::time::Instant::now();
    let outcome = build_brain_inner(app);
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
fn build_brain_inner(app: Option<tauri::AppHandle>) -> anyhow::Result<BrainOutcome> {
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
    let project: Project = if let Some(pending) = mnemo::config::take_pending_project() {
        if pending.join(Project::CODING_DIR_NAME).is_dir() {
            switched_open = true;
            Project::from_root(&pending)
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
    } else if let Some(flagged) = startup::project_flag_from_args(std::env::args()) {
        if flagged.join(Project::CODING_DIR_NAME).exists() {
            Project::from_root(&flagged)
        } else {
            eprintln!(
                "No project found at '{}'. Initializing...",
                flagged.display()
            );
            Project::init(&flagged)?
        }
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(found) = Project::find_coding_dir_ancestor(&cwd) {
            Project::from_root(&found)
        } else {
            return Ok(BrainOutcome::NeedsProject(config));
        }
    };

    // Tell the trace log where to mirror records when the user opts in via the
    // Trace tab's "Log to file" checkbox. The path is configured here (where
    // coding_dir is known) but logging stays OFF by default — writing full
    // request/response bodies to disk is a deliberate opt-in.
    trace.set_log_file_path(project.coding_dir.join("logs").join("traces.jsonl"));

    // Always-on provider-error log: every FAILED request (HTTP >= 400,
    // transport / stream errors) is appended as one compact JSON line to
    // .coding/logs/provider-errors.jsonl — independent of the traces.jsonl
    // opt-in checkbox — so provider errors are diagnosable after the fact
    // without full trace logging.
    trace.set_error_log_path(
        project
            .coding_dir
            .join("logs")
            .join("provider-errors.jsonl"),
    );

    // Always-on user-cancel log (F3): every request the consumer drops
    // mid-stream (a user interrupt — the D1 classification) is appended as
    // one compact JSON line at stamping time, so a cancel survives the trace
    // ring's rotation. Independent of the traces.jsonl opt-in checkbox.
    trace.set_cancels_log_path(project.coding_dir.join("logs").join("cancels.jsonl"));

    // Append-only terminal-record archive (F3): every record that reaches a
    // terminal state is appended once to .coding/logs/traces-history.jsonl
    // (rotated at 64 MiB, keeping the 4 newest archives), so a busy turn
    // cannot erase earlier records retrievably. Gated on the same opt-in as
    // the mirror — full request/response bodies are sensitive.
    trace.set_history_log_path(
        project
            .coding_dir
            .join("logs")
            .join("traces-history.jsonl"),
    );

    // Apply the in-memory trace limits from `[trace]` (Advanced settings).
    // Clamped at config load; the fallback branches above keep the defaults
    // baked into LlmRequestLog (16 MiB budget / 256 KiB request cap).
    trace.set_memory_budget(config.general.trace.memory_budget_mb * 1024 * 1024);
    trace.set_request_body_cap(config.general.trace.request_body_cap_kb * 1024);

    // Build the provider.
    let (endpoint, model) = startup::resolve_startup_provider(&config);

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
    let sandbox = Arc::new(Sandbox::new(&project.root)?);

    // Memory store — uses the bundled in-process embedder when configured
    // (failure-protected: zero-vector fallback, never dies), or the built-in
    // deterministic hash embedder otherwise. The shared status Arc is the
    // same handle IpcState exposes to the UI, so circuit-breaker transitions
    // surface live.
    let embedder_status = Arc::new(RwLock::new(
        mnemo::memory::embedder::EmbedderStatus::Checking,
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
                mnemo::memory::embedder::EmbedderStatus::Checking;
            (
                Arc::new(HashEmbedder::new()) as Arc<dyn mnemo::memory::embedder::Embedder>,
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
                mnemo::memory::embedder::EmbedderStatus::Checking;
            (
                Arc::new(HashEmbedder::new()) as Arc<dyn mnemo::memory::embedder::Embedder>,
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
    // when [general.laya] enables it with an endpoint — otherwise `None` with
    // status Disabled and no HTTP client at all, so the app behaves exactly as
    // before. Constructing the client does no I/O: startup is never blocked.
    let classifier_status = Arc::new(RwLock::new(
        mnemo::memory::classifier::ClassifierStatus::Disabled,
    ));
    let classifier = build_classifier(&config, classifier_status.clone());
    // The managed-runtime owner sharing the same status — inert until the
    // startup hook / Settings rewire enable managed mode.
    let laya = Arc::new(ipc::laya::LayaManager::new(classifier_status.clone()));

    // Managed mode (`mode = "managed"`): the app owns the runtime. When
    // enabled + the checkpoint is installed, the classifier client points
    // at the loopback sidecar the startup hook spawns right after this
    // (status Starting until the probe answers). Enabled but not installed
    // ⇒ a hint is logged and no client exists (the Settings → Classifier
    // setup completes it; `laya_setup` then starts the server and swaps
    // the client in). External mode is untouched — `build_classifier`
    // above handled it and returned None here.
    let mut pending_laya_start = None;
    let managed_classifier = match (
        config.general.general.laya.mode,
        config.general.general.laya.enabled,
    ) {
        (mnemo::config::LayaMode::Managed, true) => {
            let checkpoint = ipc::laya::find_checkpoint(
                config
                    .general
                    .general
                    .laya
                    .checkpoint
                    .as_deref()
                    .unwrap_or("english"),
            );
            if laya.is_checkpoint_installed(checkpoint.id) {
                match ipc::laya::LayaManager::alloc_free_port() {
                    Some(port) => {
                        pending_laya_start = Some((checkpoint.id.to_string(), port));
                        *classifier_status.write().expect("classifier status lock poisoned") =
                            mnemo::memory::classifier::ClassifierStatus::Starting;
                        ipc::laya::build_managed_classifier(port, classifier_status.clone())
                    }
                    None => {
                        eprintln!(
                            "warning: no free loopback port for the managed laya-serve; \
                             the classifier stays disabled"
                        );
                        None
                    }
                }
            } else {
                eprintln!(
                    "info: [general.laya] managed mode is enabled but the '{}' \
                     checkpoint is not downloaded — run the setup in Settings → Classifier",
                    checkpoint.id
                );
                None
            }
        }
        _ => None,
    };
    let classifier = managed_classifier.or(classifier);

    let store = match MemoryStore::open(&project.memory_db, embedder) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!("warning: failed to open memory store: {e}; using in-memory");
            Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap())
        }
    };
    let store_trait: Arc<dyn mnemo::memory::MemoryStoreTrait> = store.clone();

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
        let app_handle = app.clone();
        let store_for_index = store.clone();
        let plans_dir = project.plans_dir.clone();
        let backlog_path = project.coding_dir.join("backlog.jsonl");
        tauri::async_runtime::spawn(async move {
            // Console mode (no app handle) has no frontend to notify — the
            // reconcile still runs, but silently (the events are dropped).
            let Some(app_handle) = app_handle else {
                let guard = match ipc::memory_maintenance::claim_busy() {
                    Ok(()) => Some(ipc::memory_maintenance::BusyGuard),
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
                if let Err(e) = mnemo::memory::indexer::index_derived(
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
            let guard = match ipc::memory_maintenance::claim_busy() {
                Ok(()) => Some(ipc::memory_maintenance::BusyGuard),
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
                .map(|p| p.join(mnemo::memory::knowledge::KNOWLEDGE_DIR_NAME))
            {
                let knowledge = mnemo::memory::KnowledgeStore::new(knowledge_dir);
                match mnemo::memory::indexer::migrate_authored_typed_rows(
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
            let stale = match mnemo::memory::indexer::corpus_is_stale(
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
            let _ = app_handle.emit(
                ipc::memory_maintenance::RECONCILE_CHANNEL,
                &ipc::memory_maintenance::ReconcileEvent::Started,
            );
            // Progress events: one per completed source.
            let progress = |done: usize, total: usize| {
                let _ = app_handle.emit(
                    ipc::memory_maintenance::RECONCILE_CHANNEL,
                    &ipc::memory_maintenance::ReconcileEvent::Progress { done, total },
                );
            };
            match mnemo::memory::indexer::index_derived(
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
                    let _ = app_handle.emit(
                        ipc::memory_maintenance::RECONCILE_CHANNEL,
                        &ipc::memory_maintenance::ReconcileEvent::Done { summary },
                    );
                }
                Err(e) => {
                    eprintln!("warning: derived-index reconcile failed: {e}");
                    let _ = app_handle.emit(
                        ipc::memory_maintenance::RECONCILE_CHANNEL,
                        &ipc::memory_maintenance::ReconcileEvent::Failed {
                            error: e.to_string(),
                        },
                    );
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
    if startup::should_reembed_at_startup(
        configured_bundled,
        pending_download.is_some(),
        pending_load.is_some(),
        &status_snapshot,
    ) {
        let store_for_reembed = store.clone();
        let embedder_for_reembed = store.embedder_handle();
        tauri::async_runtime::spawn(async move {
            mnemo::memory::reembed_if_needed(&store_for_reembed, &embedder_for_reembed).await;
        });
    }

    // Load constitutions. We keep a live source (file paths + mtimes) so the
    // agent loop can re-read agent.md from disk when it changes — edits take
    // effect on the next turn without restarting.
    let global_agent_md = config_dir.join("agent.md");
    let constitution_source =
        mnemo::project::ConstitutionSource::new(&global_agent_md, &project.agent_md)?;

    // Safety rules — regex-based auto-approve backed by `.coding/safety.toml`.
    // A missing file is fine (rules start empty); the store is mtime-checked
    // so edits in the Safety tab take effect without restarting.
    let safety_rules = Arc::new(match SafetyRules::new(&project.safety_toml) {
        Ok(sr) => sr,
        Err(e) => {
            eprintln!(
                "warning: failed to load safety rules from {}: {e}; starting empty",
                project.safety_toml.display()
            );
            // Fall back to an empty store over the same path.
            SafetyRules::new(&project.safety_toml)
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
    let model_resolver = Arc::new(mnemo::model_resolver::ConfigModelResolver::new(
        Arc::new(RwLock::new(config.clone())),
        trace.clone(),
    ));
    // The shared headless debug browser — one instance for the agent tools and
    // the IPC layer (the right-panel Browser tab). Lazily spawned on first use.
    let browser = mnemo::browser::BrowserManager::new();
    // Agent-side bootstrap: browser_navigate can create the Browser tab's
    // child webview when the tab was never opened (plan 5ae26d22).
    if let Some(app) = app.as_ref() {
        attach_child_ensurer(&browser, app.clone());
    }
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
    // The per-project code knowledge graph (GitNexus-style). Opened here and
    // indexed on a background thread — startup must never block on parsing
    // the codebase. A DB-open failure disables the graph outright (the
    // `graph_*` tools are omitted, the Graph tab shows unavailable). A
    // mid-index error or panic keeps the handle live: the store lock is
    // poison-tolerant and the DB is a rebuildable cache, so the tools
    // degrade to per-call errors instead of being torn down.
    let codegraph: Option<Arc<mnemo::codegraph::CodeGraph>> = if config.general.general.codegraph {
        match mnemo::codegraph::CodeGraph::open(project.root.clone(), &project.codegraph_db) {
            Ok(graph) => {
                let graph = Arc::new(graph);
                let g = graph.clone();
                // The open-project overlay (IndexingOverlay.tsx) renders
                // this pass as a progress bar + "N/M files indexed"
                // counter. Console mode (no app handle) has no frontend
                // to notify — the pass runs silently there.
                let app_handle = app.clone();
                tauri::async_runtime::spawn(async move {
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
                    let result = match app_handle {
                        Some(handle) => {
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
                            ipc::codegraph_cmds::mark_startup_index_active(switched_open);
                            // A post-switch startup announces itself
                            // immediately: the overlay appears before
                            // the first throttled tick would arrive.
                            let forwarded =
                                Arc::new(std::sync::atomic::AtomicBool::new(switched_open));
                            if switched_open {
                                ipc::codegraph_cmds::emit_index_progress(
                                    &handle,
                                    &ipc::codegraph_cmds::IndexProgressEvent::Started,
                                );
                            }
                            let t0 = std::time::Instant::now();
                            let cb_forwarded = Arc::clone(&forwarded);
                            let cb_app = handle.clone();
                            let progress = move |done: usize, total: usize| {
                                // Record EVERY tick before any gate — the
                                // snapshot must stay complete for ticks
                                // the throttle drops.
                                ipc::codegraph_cmds::record_startup_index_progress(done, total);
                                if ipc::codegraph_cmds::startup_should_forward(
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
                                    ipc::codegraph_cmds::ensure_startup_index_visible();
                                    ipc::codegraph_cmds::emit_index_progress(
                                        &cb_app,
                                        &ipc::codegraph_cmds::IndexProgressEvent::Progress {
                                            done,
                                            total,
                                        },
                                    );
                                    cb_forwarded.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                            };
                            let pass =
                                tokio::task::spawn_blocking(move || g.index(Some(&progress))).await;
                            // Clear the snapshot BEFORE the terminal event:
                            // a fetch that lands after this reports no pass
                            // rather than stale final counts.
                            ipc::codegraph_cmds::clear_startup_index();
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
                                    ipc::codegraph_cmds::emit_index_progress(
                                        &handle,
                                        &ipc::codegraph_cmds::IndexProgressEvent::Done {
                                            summary: format!(
                                                "{} files indexed · {} symbols · {} edges",
                                                stats.files_scanned, stats.symbols, stats.edges
                                            ),
                                        },
                                    );
                                }
                                Ok(Err(e)) => {
                                    if forwarded.load(std::sync::atomic::Ordering::Relaxed) {
                                        ipc::codegraph_cmds::emit_index_progress(
                                            &handle,
                                            &ipc::codegraph_cmds::IndexProgressEvent::Failed {
                                                error: e.to_string(),
                                            },
                                        );
                                    }
                                }
                                Err(join) => {
                                    if forwarded.load(std::sync::atomic::Ordering::Relaxed) {
                                        ipc::codegraph_cmds::emit_index_progress(
                                            &handle,
                                            &ipc::codegraph_cmds::IndexProgressEvent::Failed {
                                                error: format!("index task panicked: {join}"),
                                            },
                                        );
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
                    project.codegraph_db.display()
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
        project.root.clone(),
        Some(safety_rules.clone()),
        safety_mode.clone(),
        context_manager,
        project.plans_dir.clone(),
        vision,
    )
    // Load the skill library from `.coding/skills/*.toml` so the skill tools
    // can validate + look up skills, and `skill_reload` can re-read the dir
    // live (a hand-edited skill file never needs an app restart). A missing dir
    // yields an empty registry (no skills available) — not an error.
    .with_skills(Arc::new(mnemo::skill::SkillLibrary::load(
        project.skills_dir.clone(),
    )))
    // Wire the per-context model resolver so `[models]` overrides take effect
    // at turn time (skill > subagent > state > default).
    .with_model_resolver(model_resolver.clone())
    // Wire the shared git core-operations handle so a `[git]` config save is
    // observed live by every GitTool (no registry rebuild).
    .with_core_operations(core_operations)
    // Wire the shared shell-output filter config so a `[shell_filter]` config
    // save is observed live by every ShellTool (no registry rebuild).
    .with_shell_filter_config(shell_filter)
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
    let project_mcp = mnemo::config::mcp::load_or_default(&project.mcp_toml)?;
    let mcp_servers = mnemo::config::mcp::merge_servers(&config.mcp, &project_mcp);
    let factory = factory.with_mcp(Arc::new(mnemo::mcp::McpManager::from_config(&mcp_servers)));
    let factory = Arc::new(factory);

    // Start the CodeGraph OS file watcher so the graph stays fresh on every
    // edit (agent tools, human editor, git ops) without a manual refresh. The
    // watcher debounces change bursts into incremental index() passes and is
    // best-effort: a start failure logs + disables auto-refresh, never fatal.
    // The handle MUST live on Brain — dropping it stops the watch.
    let graph_watcher = codegraph.clone().and_then(|graph| {
        match mnemo::codegraph::watcher::GraphWatcher::spawn(
            graph,
            project.root.clone(),
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
        project: Arc::new(tokio::sync::Mutex::new(project)),
        config: Arc::new(tokio::sync::Mutex::new(config)),
        sandbox,
        safety_rules,
        safety_mode,
        model_resolver,
        trace,
        browser,
        embedder_status,
        classifier_status,
        classifier,
        laya,
        pending_model_download: pending_download,
        pending_model_load: pending_load,
        pending_laya_start,
        _graph_watcher: graph_watcher,
    }))
}

#[cfg(test)]
mod switch_restart_focus_contract {
    //! Source-contract regression tests for the switch-restart window focus.
    //! The defect needs two processes + a window manager to reproduce
    //! end-to-end, so the guard pins the wiring instead (the same convention
    //! as the frontend's windowRestore source contracts): a launch that
    //! peeks the pending-project marker must focus the window it creates.

    /// The window-creation path must peek the marker and set_focus when it
    /// is present — without the guard the relaunched window opens behind
    /// whatever took the foreground when the old window was destroyed.
    #[test]
    fn switch_restart_focuses_the_restarted_window() {
        let src = include_str!("main.rs");
        assert!(
            src.contains("peek_pending_project().is_some()"),
            "the restart detection must peek the pending-project marker"
        );
        assert!(
            src.contains("window.set_focus()"),
            "the restarted window must be focused (brought on top)"
        );
    }
}
