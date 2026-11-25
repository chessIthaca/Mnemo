// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The hub side of a remote worker node: one listener that pairs nodes and
//! attaches their links, and the session surface a node's sessions are driven
//! through (plan a040713f, phase 2 step 5).
//!
//! [`HubServer::start`] binds one address. The first plain-JSON handshake
//! message on a connection decides what it is: `pair` runs the one-time
//! pairing exchange, `hello` authenticates a node link. Once linked, the node
//! answers the hub's `session.*` and `engine.events` calls while the hub
//! answers the node's `llm.*` and `mem.*` calls, and pushes `catalog.changed`
//! and `mem.version` notes. [`NodeHandle`] maps one-to-one onto phase 1's
//! `WorkerSession`, so a remote session can stand in for a local one.
//!
//! A paired hub has full command execution on its node: the node runs the
//! `SpawnOptions` the hub sends and relays every model and memory call. Pair
//! only with a hub the operator controls.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::config::ModelRef;
use crate::headless::session::ModelSwitch;
use crate::headless::{
    SessionCommand, SessionEvent, SessionId, SessionSnapshot, SpawnOptions,
};
use crate::memory::MemoryStoreTrait;
use crate::provider::LlmClient;
use crate::remote::catalog::ModelCatalog;
use crate::remote::link::{accept_hello, accept_upgrade, HubAuth, Link, LinkHandler, StreamTx};
use crate::remote::llm::{LlmHandler, LlmService};
use crate::remote::memory::{spawn_version_watch, MemoryHandler, MemoryService};
use crate::remote::pairing::{answer_pairing, NodeRegistry, PairRequest, PairingRegistry};
use crate::remote::wire::Handshake;
use crate::remote::{RemoteError, CODE_LINK_DOWN, CODE_TIMEOUT};

/// `session.spawn`: start one session; answers `{"session": <id>}`.
pub const SESSION_SPAWN: &str = "session.spawn";
/// `session.send`: relay one [`SessionCommand`].
pub const SESSION_SEND: &str = "session.send";
/// `session.snapshot`: the live session's snapshot.
pub const SESSION_SNAPSHOT: &str = "session.snapshot";
/// `session.set_model`: pin the session's next request to a model.
pub const SESSION_SET_MODEL: &str = "session.set_model";
/// `session.close`: close the session.
pub const SESSION_CLOSE: &str = "session.close";
/// `session.close_and_wait`: close and wait for the agent family to release.
pub const SESSION_CLOSE_WAIT: &str = "session.close_and_wait";
/// `session.list`: the node's open sessions plus its event sequence.
pub const SESSION_LIST: &str = "session.list";
/// `engine.events`: the node-wide event stream, resumable by sequence.
pub const ENGINE_EVENTS: &str = "engine.events";
/// `node.status`: the node's identity, version, OS and workspace.
pub const NODE_STATUS: &str = "node.status";

/// The hub's note carrying a fresh [`ModelCatalog`] to a node.
pub const CATALOG_CHANGED: &str = "catalog.changed";

/// The budget of an ordinary hub→node call. A disconnected node fails fast
/// with `link_down`; this bounds a connected-but-slow one.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// The budget of the spawn and close-and-wait calls (a spawn builds a session;
/// a close waits for its agent family).
pub const LONG_TIMEOUT: Duration = Duration::from_secs(180);
/// How often the hub checks its live links for a node whose registry file was
/// deleted (a revoked node's link is failed on the next tick).
pub const REVOKE_POLL: Duration = Duration::from_secs(1);
/// How often the hub drops ended model calls whose acknowledgement never
/// arrived. The log is also swept whenever a node's link attaches.
pub const CALL_SWEEP: Duration = Duration::from_secs(60);

/// The pause before an event follow loop retries a link that just failed, so
/// a dying link cannot make it spin. A live-map change wakes it sooner.
pub const RETRY_PAUSE: Duration = Duration::from_millis(150);

