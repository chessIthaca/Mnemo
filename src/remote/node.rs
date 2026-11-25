// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The node runtime: pair once, dial the hub forever, and run the engine a
//! hub drives (plan a040713f, phase 2 step 5).
//!
//! [`pair`] redeems a one-time code and stores the node's identity — the node
//! token is the only secret it keeps. [`run`] dials the hub, mirrors the hub's
//! model catalog into `<home>/hub-config`, and builds one
//! [`Engine`](crate::headless::Engine) per process whose model calls go
//! through [`RemoteTransport`] and whose shared memory is a
//! [`RoutedMemoryStore`]; worker sessions then run locally under the hub's
//! `session.*` calls. Between links the sessions keep running: a model call
//! waits for the hub for at most [`NodeConfig::outage_budget`].
//!
//! A paired hub has full command execution on this machine: it spawns
//! sessions with the `SpawnOptions` it sends, and every model, memory and
//! host-tool call is relayed to it. Pair only with a hub the operator
//! controls.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{watch, Mutex};

use crate::agent::policy::PermissionPolicy;
use crate::config::Config;
use crate::headless::session::ModelSwitch;
use crate::headless::{
    AgentSession, CodeEmbedder, Engine, EngineError, EngineOptions, MemoryBackend, RecvError,
    SessionCommand, SessionEvent, SessionId, SessionSnapshot,
};
use crate::memory::{HashEmbedder, MemoryStore};
use crate::remote::catalog::ModelCatalog;
use crate::remote::hub::{
    RemoteSpawn, CATALOG_CHANGED, ENGINE_EVENTS, NODE_STATUS, SESSION_CLOSE, SESSION_CLOSE_WAIT,
    SESSION_LIST, SESSION_SEND, SESSION_SET_MODEL, SESSION_SNAPSHOT, SESSION_SPAWN,
};
use crate::remote::link::{dial, LinkHandler, LinkSlot, StreamTx};
use crate::remote::llm::RemoteTransport;
use crate::remote::memory::{RoutedMemoryStore, MEM_VERSION};
use crate::remote::pairing::NodeIdentity;
use crate::remote::{RemoteError, CODE_LINK_DOWN};
use crate::workspace::Workspace;

/// The subdirectory of the node home that mirrors the hub's sanitised config.
pub const HUB_CONFIG_DIR: &str = "hub-config";
/// The subdirectory of the node home that holds the node-local SQLite store.
pub const STATE_DIR: &str = "state";
/// The subdirectory of the node home that holds checkout clones.
pub const CHECKOUTS_DIR: &str = "checkouts";

/// How many events the node's ring retains for an `engine.events` resume. A
/// reader further behind than this gets a [`EngineEventItem::Gap`] and
/// continues at the ring's first retained sequence.
pub const EVENT_RING: usize = 8192;

/// How long the first link waits for the hub's model catalog before the node
/// drops the link and retries.
pub const CATALOG_WAIT: Duration = Duration::from_secs(30);

/// The first redial delay after a failed dial.
const BACKOFF_START: Duration = Duration::from_millis(500);
/// The redial delay cap.
const BACKOFF_CAP: Duration = Duration::from_secs(5);

/// What one node runtime needs: its home, the project it runs sessions in, and
/// how long a call may wait for a dropped link.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// The node's config home (`node.json`, `hub-config/`, `state/`,
    /// `checkouts/`).
    pub home: PathBuf,
    /// The workspace root sessions without a checkout run in.
    pub workspace: PathBuf,
    /// How long a model or memory call waits for a dropped hub link before it
    /// fails the turn.
    pub outage_budget: Duration,
    /// Whether the node's engine indexes the code graph.
    pub index_codegraph: bool,
}

/// A node-runtime failure.
#[derive(Debug)]
pub enum NodeError {
    /// There is no `node.json` in the home: pair first.
    NotPaired,
    /// The hub refused the pairing or the link (a bad or expired code, an
    /// unknown or revoked node, a protocol mismatch).
    Refused(String),
    /// A local file, network or store failure.
    Io(String),
    /// The node's engine failed.
    Engine(EngineError),
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPaired => write!(f, "this node is not paired with a hub yet"),
            Self::Refused(reason) => write!(f, "the hub refused the node: {reason}"),
            Self::Io(message) => write!(f, "{message}"),
            Self::Engine(error) => write!(f, "node engine: {error}"),
        }
    }
}

impl std::error::Error for NodeError {}

/// Redeem `code` with the hub at `hub` (`host:port`) and store this node's
/// identity in `home`. The stored token is the only secret a node keeps.
pub async fn pair(
    home: &Path,
    hub: &str,
    code: &str,
    name: &str,
) -> Result<NodeIdentity, NodeError> {
    std::fs::create_dir_all(home)
        .map_err(|error| NodeError::Io(format!("could not create {}: {error}", home.display())))?;
    let identity = crate::remote::pairing::pair_tcp(hub, code, name)
        .await
        .map_err(|error| {
            if error.code == CODE_LINK_DOWN {
                NodeError::Io(error.message)
            } else {
                NodeError::Refused(error.code)
            }
        })?;
    identity.save(home).map_err(NodeError::Io)?;
    Ok(identity)
}

