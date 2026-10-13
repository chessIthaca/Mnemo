// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The headless Engine (§5.3 :295-302) — the public face over the shared
//! host wiring: an EXPLICIT workspace + config (G4 — no discovery, no
//! global-config read, no picker), the brain built through the same
//! [`wire_brain`] tail the app uses, and the [`HostRuntime`] core (3a.2).
//! 3a.5 adds `spawn` (the per-session permission threading, §5.4) + the
//! event pump + the multi-subscriber session bus. The memory-backend /
//! index / embedder option consumption lands in 3b.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

use crate::agent::policy::PermissionPolicy;
use crate::config::Config;
use crate::agent::policy::ExternalPolicy;
use crate::headless::error::EngineError;
use crate::headless::host_tools::HostTool;
use crate::headless::session::{pump_task, AgentSession, EngineInner};
use crate::headless::stats::{EngineStats, TraceApi};
use crate::headless::types::{
    EngineOptions, EventReceiver, HostEvent, SessionEvent, SessionId, SpawnOptions,
};
use crate::headless::codegraph_api::CodegraphApi;
use crate::headless::memory_api::MemoryApi;
use crate::host::brain::{wire_brain, Brain, BrainMemoryOptions};
use crate::host::notifier::{HostNotifier, SharedNotifier};
use crate::host::runtime::{spawn_agent_shared, HostRuntime};
use crate::provider::LlmClient;

/// The fan-out notifier: every host event lands on the engine's broadcast
/// bus so any number of `host_events()` subscribers see it (the GUI
/// equivalent forwards each variant to its webview channel).
struct BusNotifier {
    tx: broadcast::Sender<HostEvent>,
}

impl HostNotifier for BusNotifier {
    fn notify(&self, event: HostEvent) {
        // No subscribers yet → the event drops; a late subscriber sees
        // later events (the bus is lossy by design).
        let _ = self.tx.send(event);
    }
}

/// A headless engine (§5.3 :295): the brain + the shared host core + the
/// session event pump, started from explicit options.
pub struct Engine {
    /// The brain, held for the engine's lifetime so its Drop-managed
    /// pieces (the CodeGraph file watcher) stay alive — and read by
    /// `memory()`/`codegraph()` (3b.2 D2) for the store + graph handles.
    brain: Brain,
    /// The shared state (host core + session registry + responder maps).
    inner: Arc<EngineInner>,
    /// The session event bus — `AgentSession::events()` subscribes.
    bus: broadcast::Sender<SessionEvent>,
    /// The host-event bus — `host_events()` subscribes.
    host_events_tx: broadcast::Sender<HostEvent>,
    /// The default permission policy for spawned sessions (§5.4); a
    /// `SpawnOptions::permissions` overrides it per session.
    default_permissions: PermissionPolicy,
    /// The CONFIGURED embedding model (`bundled_embedding_model`, the
    /// "hash" sentinel mapped to None) — the memory overview's line
    /// (3b.2 D2).
    configured_embedding_model: Option<String>,
    /// The event pump task — `shutdown` aborts it.
    pump: tokio::task::JoinHandle<()>,
    /// The runtime this engine owns when started via
    /// [`start_blocking`](Engine::start_blocking) — `None` when the
    /// caller's runtime hosts it. Held purely for its lifetime.
    _owned_runtime: Option<tokio::runtime::Runtime>,
}

/// The Rust-only builder (§5.3 :292-294's first slice — job 4 completes
/// it): engine options plus injection seams a serde type cannot carry.
pub struct EngineBuilder {
    opts: EngineOptions,
    provider: Option<Arc<dyn LlmClient>>,
}

impl EngineBuilder {
    /// Start a builder from options.
    pub fn new(opts: EngineOptions) -> Self {
        Self {
            opts,
            provider: None,
        }
    }

