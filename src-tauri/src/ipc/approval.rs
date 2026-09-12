// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pending-approvals map — holds the non-serializable `oneshot::Sender`s.
//!
//! When the agent sends an `ApprovalRequest`, the `oneshot::Sender` can't cross
//! the Tauri IPC boundary. This map holds it, keyed by `(agent_id,
//! tool_call_id)`. The frontend responds via the `approve` command, which looks
//! up the sender and resolves it.
//!
//! Keying by agent is what makes `cleanup_for_agent` correct: when one agent
//! exits, only ITS pending approvals are dropped — a concurrently waiting
//! agent's approval sender is left untouched.
//!
//! Each entry also records the tool name, args, and whether the call is a core
//! operation (git merge/push). That metadata lets [`re_evaluate`] auto-resolve
//! a pending approval when the safety mode changes to one that would auto-run
//! the call — except core operations, which always stay gated.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;
use tokio::sync::oneshot;

use mnemo::agent::approval::needs_approval;
use mnemo::config::SafetyMode;
use mnemo::runtime::{AgentId, Approval};
use mnemo::tool::agent::sandbox::Sandbox;
use mnemo::tool::SafetyLevel;

/// A pending approval awaiting the user's decision, plus the metadata needed
/// to re-evaluate it when the safety mode changes.
#[derive(Debug)]
struct PendingEntry {
    sender: oneshot::Sender<Approval>,
    tool_name: String,
    args: Value,
    /// True for core operations (git merge/push) that must always prompt
    /// regardless of mode — `re_evaluate` never auto-resolves these.
    core_operation: bool,
}

/// A map of pending approval requests, keyed by `(agent_id, tool_call_id)`.
#[derive(Debug, Default)]
pub struct PendingApprovals {
    map: Mutex<HashMap<(AgentId, String), PendingEntry>>,
}