/// Run the node until `shutdown` resolves: dial the hub forever, keep the
/// hub's catalog mirrored, and serve the hub's `session.*` calls.
///
/// The engine is built once, on the first link, and reused by every later
/// link. On shutdown every session is closed and awaited before the engine
/// shuts down.
pub async fn run(
    cfg: NodeConfig,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<(), NodeError> {
    let identity = NodeIdentity::load(&cfg.home)
        .map_err(NodeError::Io)?
        .ok_or(NodeError::NotPaired)?;
    std::fs::create_dir_all(&cfg.home)
        .map_err(|error| NodeError::Io(format!("could not create {}: {error}", cfg.home.display())))?;
    let state = Arc::new(NodeState::new(cfg, identity));
    let mut backoff = Backoff::new();
    tokio::pin!(shutdown);
    loop {
        if *state.shutdown.borrow() {
            break;
        }
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let handler = Arc::new(NodeHandler {
            state: state.clone(),
            cancel: cancel_rx,
        });
        let attempt = tokio::select! {
            _ = &mut shutdown => {
                state.stop();
                break;
            }
            result = dial(&state.identity, handler) => result,
        };
        match attempt {
            Ok(link) => {
                backoff.reset();
                state.observe_hub(link.peer_epoch());
                state.slot.set(Some(link.clone()));
                if !state.wait_engine_ready(CATALOG_WAIT).await {
                    eprintln!(
                        "mnemo node: the hub sent no model catalog within {}s",
                        CATALOG_WAIT.as_secs()
                    );
                    state.slot.set(None);
                    cancel_tx.send_replace(true);
                    drop(link);
                    continue;
                }
                tokio::select! {
                    _ = &mut shutdown => {
                        state.slot.set(None);
                        cancel_tx.send_replace(true);
                        drop(link);
                        break;
                    }
                    _ = link.closed() => {
                        // The sessions keep running; the next dial resumes them.
                        state.slot.set(None);
                        cancel_tx.send_replace(true);
                    }
                }
            }
            Err(error) if error.code == CODE_LINK_DOWN => {
                tokio::select! {
                    _ = &mut shutdown => {
                        state.stop();
                        break;
                    }
                    _ = tokio::time::sleep(backoff.next_delay()) => {}
                }
            }
            Err(error) => {
                // A refusal (bad token, revoked node, protocol mismatch) is
                // final: redialing would only be refused again.
                state.stop();
                return Err(NodeError::Refused(error.code));
            }
        }
    }
    state.finish().await
}

/// The runtime state one `run` call shares with its link handlers.
struct NodeState {
    cfg: NodeConfig,
    identity: NodeIdentity,
    /// The live link, published for the routed model/memory calls.
    slot: Arc<LinkSlot>,
    /// The one engine, built on the first catalog.
    engine: Mutex<Option<Arc<Engine>>>,
    /// Flips to `true` once the engine exists.
    engine_ready: watch::Sender<bool>,
    /// The routed store, for applying `mem.version` notes.
    routed: StdMutex<Option<Arc<RoutedMemoryStore>>>,
    /// The open sessions by agent id.
    sessions: Mutex<HashMap<u64, SessionEntry>>,
    /// The node-wide event ring.
    ring: Arc<StdMutex<Ring>>,
    /// The model-catalog revision currently mirrored.
    catalog: StdMutex<Option<String>>,
    /// The last hub epoch this node attached to.
    last_epoch: StdMutex<Option<String>>,
    /// Set when the runtime stops; the forwarder and the dial loop watch it.
    shutdown: watch::Sender<bool>,
}

/// One open session as the node keeps it.
struct SessionEntry {
    name: String,
    session: Arc<AgentSession>,
}

impl NodeState {
    fn new(cfg: NodeConfig, identity: NodeIdentity) -> Self {
        let (engine_ready, _) = watch::channel(false);
        let (shutdown, _) = watch::channel(false);
        Self {
            cfg,
            identity,
            slot: Arc::new(LinkSlot::new()),
            engine: Mutex::new(None),
            engine_ready,
            routed: StdMutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            ring: Arc::new(StdMutex::new(Ring::new())),
            catalog: StdMutex::new(None),
            last_epoch: StdMutex::new(None),
            shutdown,
        }
    }

    /// The mirror of the hub's sanitised config inside the node home.
    fn hub_config_dir(&self) -> PathBuf {
        self.cfg.home.join(HUB_CONFIG_DIR)
    }

    /// Record the hub epoch, logging a restart while keeping every session.
    fn observe_hub(&self, epoch: &str) {
        let mut last = self
            .last_epoch
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(previous) = last.as_deref() {
            if previous != epoch {
                eprintln!("mnemo node: hub restarted ({previous} -> {epoch}); sessions continue");
            }
        }
        *last = Some(epoch.to_string());
    }

    /// Stop the runtime's background work (not the engine: `finish` owns it).
    fn stop(&self) {
        self.shutdown.send_replace(true);
    }

    /// Wait up to `timeout` for the first catalog to build the engine.
    async fn wait_engine_ready(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut ready = self.engine_ready.subscribe();
        loop {
            if *ready.borrow() {
                return true;
            }
            if tokio::time::timeout_at(deadline, ready.changed()).await.is_err() {
                return false;
            }
        }
    }

    /// The engine, once the first catalog built it.
    async fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.lock().await.clone()
    }

    /// The engine, waiting up to [`CATALOG_WAIT`] for the first catalog.
    async fn wait_engine(&self) -> Result<Arc<Engine>, RemoteError> {
        let deadline = tokio::time::Instant::now() + CATALOG_WAIT;
        let mut ready = self.engine_ready.subscribe();
        loop {
            if let Some(engine) = self.engine().await {
                return Ok(engine);
            }
            if tokio::time::timeout_at(deadline, ready.changed())
                .await
                .is_err()
            {
                return Err(RemoteError::link_down(
                    "the node has not mirrored a model catalog yet",
                ));
            }
        }
    }

    /// Close and await every session, then shut the engine down. Only called
    /// when the `run` shutdown future resolved.
    async fn finish(&self) -> Result<(), NodeError> {
        self.stop();
        let sessions: Vec<Arc<AgentSession>> = {
            let open = self.sessions.lock().await;
            open.values().map(|entry| entry.session.clone()).collect()
        };
        for session in sessions {
            if let Err(error) = session.close_and_wait().await {
                eprintln!("mnemo node: closing a session failed: {error}");
            }
        }
        self.sessions.lock().await.clear();
        let engine = self.engine.lock().await.take();
        if let Some(engine) = engine {
            engine.shutdown().await.map_err(NodeError::Engine)?;
        }
        Ok(())
    }
}

/// The link handler of one dial: the hub's `session.*` and `engine.events`
/// calls, and its `catalog.changed` / `mem.version` notes.
struct NodeHandler {
    state: Arc<NodeState>,
    /// Set when this link drops, so the long-lived event stream returns.
    cancel: watch::Receiver<bool>,
}

#[async_trait]
impl LinkHandler for NodeHandler {
    async fn request(
        &self,
        method: &str,
        params: Value,
        out: StreamTx,
    ) -> Result<Value, RemoteError> {
        match method {
            SESSION_SPAWN => self.state.spawn_session(params).await,
            SESSION_SEND => self.state.send_command(params).await,
            SESSION_SNAPSHOT => self.state.snapshot(params).await,
            SESSION_SET_MODEL => self.state.set_model(params).await,
            SESSION_CLOSE => self.state.close_session(params).await,
            SESSION_CLOSE_WAIT => self.state.close_and_wait(params).await,
            SESSION_LIST => self.state.list_sessions().await,
            ENGINE_EVENTS => self.state.events(params, out, self.cancel.clone()).await,
            NODE_STATUS => Ok(self.state.status().await),
            other => Err(RemoteError::unknown_method(other)),
        }
    }

    fn note(&self, method: &str, params: Value) {
        match method {
            CATALOG_CHANGED => {
                let Some(catalog) = params
                    .get("catalog")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ModelCatalog>(value).ok())
                else {
                    eprintln!("mnemo node: the hub sent a malformed catalog");
                    return;
                };
                let state = self.state.clone();
                tokio::spawn(async move {
                    if let Err(error) = apply_catalog(state, catalog).await {
                        eprintln!("mnemo node: could not apply the model catalog: {error}");
                    }
                });
            }
            MEM_VERSION => {
                if let Some(version) = params.get("version").and_then(Value::as_u64) {
                    let routed = self
                        .state
                        .routed
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .clone();
                    if let Some(routed) = routed {
                        routed.note_version(version);
                    }
                }
            }
            other => eprintln!("mnemo node: unknown hub note {other}"),
        }
    }
}

/// Mirror `catalog` and install it in the engine (building the engine on the
/// first catalog).
async fn apply_catalog(state: Arc<NodeState>, catalog: ModelCatalog) -> Result<(), NodeError> {
    let mut engine = state.engine.lock().await;
    let current = state
        .catalog
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    if current.as_deref() == Some(catalog.revision.as_str()) {
        return Ok(());
    }
    let mirror = state.hub_config_dir();
    catalog
        .write_mirror(&mirror)
        .map_err(|error| NodeError::Io(format!("could not mirror the hub config: {error}")))?;
    let config = Config::load(&mirror)
        .map_err(|error| NodeError::Io(format!("could not load the mirrored config: {error}")))?;
    match engine.as_ref() {
        Some(engine) => engine.set_model_config(config),
        None => {
            *engine = Some(Arc::new(build_engine(&state, config).await?));
            state.engine_ready.send_replace(true);
        }
    }
    *state
        .catalog
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(catalog.revision);
    Ok(())
}

