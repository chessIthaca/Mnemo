// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Turn-resolution domain logic — extracted from the IPC adapter (review M1a).
//!
//! [`TurnResolveLatch`] ensures a turn resolves the backlog **at most once**:
//! some failure paths emit a final `Error { retrying: false }` *and then*
//! `Finished`; without a latch both resolutions would run (the old contract
//! rolled back on Error and then commit-successed on Finished; today the
//! failure resolution would take the non-closure disposition and the
//! success resolution would then act on stale state). The latch also
//! defers failure/success resolution when descendants are still running.
//!
//! [`workflow_transition_counts`] gates plan-loop evidence: only a REAL
//! workflow state transition counts as proof the plan loop ran.
//!
//! [`completion_suggestion_text`] and [`reviewer_failure_suggestion_text`]
//! build the `Suggestion` text sent to a child's parent — shared by the GUI
//! event forwarder and the console runtime so both produce identical text.
//!
//! These types use only brain types ([`AgentId`], [`WorkflowState`]) — no
//! Tauri or IPC dependencies — so they live in the brain, not the adapter.

use std::collections::{HashMap, HashSet};

use crate::runtime::AgentId;
use crate::workflow::WorkflowState;

/// Outcome of a terminal event for Run-All / auto-feed bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveAction {
    /// Call `on_main_turn_resolved(true, None)`.
    Success,
    /// Call `on_main_turn_resolved(false, Some(note))`.
    Failure(String),
    /// Already resolved this turn, or still waiting on descendants — no-op.
    None,
}

/// Per-turn latch so a final `Error` and a subsequent `Finished` cannot both
/// resolve Run-All / auto-feed for the same agent turn.
///
/// Also defers failure resolution when the main agent errors while descendants
/// are still running — the next idle terminal event (Finished, or Error after
/// children drain) delivers the failure once.
#[derive(Debug, Default)]
pub struct TurnResolveLatch {
    /// Agents whose turn failed (final Error seen). Value is the error note.
    failed_note: HashMap<AgentId, String>,
    /// Agents whose turn already called `on_main_turn_resolved` this turn.
    resolved: HashSet<AgentId>,
    /// Agents whose workflow changed state at least once this turn (a
    /// `WorkflowStateChanged` event was observed). Proof the plan loop
    /// actually RAN during the turn — `Complete` is a resting state, so a
    /// turn that never entered a plan (freeform answer, `ask_user` before
    /// planning) ends in `Complete` too and must NOT count as done.
    workflow_changed: HashSet<AgentId>,
    /// Agents whose Finished arrived while descendants were still running —
    /// the success is deferred until descendants drain. Failures are already
    /// tracked via `failed_note`; this tracks deferred SUCCESSES so they are
    /// not lost when the next turn starts (the bug that caused backlog items
    /// to be marked Failed despite the work being done and merged).
    pending_finished: HashSet<AgentId>,
    /// Agents whose ROOT plan was abandoned this turn — an
    /// `Executing`/`Reviewing` → `Planning` workflow transition was observed
    /// (the only channel-visible path INTO `Planning` mid-flight: the
    /// dispatch-time `Complete` → `Planning` entry never touches the event
    /// channel, and a sub-plan abandon pops to its parent and stays
    /// `Executing`). Drives the plan-tied `Failed` transition (backlog
    /// 45dcf577): failure means exactly "the plan was abandoned".
    plan_abandoned: HashSet<AgentId>,
    /// The abandoned ROOT plan's id per agent (backlog bba2c82d) — captured
    /// from the turn's transition evidence by the forwarder (the PREVIOUS
    /// top plan id tracked across `WorkflowStateChanged` events; after the
    /// abandonment pop the live `main_agent_top_plan_id` no longer names
    /// the abandoned plan). `None` = the id is unknown (the abandonment
    /// predates the tracking or the agent's first event was the
    /// abandonment) — the Failed-transition guard then falls back to the
    /// pre-guard behavior.
    abandoned_plan_ids: HashMap<AgentId, Option<String>>,
}

impl TurnResolveLatch {
    /// A new turn started — clear any stale marks.
    pub fn on_started(&mut self, agent_id: AgentId) {
        self.failed_note.remove(&agent_id);
        self.resolved.remove(&agent_id);
        self.workflow_changed.remove(&agent_id);
        self.pending_finished.remove(&agent_id);
        self.plan_abandoned.remove(&agent_id);
        self.abandoned_plan_ids.remove(&agent_id);
    }