/// How the hub listens and where it keeps its paired-node registry and id.
#[derive(Debug, Clone)]
pub struct HubOptions {
    /// The address the single listener binds (loopback in tests).
    pub bind: SocketAddr,
    /// The directory of paired-node records and the hub id.
    pub state_dir: PathBuf,
}

/// What the hub's handlers need from the app: model clients, the shared
/// memory store, and the current model catalog.
#[async_trait]
pub trait HubServices: Send + Sync + 'static {
    /// The client for `model`, or `None` when the hub has no such
    /// endpoint/model configured (the node then gets `model_not_configured`).
    fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>>;
    /// The hub's shared memory store, or `None` while it is unavailable.
    fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>>;
    /// The catalog the node mirrors for its model routing.
    fn catalog(&self) -> ModelCatalog;
}

/// Bridges one [`HubServices`] value to the narrower service traits the
/// `llm.*` and `mem.*` handlers take.
struct ServicesAdapter(Arc<dyn HubServices>);

impl LlmService for ServicesAdapter {
    fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
        self.0.model_client(model)
    }
}

impl MemoryService for ServicesAdapter {
    fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
        self.0.memory()
    }
}

/// A session spawned on a node: the engine's [`SpawnOptions`] plus the node
/// checkout it runs in.
///
/// `options.worktree` must be `None` — an absolute worktree path of the hub
/// means nothing on the node. `checkout` names one directory under the node's
/// `<home>/checkouts/`; `None` runs the session at the node's engine root.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSpawn {
    /// The engine spawn options (the hub's worktree path must be `None`).
    pub options: SpawnOptions,
    /// One checkout directory component under the node's `checkouts/` dir.
    pub checkout: Option<String>,
}

/// One open session as the node reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSessionInfo {
    /// The session's id.
    pub session: SessionId,
    /// The session's display name.
    pub name: String,
}

/// One item of a node's engine-wide event stream.
#[derive(Debug, Clone)]
pub enum EngineEventItem {
    /// One event at sequence number `seq`.
    Event {
        /// The node-wide sequence number.
        seq: u64,
        /// The event itself.
        event: SessionEvent,
    },
    /// Events `from..=to` were lost (a node-side ring overflow); the stream
    /// continues at `to + 1`.
    Gap {
        /// First lost sequence number.
        from: u64,
        /// Last lost sequence number.
        to: u64,
    },
}

/// What the hub's link-side handlers need to route a node's calls.
struct HubLinkHandler {
    node_id: String,
    llm: Arc<LlmHandler>,
    memory: Arc<MemoryHandler>,
}

#[async_trait]
impl LinkHandler for HubLinkHandler {
    async fn request(
        &self,
        method: &str,
        params: Value,
        out: StreamTx,
    ) -> Result<Value, RemoteError> {
        if method.starts_with("mem.") {
            return match self.memory.request(method, params).await {
                Some(answer) => answer,
                None => Err(RemoteError::unknown_method(method)),
            };
        }
        match self
            .llm
            .request(&self.node_id, method, params, out)
            .await
        {
            Some(answer) => answer,
            None => Err(RemoteError::unknown_method(method)),
        }
    }

    fn note(&self, method: &str, params: Value) {
        if self.llm.note(&self.node_id, method, params.clone()) {
            return;
        }
        eprintln!(
            "mnemo hub: node {} sent an unknown note {method}",
            self.node_id
        );
    }
}

/// One attached node link and its version watcher.
struct LiveNode {
    link: Link,
    version_watch: JoinHandle<()>,
    /// Distinguishes this attachment from a later one of the same node, so a
    /// stale connection's teardown never detaches its successor.
    generation: u64,
}

