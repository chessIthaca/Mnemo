// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The worker-session seam: every place the team runtime drives a worker's
//! live session goes through [`WorkerSession`] instead of the engine's
//! concrete [`AgentSession`], so a session that lives on another machine can
//! stand in for a local one. [`LocalWorkerSession`] is the engine-backed
//! implementation; the team holds a [`SharedWorkerSession`].
//!
//! Errors cross the seam as the text a caller prints: the engine's error
//! type is an in-process detail a remote session cannot reproduce.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use mnemo::headless::session::ModelSwitch;
use mnemo::headless::{
    AgentSession, EventReceiver, SessionCommand, SessionEvent, SessionId, SessionSnapshot,
};
use mnemo::remote::hub::NodeHandle;
use mnemo::remote::{CODE_LINK_DOWN, CODE_TIMEOUT};
use tokio::sync::broadcast;

/// One worker's live session, as the team drives it.
#[async_trait::async_trait]
pub trait WorkerSession: Send + Sync {
    /// The session's stable id.
    fn id(&self) -> SessionId;

    /// A new event subscription: every subscriber sees every event.
    fn events(&self) -> EventReceiver<SessionEvent>;

    /// Whether the session is still open: `false` once it is closed. Callers
    /// that used to treat a missing session as "do nothing" check this
    /// first, because a snapshot of a closed session can still answer with
    /// an empty one.
    async fn is_open(&self) -> bool;

    /// Send a command; a closed session refuses every command.
    async fn send(&self, command: SessionCommand) -> Result<(), String>;

    /// The transcript, model, context usage and pending interaction now.
    async fn snapshot(&self) -> Result<SessionSnapshot, String>;

    /// Switch the serving model of the live session.
    async fn set_model(
        &self,
        model: &str,
        effort: Option<String>,
    ) -> Result<ModelSwitch, String>;

    /// Close the session; later commands fail.
    async fn close(&self) -> Result<(), String>;

    /// Close the session and wait until its agent family released its loops
    /// (a worktree session holds the checkout's code graph until then).
    async fn close_and_wait(&self) -> Result<(), String>;
}

/// The handle the team keeps for a live worker session.
pub type SharedWorkerSession = Arc<dyn WorkerSession>;

/// A session the shared engine runs in this process: every method is the
/// engine session's method of the same name, with the engine's error turned
/// into its display text.
pub(crate) struct LocalWorkerSession {
    session: AgentSession,
}

impl LocalWorkerSession {
    pub(crate) fn new(session: AgentSession) -> Self {
        Self { session }
    }
}

#[async_trait::async_trait]
impl WorkerSession for LocalWorkerSession {
    fn id(&self) -> SessionId {
        self.session.id()
    }

    fn events(&self) -> EventReceiver<SessionEvent> {
        self.session.events()
    }

    async fn is_open(&self) -> bool {
        self.session.is_open().await
    }

    async fn send(&self, command: SessionCommand) -> Result<(), String> {
        self.session.send(command).await.map_err(|error| error.to_string())
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, String> {
        self.session.snapshot().await.map_err(|error| error.to_string())
    }

    async fn set_model(
        &self,
        model: &str,
        effort: Option<String>,
    ) -> Result<ModelSwitch, String> {
        self.session
            .set_model(model, effort)
            .await
            .map_err(|error| error.to_string())
    }

    async fn close(&self) -> Result<(), String> {
        self.session.close().await.map_err(|error| error.to_string())
    }

    async fn close_and_wait(&self) -> Result<(), String> {
        self.session
            .close_and_wait()
            .await
            .map_err(|error| error.to_string())
    }
}

/// A session that runs on a paired worker node: every method is one
/// [`NodeHandle`] call, and the events come from the hub's per-node demux.
///
/// [`WorkerSession::is_open`] is deliberately not the node handle's own
/// `is_open`: a node handle answers `false` on every error, and a worker on a
/// node whose link is down is *not* closed — it keeps running there, and it
/// continues on the hub when the link returns within the outage budget. Only a
/// confirmed answer or a non-link failure closes the session here.
pub struct RemoteWorkerSession {
    node: NodeHandle,
    /// The node's display name, for error text a person reads.
    name: String,
    id: SessionId,
    /// One node-wide demux per node feeds every session's sender; the pump
    /// filters by `is_from`, exactly as it does on the shared local bus.
    events: broadcast::Sender<SessionEvent>,
    closed: AtomicBool,
}

impl RemoteWorkerSession {
    pub(crate) fn new(
        node: NodeHandle,
        name: String,
        id: SessionId,
        events: broadcast::Sender<SessionEvent>,
    ) -> Self {
        Self {
            node,
            name,
            id,
            events,
            closed: AtomicBool::new(false),
        }
    }

    /// The text every failure of this session carries: which host it was on.
    fn failed(&self, error: impl std::fmt::Display) -> String {
        format!("host '{}': {error}", self.name)
    }
}

#[async_trait::async_trait]
impl WorkerSession for RemoteWorkerSession {
    fn id(&self) -> SessionId {
        self.id
    }

    fn events(&self) -> EventReceiver<SessionEvent> {
        EventReceiver::from_broadcast(self.events.subscribe())
    }

    async fn is_open(&self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        match self.node.list_sessions().await {
            Ok(sessions) => sessions.iter().any(|info| info.session == self.id),
            // The link is down (or slow): the session may still be running on
            // the node, so it is not closed. The team monitor owns the outage
            // budget and reports a lost host from there.
            Err(error) if error.code == CODE_LINK_DOWN || error.code == CODE_TIMEOUT => true,
            Err(_) => false,
        }
    }

    async fn send(&self, command: SessionCommand) -> Result<(), String> {
        self.node
            .send(self.id, command)
            .await
            .map_err(|error| self.failed(error))
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, String> {
        self.node
            .snapshot(self.id)
            .await
            .map_err(|error| self.failed(error))
    }

    async fn set_model(
        &self,
        model: &str,
        effort: Option<String>,
    ) -> Result<ModelSwitch, String> {
        self.node
            .set_model(self.id, model, effort)
            .await
            .map_err(|error| self.failed(error))
    }

    async fn close(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        self.node
            .close(self.id)
            .await
            .map_err(|error| self.failed(error))
    }

    async fn close_and_wait(&self) -> Result<(), String> {
        self.closed.store(true, Ordering::SeqCst);
        self.node
            .close_and_wait(self.id)
            .await
            .map_err(|error| self.failed(error))
    }
}