    /// A final Error arrived. `descendants_running` gates whether failure can
    /// resolve now (same rule as success on Finished).
    pub fn on_final_error(
        &mut self,
        agent_id: AgentId,
        note: String,
        descendants_running: bool,
    ) -> ResolveAction {
        if self.resolved.contains(&agent_id) {
            return ResolveAction::None;
        }
        self.failed_note.insert(agent_id, note.clone());
        if descendants_running {
            return ResolveAction::None;
        }
        self.resolved.insert(agent_id);
        ResolveAction::Failure(note)
    }

    /// A Finished arrived. Success only if no prior final Error; if a prior
    /// Error was deferred (descendants), deliver failure now when idle.
    pub fn on_finished(&mut self, agent_id: AgentId, descendants_running: bool) -> ResolveAction {
        if self.resolved.contains(&agent_id) {
            // Already resolved (e.g. Error path) — Finished is idle bookkeeping.
            self.failed_note.remove(&agent_id);
            self.pending_finished.remove(&agent_id);
            return ResolveAction::None;
        }
        if descendants_running {
            // Defer: record the pending success so it can be flushed when
            // descendants drain (flush_deferred_main_failure). Without this,
            // the deferred success is lost when the next turn starts
            // (on_started clears everything) and the resolution slides to a
            // later turn where the workflow state is wrong.
            self.pending_finished.insert(agent_id);
            return ResolveAction::None;
        }
        if let Some(note) = self.failed_note.remove(&agent_id) {
            self.resolved.insert(agent_id);
            self.pending_finished.remove(&agent_id);
            return ResolveAction::Failure(note);
        }
        self.resolved.insert(agent_id);
        self.pending_finished.remove(&agent_id);
        ResolveAction::Success
    }

    /// Record that the agent's workflow changed state this turn (a
    /// `WorkflowStateChanged` event was observed).
    pub fn note_workflow_changed(&mut self, agent_id: AgentId) {
        self.workflow_changed.insert(agent_id);
    }

    /// Whether any workflow state transition was observed for the agent this
    /// turn — evidence the plan loop actually ran (see `workflow_changed`).
    pub fn workflow_changed(&self, agent_id: AgentId) -> bool {
        self.workflow_changed.contains(&agent_id)
    }

    /// Record that the agent's ROOT plan was abandoned this turn (an
    /// `Executing`/`Reviewing` → `Planning` workflow transition was
    /// observed — see `plan_abandoned`). `plan_id` is the abandoned
    /// plan's id from the turn's transition evidence (backlog bba2c82d):
    /// the forwarder passes the PREVIOUS top plan id it tracked before
    /// this event — the event's own `top_plan_id` is the post-pop top.
    pub fn note_plan_abandoned(&mut self, agent_id: AgentId, plan_id: Option<String>) {
        self.plan_abandoned.insert(agent_id);
        self.abandoned_plan_ids.insert(agent_id, plan_id);
    }

    /// Whether the agent's root plan was abandoned this turn (see
    /// `plan_abandoned`) — the one turn event that marks a dispatched
    /// backlog item `Failed` (backlog 45dcf577).
    pub fn plan_abandoned(&self, agent_id: AgentId) -> bool {
        self.plan_abandoned.contains(&agent_id)
    }

    /// The abandoned ROOT plan's id for the agent this turn (backlog
    /// bba2c82d) — `None` when unknown (see `abandoned_plan_ids`). The
    /// Failed-transition guard uses it as the linkage evidence: only an
    /// item whose `plan_id` IS the abandoned plan may flip to `Failed`.
    pub fn abandoned_plan_id(&self, agent_id: AgentId) -> Option<&str> {
        self.abandoned_plan_ids
            .get(&agent_id)
            .and_then(|p| p.as_deref())
    }

    /// Drop state when the agent task exits.
    pub fn on_exited(&mut self, agent_id: AgentId) {
        self.failed_note.remove(&agent_id);
        self.resolved.remove(&agent_id);
        self.workflow_changed.remove(&agent_id);
        self.pending_finished.remove(&agent_id);
        self.plan_abandoned.remove(&agent_id);
        self.abandoned_plan_ids.remove(&agent_id);
    }