/// The state one [`HubServer`] (and every handle it minted) shares.
struct HubState {
    hub_id: String,
    epoch: String,
    services: Arc<dyn HubServices>,
    nodes: Arc<NodeRegistry>,
    pairing: PairingRegistry,
    llm: Arc<LlmHandler>,
    memory: Arc<MemoryHandler>,
    live: StdMutex<HashMap<String, LiveNode>>,
    /// The per-connection tasks, so a shutdown aborts the ones parked on a
    /// link instead of leaving them to notice the socket went away.
    connections: StdMutex<Vec<JoinHandle<()>>>,
    changed: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
    generation: AtomicU64,
}

/// The hub: one listener for pairing and node links.
pub struct HubServer {
    local_addr: SocketAddr,
    state: Arc<HubState>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl HubServer {
    /// Bind `options.bind` and start accepting pairing and link connections.
    ///
    /// Needs a Tokio runtime (it spawns the accept loop); without one this
    /// returns an `io::Error`.
    pub fn start(options: HubOptions, services: Arc<dyn HubServices>) -> io::Result<Self> {
        std::fs::create_dir_all(&options.state_dir)?;
        let listener = std::net::TcpListener::bind(options.bind)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| io::Error::other("HubServer::start needs a Tokio runtime"))?;
        let hub_id = hub_id_for(&options.state_dir)?;
        let (changed, _) = watch::channel(0u64);
        let (shutdown, _) = watch::channel(false);
        let adapter = Arc::new(ServicesAdapter(services.clone()));
        let state = Arc::new(HubState {
            hub_id,
            epoch: crate::remote::auth::hex_encode(&crate::remote::auth::random_32()[..8]),
            services,
            nodes: Arc::new(NodeRegistry::new(options.state_dir)),
            pairing: PairingRegistry::new(),
            llm: LlmHandler::new(adapter.clone()),
            memory: MemoryHandler::new(adapter),
            live: StdMutex::new(HashMap::new()),
            connections: StdMutex::new(Vec::new()),
            changed,
            shutdown,
            generation: AtomicU64::new(0),
        });
        let accept_state = state.clone();
        let accept_task = runtime.spawn(async move {
            match TcpListener::from_std(listener) {
                Ok(listener) => accept_loop(accept_state, listener).await,
                Err(error) => {
                    eprintln!("mnemo hub: could not adopt the listening socket: {error}");
                }
            }
        });
        let house_state = state.clone();
        let house_task = runtime.spawn(housekeeping(house_state));
        Ok(Self {
            local_addr,
            state,
            tasks: StdMutex::new(vec![accept_task, house_task]),
        })
    }

    /// The address the listener bound (useful when `bind` named port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// The one-time pairing codes.
    pub fn pairing(&self) -> &PairingRegistry {
        &self.state.pairing
    }

    /// A handle for one paired node. It exists before the node connects:
    /// calls fail fast with `link_down` until a link attaches.
    pub fn node(&self, node_id: &str) -> NodeHandle {
        NodeHandle {
            state: self.state.clone(),
            node_id: node_id.to_string(),
        }
    }