    /// Replace the factory's base provider — the scripted-mock seam for
    /// the engine fixtures (and custom endpoints before job 4's register
    /// surface).
    pub fn provider(mut self, provider: Arc<dyn LlmClient>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Build + start (identical to [`Engine::start`] plus the injections).
    pub async fn start(self) -> Result<Engine, EngineError> {
        start_inner(self.opts, self.provider).await
    }
}

impl Engine {
    /// Start an engine on an EXPLICIT workspace + config (§5.3 :297).
    /// Never blocks and never shows a picker (§7 row 9): an unusable
    /// workspace — the root missing, or `require_git` unmet on a non-repo
    /// root — is [`EngineError`] code `no_project`.
    pub async fn start(opts: EngineOptions) -> Result<Engine, EngineError> {
        EngineBuilder::new(opts).start().await
    }

    /// The Rust-only builder (the injection seams).
    pub fn builder(opts: EngineOptions) -> EngineBuilder {
        EngineBuilder::new(opts)
    }

    /// Register a Rust-side custom tool (G2, §5.9.2) — added to the Host
    /// group for every SUBSEQUENT spawn, executing in-process like any
    /// kernel tool (NOT a spec-based host tool — no kernel↔host
    /// round-trip; `execute` runs normally). Duplicate names against
    /// another engine tool or a spawn's spec shims refuse the spawn (the
    /// spawn-side check); never name one like a kernel tool.
    pub fn register_tool(&self, tool: std::sync::Arc<dyn crate::tool::Tool>) {
        self.inner
            .host_tools
            .lock()
            .expect("host tools lock poisoned")
            .push(tool);
    }

    /// Register a named policy factory (G6 tier 1, D4, §5.9.6): a
    /// spawn's `policy` field resolves the name here — an unknown name
    /// fails the spawn with `spawn_failed`. The factory builds the
    /// session's policy through the existing build machinery
    /// (`build_policy` + `attach_loop`) on a PER-SPAWN snapshot clone of
    /// the engine's factory — the named factory never leaks into
    /// another spawn (review F1, 3c.6); a same-named entry is replaced
    /// (last registration wins). Register before spawning.
    ///
    /// CONTRACT (review F2, 3c.6): the build always runs with
    /// `PolicyFactoryCtx.prompt = None` (a registered factory composes
    /// its own head), and a REFUSING factory — e.g. a NoPolicy-semantics
    /// factory erroring with `KernelError::PromptRequired` — does NOT
    /// fail the spawn: the infallible 2b build's factory-level fallback
    /// attaches `NoPolicy::with_coding_default()` (the GATED coding
    /// default, §5.9.1) and the spawn succeeds. A registered factory
    /// must build without a caller spec; failing the spawn on refusal
    /// needs the build path to become fallible — job 5's follow-up, not
    /// the 3c surface.
    pub fn register_policy(
        &self,
        name: impl Into<String>,
        factory: std::sync::Arc<dyn crate::agent::policy::AgentPolicyFactory>,
    ) {
        self.inner
            .policy_registry
            .lock()
            .expect("policy registry lock poisoned")
            .insert(name.into(), factory);
    }