    /// If `main_id` has a deferred resolution (failure or success) and no
    /// descendants are running, deliver it now that the tree is idle.
    ///
    /// Failures are always flushed (a failure is terminal). Successes are
    /// flushed too — but the caller (`try_flush_deferred_main_resolution`)
    /// gates the success delivery on the workflow being `Complete`, so a
    /// deferred success while the workflow is still `Reviewing` (the main
    /// agent spawned a reviewer and ended its turn) is NOT delivered
    /// prematurely — the main agent will be resumed to finish its plan.
    pub fn flush_deferred_main_failure(
        &mut self,
        main_id: AgentId,
        descendants_running: bool,
    ) -> ResolveAction {
        if descendants_running || self.resolved.contains(&main_id) {
            return ResolveAction::None;
        }
        if let Some(note) = self.failed_note.remove(&main_id) {
            self.resolved.insert(main_id);
            self.pending_finished.remove(&main_id);
            return ResolveAction::Failure(note);
        }
        if self.pending_finished.remove(&main_id) {
            self.resolved.insert(main_id);
            return ResolveAction::Success;
        }
        ResolveAction::None
    }

    /// Re-mark a pending-finished entry that was consumed by
    /// `flush_deferred_main_failure` but whose success delivery was deferred
    /// (the workflow was not yet `Complete`). This lets a later
    /// descendant-drain or the next turn still resolve the item.
    pub fn remark_pending_finished(&mut self, agent_id: AgentId) {
        if self.resolved.contains(&agent_id) {
            self.resolved.remove(&agent_id);
            self.pending_finished.insert(agent_id);
        }
    }
}

/// Whether an observed `WorkflowStateChanged` counts as plan-loop evidence:
/// only a REAL transition (new state differs from the last observed one).
/// `None` prev (first-ever observation for the agent) counts — the map only
/// tracks states we've seen events for. Guards against a no-op event emitted
/// with the unchanged state (defense in depth behind the `result.success`
/// gating at the emission site — review finding 1, 2026-08-20: a failed
/// workflow tool call must never fake "the loop ran" evidence).
pub fn workflow_transition_counts(prev: Option<WorkflowState>, new: WorkflowState) -> bool {
    prev != Some(new)
}

/// The failure text for the failed-reviewer protocol: a `role: "reviewer"`
/// child ended (failed OR finished cleanly) with NO report — the review did
/// not happen, and respawning the same reviewer on the same model almost
/// certainly reproduces the same failure. The parent must use `ask_user`
/// instead of blindly respawning: retry on a different model, or abandon the
/// review (the main agent can never author a review itself — reviewer-only
/// authorship is structural).
pub fn reviewer_failure_suggestion_text(child_name: &str) -> String {
    format!(
        "[background reviewer \"{child_name}\" FAILED to produce a review report — no report \
         was written, so the review did not happen. Do NOT respawn it unchanged (the same task \
         on the same model will almost certainly fail the same way). Use ask_user to ask the \
         user how to proceed: retry the review on a different model, or abandon the review \
         (abandon_plan returns to Planning). The main agent can never author a review itself]"
    )
}

/// Build the completion-notification `Suggestion` text sent to a child's
/// parent agent. When the child recorded a review-report path (via
/// `write_review_report`), the path is included so the parent can read the
/// report directly — instead of a generic "read its report" message that
/// forces the parent to search for the file (unreliable with multiple
/// concurrent reviewers). Extracted as a pure helper so the path-inclusion
/// logic is unit-testable without constructing a full `AgentLoop`.
///
/// Shared by the GUI event forwarder and the console runtime so both produce
/// identical notification text — a console parent gets the same steer as a
/// GUI parent (the report path when one was written, an explicit "no report
/// was written" otherwise).
pub fn completion_suggestion_text(
    child_name: &str,
    outcome: &str,
    report_path: Option<&str>,
) -> String {
    match report_path {
        Some(path) => format!(
            "[background agent \"{child_name}\" {outcome} — read its report at {path} and \
             combine the results into a plan]"
        ),
        None => format!(
            "[background agent \"{child_name}\" {outcome} — no report was written; combine \
             the results from its session output into a plan]"
        ),
    }
}

#[cfg(test)]
mod turn_resolve_latch_tests {
    use super::{workflow_transition_counts, ResolveAction, TurnResolveLatch};
    use crate::workflow::WorkflowState;