    /// Wait at most `timeout` for `node_id`'s link to attach.
    pub async fn wait_connected(&self, node_id: &str, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut changed = self.state.changed.subscribe();
        loop {
            if self
                .state
                .live
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains_key(node_id)
            {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            let _ = tokio::time::timeout_at(deadline, changed.changed()).await;
        }
    }

    /// Push the current model catalog to every attached node. The hub also
    /// pushes it automatically whenever a link attaches.
    pub fn push_catalog(&self) {
        let payload = json!({ "catalog": self.state.services.catalog() });
        for node in self
            .state
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .values()
        {
            node.link.notify(CATALOG_CHANGED, payload.clone());
        }
    }

    /// Stop the listener, fail every live link, and stop the housekeeping.
    pub async fn shutdown(self) {
        self.halt();
        tokio::task::yield_now().await;
    }

    /// The synchronous half of [`HubServer::shutdown`], shared with `Drop`.
    fn halt(&self) {
        self.state.shutdown.send_replace(true);
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain(..)
        {
            task.abort();
        }
        // Close the links explicitly: a connection task holds its own `Link`
        // clone while parked on `closed()`, so merely dropping the map's
        // handle would never fail the link.
        let mut live = self
            .state
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (_, node) in live.drain() {
            node.version_watch.abort();
            node.link.close();
        }
        drop(live);
        for task in self
            .state
            .connections
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

impl Drop for HubServer {
    fn drop(&mut self) {
        self.halt();
    }
}

/// A stable hub id in `<state_dir>/hub-id`, minted on first start.
fn hub_id_for(state_dir: &Path) -> io::Result<String> {
    let path = state_dir.join("hub-id");
    match std::fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        _ => {
            let id = crate::remote::auth::hex_encode(&crate::remote::auth::random_32()[..16]);
            std::fs::write(&path, &id)?;
            Ok(id)
        }
    }
}

/// How many decoded event items [`NodeHandle::events`] buffers before its
/// consumer falls behind (the node's own ring absorbs more).
const EVENT_QUEUE: usize = 1024;

/// The hub's handle for one node's sessions: every method is one `session.*`
/// call over the current link, except [`events`](NodeHandle::events), a
/// node-wide stream that follows the node across reconnects.
///
/// A disconnected node fails fast with `link_down`. `send` can answer with
/// `link_down` too: a host-tool result that arrives after the node's own
/// host-tool timeout is answered by the engine with an error the caller must
/// ignore (the turn has already resolved that call).
#[derive(Clone)]
pub struct NodeHandle {
    state: Arc<HubState>,
    node_id: String,
}

impl NodeHandle {
    /// The node's id.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// The current link, or a fail-fast `link_down` while the node is away.
    fn link(&self) -> Result<Link, RemoteError> {
        self.state
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&self.node_id)
            .map(|node| node.link.clone())
            .ok_or_else(|| {
                RemoteError::link_down(format!("node {} is not connected", self.node_id))
            })
    }

    /// One hub→node call with `timeout`.
    async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RemoteError> {
        let link = self.link()?;
        link.call(method, params, timeout).await
    }

    /// Start one session on the node (spawn budget 180 s).
    pub async fn spawn_session(&self, spawn: RemoteSpawn) -> Result<SessionId, RemoteError> {
        let params = serde_json::to_value(&spawn)
            .map_err(|error| RemoteError::internal(format!("could not encode the spawn: {error}")))?;
        let answer = self.call(SESSION_SPAWN, params, LONG_TIMEOUT).await?;
        serde_json::from_value(
            answer
                .get("session")
                .cloned()
                .ok_or_else(|| RemoteError::internal("session.spawn answered without a session"))?,
        )
        .map_err(|error| RemoteError::internal(format!("bad session.spawn answer: {error}")))
    }

    /// Send one command to a live session.
    ///
    /// A [`SessionCommand::HostToolResult`] that arrives after the node-side
    /// host-tool timeout is answered with an engine error (the call already
    /// resolved by timeout); the caller must ignore that error — the turn
    /// carries on either way.
    pub async fn send(
        &self,
        session: SessionId,
        command: SessionCommand,
    ) -> Result<(), RemoteError> {
        let params = serde_json::to_value(&command)
            .map_err(|error| RemoteError::bad_params(format!("bad command: {error}")))?;
        self.call(
            SESSION_SEND,
            json!({ "session": session, "command": params }),
            CALL_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    /// The live session's snapshot.
    pub async fn snapshot(&self, session: SessionId) -> Result<SessionSnapshot, RemoteError> {
        let answer = self
            .call(SESSION_SNAPSHOT, json!({ "session": session }), CALL_TIMEOUT)
            .await?;
        serde_json::from_value(answer)
            .map_err(|error| RemoteError::internal(format!("bad session.snapshot answer: {error}")))
    }

    /// Pin the session's next request to `model` with `reasoning_effort`.
    pub async fn set_model(
        &self,
        session: SessionId,
        model: &str,
        reasoning_effort: Option<String>,
    ) -> Result<ModelSwitch, RemoteError> {
        let answer = self
            .call(
                SESSION_SET_MODEL,
                json!({
                    "session": session,
                    "model": model,
                    "reasoning_effort": reasoning_effort,
                }),
                CALL_TIMEOUT,
            )
            .await?;
        serde_json::from_value(answer)
            .map_err(|error| RemoteError::internal(format!("bad session.set_model answer: {error}")))
    }

    /// Close the session (does not wait for its agent family).
    pub async fn close(&self, session: SessionId) -> Result<(), RemoteError> {
        self.call(SESSION_CLOSE, json!({ "session": session }), CALL_TIMEOUT)
            .await?;
        Ok(())
    }

    /// Close the session and wait until its agent family released its loops
    /// (close budget 180 s).
    pub async fn close_and_wait(&self, session: SessionId) -> Result<(), RemoteError> {
        self.call(
            SESSION_CLOSE_WAIT,
            json!({ "session": session }),
            LONG_TIMEOUT,
        )
        .await?;
        Ok(())
    }

    /// The node's open sessions.
    pub async fn list_sessions(&self) -> Result<Vec<RemoteSessionInfo>, RemoteError> {
        let answer = self.call(SESSION_LIST, json!({}), CALL_TIMEOUT).await?;
        serde_json::from_value(
            answer
                .get("sessions")
                .cloned()
                .ok_or_else(|| RemoteError::internal("session.list answered without sessions"))?,
        )
        .map_err(|error| RemoteError::internal(format!("bad session.list answer: {error}")))
    }

    /// Whether `session` is still open on the node (one `session.list`).
    pub async fn is_open(&self, session: SessionId) -> bool {
        match self.list_sessions().await {
            Ok(sessions) => sessions.iter().any(|info| info.session == session),
            Err(_) => false,
        }
    }

    /// A node-wide event subscription: every session's events, in the node's
    /// sequence order. It survives reconnects — after a link comes back the
    /// subscription resumes at `last_seq + 1` — and reports lost ranges as
    /// [`EngineEventItem::Gap`].
    pub fn events(&self) -> mpsc::Receiver<EngineEventItem> {
        let (tx, rx) = mpsc::channel(EVENT_QUEUE);
        let state = self.state.clone();
        let node_id = self.node_id.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move { follow_events(state, node_id, tx).await });
            }
            Err(_) => {
                eprintln!("mnemo hub: events() needs a Tokio runtime");
            }
        }
        rx
    }