    /// Spawn a session (§5.3 :303-314): a parentless agent with its own
    /// plans dir, running under the resolved permission policy — the
    /// `SpawnOptions::permissions` override, else the engine default
    /// (§5.4's engine exposure).
    pub async fn spawn(&self, opts: SpawnOptions) -> Result<AgentSession, EngineError> {
        // 3c/G6 tiers 1 + 2 (D4, §5.9.6): the named-policy resolution
        // and the declarative guardrails. A named policy (tier 1) IS
        // the policy — exclusive with a non-default guardrails spec
        // and with an external layer (compose guardrails +
        // external_policy instead; a DEFAULT-shaped spec is a no-op,
        // not a conflict).
        let guardrails = opts.guardrails.filter(|g| !g.is_default());
        if opts.policy.is_some() {
            if guardrails.is_some() {
                return Err(EngineError::spawn_failed(
                    "policy and guardrails are exclusive — the named policy IS the policy",
                ));
            }
            if opts.external_policy.is_some() {
                return Err(EngineError::spawn_failed(
                    "policy and external_policy are exclusive — compose guardrails + external_policy instead",
                ));
            }
        }
        // Both tiers build on a PER-SPAWN factory clone — the shared
        // factory is never mutated: tier 1 swaps the CLONE's policy
        // factory (the snapshot-clone slot, review F1/3c.6 — the named
        // factory is seeded into the clone's own fresh lock, so it dies
        // with the clone and never leaks into a later spawn), tier 2
        // threads the tool selection so the registration table narrows
        // the registry (R1: the array changes only at spawn).
        let factory = match (&opts.policy, &guardrails) {
            (Some(name), _) => {
                let named = self
                    .inner
                    .policy_registry
                    .lock()
                    .expect("policy registry lock poisoned")
                    .get(name)
                    .cloned()
                    .ok_or_else(|| {
                        EngineError::spawn_failed(format!("unknown policy '{name}'"))
                    })?;
                let per_spawn = (*self.inner.host.factory).clone();
                per_spawn.set_policy_factory(named);
                std::sync::Arc::new(per_spawn)
            }
            (None, Some(g)) => {
                let per_spawn = (*self.inner.host.factory)
                    .clone()
                    .with_tool_selection(g.tools.clone());
                std::sync::Arc::new(per_spawn)
            }
            (None, None) => self.inner.host.factory.clone(),
        };
        // Tier 2: the guardrails' limits override the option's.
        let effective_limits = guardrails
            .as_ref()
            .and_then(|g| g.limits.clone())
            .or(opts.limits);
        let name = opts.name.unwrap_or_else(|| "session".to_string());
        let model = match opts.model.as_deref() {
            Some(spec) => Some(parse_model_ref(spec).map_err(EngineError::spawn_failed)?),
            None => None,
        };
        let permission = opts
            .permissions
            .unwrap_or_else(|| self.default_permissions.clone());
        // 3c/G2 (§5.9.2): the session's host tools — engine-registered
        // Rust tools plus the per-spawn spec shims. Duplicate names within
        // the combined list refuse the spawn; a name colliding with a
        // kernel built-in is the caller's contract (the registry's main
        // map wins the dispatch lookup).
        let mut host_tools: Vec<std::sync::Arc<dyn crate::tool::Tool>> = self
            .inner
            .host_tools
            .lock()
            .expect("host tools lock poisoned")
            .clone();
        host_tools.extend(opts.host_tools.into_iter().map(HostTool::new));
        // 3c/G6 tier 2: a tool selection excluding the Host group drops
        // the session's host tools too (the factory's registration
        // table covers the kernel names; the Host group's members live
        // here).
        if let Some(guardrails) = &guardrails {
            host_tools.retain(|tool| {
                guardrails
                    .tools
                    .selects(crate::agent::factory::ToolGroup::Host, tool.name())
            });
        }
        {
            let mut seen = std::collections::HashSet::new();
            for tool in &host_tools {
                if !seen.insert(tool.name().to_string()) {
                    return Err(EngineError::spawn_failed(format!(
                        "duplicate host tool name '{}'",
                        tool.name()
                    )));
                }
            }
        }
        let (agent, _) = spawn_agent_shared(
            factory,
            self.inner.host.manager.clone(),
            self.inner.host.agent_loops.clone(),
            name,
            None, // no initial prompt — sessions drive via send
            None, // parentless — a session is its own main
            model,
            None, // no role
            true, // own plans dir (a parentless session agent)
            None, // no worktree binding (the option wires with 3c)
            Some(permission),
            // 3a.6 (G5): the resume seed — the transcript's messages
            // become the initial conversation (§5.9.5 :693-695).
            opts.resume.map(|transcript| transcript.messages),
            // 3a.7 (G7, §5.9.7) + 3c/G6 tier 2: the per-session limits —
            // the guardrails' limits override the option's.
            effective_limits,
            // 3c/G2 (§5.9.2): the session's host tools.
            host_tools,
        )
        .await
        .map_err(EngineError::spawn_failed)?;
        // 3c/G6 tiers 2 + 3 (§5.9.6): the declarative base swap, then
        // the external layer — both post-build (the loop's policy is
        // interior-mutable since 3c; nothing reads it between the
        // build and the first prompt), so no per-spawn factory
        // threading. ORDER: the external layer reads the CURRENT
        // policy, so the declarative swap lands first (guardrails +
        // external compose — a declarative base under the
        // hook-consulting layer).
        if guardrails.is_some() || opts.external_policy.is_some() {
            let loop_arc = self
                .inner
                .host
                .agent_loops
                .lock()
                .await
                .get(&agent)
                .cloned()
                .ok_or_else(|| {
                    EngineError::spawn_failed("the agent's loop vanished mid-spawn")
                })?;
            if let Some(guardrails) = guardrails {
                let declarative =
                    crate::agent::policy::DeclarativePolicy::new(std::sync::Arc::new(
                        guardrails,
                    ))
                    .map_err(EngineError::spawn_failed)?;
                loop_arc.set_policy(std::sync::Arc::new(declarative));
            }
            if let Some(spec) = opts.external_policy {
                // R1: gate-only — the spec can never change the tool
                // array mid-session; unselected hooks stay on the
                // base policy.
                let external =
                    std::sync::Arc::new(ExternalPolicy::new(loop_arc.policy(), spec));
                external.set_loop(std::sync::Arc::downgrade(&loop_arc));
                loop_arc.set_policy(external);
            }
        }
        let id = SessionId::new(agent);
        self.inner.sessions.lock().await.insert(id, agent);
        Ok(AgentSession {
            id,
            agent,
            inner: self.inner.clone(),
            bus: self.bus.clone(),
        })
    }