    /// Quality C1: final Error then Finished must not both count as terminal
    /// resolutions — Finished after a failure is idle bookkeeping only.
    #[test]
    fn final_error_then_finished_is_not_success() {
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        assert!(matches!(
            latch.on_final_error(1, "boom".into(), false),
            ResolveAction::Failure(n) if n == "boom"
        ));
        // Second Error must not double-resolve.
        assert_eq!(
            latch.on_final_error(1, "again".into(), false),
            ResolveAction::None
        );
        // Finished after resolved failure is idle bookkeeping.
        assert_eq!(latch.on_finished(1, false), ResolveAction::None);
    }

    #[test]
    fn clean_finished_counts_as_success() {
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        assert_eq!(latch.on_finished(1, false), ResolveAction::Success);
    }

    #[test]
    fn started_clears_stale_failure_mark() {
        let mut latch = TurnResolveLatch::default();
        let _ = latch.on_final_error(1, "old".into(), false);
        latch.on_started(1);
        assert_eq!(
            latch.on_finished(1, false),
            ResolveAction::Success,
            "new turn after prior failure must be allowed to succeed"
        );
    }

    #[test]
    fn exited_clears_failure_mark() {
        let mut latch = TurnResolveLatch::default();
        let _ = latch.on_final_error(2, "x".into(), true); // deferred
        latch.on_exited(2);
        assert_eq!(latch.on_finished(2, false), ResolveAction::Success);
    }

    #[test]
    fn plan_abandoned_flag_round_trips_and_clears_per_turn() {
        // Backlog 45dcf577: the abandonment mark is per-turn evidence — set
        // by the forwarder on an Executing/Reviewing → Planning transition,
        // read by the resolution paths (the one true `Failed`), cleared when
        // the next turn starts or the agent exits.
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        assert!(!latch.plan_abandoned(1));
        latch.note_plan_abandoned(1, None);
        assert!(latch.plan_abandoned(1));
        // A new turn clears the mark.
        latch.on_started(1);
        assert!(!latch.plan_abandoned(1));
        // Agent exit clears it too.
        latch.note_plan_abandoned(1, None);
        latch.on_exited(1);
        assert!(!latch.plan_abandoned(1));
        // Agents are independent.
        latch.note_plan_abandoned(2, None);
        assert!(!latch.plan_abandoned(1));
        assert!(latch.plan_abandoned(2));
    }

    #[test]
    fn abandoned_plan_id_round_trips_and_clears_per_turn() {
        // (backlog bba2c82d) The latch carries the abandoned plan's id
        // from the turn's transition evidence — the Failed guard's
        // linkage input (after the abandonment pop the live
        // main_agent_top_plan_id no longer names the abandoned plan).
        // Unknown (None) when the evidence predates the tracking;
        // cleared with the flag on turn start and agent exit.
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        assert_eq!(latch.abandoned_plan_id(1), None);
        latch.note_plan_abandoned(1, Some("abc123".to_string()));
        assert!(latch.plan_abandoned(1));
        assert_eq!(latch.abandoned_plan_id(1), Some("abc123"));
        // The blind case: the id is unknown but the flag is still set.
        latch.on_started(1);
        latch.note_plan_abandoned(1, None);
        assert!(latch.plan_abandoned(1));
        assert_eq!(latch.abandoned_plan_id(1), None);
        // A new turn clears both.
        latch.on_started(1);
        assert!(!latch.plan_abandoned(1));
        assert_eq!(latch.abandoned_plan_id(1), None);
        // Agent exit clears both.
        latch.note_plan_abandoned(1, Some("abc123".to_string()));
        latch.on_exited(1);
        assert!(!latch.plan_abandoned(1));
        assert_eq!(latch.abandoned_plan_id(1), None);
        // Agents are independent.
        latch.note_plan_abandoned(2, Some("zzz".to_string()));
        assert_eq!(latch.abandoned_plan_id(1), None);
        assert_eq!(latch.abandoned_plan_id(2), Some("zzz"));
    }

    #[test]
    fn agents_are_independent() {
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        latch.on_started(2);
        assert!(matches!(
            latch.on_final_error(1, "fail".into(), false),
            ResolveAction::Failure(_)
        ));
        assert_eq!(
            latch.on_finished(2, false),
            ResolveAction::Success,
            "agent 2 success must not be blocked by agent 1 failure"
        );
        assert_eq!(latch.on_finished(1, false), ResolveAction::None);
    }