    /// The node's identity, version, OS, workspace and open sessions.
    pub async fn status(&self) -> Result<Value, RemoteError> {
        self.call(NODE_STATUS, json!({}), CALL_TIMEOUT).await
    }
}

/// Follow one node's `engine.events` stream across reconnects, forwarding
/// decoded items to `tx` and resuming at `last_seq + 1` after a link drop.
async fn follow_events(
    state: Arc<HubState>,
    node_id: String,
    tx: mpsc::Sender<EngineEventItem>,
) {
    let mut next = 1u64;
    loop {
        let Some(link) = wait_link(&state, &node_id).await else {
            return;
        };
        let opened = tokio::time::timeout(
            CALL_TIMEOUT,
            link.call_stream(ENGINE_EVENTS, json!({ "from": next })),
        )
        .await;
        let (_answer, mut items) = match opened {
            Ok(Ok(opened)) => opened,
            Ok(Err(error)) => {
                if error.code == CODE_LINK_DOWN || error.code == CODE_TIMEOUT {
                    // The map may still hold a dying link; wait for the
                    // change (or a short pause) instead of hot-spinning.
                    pause_or_change(&state).await;
                    continue;
                }
                eprintln!("mnemo hub: engine.events on node {node_id} failed: {error}");
                return;
            }
            Err(_) => {
                pause_or_change(&state).await;
                continue;
            }
        };
        while let Some(item) = items.next().await {
            let Ok(item) = item else { break };
            match decode_event_item(&item.d) {
                Ok(EngineEventItem::Event { seq, event }) => {
                    next = seq.saturating_add(1);
                    if tx
                        .send(EngineEventItem::Event { seq, event })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                Ok(EngineEventItem::Gap { from, to }) => {
                    next = to.saturating_add(1).max(from);
                    if tx.send(EngineEventItem::Gap { from, to }).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    eprintln!("mnemo hub: bad engine.events item from {node_id}: {error}");
                    return;
                }
            }
        }
        // The stream ended with the link; wait for the next one and resume.
        pause_or_change(&state).await;
    }
}

/// Wait for the live-map to change, the hub to shut down, or [`RETRY_PAUSE`]
/// — whichever comes first. Keeps a follow loop from spinning on a link that
/// is already failing.
async fn pause_or_change(state: &Arc<HubState>) {
    let mut changes = state.changed.subscribe();
    let mut shutdown = state.shutdown.subscribe();
    tokio::select! {
        _ = changes.changed() => {}
        _ = shutdown.changed() => {}
        _ = tokio::time::sleep(RETRY_PAUSE) => {}
    }
}

/// Decode one `engine.events` item.
fn decode_event_item(data: &Value) -> Result<EngineEventItem, RemoteError> {
    if let Some(gap) = data.get("gap") {
        let from = gap
            .get("from")
            .and_then(Value::as_u64)
            .ok_or_else(|| RemoteError::internal("a gap item carries no from"))?;
        let to = gap
            .get("to")
            .and_then(Value::as_u64)
            .ok_or_else(|| RemoteError::internal("a gap item carries no to"))?;
        return Ok(EngineEventItem::Gap { from, to });
    }
    let seq = data
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| RemoteError::internal("an event item carries no seq"))?;
    let event = serde_json::from_value(
        data.get("event")
            .cloned()
            .ok_or_else(|| RemoteError::internal("an event item carries no event"))?,
    )
    .map_err(|error| RemoteError::internal(format!("bad event item: {error}")))?;
    Ok(EngineEventItem::Event { seq, event })
}

