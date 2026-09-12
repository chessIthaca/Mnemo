// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The multi-agent runtime — spawn, route commands, fan-in events.
//!
//! The agent and UI are fully decoupled actors. The `AgentManager` sits between
//! them: it spawns agents as tokio tasks, routes commands to the right agent,
//! and fans-in their events. Multi-agent-ready from day one — spawning a second
//! agent is just another `manager.spawn()`.

pub mod agent;
pub mod channels;
pub mod correction;
pub mod turn_resolve;

pub use channels::{
    AgentCommand, AgentConfig, AgentEvent, AgentHandle, AgentId, Approval, ContextBreakdown,
    PhaseKind, QuestionOption, SerializedEvent, SteerPayload, UserAnswer,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

/// The manager that spawns agents and fans-in their events.
pub struct AgentManager {
    agents: HashMap<AgentId, AgentHandle>,
    fanin_tx: mpsc::Sender<(AgentId, AgentEvent)>,
    fanin_rx: mpsc::Receiver<(AgentId, AgentEvent)>,
    next_id: AtomicU64,
}

impl AgentManager {
    /// Create a new manager with a bounded fan-in channel.
    pub fn new(channel_capacity: usize) -> Self {
        let (fanin_tx, fanin_rx) = mpsc::channel(channel_capacity);
        Self {
            agents: HashMap::new(),
            fanin_tx,
            fanin_rx,
            next_id: AtomicU64::new(1),
        }
    }

    /// Get the sender half of the fan-in channel (cloned to each agent at spawn).
    pub fn fanin_sender(&self) -> mpsc::Sender<(AgentId, AgentEvent)> {
        self.fanin_tx.clone()
    }

    /// Allocate the next agent id.
    pub fn next_id(&self) -> AgentId {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Register an agent handle (after spawning its task).
    pub fn register(&mut self, handle: AgentHandle) {
        self.agents.insert(handle.id, handle);
    }

    /// Send a command to an agent by id.
    pub fn send(&self, id: AgentId, cmd: AgentCommand) -> Result<(), AgentCommand> {
        match self.agents.get(&id) {
            Some(handle) => handle.command_tx.try_send(cmd).map_err(|e| match e {
                mpsc::error::TrySendError::Full(cmd) => cmd,
                mpsc::error::TrySendError::Closed(cmd) => cmd,
            }),
            None => Err(cmd),
        }
    }

    /// Read the next fanned-in event. Returns `None` when all agents are gone.
    pub async fn next_event(&mut self) -> Option<(AgentId, AgentEvent)> {
        self.fanin_rx.recv().await
    }

    /// Take ownership of the fan-in receiver. Used by the event forwarder so
    /// it can read events without holding the manager lock (which would
    /// deadlock Tauri commands while waiting for the next event).
    pub fn take_fanin_rx(&mut self) -> Option<mpsc::Receiver<(AgentId, AgentEvent)>> {
        // Replace with a dummy receiver that never yields (the forwarder owns
        // the real one now). next_event() on the dummy returns None immediately.
        let dummy = mpsc::channel(1).1;
        Some(std::mem::replace(&mut self.fanin_rx, dummy))
    }

    /// List all agent handles (for the UI agent switcher).
    pub fn list(&self) -> Vec<&AgentHandle> {
        let mut handles: Vec<&AgentHandle> = self.agents.values().collect();
        handles.sort_by_key(|h| h.id);
        handles
    }

    /// Get an agent handle by id.
    pub fn get(&self, id: AgentId) -> Option<&AgentHandle> {
        self.agents.get(&id)
    }

    /// The parent agent that spawned the given agent via the `spawn_agent`
    /// tool, if any. `None` for the main/UI agents and for unknown ids. Used
    /// by the event forwarder to route a completion notification back to the
    /// parent when a background agent finishes its task.
    pub fn parent_id(&self, id: AgentId) -> Option<AgentId> {
        self.agents.get(&id).and_then(|h| h.parent_id)
    }

    /// The role the given agent was spawned with (`Some("reviewer")` for a
    /// read-only reviewer sub-agent; `None` otherwise). Used by the event
    /// forwarder to apply role-specific completion handling.
    pub fn role(&self, id: AgentId) -> Option<String> {
        self.agents.get(&id).and_then(|h| h.role.clone())
    }

    /// Mark an agent as running (or not). Called by the event forwarder on
    /// `Started`/`Finished` events. No-op if the agent isn't registered.
    pub fn set_running(&self, id: AgentId, running: bool) {
        if let Some(handle) = self.agents.get(&id) {
            handle.set_running(running);
        }
    }

    /// Remove an agent (after it finishes/cancels).
    pub fn remove(&mut self, id: AgentId) {
        self.agents.remove(&id);
    }

    /// The id of the "main" agent — the primary interactive agent that backlog
    /// prompts are dispatched to. The main agent is the one with no parent
    /// (`parent_id == None`) and the smallest id (it's built first in
    /// `main.rs` before any UI- or tool-spawned agent exists). Returns `None`
    /// when no main agent is registered (e.g. it exited).
    ///
    /// Subagents (those with a recorded parent) are never returned: backlog
    /// prompts must only ever go to the main agent — subagents accept steers,
    /// not prompts.
    pub fn main_agent_id(&self) -> Option<AgentId> {
        self.agents
            .values()
            .filter(|h| h.parent_id.is_none())
            .map(|h| h.id)
            .min()
    }

    /// Whether any registered agent is a running descendant of `id`. Used by
    /// the backlog Run-All loop to know when a turn is *fully* resolved: the
    /// agent must be idle AND none of its spawned subagents (or theirs) may
    /// still be running. Walks the `parent_id` chain so a grandchild counts
    /// too, not just direct children.
    pub fn has_running_descendants(&self, id: AgentId) -> bool {
        self.agents
            .values()
            .any(|h| h.is_running() && self.is_descendant_of(h.id, id))
    }

    /// Whether `candidate` is a descendant of `ancestor` via the `parent_id`
    /// chain (child → parent → grandparent …). Bounded by the number of
    /// registered agents so a cyclic chain (shouldn't happen) can't loop.
    fn is_descendant_of(&self, candidate: AgentId, ancestor: AgentId) -> bool {
        let mut current = candidate;
        for _ in 0..self.agents.len() {
            match self.agents.get(&current).and_then(|h| h.parent_id) {
                Some(parent) if parent == ancestor => return true,
                Some(parent) => current = parent,
                None => return false,
            }
        }
        false
    }

    /// Number of active agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Whether there are no agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

/// Spawns background agents from within the agent loop.
///
/// This is the seam that lets the `spawn_agent` **tool** (in the core library)
/// start a new agent without depending on the Tauri IPC layer. The IPC layer
/// provides the concrete implementation (backed by the real `AgentManager` +
/// `AgentLoopFactory`); the tool only knows this trait.
///
/// Keeping this behind a trait (rather than giving the tool an `AgentManager`
/// directly) is deliberate: the `AgentManager` alone can't build a per-agent
/// `AgentLoop` (that's the factory's job) and doesn't own the async runtime the
/// task is spawned on. The IPC implementation bundles all of that.
#[async_trait::async_trait]
pub trait AgentSpawner: Send + Sync {
    /// Spawn a new background agent with the given display name and task
    /// description, and kick off its first turn.
    ///
    /// `role` optionally constrains the spawned agent's tool surface (e.g.
    /// `Some("reviewer")` for a read-only reviewer). `None` spawns an
    /// unrestricted sub-agent.
    ///
    /// Returns the new agent's id on success, or an error message describing
    /// why the spawn failed (e.g. the agent runtime is unavailable).
    async fn spawn(&self, name: &str, task: &str, role: Option<String>) -> Result<AgentId, String>;

    /// If this spawner can record a parent for the agents it spawns, return
    /// it as a [`ParentAwareSpawner`] trait object. The default returns `None`
    /// (no parent tracking); the IPC-layer spawner overrides it to return
    /// `Some(self)`. This keeps [`AgentSpawner`] object-safe while letting the
    /// `spawn_agent` tool reach the parent-aware path without knowing the
    /// concrete type.
    fn parent_aware(&self) -> Option<&dyn ParentAwareSpawner> {
        None
    }
}

/// A spawner that can record the spawning agent as the new agent's parent.
///
/// Implemented by the concrete IPC-layer spawner and reached from the
/// `spawn_agent` tool via [`AgentSpawner::parent_aware`]. When the tool knows
/// its owning agent's id, it uses this to register the child as that agent's
/// child, so the completion-notification feedback loop fires when the child
/// finishes. Kept separate from [`AgentSpawner`] so the base trait stays
/// simple and mock-friendly.
#[async_trait::async_trait]
pub trait ParentAwareSpawner: Send + Sync {
    /// Spawn a background agent on behalf of `parent_id` (the agent whose
    /// `spawn_agent` tool call triggered this). `parent_id: None` behaves like
    /// [`AgentSpawner::spawn`] (no parent, no completion notification).
    ///
    /// `model` is an optional forced model (endpoint + model id) the spawned
    /// agent runs on for every turn, overriding the normal subagent/state/skill
    /// resolution. `None` uses the default/subagent model. Only the parent-aware
    /// path supports a model override.
    ///
    /// `role` optionally constrains the spawned agent's tool surface (e.g.
    /// `Some("reviewer")` for a read-only reviewer). `None` spawns an
    /// unrestricted sub-agent.
    async fn spawn_with_parent(
        &self,
        name: &str,
        task: &str,
        parent_id: Option<AgentId>,
        model: Option<crate::config::ModelRef>,
        role: Option<String>,
    ) -> Result<AgentId, String>;
}

/// A spawner-side seam that reports whether an agent has any running spawned
/// descendants.
///
/// Like [`AgentSpawner`], this is a brain-side trait: the workflow / dispatch
/// layer consults it to gate state transitions on running subagents without
/// depending on the IPC layer. The concrete implementation is the IPC-layer
/// spawner (backed by the real [`AgentManager`]), reached from the
/// `spawn_agent` tool / dispatch via the [`AgentLoopFactory`] wiring.
///
/// Used by the dispatch layer to enforce the review-exit transition gate
/// (backlog 569b5922) — the agent cannot END a workflow phase while spawned
/// subagents are still running: `finish` (Reviewing→Complete) is always
/// refused, and `complete_step` is refused only when the call would complete
/// the ROOT plan (the transition that exits Executing). All other
/// state-touching tools (non-final complete_step checklist ticks,
/// update_plan, create_plan sub-plan pushes, abandon_plan, skill_*) are
/// deliberately ungated so parallel subagent work proceeds. Also used by the
/// runtime auto-continue logic (runtime/agent.rs `run_turn_with_retry`) to
/// park a parent whose turn ends while its descendants run.
///
/// Update timing (backlog 2c406d72): the tracker is updated synchronously
/// BEFORE the child-finish notification is sent — the IPC event forwarder
/// clears the finishing agent's running flag first and only then sends the
/// completion `Suggestion` to the parent (`notify_parent_on_completion` in
/// src-tauri/src/ipc/events.rs). The notification and the tracker therefore
/// agree by construction: the moment the parent can observe that a child
/// finished, this method already reports it as not running.
#[async_trait::async_trait]
pub trait DescendantTracker: Send + Sync {
    /// Whether the agent with `agent_id` has any running spawned descendants
    /// (direct children or theirs, recursively). Walks the `parent_id` chain
    /// so a grandchild counts too.
    async fn has_running_descendants(&self, agent_id: AgentId) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn next_id_increments() {
        let mgr = AgentManager::new(16);
        assert_eq!(mgr.next_id(), 1);
        assert_eq!(mgr.next_id(), 2);
        assert_eq!(mgr.next_id(), 3);
    }

    #[tokio::test]
    async fn send_to_unknown_agent_fails() {
        let mut mgr = AgentManager::new(16);
        let (tx, _rx) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "test".into(), tx));
        // Sending to a non-existent agent returns the command.
        let cmd = AgentCommand::Prompt {
            text: "hi".into(),
            images: vec![],
        };
        let result = mgr.send(999, cmd.clone());
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn fanin_routes_events() {
        let mut mgr = AgentManager::new(16);
        let fanin_tx = mgr.fanin_sender();
        // Simulate an agent sending an event.
        fanin_tx.send((1, AgentEvent::Started)).await.unwrap();
        let (id, event) = mgr.next_event().await.unwrap();
        assert_eq!(id, 1);
        assert!(matches!(event, AgentEvent::Started));
    }

    #[tokio::test]
    async fn running_state_starts_false_and_is_settable() {
        // A freshly registered agent is not running. set_running flips the
        // flag, which list_agents reads (the event forwarder calls this on
        // Started/Finished events).
        let mut mgr = AgentManager::new(16);
        let (tx, _rx) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "test".into(), tx));

        assert_eq!(mgr.list().len(), 1);
        assert!(!mgr.get(1).unwrap().is_running());

        mgr.set_running(1, true);
        assert!(mgr.get(1).unwrap().is_running());

        mgr.set_running(1, false);
        assert!(!mgr.get(1).unwrap().is_running());
    }

    #[tokio::test]
    async fn remove_drops_dead_agent_from_list() {
        // After an agent finishes (Finished event), the forwarder calls
        // remove() so the dead handle doesn't linger in list_agents forever.
        let mut mgr = AgentManager::new(16);
        let (tx, _rx) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "test".into(), tx));
        assert_eq!(mgr.len(), 1);

        mgr.remove(1);
        assert_eq!(mgr.len(), 0);
        assert!(mgr.list().is_empty());
        assert!(mgr.get(1).is_none());
    }

    #[tokio::test]
    async fn has_ever_started_latches_on_first_run_and_never_clears() {
        // The pending-vs-completed distinction cleanup_inactive_subagents
        // needs (live 2026-09-08, plan ccff0138): a freshly registered agent
        // is not running AND has never started (pending); after one turn it
        // is not running but HAS started (completed idle). `running` alone
        // cannot tell the two apart.
        let mut mgr = AgentManager::new(16);
        let (tx, _rx) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "test".into(), tx));

        let h = mgr.get(1).unwrap();
        assert!(!h.is_running() && !h.has_ever_started(), "pending");

        mgr.set_running(1, true);
        let h = mgr.get(1).unwrap();
        assert!(h.is_running() && h.has_ever_started(), "running");

        mgr.set_running(1, false);
        let h = mgr.get(1).unwrap();
        assert!(
            !h.is_running() && h.has_ever_started(),
            "completed idle — the latch must NOT clear with the running flag"
        );
    }

    #[tokio::test]
    async fn set_running_for_unknown_agent_is_noop() {
        // Setting running state for a non-existent agent must not panic.
        let mgr = AgentManager::new(16);
        mgr.set_running(999, true);
        mgr.set_running(999, false);
    }

    #[tokio::test]
    async fn parent_id_is_none_by_default_and_some_after_with_parent() {
        // A UI/main agent (plain AgentHandle) has no parent. A tool-spawned
        // agent built via `with_parent` records its spawning agent, so the
        // event forwarder can route a completion notification back to it.
        let mut mgr = AgentManager::new(16);
        let (tx1, _r1) = mpsc::channel(1);
        let (tx2, _r2) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "main".into(), tx1));
        mgr.register(AgentHandle::new(2, "reviewer".into(), tx2).with_parent(1));

        assert_eq!(mgr.parent_id(1), None, "main agent has no parent");
        assert_eq!(
            mgr.parent_id(2),
            Some(1),
            "tool-spawned agent must record its parent"
        );
        assert_eq!(mgr.parent_id(999), None, "unknown agent has no parent");
    }

    #[tokio::test]
    async fn send_to_parent_after_child_registered() {
        // The completion path: the forwarder reads parent_id(child) and sends
        // the parent a Suggestion. Verify the manager can actually deliver a
        // Suggestion to a registered parent (and that sending to a removed
        // parent fails gracefully rather than panicking).
        let mut mgr = AgentManager::new(16);
        let (parent_tx, mut parent_rx) = mpsc::channel(4);
        let (child_tx, _child_rx) = mpsc::channel(4);
        mgr.register(AgentHandle::new(1, "main".into(), parent_tx));
        mgr.register(AgentHandle::new(2, "child".into(), child_tx).with_parent(1));

        // Route the notification like the forwarder does.
        let parent = mgr.parent_id(2).expect("child must have a parent");
        mgr.send(
            parent,
            AgentCommand::Suggestion("[background agent \"child\" finished]".into()),
        )
        .expect("send to live parent must succeed");

        match parent_rx.recv().await {
            Some(AgentCommand::Suggestion(text)) => {
                assert!(
                    text.text.contains("child"),
                    "notification names the child"
                );
            }
            other => panic!("expected a Suggestion to the parent, got {other:?}"),
        }

        // If the parent is removed first, sending the notification must fail
        // gracefully (Err), not panic — the forwarder drops it best-effort.
        mgr.remove(1);
        assert!(
            mgr.parent_id(2).is_some(),
            "child still records its parent id"
        );
        let result = mgr.send(parent, AgentCommand::Suggestion("x".into()));
        assert!(result.is_err(), "send to a removed parent returns Err");
    }

    #[tokio::test]
    async fn main_agent_id_is_the_parentless_agent_with_smallest_id() {
        // The main agent (no parent) is the dispatch target for backlog
        // prompts. Subagents (with a parent) must never be selected, even if
        // they somehow get a smaller id.
        let mut mgr = AgentManager::new(16);
        let (tx1, _r1) = mpsc::channel(1);
        let (tx2, _r2) = mpsc::channel(1);
        let (tx3, _r3) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "main".into(), tx1));
        mgr.register(AgentHandle::new(2, "sub".into(), tx2).with_parent(1));
        mgr.register(AgentHandle::new(3, "second-main".into(), tx3));

        assert_eq!(mgr.main_agent_id(), Some(1));
    }

    #[tokio::test]
    async fn main_agent_id_none_when_only_subagents_registered() {
        // If only tool-spawned agents exist, there is no main agent.
        let mut mgr = AgentManager::new(16);
        let (tx, _r) = mpsc::channel(1);
        mgr.register(AgentHandle::new(5, "sub".into(), tx).with_parent(99));
        assert_eq!(mgr.main_agent_id(), None);
    }

    #[tokio::test]
    async fn has_running_descendants_only_for_running_children() {
        // Direct child running → true. Once it stops → false. A grandchild
        // (child of child) running also counts.
        let mut mgr = AgentManager::new(16);
        let (tx1, _r1) = mpsc::channel(1);
        let (tx2, _r2) = mpsc::channel(1);
        let (tx3, _r3) = mpsc::channel(1);
        mgr.register(AgentHandle::new(1, "main".into(), tx1));
        mgr.register(AgentHandle::new(2, "child".into(), tx2).with_parent(1));
        mgr.register(AgentHandle::new(3, "grandchild".into(), tx3).with_parent(2));

        assert!(!mgr.has_running_descendants(1), "nothing running yet");

        mgr.set_running(2, true);
        assert!(mgr.has_running_descendants(1), "direct child running");

        mgr.set_running(2, false);
        mgr.set_running(3, true);
        assert!(mgr.has_running_descendants(1), "grandchild running");

        mgr.set_running(3, false);
        assert!(!mgr.has_running_descendants(1), "all descendants idle");

        // The main agent itself running is NOT its own descendant.
        mgr.set_running(1, true);
        assert!(!mgr.has_running_descendants(1), "self is not a descendant");
    }
}
