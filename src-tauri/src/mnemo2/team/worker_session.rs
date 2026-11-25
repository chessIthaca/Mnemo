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

use std::sync::Arc;

use mnemo::headless::session::ModelSwitch;
use mnemo::headless::{
    AgentSession, EventReceiver, SessionCommand, SessionEvent, SessionId, SessionSnapshot,
};

/// One worker's live session, as the team drives it.
#[async_trait::async_trait]
pub(crate) trait WorkerSession: Send + Sync {
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
pub(crate) type SharedWorkerSession = Arc<dyn WorkerSession>;

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