/// Wait until `node_id` has a live link, or the hub shuts down.
async fn wait_link(state: &Arc<HubState>, node_id: &str) -> Option<Link> {
    let mut changes = state.changed.subscribe();
    let mut shutdown = state.shutdown.subscribe();
    loop {
        if let Some(link) = state
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(node_id)
            .map(|node| node.link.clone())
        {
            return Some(link);
        }
        tokio::select! {
            _ = changes.changed() => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return None;
                }
            }
        }
    }
}

/// The accept loop: one task per connection, pairing or link by first message.
async fn accept_loop(state: Arc<HubState>, listener: TcpListener) {
    let mut shutdown = state.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue };
                let connection_state = state.clone();
                let task = tokio::spawn(async move {
                    if let Err(error) = serve_connection(connection_state, stream).await {
                        eprintln!("mnemo hub: {error}");
                    }
                });
                let mut connections = state
                    .connections
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                // Keep the list bounded over a long-lived hub: finished
                // connections are only kept for the shutdown abort.
                connections.retain(|task| !task.is_finished());
                connections.push(task);
            }
        }
    }
}

/// One accepted connection: upgrade, route by the first handshake message,
/// then run the pairing exchange or attach the link.
async fn serve_connection(state: Arc<HubState>, stream: TcpStream) -> Result<(), RemoteError> {
    let (mut ws, first) = accept_upgrade(stream).await?;
    let node_id = match first {
        Handshake::Pair {
            proto,
            code,
            name,
            os,
            ..
        } => {
            let record = answer_pairing(
                &mut ws,
                PairRequest {
                    proto,
                    code: &code,
                    name: &name,
                    os: &os,
                },
                &state.pairing,
                &state.nodes,
                &state.hub_id,
            )
            .await?;
            if let Some(record) = record {
                eprintln!(
                    "mnemo hub: paired node {} ({}, {})",
                    record.node_id, record.name, record.os
                );
            }
            return Ok(());
        }
        Handshake::Hello { ref node_id, .. } => node_id.clone(),
        other => {
            return Err(RemoteError::internal(format!(
                "unexpected handshake message: {}",
                other.kind()
            )))
        }
    };
    let authz =
        HubAuth::with_registry(state.hub_id.clone(), state.epoch.clone(), state.nodes.clone());
    let handler = Arc::new(HubLinkHandler {
        node_id: node_id.clone(),
        llm: state.llm.clone(),
        memory: state.memory.clone(),
    });
    let link = accept_hello(ws, first, authz, handler).await?;
    // The handshake checked the token against the registry; a record that
    // vanished meanwhile (a revoke racing the handshake) must not attach.
    match state.nodes.get(&node_id) {
        Ok(Some(_)) => {}
        _ => {
            drop(link);
            return Ok(());
        }
    }
    let _ = state.nodes.touch(&node_id);
    let generation = attach(&state, &node_id, link.clone());
    link.closed().await;
    detach(&state, &node_id, generation);
    Ok(())
}

