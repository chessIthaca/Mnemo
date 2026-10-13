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
use crate::headless::error::EngineError;
use crate::headless::session::{pump_task, AgentSession, EngineInner};
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

    /// Spawn a session (§5.3 :303-314): a parentless agent with its own
    /// plans dir, running under the resolved permission policy — the
    /// `SpawnOptions::permissions` override, else the engine default
    /// (§5.4's engine exposure).
    pub async fn spawn(&self, opts: SpawnOptions) -> Result<AgentSession, EngineError> {
        if let Some(name) = &opts.policy {
            return Err(EngineError::spawn_failed(format!(
                "unknown policy '{name}' — the registry lands with job 4's builder"
            )));
        }
        let name = opts.name.unwrap_or_else(|| "session".to_string());
        let model = match opts.model.as_deref() {
            Some(spec) => Some(parse_model_ref(spec).map_err(EngineError::spawn_failed)?),
            None => None,
        };
        let permission = opts
            .permissions
            .unwrap_or_else(|| self.default_permissions.clone());
        let (agent, _) = spawn_agent_shared(
            self.inner.host.factory.clone(),
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
            // 3a.7 (G7, §5.9.7): the per-session limits.
            opts.limits,
        )
        .await
        .map_err(EngineError::spawn_failed)?;
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
        parked: Mutex::new(HashSet::new()),
    });
    let pump = tokio::spawn(pump_task(fanin, bus.clone(), inner.clone()));

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