/// Build the node's one engine: remote model transport, routed shared memory,
/// no derived-index watch, no code embedder.
async fn build_engine(state: &Arc<NodeState>, _config: Config) -> Result<Engine, NodeError> {
    let state_dir = state.cfg.home.join(STATE_DIR);
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        NodeError::Io(format!("could not create {}: {error}", state_dir.display()))
    })?;
    let memory_path = state_dir.join("memory.db");
    let local = MemoryStore::open(&memory_path, Arc::new(HashEmbedder::new()))
        .map_err(|error| NodeError::Io(format!("could not open {}: {error}", memory_path.display())))?;
    let routed = RoutedMemoryStore::new(
        Arc::new(local),
        state.slot.clone(),
        state.cfg.outage_budget,
    );
    *state
        .routed
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(routed.clone());
    let options = EngineOptions {
        workspace: Workspace::new(state.cfg.workspace.clone()),
        config_dir: Some(state.hub_config_dir()),
        permissions: PermissionPolicy::OverridePermissions,
        memory: MemoryBackend::Sqlite { path: None },
        index_codegraph: state.cfg.index_codegraph,
        require_git: false,
        auto_install_embedder: false,
    };
    let engine = Engine::builder(options)
        .model_transport(Arc::new(RemoteTransport::new(
            state.slot.clone(),
            state.cfg.outage_budget,
        )))
        .memory_store(routed)
        .derived_index_watch(false)
        .code_embedder(CodeEmbedder::Off)
        .start()
        .await
        .map_err(NodeError::Engine)?;
    // Subscribe the node-wide forwarder BEFORE any session can spawn, so no
    // event of a session is missed.
    spawn_forwarder(state, engine.events());
    Ok(engine)
}

/// Forward the engine's event stream into the node's ring, recording a lag as
/// an explicit gap.
fn spawn_forwarder(
    state: &Arc<NodeState>,
    events: Option<crate::headless::EventReceiver<SessionEvent>>,
) {
    let Some(mut events) = events else { return };
    let ring = state.ring.clone();
    let mut shutdown = state.shutdown.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
                received = events.recv() => match received {
                    Ok(event) => ring.lock().unwrap_or_else(|poison| poison.into_inner()).push_event(event),
                    Err(RecvError::Lagged(skipped)) => ring.lock().unwrap_or_else(|poison| poison.into_inner()).push_gap(skipped),
                    Err(RecvError::Closed) => return,
                }
            }
        }
    });
}

/// What a `session.*` call carries: the node's open sessions and its event
/// sequence, or the session handler's answer.
impl NodeState {
    /// `node.status`: the node's identity, workspace and open sessions.
    async fn status(&self) -> Value {
        let sessions: Vec<Value> = {
            let open = self.sessions.lock().await;
            open.iter()
                .map(|(agent, entry)| {
                    json!({ "session": SessionId::new(*agent), "name": entry.name })
                })
                .collect()
        };
        json!({
            "node_id": self.identity.node_id,
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "workspace": self.cfg.workspace.to_string_lossy(),
            "hub": self.identity.hub,
            "sessions": sessions,
        })
    }

    /// `session.spawn`: validate the checkout, spawn on the engine, remember
    /// the session.
    async fn spawn_session(&self, params: Value) -> Result<Value, RemoteError> {
        let spawn: RemoteSpawn = serde_json::from_value(params)
            .map_err(|error| RemoteError::bad_params(format!("bad session.spawn: {error}")))?;
        if spawn.options.worktree.is_some() {
            return Err(RemoteError::bad_params(
                "a remote spawn carries no worktree; name a checkout instead",
            ));
        }
        let checkout = match spawn.checkout.as_deref() {
            Some(name) => Some(checkout_path(&self.cfg.home, name)?),
            None => None,
        };
        let engine = self.wait_engine().await?;
        let name = spawn
            .options
            .name
            .clone()
            .unwrap_or_else(|| "remote session".to_string());
        let mut options = spawn.options;
        options.worktree = checkout;
        let session = engine.spawn(options).await.map_err(RemoteError::engine)?;
        let id = session.id();
        self.sessions.lock().await.insert(
            id.agent_id(),
            SessionEntry {
                name,
                session: Arc::new(session),
            },
        );
        Ok(json!({ "session": id }))
    }

    /// The live session a `session.*` call names.
    async fn session(&self, params: &Value) -> Result<Arc<AgentSession>, RemoteError> {
        let id = session_id(params)?;
        self.sessions
            .lock()
            .await
            .get(&id.agent_id())
            .map(|entry| entry.session.clone())
            .ok_or_else(|| {
                RemoteError::engine(EngineError::session_closed(format!(
                    "session {} is not open on this node",
                    id.agent_id()
                )))
            })
    }

    /// `session.send`: relay one command.
    async fn send_command(&self, params: Value) -> Result<Value, RemoteError> {
        let session = self.session(&params).await?;
        let command: SessionCommand = serde_json::from_value(
            params
                .get("command")
                .cloned()
                .ok_or_else(|| RemoteError::bad_params("session.send carries no command"))?,
        )
        .map_err(|error| RemoteError::bad_params(format!("bad session command: {error}")))?;
        session.send(command).await.map_err(RemoteError::engine)?;
        Ok(json!({}))
    }

    /// `session.snapshot`: the live snapshot.
    async fn snapshot(&self, params: Value) -> Result<Value, RemoteError> {
        let session = self.session(&params).await?;
        let snapshot: SessionSnapshot = session.snapshot().await.map_err(RemoteError::engine)?;
        serde_json::to_value(snapshot)
            .map_err(|error| RemoteError::internal(format!("could not encode the snapshot: {error}")))
    }

    /// `session.set_model`: pin the next request.
    async fn set_model(&self, params: Value) -> Result<Value, RemoteError> {
        let session = self.session(&params).await?;
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| RemoteError::bad_params("session.set_model carries no model"))?
            .to_string();
        let effort = params
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_string);
        let switch: ModelSwitch = session
            .set_model(&model, effort)
            .await
            .map_err(RemoteError::engine)?;
        serde_json::to_value(switch)
            .map_err(|error| RemoteError::internal(format!("could not encode the switch: {error}")))
    }

    /// `session.close`: close without waiting.
    async fn close_session(&self, params: Value) -> Result<Value, RemoteError> {
        let session = self.session(&params).await?;
        session.close().await.map_err(RemoteError::engine)?;
        Ok(json!({}))
    }

    /// `session.close_and_wait`: close and wait for the agent family.
    async fn close_and_wait(&self, params: Value) -> Result<Value, RemoteError> {
        let session = self.session(&params).await?;
        session.close_and_wait().await.map_err(RemoteError::engine)?;
        Ok(json!({}))
    }

    /// `session.list`: the open sessions plus the event ring's last sequence.
    async fn list_sessions(&self) -> Result<Value, RemoteError> {
        let entries: Vec<(SessionId, String, Arc<AgentSession>)> = {
            let open = self.sessions.lock().await;
            open.iter()
                .map(|(agent, entry)| (SessionId::new(*agent), entry.name.clone(), entry.session.clone()))
                .collect()
        };
        let mut live = Vec::new();
        let mut closed = Vec::new();
        for (id, name, session) in entries {
            if session.is_open().await {
                live.push(json!({ "session": id, "name": name }));
            } else {
                closed.push(id.agent_id());
            }
        }
        if !closed.is_empty() {
            let mut open = self.sessions.lock().await;
            for agent in closed {
                open.remove(&agent);
            }
        }
        let last_seq = self
            .ring
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .last_seq;
        Ok(json!({ "sessions": live, "last_seq": last_seq }))
    }

    /// `engine.events`: stream the ring from `from`, reporting a gap when the
    /// requested sequence is older than the ring retains, and continue with
    /// live events until the link drops or the node stops.
    async fn events(
        &self,
        params: Value,
        out: StreamTx,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value, RemoteError> {
        let from = params.get("from").and_then(Value::as_u64).unwrap_or(1).max(1);
        let (first, initial_gap) = self
            .ring
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .open(from);
        let mut changes = self
            .ring
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .changed
            .subscribe();
        out.start(json!({ "first": first })).await?;
        if let Some((gap_from, gap_to)) = initial_gap {
            out.item(json!({ "gap": { "from": gap_from, "to": gap_to } }))
                .await?;
        }
        let mut cursor = first;
        loop {
            if *cancel.borrow() {
                return Ok(Value::Null);
            }
            let batch = self
                .ring
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .since(cursor);
            if batch.is_empty() {
                tokio::select! {
                    _ = changes.changed() => continue,
                    _ = cancel.changed() => {
                        if *cancel.borrow() {
                            return Ok(Value::Null);
                        }
                    }
                }
                continue;
            }
            for entry in batch {
                let payload = entry.to_json();
                if out.item(payload).await.is_err() {
                    // The link went away; the hub resumes from its last seq.
                    return Ok(Value::Null);
                }
                cursor = entry.last_seq().saturating_add(1);
            }
        }
    }
}