    /// Re-acquire a live session's handle by id (§5.3 :299): `None`
    /// once the session ended (closed or shut down). The handle sees
    /// the SAME parked state — parking is engine-scoped (the pump sets
    /// it on Finished{LimitReached}), never per-handle (review round-1
    /// F1/F3).
    pub async fn session(&self, id: &SessionId) -> Option<AgentSession> {
        let agent = *self.inner.sessions.lock().await.get(id)?;
        Some(AgentSession {
            id: *id,
            agent,
            inner: self.inner.clone(),
            bus: self.bus.clone(),
        })
    }

    /// The engine's host-event bus (§5.3 :300).
    pub fn host_events(&self) -> EventReceiver<HostEvent> {
        EventReceiver {
            rx: self.host_events_tx.subscribe(),
        }
    }

    /// Shut the engine down (§5.3 :301): cancel every live session,
    /// deregister, stop the pump. (The session-end consolidation rides
    /// the kernel's existing shutdown path when the loops' tasks end.)
    pub async fn shutdown(self) -> Result<(), EngineError> {
        let agents: Vec<crate::runtime::AgentId> = self
            .inner
            .sessions
            .lock()
            .await
            .values()
            .copied()
            .collect();
        for agent in agents {
            let mgr = self.inner.host.manager.lock().await;
            let _ = mgr.send(agent, crate::runtime::AgentCommand::Cancel);
        }
        self.inner.sessions.lock().await.clear();
        self.pump.abort();
        Ok(())
    }

    /// §5.6's memory data API — the debug/read + maintenance surface,
    /// ONE implementation shared with the app's IPC commands. Errors
    /// code `memory_disabled` when `EngineOptions.memory` was
    /// `Disabled` (no store: no recall, no consolidation, no capture).
    pub fn memory(&self) -> Result<MemoryApi, EngineError> {
        let store = self
            .brain
            .memory_store
            .clone()
            .ok_or_else(EngineError::memory_disabled)?;
        Ok(MemoryApi::from_parts(
            Some(store),
            self.brain.embedder_status.clone(),
            self.configured_embedding_model.clone(),
        ))
    }

    /// §5.6's code-graph data API — status, refresh, rebuild, query. An
    /// `index_codegraph: false` engine gets the unavailable shape (no
    /// graph was opened, no watcher started).
    pub fn codegraph(&self) -> CodegraphApi {
        CodegraphApi::from_parts(self.brain.factory.codegraph_handle())
    }

    /// §5.7's engine-wide stats pull — the store's project + savings
    /// aggregates plus the steering-effectiveness snapshot (the same
    /// counters the app's Trace tab reads). Errors `memory_disabled`
    /// on a `MemoryBackend::Disabled` engine: both aggregates are
    /// store reads (the steering half needs no store).
    pub async fn stats(&self) -> Result<EngineStats, EngineError> {
        let store = self
            .brain
            .factory
            .memory_handle()
            .ok_or_else(EngineError::memory_disabled)?;
        let project_stats = store
            .project_stats()
            .await
            .map_err(|e| EngineError::stats_failed(format!("project stats read failed: {e}")))?;
        let savings_stats = store
            .savings_stats()
            .await
            .map_err(|e| EngineError::stats_failed(format!("savings stats read failed: {e}")))?;
        Ok(EngineStats {
            project_stats,
            savings_stats,
            steering_snapshot: self.brain.factory.steering().snapshot(),
        })
    }