impl PendingApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of pending approvals.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .expect("pending approvals mutex poisoned")
            .len()
    }

    /// Whether there are no pending approvals.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Store a pending approval sender for the given agent + tool call, along
    /// with the metadata needed to re-evaluate it on a safety-mode change.
    pub fn insert(
        &self,
        agent_id: AgentId,
        tool_call_id: String,
        sender: oneshot::Sender<Approval>,
        tool_name: String,
        args: Value,
        core_operation: bool,
    ) {
        let mut map = self.map.lock().expect("pending approvals mutex poisoned");
        map.insert(
            (agent_id, tool_call_id),
            PendingEntry {
                sender,
                tool_name,
                args,
                core_operation,
            },
        );
    }

    /// Resolve a pending approval by `tool_call_id`. Returns `true` if found.
    ///
    /// The frontend's `approve` command only supplies a `tool_call_id` (it has
    /// no agent id), but a `tool_call_id` is globally unique per tool call, so
    /// a pending approval is unambiguously identified by it. We therefore scan
    /// for the single entry whose `tool_call_id` matches, regardless of which
    /// agent owns it. Cleanup — not resolution — is what must be agent-scoped.
    pub fn resolve(&self, tool_call_id: &str, approval: Approval) -> bool {
        let mut map = self.map.lock().expect("pending approvals mutex poisoned");
        // Find the (unique) key whose tool_call_id matches.
        let key = map.keys().find(|(_, tcid)| tcid == tool_call_id).cloned();
        if let Some(key) = key {
            if let Some(entry) = map.remove(&key) {
                let _ = entry.sender.send(approval);
                return true;
            }
        }
        false
    }

    /// Re-evaluate all pending approvals against a new safety mode.
    ///
    /// Called after the safety mode changes (via `set_safety_mode` or
    /// `save_settings`). For each pending approval, if the new mode would
    /// auto-run the call — i.e. `needs_approval` returns `false` — resolve it
    /// as `Approved` so the agent proceeds without waiting for the user.
    ///
    /// Core operations (`core_operation == true`, e.g. git merge/push) are
    /// **never** auto-resolved: they must always prompt regardless of mode, so
    /// a mode change cannot silently approve them. Entries that still need
    /// approval under the new mode are left untouched (the user must still
    /// answer them).
    ///
    /// Returns the number of approvals auto-resolved.
    pub fn re_evaluate(&self, mode: SafetyMode, sandbox: &Sandbox) -> usize {
        let mut map = self.map.lock().expect("pending approvals mutex poisoned");
        let mut resolved = 0;
        // Collect the keys to auto-resolve first, then remove + send outside the
        // borrow of the iteration (can't send while iterating the map).
        let to_resolve: Vec<(AgentId, String)> = map
            .iter()
            .filter(|(_, entry)| {
                !entry.core_operation
                    && !needs_approval(
                        SafetyLevel::NeedsApproval,
                        mode,
                        &entry.tool_name,
                        &entry.args,
                        sandbox,
                    )
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in to_resolve {
            if let Some(entry) = map.remove(&key) {
                let _ = entry.sender.send(Approval::Approve);
                resolved += 1;
            }
        }
        resolved
    }

    /// Drop all pending approval senders belonging to one exited agent.
    ///
    /// Called by the event forwarder when an agent is removed (on `Finished` /
    /// `Exited`). Only entries whose key's `agent_id` matches are removed —
    /// other agents' pending approvals are left untouched, so a concurrently
    /// waiting agent's approval sender is never dropped by a peer's exit.
    /// Without this, an exited agent's oneshot senders would leak: the agent
    /// task is gone, so the receiver is dropped, but the sender would stay in
    /// the map forever. Dropping the sender is harmless (the receiver already
    /// got `Err` on drop); this just frees that agent's map entries.
    ///
    /// Returns the count of dropped senders.
    pub fn cleanup_for_agent(&self, agent_id: AgentId) -> usize {
        let mut map = self.map.lock().expect("pending approvals mutex poisoned");
        let before = map.len();
        map.retain(|(aid, _), _| *aid != agent_id);
        before - map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build a sandbox rooted at a temp dir for re_evaluate tests.
    fn test_sandbox() -> Sandbox {
        let dir = tempdir().unwrap();
        Sandbox::new(dir.path()).unwrap()
    }

    #[tokio::test]
    async fn insert_and_resolve() {
        let pending = PendingApprovals::new();
        let (tx, rx) = oneshot::channel();
        pending.insert(
            1,
            "call_1".into(),
            tx,
            "file_edit".into(),
            serde_json::json!({"path": "src/x.rs"}),
            false,
        );
        assert_eq!(pending.len(), 1);

        let resolved = pending.resolve("call_1", Approval::Approve);
        assert!(resolved);

        let approval = rx.await.unwrap();
        assert_eq!(approval, Approval::Approve);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn resolve_unknown_returns_false() {
        let pending = PendingApprovals::new();
        assert!(!pending.resolve("nonexistent", Approval::Deny));
    }

    #[tokio::test]
    async fn cleanup_for_agent_drops_only_that_agents_pending() {
        // Two agents each have a pending approval. Cleaning up agent A must
        // drop ONLY A's entry — agent B's sender must survive (the H4 bug:
        // previously the whole map was cleared on any agent exit).
        let pending = PendingApprovals::new();
        let (tx_a, _rx_a) = oneshot::channel();
        let (tx_b, rx_b) = oneshot::channel();
        pending.insert(
            1,
            "call_a".into(),
            tx_a,
            "file_edit".into(),
            serde_json::json!({}),
            false,
        );
        pending.insert(
            2,
            "call_b".into(),
            tx_b,
            "file_edit".into(),
            serde_json::json!({}),
            false,
        );
        assert_eq!(pending.len(), 2);

        let dropped = pending.cleanup_for_agent(1);
        assert_eq!(dropped, 1, "only agent 1's entry should be dropped");
        assert_eq!(pending.len(), 1, "agent 2's entry must remain");

        // Agent 2's approval is still resolvable — its sender wasn't dropped.
        let resolved = pending.resolve("call_b", Approval::Approve);
        assert!(resolved);
        let approval = rx_b.await.unwrap();
        assert_eq!(approval, Approval::Approve);
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn cleanup_for_agent_when_empty_is_zero() {
        let pending = PendingApprovals::new();
        assert_eq!(pending.cleanup_for_agent(1), 0);
    }

    // ---- re_evaluate ----

    #[tokio::test]
    async fn re_evaluate_auto_resolves_on_autonomous() {
        // A pending file_edit approval: under Autonomous mode it would
        // auto-run, so re_evaluate resolves it as Approved.
        let pending = PendingApprovals::new();
        let (tx, rx) = oneshot::channel();
        pending.insert(
            1,
            "call_1".into(),
            tx,
            "file_edit".into(),
            serde_json::json!({"path": "src/x.rs"}),
            false,
        );
        let sandbox = test_sandbox();
        let n = pending.re_evaluate(SafetyMode::Autonomous, &sandbox);
        assert_eq!(n, 1, "the file_edit approval should be auto-resolved");
        assert!(pending.is_empty());
        assert_eq!(rx.await.unwrap(), Approval::Approve);
    }

    #[tokio::test]
    async fn re_evaluate_skips_core_operations() {
        // A core operation (git merge) must NEVER be auto-resolved, even under
        // Autonomous mode — it always requires a contemporaneous user approval.
        let pending = PendingApprovals::new();
        let (tx, _rx) = oneshot::channel();
        pending.insert(
            1,
            "call_1".into(),
            tx,
            "git".into(),
            serde_json::json!({"subcommand": "merge", "branch": "feat"}),
            true, // core_operation
        );
        let sandbox = test_sandbox();
        let n = pending.re_evaluate(SafetyMode::Autonomous, &sandbox);
        assert_eq!(n, 0, "core operations must not be auto-resolved");
        assert_eq!(pending.len(), 1, "the core-op approval must remain pending");
    }

    #[tokio::test]
    async fn re_evaluate_leaves_still_pending_alone() {
        // Under ApproveEachAction, a file_edit still needs approval —
        // re_evaluate must leave it untouched.
        let pending = PendingApprovals::new();
        let (tx, _rx) = oneshot::channel();
        pending.insert(
            1,
            "call_1".into(),
            tx,
            "file_edit".into(),
            serde_json::json!({"path": "src/x.rs"}),
            false,
        );
        let sandbox = test_sandbox();
        let n = pending.re_evaluate(SafetyMode::ApproveEachAction, &sandbox);
        assert_eq!(
            n, 0,
            "nothing should be auto-resolved under ApproveEachAction"
        );
        assert_eq!(pending.len(), 1, "the approval must remain pending");
    }

    #[tokio::test]
    async fn re_evaluate_auto_resolves_project_scoped_under_auto_approve_project() {
        // Under AutoApproveProject, a project-scoped file_edit auto-runs.
        let pending = PendingApprovals::new();
        let (tx, rx) = oneshot::channel();
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // Create the file so the sandbox validate succeeds (project-scoped).
        std::fs::write(dir.path().join("src_x.rs"), "").unwrap();
        pending.insert(
            1,
            "call_1".into(),
            tx,
            "file_edit".into(),
            serde_json::json!({"path": "src_x.rs"}),
            false,
        );
        let n = pending.re_evaluate(SafetyMode::AutoApproveProject, &sandbox);
        assert_eq!(n, 1, "project-scoped file_edit should auto-resolve");
        assert_eq!(rx.await.unwrap(), Approval::Approve);
    }

    #[tokio::test]
    async fn re_evaluate_mixed_core_and_non_core() {
        // One core op + one regular op pending. Under Autonomous, only the
        // regular op is auto-resolved; the core op stays.
        let pending = PendingApprovals::new();
        let (tx_core, _rx_core) = oneshot::channel();
        let (tx_file, rx_file) = oneshot::channel();
        pending.insert(
            1,
            "call_merge".into(),
            tx_core,
            "git".into(),
            serde_json::json!({"subcommand": "merge", "branch": "feat"}),
            true,
        );
        pending.insert(
            1,
            "call_edit".into(),
            tx_file,
            "file_edit".into(),
            serde_json::json!({"path": "src/x.rs"}),
            false,
        );
        let sandbox = test_sandbox();
        let n = pending.re_evaluate(SafetyMode::Autonomous, &sandbox);
        assert_eq!(n, 1, "only the non-core approval should be resolved");
        assert_eq!(pending.len(), 1, "the core-op approval must remain");
        assert_eq!(rx_file.await.unwrap(), Approval::Approve);
    }

    #[tokio::test]
    async fn re_evaluate_empty_is_zero() {
        let pending = PendingApprovals::new();
        let sandbox = test_sandbox();
        assert_eq!(pending.re_evaluate(SafetyMode::Autonomous, &sandbox), 0);
    }
}