/// The session id a `session.*` call names.
fn session_id(params: &Value) -> Result<SessionId, RemoteError> {
    serde_json::from_value(
        params
            .get("session")
            .cloned()
            .ok_or_else(|| RemoteError::bad_params("the call carries no session"))?,
    )
    .map_err(|error| RemoteError::bad_params(format!("bad session id: {error}")))
}

/// Resolve one checkout name to `<home>/checkouts/<name>`.
///
/// The name must be a single path component (no separators, no `..`) and the
/// directory must already exist — the hub never names an absolute path.
fn checkout_path(home: &Path, name: &str) -> Result<PathBuf, RemoteError> {
    let mut components = Path::new(name).components();
    let single = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none();
    if !single {
        return Err(RemoteError::bad_params(format!(
            "checkout {name:?} is not a single path component"
        )));
    }
    let path = home.join(CHECKOUTS_DIR).join(name);
    if !path.is_dir() {
        return Err(RemoteError::bad_params(format!(
            "checkout {name:?} does not exist on this node"
        )));
    }
    Ok(path)
}

/// One entry of the node's event ring.
#[derive(Clone)]
enum RingEntry {
    Event { seq: u64, event: SessionEvent },
    Gap { from: u64, to: u64 },
}

impl RingEntry {
    fn first_seq(&self) -> u64 {
        match self {
            Self::Event { seq, .. } => *seq,
            Self::Gap { from, .. } => *from,
        }
    }

    fn last_seq(&self) -> u64 {
        match self {
            Self::Event { seq, .. } => *seq,
            Self::Gap { to, .. } => *to,
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Event { seq, event } => json!({ "seq": seq, "event": event }),
            Self::Gap { from, to } => json!({ "gap": { "from": from, "to": to } }),
        }
    }
}

/// The node-wide event ring: every session's events in one sequence, with
/// explicit gaps for events the engine's broadcast receiver lagged past.
struct Ring {
    entries: VecDeque<RingEntry>,
    last_seq: u64,
    changed: watch::Sender<u64>,
}

impl Ring {
    fn new() -> Self {
        let (changed, _) = watch::channel(0u64);
        Self {
            entries: VecDeque::new(),
            last_seq: 0,
            changed,
        }
    }

    /// Record one event at the next sequence number.
    fn push_event(&mut self, event: SessionEvent) {
        let seq = self.last_seq.saturating_add(1);
        self.entries.push_back(RingEntry::Event { seq, event });
        self.last_seq = seq;
        self.trim();
        self.changed.send_replace(seq);
    }

    /// Record `skipped` lost events as one gap and advance past them.
    fn push_gap(&mut self, skipped: u64) {
        if skipped == 0 {
            return;
        }
        let from = self.last_seq.saturating_add(1);
        let to = self.last_seq.saturating_add(skipped);
        self.entries.push_back(RingEntry::Gap { from, to });
        self.last_seq = to;
        self.trim();
        self.changed.send_replace(to);
    }

    /// Drop the oldest entries beyond the ring's capacity.
    fn trim(&mut self) {
        while self.entries.len() > EVENT_RING {
            self.entries.pop_front();
        }
    }

    /// The first sequence the ring can answer for `from`, plus the gap that
    /// must precede it when `from` is older than the ring (or newer than the
    /// node's sequence).
    fn open(&self, from: u64) -> (u64, Option<(u64, u64)>) {
        let from = from.max(1);
        if let Some(oldest) = self.entries.front().map(RingEntry::first_seq) {
            if from < oldest {
                return (oldest, Some((from, oldest.saturating_sub(1))));
            }
        }
        if from > self.last_seq.saturating_add(1) {
            let to = from.saturating_sub(1);
            return (from, Some((self.last_seq.saturating_add(1), to)));
        }
        (from, None)
    }

    /// Every retained entry at or after `cursor`.
    fn since(&self, cursor: u64) -> Vec<RingEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.last_seq() >= cursor)
            .cloned()
            .collect()
    }
}