    /// §5.7's trace pull — list/get/clear over the shared LLM request
    /// log (the same log the app's Trace tab reads).
    pub fn trace(&self) -> TraceApi {
        TraceApi::from_parts(self.brain.trace.clone())
    }

    /// Start from a plain (non-async) context — the engine owns a static
    /// tokio runtime, so it works from a plain `fn main` (§7 row 15).
    pub fn start_blocking(opts: EngineOptions) -> Result<Engine, EngineError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| EngineError::start_failed(format!("runtime build failed: {e}")))?;
        let engine = runtime.block_on(Self::start(opts))?;
        Ok(Engine {
            _owned_runtime: Some(runtime),
            ..engine
        })
    }
}

/// The shared start body: the workspace gates, the config, the buses, the
/// brain (with the optional provider injection), the host core, the pump.
async fn start_inner(
    opts: EngineOptions,
    provider: Option<Arc<dyn LlmClient>>,
) -> Result<Engine, EngineError> {
    // The workspace gates — row 9 + the §5.9.3 :664 require_git rule
    // (either the option or the workspace marker demands a repo).
    if !opts.workspace.root.is_dir() {
        return Err(EngineError::no_project(format!(
            "workspace root '{}' is not a directory",
            opts.workspace.root.display()
        )));
    }
    if (opts.require_git || opts.workspace.require_git) && !opts.workspace.is_git_repo() {
        return Err(EngineError::no_project(format!(
            "workspace root '{}' is not a git repository (require_git)",
            opts.workspace.root.display()
        )));
    }

    // The config dir: explicit → $MNEMO_HOME → ~/.mnemo (§5.3 :287).
    let config_dir = match opts.config_dir {
        Some(dir) => dir,
        None => default_config_dir(),
    };
    let config = Config::load(&config_dir).map_err(|e| {
        EngineError::start_failed(format!(
            "config load from '{}' failed: {e}",
            config_dir.display()
        ))
    })?;

    // 3b.2 (D2): the memory overview's configured-model line — resolved
    // BEFORE wire_brain consumes the config. The "hash" sentinel
    // (keyword-only opt-out) is not a real model id — map it to None
    // (the same filter the IPC overview applies).
    let configured_embedding_model = config
        .general
        .general
        .bundled_embedding_model
        .clone()
        .filter(|m| !m.eq_ignore_ascii_case(crate::config::EMBEDDING_MODEL_SENTINEL_HASH));

    // The host-events bus: the engine ALWAYS has one.
    let (host_events_tx, _) = broadcast::channel(256);
    let notifier: SharedNotifier = Arc::new(BusNotifier {
        tx: host_events_tx.clone(),
    });
    // 3b.3 (D3): the startup host events forward through this clone —
    // wire_brain below consumes the original.
    let notifier_events = notifier.clone();

    // The brain — the SAME wiring tail the app uses, with the engine's
    // explicit inputs. A headless start is never "switched open".
    let rt = tokio::runtime::Handle::current();
    let workspace = Arc::new(opts.workspace.clone());
    // 3b.1 (D1): the engine's memory/index decisions govern — a
    // headless caller's explicit EngineOptions win over the config's
    // defaults (the GUI passes the config values into EngineOptions at
    // job 5).
    let (memory_enabled, memory_path) = match opts.memory {
        crate::headless::types::MemoryBackend::Sqlite { path } => (true, path),
        crate::headless::types::MemoryBackend::Disabled => (false, None),
    };
    let brain = wire_brain(
        config_dir,
        config,
        workspace,
        false,
        Some(notifier),
        rt,
        BrainMemoryOptions {
            memory_enabled,
            memory_path,
            index_codegraph: opts.index_codegraph,
        },
    )
    .map_err(|e| EngineError::start_failed(format!("brain build failed: {e}")))?;

    // The optional provider injection (the builder's test seam) — BEFORE
    // the host core builds the main agent.
    if let Some(provider) = provider {
        brain.factory.set_provider(provider);
    }

    // 3a.5 (D5): the engine's agents are FREE agents — NoPolicy with the
    // UNGATED dispatch gate (the workflow state never gates them; row
    // 11's engine form). The app keeps its own policy wiring.
    brain
        .factory
        .set_policy_factory(std::sync::Arc::new(
            crate::agent::policy::FreePolicyFactory,
        ));

    // The shared host core — the same assembly the console delegates to.
    let mut host = HostRuntime::start(brain.factory.clone())
        .await
        .map_err(EngineError::start_failed)?;

    // The session event pump: the fan-in receiver moves into a dedicated
    // task (lifecycle bookkeeping + responder stashing + the broadcast).
    let fanin = host.take_fanin_rx();
    let (bus, _) = broadcast::channel(1024);
    let inner = Arc::new(EngineInner {
        host,
        sessions: Mutex::new(HashMap::new()),
        pending_approvals: Mutex::new(HashMap::new()),
        pending_questions: Mutex::new(HashMap::new()),
        pending_host_tools: Mutex::new(HashMap::new()),
        pending_hooks: Mutex::new(HashMap::new()),
        host_tools: std::sync::Mutex::new(Vec::new()),
        policy_registry: std::sync::Mutex::new(HashMap::new()),
        parked: Mutex::new(HashSet::new()),
        tallies: std::sync::Mutex::new(HashMap::new()),
    });
    let pump = tokio::spawn(pump_task(fanin, bus.clone(), inner.clone()));

    // 3b.3 (D3, §5.6 :422-425, row 17): the startup host events — the
    // engine does what the GUI setup hook + console do. The events ride
    // QUEUED tasks so a subscriber connecting during `start` still sees
    // them (the bus is lossy; a synchronous notify here would be gone
    // before the caller's first `host_events()` — current-thread
    // runtimes poll the queue at the caller's first await).
    let status_arc = brain.embedder_status.clone();
    let notify_status = notifier_events.clone();
    tokio::spawn(async move {
        let snapshot = status_arc
            .read()
            .expect("embedder status lock poisoned")
            .clone();
        notify_status.notify(HostEvent::EmbedderStatus(snapshot));
    });
    // The deferred bundled-model load: the brain's kernel twin of the
    // app's startup path (§5.6); each status transition forwards onto
    // the bus. No pending model / no store → the method no-ops.
    let notify_load = notifier_events.clone();
    brain.spawn_startup_embedder_load(move |s| {
        notify_load.notify(HostEvent::EmbedderStatus(s));
    });
    // (3b.3 note) The graph's startup index pass already runs inside
    // wire_brain (the graph construction spawns it with the shared
    // notifier), streaming HostEvent::IndexProgress onto this bus —
    // the engine must NOT spawn a second pass: it would double-index
    // and its mtime fast path emits no ticks (0 waves) while racing the
    // real pass on the SQLite lock. Row 17's progress/ready contract is
    // served by that existing stream (the §5.1 sketch's
    // CodegraphProgress/Ready names predate job 1's IndexProgress
    // landing — see notifier.rs's doc note).

    Ok(Engine {
        brain,
        inner,
        bus,
        host_events_tx,
        default_permissions: opts.permissions,
        configured_embedding_model,
        pump,
        _owned_runtime: None,
    })
}

/// Parse an "endpoint/model" spec into a [`crate::config::ModelRef`].
fn parse_model_ref(spec: &str) -> Result<crate::config::ModelRef, String> {
    let (endpoint, model) = spec
        .split_once('/')
        .ok_or_else(|| format!("model spec '{spec}' must be \"endpoint/model\""))?;
    Ok(crate::config::ModelRef {
        endpoint: endpoint.to_string(),
        model: model.to_string(),
        reasoning_effort: None,
    })
}

/// The engine's config-dir default (§5.3 :287): `$MNEMO_HOME`, else
/// `~/.mnemo`. Deliberately NOT [`crate::config::global_config_dir`] — G4
/// (:676-677) keeps that for `Setup::open(None)` and the app.
fn default_config_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("MNEMO_HOME") {
        return PathBuf::from(home);
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        return dirs.home_dir().join(".mnemo");
    }
    PathBuf::from(".mnemo")
}