    #[test]
    fn error_with_descendants_defers_until_finished_idle() {
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        assert_eq!(
            latch.on_final_error(1, "parent failed".into(), true),
            ResolveAction::None,
            "must not resolve while descendants run"
        );
        assert_eq!(
            latch.on_finished(1, true),
            ResolveAction::None,
            "still deferred while descendants run"
        );
        assert!(matches!(
            latch.on_finished(1, false),
            ResolveAction::Failure(n) if n == "parent failed"
        ));
    }

    #[test]
    fn workflow_changed_tracks_transitions_per_turn() {
        let mut latch = TurnResolveLatch::default();
        assert!(!latch.workflow_changed(1), "no observation yet");
        latch.note_workflow_changed(1);
        assert!(latch.workflow_changed(1));
        assert!(!latch.workflow_changed(2), "other agents unaffected");
        latch.on_started(1);
        assert!(!latch.workflow_changed(1), "a new turn resets the evidence");
        latch.note_workflow_changed(1);
        latch.on_exited(1);
        assert!(!latch.workflow_changed(1), "agent exit clears the evidence");
    }

    #[test]
    fn dispatch_time_planning_entry_is_not_loop_evidence() {
        let mut latch = TurnResolveLatch::default();
        latch.note_workflow_changed(1);
        latch.on_started(1);
        assert!(
            !latch.workflow_changed(1),
            "a pre-turn transition never counts as this turn's loop evidence"
        );
    }

    #[test]
    fn workflow_transition_counts_requires_a_real_state_change() {
        assert!(
            workflow_transition_counts(Some(WorkflowState::Complete), WorkflowState::Complete)
                == false,
            "a no-op event (same state) is NOT evidence"
        );
        assert!(
            workflow_transition_counts(None, WorkflowState::Complete),
            "first-ever observation counts (prev unknown)"
        );
        assert!(
            workflow_transition_counts(Some(WorkflowState::Complete), WorkflowState::Executing),
            "Complete → Executing is evidence"
        );
        assert!(
            workflow_transition_counts(Some(WorkflowState::Reviewing), WorkflowState::Complete),
            "Reviewing → Complete is evidence"
        );
    }

    #[test]
    fn deferred_success_is_flushed_when_descendants_drain() {
        let mut latch = TurnResolveLatch::default();
        latch.on_started(1);
        assert_eq!(
            latch.on_finished(1, true),
            ResolveAction::None,
            "must not resolve while descendants run"
        );
        assert_eq!(
            latch.flush_deferred_main_failure(1, false),
            ResolveAction::Success,
            "a deferred success must be flushed when descendants drain"
        );
    }
}

#[cfg(test)]
mod completion_suggestion_tests {
    use super::{completion_suggestion_text, reviewer_failure_suggestion_text};

    #[test]
    fn includes_report_path_when_present() {
        let text = completion_suggestion_text(
            "reviewer",
            "finished",
            Some(".coding/reviews/2026-04-04-x.md"),
        );
        assert!(text.contains("reviewer"));
        assert!(text.contains("finished"));
        assert!(
            text.contains(".coding/reviews/2026-04-04-x.md"),
            "text should name the report path, got: {text}"
        );
        assert!(text.contains("read its report at"));
    }

    #[test]
    fn generic_text_when_no_report_path() {
        // Backlog 5b46674d: the report-less completion text must SAY so —
        // "no report was written" — instead of the old generic "read its
        // report" (which sent the parent hunting for a report that never
        // existed; live-observed 2026-12-30: two reviewers finished
        // report-less and the parent needed shell forensics to find out).
        let text = completion_suggestion_text("investigator", "failed", None);
        assert!(text.contains("investigator"));
        assert!(text.contains("failed"));
        assert!(text.contains("no report was written"), "text: {text}");
        assert!(
            !text.contains("read its report at"),
            "no path → no 'at <path>' clause, got: {text}"
        );
    }

    #[test]
    fn reviewer_failure_text_distinct_and_instructs_ask_user() {
        let text = reviewer_failure_suggestion_text("reviewer");
        assert!(text.contains("FAILED"), "text: {text}");
        assert!(text.contains("no report"), "text: {text}");
        assert!(text.contains("Do NOT respawn"), "text: {text}");
        assert!(text.contains("ask_user"), "text: {text}");
        assert!(text.contains("different model"), "text: {text}");
        assert!(text.contains("abandon the review"), "text: {text}");
        assert!(
            !text.contains("self-review"),
            "must not mention self-review, got: {text}"
        );
    }
}