/// The redial backoff: 0.5, 1, 2, 4, then 5 seconds, each ±20% jitter.
struct Backoff {
    next: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            next: BACKOFF_START,
        }
    }

    fn reset(&mut self) {
        self.next = BACKOFF_START;
    }

    /// The next delay, advancing the ladder.
    fn next_delay(&mut self) -> Duration {
        let base = self.next;
        let doubled = base.saturating_mul(2).min(BACKOFF_CAP);
        self.next = doubled.max(BACKOFF_START);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0) as u64;
        let factor = 0.8 + ((nanos % 401) as f64) / 1000.0;
        base.mul_f64(factor)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    use std::net::SocketAddr;

    use futures::stream::{self, BoxStream};
    use serde_json::{json, Value};

    use super::*;
    use crate::config::{Endpoint, ModelRef, ModelSpec};
    use crate::headless::{HostToolSpec, SessionCommand, SpawnOptions};
    use crate::memory::{MemoryFilter, MemoryStoreTrait};
    use crate::provider::trace::LlmRequestContext;
    use crate::provider::{
        Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice,
        ToolSchema,
    };
    use crate::remote::hub::{EngineEventItem, HubOptions, HubServer, HubServices, RemoteSpawn};
    use crate::remote::pairing::NodeRegistry;
    use crate::runtime::channels::SerializableAgentEvent;
    use crate::tool::{SafetyLevel, ToolResult};

    // ── scratch dirs (inside the checkout's target/, never %TEMP%) ──────────

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("tmp")
            .join(format!(
                "remote5-{tag}-{}-{nanos}-{}",
                std::process::id(),
                NEXT.fetch_add(1, AtomicOrdering::SeqCst)
            ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    // ── the scripted hub model ─────────────────────────────────────────────

    /// One scripted stream step: wait `delay`, then yield `event`.
    #[derive(Clone)]
    struct Step {
        delay: Duration,
        event: LlmEvent,
    }

    fn step(delay: Duration, event: LlmEvent) -> Step {
        Step { delay, event }
    }

    fn text(value: &str) -> Step {
        step(Duration::ZERO, LlmEvent::TextDelta { text: value.into() })
    }

    fn finish_stop() -> Step {
        step(Duration::ZERO, LlmEvent::Finish { reason: FinishReason::Stop })
    }

    fn finish_tools() -> Step {
        step(
            Duration::ZERO,
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        )
    }

    fn tool_call(id: &str, name: &str, args: Value) -> Vec<Step> {
        vec![
            step(
                Duration::ZERO,
                LlmEvent::ToolCallStart {
                    index: 0,
                    id: id.into(),
                    name: name.into(),
                },
            ),
            step(
                Duration::ZERO,
                LlmEvent::ToolCallArgumentDelta {
                    index: 0,
                    fragment: args.to_string(),
                },
            ),
            finish_tools(),
        ]
    }

    /// The hub's model endpoint: pops one script per request.
    struct ScriptedClient {
        scripts: Arc<StdMutex<VecDeque<Vec<Step>>>>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for ScriptedClient {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }

        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }

        fn model(&self) -> &str {
            "m"
        }

        fn provider_name(&self) -> &str {
            "scripted"
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            let steps = self
                .scripts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .pop_front()
                .ok_or_else(|| crate::error::Error::Provider("no script left".into()))?;
            Ok(Box::pin(stream::unfold(
                steps.into_iter(),
                |mut steps| async move {
                    let step = steps.next()?;
                    if !step.delay.is_zero() {
                        tokio::time::sleep(step.delay).await;
                    }
                    Some((step.event, steps))
                },
            )))
        }

        async fn complete_with_context(
            &self,
            messages: &[Message],
            tools: &[ToolSchema],
            tool_choice: Option<ToolChoice>,
            _context: &LlmRequestContext,
        ) -> crate::error::Result<BoxStream<'_, LlmEvent>> {
            self.complete(messages, tools, tool_choice).await
        }
    }

    /// The hub's services for a test: a scripted model, a real memory store
    /// and a catalog built from `config`.
    struct TestServices {
        config: StdMutex<Config>,
        catalog: StdMutex<ModelCatalog>,
        scripts: Arc<StdMutex<VecDeque<Vec<Step>>>>,
        seen: StdMutex<Vec<ModelRef>>,
        store: Arc<MemoryStore>,
    }

    impl TestServices {
        fn new(config: Config, store: Arc<MemoryStore>) -> Arc<Self> {
            let catalog = ModelCatalog::from_config(&config, String::new());
            Arc::new(Self {
                config: StdMutex::new(config),
                catalog: StdMutex::new(catalog),
                scripts: Arc::new(StdMutex::new(VecDeque::new())),
                seen: StdMutex::new(Vec::new()),
                store,
            })
        }

        fn push_script(&self, steps: Vec<Step>) {
            self.scripts
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push_back(steps);
        }

        fn seen(&self) -> Vec<ModelRef> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }

        fn set_config(&self, config: Config) {
            let catalog = ModelCatalog::from_config(&config, String::new());
            *self.config.lock().unwrap_or_else(|poison| poison.into_inner()) = config;
            *self.catalog.lock().unwrap_or_else(|poison| poison.into_inner()) = catalog;
        }
    }

    impl HubServices for TestServices {
        fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(model.clone());
            Some(Arc::new(ScriptedClient {
                scripts: self.scripts.clone(),
                caps: Capabilities::openai(),
            }))
        }

        fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
            Some(self.store.clone())
        }

        fn catalog(&self) -> ModelCatalog {
            self.catalog
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    // ── a hub under test ────────────────────────────────────────────────────

    struct TestHub {
        server: HubServer,
        services: Arc<TestServices>,
        store: Arc<MemoryStore>,
        state_dir: PathBuf,
    }

    impl TestHub {
        async fn start(tag: &str) -> Self {
            Self::start_with(tag, test_config(vec![endpoint("hub")]), None, None).await
        }

        async fn start_with(
            tag: &str,
            config: Config,
            bind: Option<SocketAddr>,
            state_dir: Option<PathBuf>,
        ) -> Self {
            let root = scratch(tag);
            let state_dir = state_dir.unwrap_or_else(|| root.join("hub"));
            let store = Arc::new(
                MemoryStore::open(&root.join("hub-memory.db"), Arc::new(HashEmbedder::new()))
                    .expect("hub memory store"),
            );
            let services = TestServices::new(config, store.clone());
            let server = HubServer::start(
                HubOptions {
                    bind: bind.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()),
                    state_dir: state_dir.clone(),
                },
                services.clone(),
            )
            .expect("hub start");
            Self {
                server,
                services,
                store,
                state_dir,
            }
        }
    }

    fn endpoint(name: &str) -> Endpoint {
        Endpoint {
            name: name.into(),
            ..Endpoint::test_default()
        }
    }

    /// An endpoint whose model id is unique, so a model ref can only resolve
    /// through it.
    fn endpoint_with_model(name: &str, model: &str) -> Endpoint {
        Endpoint {
            name: name.into(),
            models: vec![ModelSpec {
                id: model.into(),
                ..ModelSpec::test_default()
            }],
            ..Endpoint::test_default()
        }
    }

    fn test_config(endpoints: Vec<Endpoint>) -> Config {
        Config {
            endpoints,
            ..Config::default()
        }
    }

    fn transcript_text(snapshot: &SessionSnapshot) -> String {
        serde_json::to_string(&snapshot.transcript).unwrap_or_default()
    }

    fn spawn_options(model: &str) -> RemoteSpawn {
        RemoteSpawn {
            options: SpawnOptions {
                name: Some("remote test".into()),
                model: Some(model.into()),
                reasoning_effort: None,
                worktree: None,
                check_commands: Vec::new(),
                permissions: None,
                policy: None,
                resume: None,
                limits: None,
                host_tools: Vec::new(),
                external_policy: None,
                guardrails: None,
                trace_identity: None,
                pinned_task: None,
                skip_turn_routing: false,
            },
            checkout: None,
        }
    }

    // ── a node process under test ───────────────────────────────────────────

    struct NodeProc {
        home: PathBuf,
        node_id: String,
        stop: watch::Sender<bool>,
        task: tokio::task::JoinHandle<Result<(), NodeError>>,
    }

    impl NodeProc {
        async fn stop(self) {
            self.stop.send_replace(true);
            let _ = self.task.await;
        }
    }

    async fn paired_node(server: &HubServer, tag: &str, budget: Duration) -> NodeProc {
        let root = scratch(tag);
        let home = root.join("node");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let code = server.pairing().issue_code("test node");
        pair(
            &home,
            &server.local_addr().to_string(),
            &code.code,
            "test node",
        )
        .await
        .expect("pair");
        let identity = NodeIdentity::load(&home).unwrap().expect("identity");
        let cfg = NodeConfig {
            home: home.clone(),
            workspace,
            outage_budget: budget,
            index_codegraph: false,
        };
        let (stop, mut rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            run(cfg, async move {
                let _ = rx.wait_for(|ready| *ready).await;
            })
            .await
        });
        NodeProc {
            home,
            node_id: identity.node_id,
            stop,
            task,
        }
    }

    // ── a byte-forwarding proxy the tests can cut ──────────────────────────

    struct Cutter {
        addr: SocketAddr,
        target: Arc<StdMutex<SocketAddr>>,
        connections: Arc<StdMutex<Vec<tokio::task::JoinHandle<()>>>>,
        stop: watch::Sender<bool>,
    }

    impl Cutter {
        async fn start(target: SocketAddr) -> Self {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("cutter bind");
            let addr = listener.local_addr().expect("cutter addr");
            let target = Arc::new(StdMutex::new(target));
            let connections = Arc::new(StdMutex::new(Vec::new()));
            let (stop, mut stop_rx) = watch::channel(false);
            let targets = target.clone();
            let live = connections.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stop_rx.wait_for(|ready| *ready) => return,
                        accepted = listener.accept() => {
                            let Ok((client, _)) = accepted else { continue };
                            let target = *targets
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner());
                            let handle = tokio::spawn(async move {
                                let Ok(server) = tokio::net::TcpStream::connect(target).await else {
                                    return;
                                };
                                let (mut client_read, mut client_write) = client.into_split();
                                let (mut server_read, mut server_write) = server.into_split();
                                let up = tokio::io::copy(&mut client_read, &mut server_write);
                                let down = tokio::io::copy(&mut server_read, &mut client_write);
                                tokio::select! {
                                    _ = up => {}
                                    _ = down => {}
                                }
                            });
                            live.lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .push(handle);
                        }
                    }
                }
            });
            Self {
                addr,
                target,
                connections,
                stop,
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }

        fn point_at(&self, addr: SocketAddr) {
            *self
                .target
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = addr;
        }

        fn cut(&self) {
            for handle in self
                .connections
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .drain(..)
            {
                handle.abort();
            }
        }
    }

    impl Drop for Cutter {
        fn drop(&mut self) {
            self.stop.send_replace(true);
            self.cut();
        }
    }

    // ── polling ────────────────────────────────────────────────────────────

    async fn eventually<F, Fut>(limit: Duration, mut check: F) -> bool
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if check().await {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }

    // ── the tests ──────────────────────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn node_runs_session_served_by_hub_model() {
        let hub = TestHub::start("runs").await;
        hub.services
            .push_script(vec![text("hello from the hub"), finish_stop()]);
        let node = paired_node(&hub.server, "runs", Duration::from_secs(20)).await;
        assert!(
            hub.server
                .wait_connected(&node.node_id, Duration::from_secs(15))
                .await,
            "the node should attach"
        );
        let handle = hub.server.node(&node.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "say hello".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        let handle_for_check = handle.clone();
        let arrived = eventually(Duration::from_secs(25), || {
            let handle = handle_for_check.clone();
            async move {
                handle
                    .snapshot(session)
                    .await
                    .map(|snapshot| transcript_text(&snapshot).contains("hello from the hub"))
                    .unwrap_or(false)
            }
        })
        .await;
        assert!(arrived, "the hub-served text should reach the transcript");
        assert!(
            hub.services
                .seen()
                .iter()
                .any(|model| model.endpoint == "hub" && model.model == "m"),
            "the hub should have served the model call"
        );
        handle.close_and_wait(session).await.expect("close");
        node.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn memory_write_tool_lands_in_hub_store() {
        let hub = TestHub::start("memory").await;
        hub.services.push_script(tool_call(
            "call-1",
            "memory_write",
            json!({
                "tier": "semantic",
                "title": "a fact from the remote node",
                "content": "the remote node wrote this zebra fact",
                "evidence": "node test",
            }),
        ));
        hub.services.push_script(vec![text("noted"), finish_stop()]);
        let node = paired_node(&hub.server, "memory", Duration::from_secs(20)).await;
        assert!(hub
            .server
            .wait_connected(&node.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&node.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "remember the zebra".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        let store = hub.store.clone();
        let landed = eventually(Duration::from_secs(25), || {
            let store = store.clone();
            async move {
                store
                    .recall_peek("zebra", &MemoryFilter::default())
                    .await
                    .map(|hits| {
                        hits.iter()
                            .any(|hit| hit.memory.title.contains("remote node"))
                    })
                    .unwrap_or(false)
            }
        })
        .await;
        assert!(landed, "memory_write should land in the hub's store");
        handle.close_and_wait(session).await.expect("close");
        node.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_tool_call_relays_to_hub_and_back() {
        let hub = TestHub::start("hosttool").await;
        hub.services.push_script(tool_call(
            "call-7",
            "message_parent",
            json!({ "text": "status please" }),
        ));
        hub.services
            .push_script(vec![text("delivered to the hub"), finish_stop()]);
        let node = paired_node(&hub.server, "hosttool", Duration::from_secs(20)).await;
        assert!(hub
            .server
            .wait_connected(&node.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&node.node_id);
        let mut events = handle.events();
        let mut spawn = spawn_options("hub/m");
        spawn.options.host_tools = vec![HostToolSpec {
            name: "message_parent".into(),
            description: "Send a message to the lead".into(),
            parameters: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
            safety: SafetyLevel::AutoRun,
            timeout_ms: Some(15_000),
        }];
        let session = handle.spawn_session(spawn).await.expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "message the lead".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        let mut call_id = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while call_id.is_none() {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(EngineEventItem::Event { event, .. })) => {
                    let value = serde_json::to_value(&event.event).unwrap_or(Value::Null);
                    if value.get("kind").and_then(Value::as_str) == Some("host_tool_call")
                        && value.get("name").and_then(Value::as_str) == Some("message_parent")
                    {
                        call_id = value
                            .get("call_id")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                }
                Ok(Some(EngineEventItem::Gap { .. })) => {}
                Ok(None) | Err(_) => break,
            }
        }
        let call_id = call_id.expect("the host tool call should reach the hub");
        handle
            .send(
                session,
                SessionCommand::HostToolResult {
                    call_id,
                    result: Some(ToolResult::success("delivered")),
                    error: None,
                },
            )
            .await
            .expect("host tool result");
        let handle_for_check = handle.clone();
        let done = eventually(Duration::from_secs(25), || {
            let handle = handle_for_check.clone();
            async move {
                handle
                    .snapshot(session)
                    .await
                    .map(|snapshot| {
                        transcript_text(&snapshot).contains("delivered to the hub")
                    })
                    .unwrap_or(false)
            }
        })
        .await;
        assert!(done, "the answered host tool should let the turn finish");
        handle.close_and_wait(session).await.expect("close");
        node.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn set_model_switches_on_next_request_via_hub() {
        let hub = TestHub::start_with(
            "setmodel",
            test_config(vec![endpoint("hub"), endpoint("alt")]),
            None,
            None,
        )
        .await;
        hub.services
            .push_script(vec![text("first answer"), finish_stop()]);
        hub.services
            .push_script(vec![text("second answer"), finish_stop()]);
        let node = paired_node(&hub.server, "setmodel", Duration::from_secs(20)).await;
        assert!(hub
            .server
            .wait_connected(&node.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&node.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "first".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        let handle_for_check = handle.clone();
        assert!(
            eventually(Duration::from_secs(25), || {
                let handle = handle_for_check.clone();
                async move {
                    handle
                        .snapshot(session)
                        .await
                        .map(|snapshot| transcript_text(&snapshot).contains("first answer"))
                        .unwrap_or(false)
                }
            })
            .await,
            "the first turn should finish on the hub model"
        );
        let switch = handle
            .set_model(session, "alt/m", None)
            .await
            .expect("set_model");
        assert!(
            matches!(
                switch,
                ModelSwitch::NextRequest { .. } | ModelSwitch::CompactsFirst { .. }
            ),
            "the hub should report the switch"
        );
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "second".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt 2");
        let handle_for_check = handle.clone();
        assert!(
            eventually(Duration::from_secs(25), || {
                let handle = handle_for_check.clone();
                async move {
                    handle
                        .snapshot(session)
                        .await
                        .map(|snapshot| transcript_text(&snapshot).contains("second answer"))
                        .unwrap_or(false)
                }
            })
            .await,
            "the second turn should finish on the switched model"
        );
        assert!(
            hub.services
                .seen()
                .iter()
                .any(|model| model.endpoint == "alt" && model.model == "m"),
            "the next request should resolve through the hub to the switched model"
        );
        handle.close_and_wait(session).await.expect("close");
        node.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn snapshot_and_close_and_wait_over_link() {
        let hub = TestHub::start("snapshot").await;
        hub.services
            .push_script(vec![text("snapshot answer"), finish_stop()]);
        let node = paired_node(&hub.server, "snapshot", Duration::from_secs(20)).await;
        assert!(hub
            .server
            .wait_connected(&node.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&node.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "snapshot me".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        let handle_for_check = handle.clone();
        assert!(
            eventually(Duration::from_secs(25), || {
                let handle = handle_for_check.clone();
                async move {
                    handle
                        .snapshot(session)
                        .await
                        .map(|snapshot| transcript_text(&snapshot).contains("snapshot answer"))
                        .unwrap_or(false)
                }
            })
            .await
        );
        let snapshot = handle.snapshot(session).await.expect("snapshot");
        assert!(
            !snapshot.transcript.messages.is_empty(),
            "the snapshot should carry the transcript"
        );
        handle.close_and_wait(session).await.expect("close_and_wait");
        assert!(!handle.is_open(session).await, "the session should be closed");
        assert!(
            handle.list_sessions().await.expect("list").is_empty(),
            "no sessions should remain"
        );
        assert!(
            handle
                .send(session, SessionCommand::Interrupt)
                .await
                .is_err(),
            "a command to a closed session must fail"
        );
        node.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn events_resume_after_link_cut_without_loss() {
        let hub = TestHub::start("resume").await;
        let cutter = Cutter::start(hub.server.local_addr()).await;
        hub.services
            .push_script(vec![text("first turn"), finish_stop()]);
        let root = scratch("resume-node");
        let home = root.join("node");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let code = hub.server.pairing().issue_code("cutter node");
        pair(&home, &cutter.addr().to_string(), &code.code, "cutter node")
            .await
            .expect("pair");
        let identity = NodeIdentity::load(&home).unwrap().unwrap();
        let (stop, mut rx) = watch::channel(false);
        let cfg = NodeConfig {
            home,
            workspace,
            outage_budget: Duration::from_secs(20),
            index_codegraph: false,
        };
        let task = tokio::spawn(async move {
            run(cfg, async move {
                let _ = rx.wait_for(|ready| *ready).await;
            })
            .await
        });
        assert!(hub
            .server
            .wait_connected(&identity.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&identity.node_id);
        let mut events = handle.events();
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "turn one".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt 1");
        let mut seqs = Vec::new();
        let mut texts = String::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
        while !texts.contains("first turn") {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                panic!("the first turn's events did not arrive");
            }
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(EngineEventItem::Event { seq, event })) => {
                    seqs.push(seq);
                    texts.push_str(&serde_json::to_string(&event).unwrap_or_default());
                }
                Ok(Some(EngineEventItem::Gap { .. })) => {}
                _ => break,
            }
        }
        assert!(texts.contains("first turn"));
        // Cut the link: every forwarded connection dies, the node redials.
        cutter.cut();
        assert!(
            eventually(Duration::from_secs(20), || {
                let handle = handle.clone();
                async move { handle.list_sessions().await.is_ok() }
            })
            .await,
            "the node should reattach after the cut"
        );
        hub.services
            .push_script(vec![text("second turn"), finish_stop()]);
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "turn two".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt 2");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
        while !texts.contains("second turn") {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                panic!("the second turn's events did not arrive");
            }
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(EngineEventItem::Event { seq, event })) => {
                    seqs.push(seq);
                    texts.push_str(&serde_json::to_string(&event).unwrap_or_default());
                }
                Ok(Some(EngineEventItem::Gap { .. })) => panic!("no gap expected on a clean resume"),
                _ => break,
            }
        }
        assert!(texts.contains("second turn"));
        assert!(
            seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
            "the resumed stream must be contiguous: {seqs:?}"
        );
        handle.close_and_wait(session).await.expect("close");
        stop.send_replace(true);
        let _ = task.await;
    }

    #[test]
    fn event_ring_overflow_reports_gap() {
        fn sample_event(text: &str) -> SessionEvent {
            SessionEvent {
                agent: 1,
                event: SerializableAgentEvent::TextDelta { text: text.into() },
                rates: None,
            }
        }
        let mut ring = Ring::new();
        for index in 0..(EVENT_RING + 5) {
            ring.push_event(sample_event(&format!("event {index}")));
        }
        assert_eq!(ring.last_seq, (EVENT_RING + 5) as u64);
        let (first, gap) = ring.open(1);
        assert_eq!(first, 6, "the ring should have dropped the first five");
        assert_eq!(gap, Some((1, 5)), "the overflow should report a gap");
        assert_eq!(ring.since(first).len(), EVENT_RING);
        // A lagged broadcast receiver records an explicit gap in sequence.
        ring.push_gap(3);
        assert_eq!(ring.last_seq, (EVENT_RING + 8) as u64);
        let (first, gap) = ring.open((EVENT_RING + 6) as u64);
        assert_eq!(first, (EVENT_RING + 6) as u64);
        assert_eq!(gap, None);
        let entries = ring.since((EVENT_RING + 6) as u64);
        assert_eq!(entries.len(), 1, "only the gap entry is at that seq");
        assert!(matches!(entries[0], RingEntry::Gap { .. }));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn hub_restart_mid_call_resumes_turn() {
        let hub = TestHub::start("restart").await;
        let addr = hub.server.local_addr();
        let cutter = Cutter::start(addr).await;
        // The first hub's model call pauses before its first item, so the cut
        // happens with nothing received (the resume path re-issues it).
        hub.services.push_script(vec![
            step(
                Duration::from_secs(5),
                LlmEvent::TextDelta {
                    text: "stale hub answer".into(),
                },
            ),
            finish_stop(),
        ]);
        let root = scratch("restart-node");
        let home = root.join("node");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let code = hub.server.pairing().issue_code("restart node");
        pair(&home, &cutter.addr().to_string(), &code.code, "restart node")
            .await
            .expect("pair");
        let identity = NodeIdentity::load(&home).unwrap().unwrap();
        let (stop, mut rx) = watch::channel(false);
        let cfg = NodeConfig {
            home,
            workspace,
            outage_budget: Duration::from_secs(30),
            index_codegraph: false,
        };
        let task = tokio::spawn(async move {
            run(cfg, async move {
                let _ = rx.wait_for(|ready| *ready).await;
            })
            .await
        });
        assert!(hub
            .server
            .wait_connected(&identity.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&identity.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "survive the restart".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        // Let the model call start, then cut the link and swap the hub.
        let hub1 = hub;
        let state_dir = hub1.state_dir.clone();
        tokio::time::sleep(Duration::from_millis(200)).await;
        cutter.point_at("127.0.0.1:1".parse().unwrap());
        cutter.cut();
        hub1.server.shutdown().await;
        let second = TestHub::start_with(
            "restart-2",
            test_config(vec![endpoint("hub")]),
            Some(addr),
            Some(state_dir),
        )
        .await;
        second
            .services
            .push_script(vec![text("resumed after restart"), finish_stop()]);
        cutter.point_at(addr);
        assert!(
            second
                .server
                .wait_connected(&identity.node_id, Duration::from_secs(20))
                .await,
            "the node should reattach to the restarted hub"
        );
        let handle = second.server.node(&identity.node_id);
        let handle_for_check = handle.clone();
        let resumed = eventually(Duration::from_secs(30), || {
            let handle = handle_for_check.clone();
            async move {
                handle
                    .snapshot(session)
                    .await
                    .map(|snapshot| transcript_text(&snapshot).contains("resumed after restart"))
                    .unwrap_or(false)
            }
        })
        .await;
        assert!(resumed, "the paused turn should resume on the new hub");
        let _ = handle.close_and_wait(session).await;
        stop.send_replace(true);
        let _ = task.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn outage_beyond_budget_fails_turn_boundedly() {
        let hub = TestHub::start("outage").await;
        let hub_addr = hub.server.local_addr();
        let cutter = Cutter::start(hub_addr).await;
        hub.services.push_script(vec![step(
            Duration::from_secs(60),
            LlmEvent::TextDelta {
                text: "never arrives".into(),
            },
        )]);
        let root = scratch("outage-node");
        let home = root.join("node");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let code = hub.server.pairing().issue_code("outage node");
        pair(&home, &cutter.addr().to_string(), &code.code, "outage node")
            .await
            .expect("pair");
        let identity = NodeIdentity::load(&home).unwrap().unwrap();
        let (stop, mut rx) = watch::channel(false);
        let cfg = NodeConfig {
            home,
            workspace,
            outage_budget: Duration::from_secs(2),
            index_codegraph: false,
        };
        let task = tokio::spawn(async move {
            run(cfg, async move {
                let _ = rx.wait_for(|ready| *ready).await;
            })
            .await
        });
        assert!(hub
            .server
            .wait_connected(&identity.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&identity.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "wait forever".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        tokio::time::sleep(Duration::from_millis(250)).await;
        // The hub becomes unreachable; the node's model call must give up
        // after its 2 s outage budget, not wait for the hub forever.
        cutter.point_at("127.0.0.1:1".parse().unwrap());
        cutter.cut();
        tokio::time::sleep(Duration::from_secs(4)).await;
        cutter.point_at(hub_addr);
        assert!(
            hub.server
                .wait_connected(&identity.node_id, Duration::from_secs(20))
                .await,
            "the node should redial once the hub is reachable again"
        );
        // A fresh subscription replays the ring from the first event: the
        // turn must have failed with the outage message while the link was
        // down (the budget is 2 s, the outage lasted about 4 s).
        let handle = hub.server.node(&identity.node_id);
        let mut events = handle.events();
        let mut saw_outage = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !saw_outage {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match tokio::time::timeout(left, events.recv()).await {
                Ok(Some(EngineEventItem::Event { event, .. })) => {
                    let text = serde_json::to_string(&event).unwrap_or_default();
                    if text.contains("no connection to the hub") {
                        saw_outage = true;
                    }
                }
                Ok(Some(EngineEventItem::Gap { .. })) => {}
                _ => break,
            }
        }
        assert!(
            saw_outage,
            "the model call should fail with the outage message once the budget expires"
        );
        stop.send_replace(true);
        let _ = task.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn refused_node_stops_redialing() {
        let hub = TestHub::start("refused").await;
        let root = scratch("refused-node");
        let home = root.join("node");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let code = hub.server.pairing().issue_code("refused node");
        pair(
            &home,
            &hub.server.local_addr().to_string(),
            &code.code,
            "refused node",
        )
        .await
        .expect("pair");
        let identity = NodeIdentity::load(&home).unwrap().unwrap();
        // Revoke the node behind its back: the registry file disappears.
        NodeRegistry::new(hub.state_dir.clone())
            .revoke(&identity.node_id)
            .expect("revoke");
        let cfg = NodeConfig {
            home,
            workspace,
            outage_budget: Duration::from_secs(2),
            index_codegraph: false,
        };
        let (never, mut rx) = watch::channel(false);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            run(cfg, async move {
                let _ = rx.wait_for(|ready| *ready).await;
            }),
        )
        .await;
        never.send_replace(true);
        match result {
            Ok(Err(NodeError::Refused(reason))) => {
                assert!(reason.contains("unknown") || reason.contains("node"), "{reason}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn catalog_change_reaches_node_resolver() {
        let hub = TestHub::start("catalog").await;
        let node = paired_node(&hub.server, "catalog", Duration::from_secs(20)).await;
        assert!(hub
            .server
            .wait_connected(&node.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&node.node_id);
        // A model that exists only in the NEW catalog: until the update lands
        // the node's resolver cannot build it.
        hub.services
            .set_config(test_config(vec![
                endpoint("hub"),
                endpoint_with_model("second", "m2"),
            ]));
        hub.services
            .push_script(vec![text("new catalog model"), finish_stop()]);
        hub.server.push_catalog();
        let mut session = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while session.is_none() {
            match handle.spawn_session(spawn_options("second/m2")).await {
                Ok(id) => session = Some(id),
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => panic!("the new catalog never reached the node: {error}"),
            }
        }
        let session = session.unwrap();
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "use the new model".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        let handle_for_check = handle.clone();
        assert!(
            eventually(Duration::from_secs(25), || {
                let handle = handle_for_check.clone();
                async move {
                    handle
                        .snapshot(session)
                        .await
                        .map(|snapshot| transcript_text(&snapshot).contains("new catalog model"))
                        .unwrap_or(false)
                }
            })
            .await,
            "the new catalog's model should serve"
        );
        assert!(
            hub.services
                .seen()
                .iter()
                .any(|model| model.endpoint == "second" && model.model == "m2"),
            "the resolver should route through the new endpoint"
        );
        handle.close_and_wait(session).await.expect("close");
        node.stop().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn node_home_holds_only_token_secret() {
        let mut config = test_config(vec![endpoint("hub")]);
        config
            .keys
            .insert("hub".to_string(), "sk-secret-hub-key".to_string());
        let hub = TestHub::start_with("secrets", config, None, None).await;
        hub.services
            .push_script(vec![text("done"), finish_stop()]);
        let node = paired_node(&hub.server, "secrets", Duration::from_secs(20)).await;
        assert!(hub
            .server
            .wait_connected(&node.node_id, Duration::from_secs(15))
            .await);
        let handle = hub.server.node(&node.node_id);
        let session = handle
            .spawn_session(spawn_options("hub/m"))
            .await
            .expect("spawn");
        handle
            .send(
                session,
                SessionCommand::Prompt {
                    text: "hello".into(),
                    images: Vec::new(),
                },
            )
            .await
            .expect("prompt");
        // Let the mirrored config land on disk.
        let mirror = node.home.join(HUB_CONFIG_DIR);
        assert!(
            eventually(Duration::from_secs(20), || {
                let mirror = mirror.clone();
                async move { mirror.join("config.toml").exists() }
            })
            .await,
            "the node should mirror the hub config"
        );
        handle.close_and_wait(session).await.expect("close");
        let home = node.home.clone();
        node.stop().await;
        let identity = NodeIdentity::load(&home).unwrap().expect("identity");
        assert_eq!(identity.token.len(), 64, "the node keeps its 64-hex token");
        let token_files = files_containing(&home, &identity.token);
        assert_eq!(
            token_files,
            vec![NodeIdentity::path(&home)],
            "the node token must live only in node.json"
        );
        assert!(
            files_containing(&home, "sk-secret-hub-key").is_empty(),
            "no hub API key may reach the node"
        );
        let mirrored = Config::load(&mirror).expect("mirrored config loads");
        assert!(
            mirrored.keys.is_empty(),
            "the mirrored config carries no API keys"
        );
    }

    /// Every file under `root` whose text contains `needle`.
    fn files_containing(root: &Path, needle: &str) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if std::fs::read_to_string(&path)
                    .map(|text| text.contains(needle))
                    .unwrap_or(false)
                {
                    found.push(path);
                }
            }
        }
        found.sort();
        found
    }
}
