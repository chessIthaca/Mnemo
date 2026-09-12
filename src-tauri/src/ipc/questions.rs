// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pending-questions map — holds the non-serializable `oneshot::Sender`s for
//! `ask_user` questions.
//!
//! Twin of [`crate::ipc::approval::PendingApprovals`]. When the agent sends a
//! `UserQuestion` event, the `oneshot::Sender<UserAnswer>` can't cross the
//! Tauri IPC boundary. This map holds it, keyed by `(agent_id,
//! question_id)`. The frontend responds via the `answer_question` command,
//! which looks up the sender and resolves it.
//!
//! Keying by agent is what makes `cleanup_for_agent` correct: when one agent
//! exits, only ITS pending questions are dropped — a concurrently waiting
//! agent's question sender is left untouched.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use mnemo::runtime::{AgentId, UserAnswer};

/// A pending question awaiting the user's answer.
#[derive(Debug)]
struct PendingEntry {
    sender: oneshot::Sender<UserAnswer>,
}

/// A map of pending question senders, keyed by `(agent_id, question_id)`.
///
/// Mirrors [`crate::ipc::approval::PendingApprovals`] in shape + semantics:
/// `resolve` finds the single entry whose `question_id` matches (a question
/// id is globally unique per ask_user call), and `cleanup_for_agent` drops
/// only one exited agent's entries.
#[derive(Debug, Default)]
pub struct PendingQuestions {
    map: Mutex<HashMap<(AgentId, String), PendingEntry>>,
}

impl PendingQuestions {
    /// Create an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of pending questions.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map
            .lock()
            .expect("pending questions mutex poisoned")
            .len()
    }

    /// Whether there are no pending questions.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Store a pending question sender for the given agent + question id.
    pub fn insert(
        &self,
        agent_id: AgentId,
        question_id: String,
        sender: oneshot::Sender<UserAnswer>,
    ) {
        let mut map = self.map.lock().expect("pending questions mutex poisoned");
        map.insert((agent_id, question_id), PendingEntry { sender });
    }

    /// Resolve a pending question by `question_id`. Returns `true` if found.
    ///
    /// The frontend's `answer_question` command only supplies a `question_id`
    /// (it has no agent id), but a `question_id` is globally unique per
    /// `ask_user` call, so a pending question is unambiguously identified by
    /// it. We scan for the single entry whose `question_id` matches, regardless
    /// of which agent owns it (mirrors `PendingApprovals::resolve`).
    pub fn resolve(&self, question_id: &str, answer: UserAnswer) -> bool {
        let mut map = self.map.lock().expect("pending questions mutex poisoned");
        // Find the (unique) key whose question_id matches.
        let key = map.keys().find(|(_, qid)| qid == question_id).cloned();
        if let Some(key) = key {
            if let Some(entry) = map.remove(&key) {
                let _ = entry.sender.send(answer);
                return true;
            }
        }
        false
    }

    /// Drop all pending question senders belonging to one exited agent.
    ///
    /// Called by the event forwarder when an agent is removed (on `Finished` /
    /// `Exited` / final `Error`). Only entries whose key's `agent_id` matches
    /// are removed — other agents' pending questions are left untouched.
    /// Returns the count of dropped senders.
    pub fn cleanup_for_agent(&self, agent_id: AgentId) -> usize {
        let mut map = self.map.lock().expect("pending questions mutex poisoned");
        let before = map.len();
        map.retain(|(aid, _), _| *aid != agent_id);
        before - map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_and_resolve_choice() {
        let pending = PendingQuestions::new();
        let (tx, rx) = oneshot::channel();
        pending.insert(1, "q_1".into(), tx);
        assert_eq!(pending.len(), 1);

        let resolved = pending.resolve("q_1", UserAnswer::Choice { index: 2 });
        assert!(resolved);

        let answer = rx.await.unwrap();
        assert_eq!(answer, UserAnswer::Choice { index: 2 });
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn insert_and_resolve_freeform() {
        let pending = PendingQuestions::new();
        let (tx, rx) = oneshot::channel();
        pending.insert(1, "q_2".into(), tx);

        pending.resolve(
            "q_2",
            UserAnswer::Freeform {
                text: "it's purple".into(),
            },
        );
        let answer = rx.await.unwrap();
        assert_eq!(
            answer,
            UserAnswer::Freeform {
                text: "it's purple".into()
            }
        );
    }

    #[tokio::test]
    async fn resolve_unknown_returns_false() {
        let pending = PendingQuestions::new();
        assert!(!pending.resolve("nonexistent", UserAnswer::Choice { index: 0 }));
    }

    #[tokio::test]
    async fn cleanup_for_agent_drops_only_that_agents_pending() {
        // Two agents each have a pending question. Cleaning up agent A must
        // drop ONLY A's entry — agent B's sender must survive.
        let pending = PendingQuestions::new();
        let (tx_a, _rx_a) = oneshot::channel();
        let (tx_b, rx_b) = oneshot::channel();
        pending.insert(1, "q_a".into(), tx_a);
        pending.insert(2, "q_b".into(), tx_b);
        assert_eq!(pending.len(), 2);

        let dropped = pending.cleanup_for_agent(1);
        assert_eq!(dropped, 1, "only agent 1's entry should be dropped");
        assert_eq!(pending.len(), 1, "agent 2's entry must remain");

        // Agent 2's question is still resolvable — its sender wasn't dropped.
        let resolved = pending.resolve("q_b", UserAnswer::Choice { index: 0 });
        assert!(resolved);
        let answer = rx_b.await.unwrap();
        assert_eq!(answer, UserAnswer::Choice { index: 0 });
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn cleanup_for_agent_when_empty_is_zero() {
        let pending = PendingQuestions::new();
        assert_eq!(pending.cleanup_for_agent(1), 0);
    }
}