/// Publish a freshly authenticated link for `node_id` and start pushing its
/// memory version. Answers this attachment's generation.
fn attach(state: &Arc<HubState>, node_id: &str, link: Link) -> u64 {
    state.llm.sweep();
    let payload = json!({ "catalog": state.services.catalog() });
    link.notify(CATALOG_CHANGED, payload);
    let adapter = Arc::new(ServicesAdapter(state.services.clone()));
    let version_watch = spawn_version_watch(adapter, link.clone());
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let previous = state
        .live
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            node_id.to_string(),
            LiveNode {
                link,
                version_watch,
                generation,
            },
        );
    if let Some(previous) = previous {
        // A second connection for the same node replaces the first: stop its
        // watch and close its link (the previous connection task holds its own
        // handle, so dropping ours alone would not fail it).
        previous.version_watch.abort();
        previous.link.close();
    }
    let next = {
        let current = *state.changed.borrow();
        current.wrapping_add(1)
    };
    state.changed.send_replace(next);
    generation
}

/// Retire this connection's live entry, unless a newer attachment replaced it.
fn detach(state: &Arc<HubState>, node_id: &str, generation: u64) {
    let removed = {
        let mut live = state
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match live.get(node_id) {
            Some(node) if node.generation == generation => live.remove(node_id),
            _ => None,
        }
    };
    if let Some(node) = removed {
        node.version_watch.abort();
    }
    let next = {
        let current = *state.changed.borrow();
        current.wrapping_add(1)
    };
    state.changed.send_replace(next);
}

/// Periodic hub upkeep: drop revoked nodes' live links and sweep the model
/// call log.
async fn housekeeping(state: Arc<HubState>) {
    let mut shutdown = state.shutdown.subscribe();
    let mut ticks: u64 = 0;
    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return;
                }
            }
            _ = tokio::time::sleep(REVOKE_POLL) => {}
        }
        if *shutdown.borrow() {
            return;
        }
        ticks += 1;
        if ticks.is_multiple_of(CALL_SWEEP.as_secs().max(1)) {
            state.llm.sweep();
        }
        sweep_revoked(&state);
    }
}

/// Fail the live links of nodes whose registry file was deleted.
fn sweep_revoked(state: &Arc<HubState>) {
    let gone: Vec<LiveNode> = {
        let mut live = state
            .live
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let revoked: Vec<String> = live
            .keys()
            .filter(|node_id| !matches!(state.nodes.get(node_id), Ok(Some(_))))
            .cloned()
            .collect();
        revoked
            .into_iter()
            .filter_map(|node_id| live.remove(&node_id))
            .collect()
    };
    if gone.is_empty() {
        return;
    }
    for node in gone {
        node.version_watch.abort();
        node.link.close();
    }
    let next = {
        let current = *state.changed.borrow();
        current.wrapping_add(1)
    };
    state.changed.send_replace(next);
}
