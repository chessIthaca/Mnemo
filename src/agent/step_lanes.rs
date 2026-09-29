// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Step-granular lane routing (backlog ad56c7bd): the escalation lane ladder's
//! PURE decision logic — no classifier, no loop, no config plumbing, so every
//! rule below is unit-testable without a running turn.
//!
//! The ladder resolves a plan STEP, not a whole turn: at each step boundary
//! the compound reflex call ([`super::reflex`]) classifies the step in ONE
//! request, and the rung it answers — `cheap` → default → `medium` → `high` →
//! `escalate` — selects the matching `[general.routing]` lane target
//! ([`Cheap`](LaneRung::Cheap) rides the pre-prompt `cheap` target) for the
//! iterations that follow. A rung whose target is unset (the default) or
//! dangling falls through to the turn's configured model: the ladder NEVER
//! invents a model id, and a miss is always the fail-safe.
//!
//! # When a classification happens
//!
//! [`needs_classify`] gates every call: a NEW plan step is [`Fresh`]
//! (classified on its first attempt, never before), and the SAME step is
//! re-asked ([`ReClassify`](ClassifyHow::ReClassify)) only after a failed
//! cycle — the loop bumps its escalation epoch when a confident failure-triage
//! verdict lands, so a step can never be routed up before its first attempt.
//! Between those events the memoized [`LaneState`] is reused: one classifier
//! call per step per attempt, not one per iteration.
//!
//! # What is logged
//!
//! Every classification writes a routing row ([`decided_row`] /
//! [`fallback_row`]) carrying the rung (`lane`), the step index and the
//! selection — the labeled corpus the ladder needs before enforcement is
//! defensible. A DECIDED classification additionally feeds the reflex corpus
//! ([`super::reflex::ReflexHandle::log_decision`], the caller's job). A
//! below-gate or protocol-deviation answer is itself the calibration signal
//! and gets a row; a `NoAnswer` writes nothing at all, mirroring the
//! pre-prompt routing contract ([`fallback_is_loggable`]).
//!
//! **Shadow-first**: while `[general.routing] enforce` is off the decision is
//! taken and logged but [`LaneState::effective_target`] stays `None`, so the
//! model is never switched. The `risk_flag` the reflex decision carries is
//! recorded for the corpus and never routes on its own.

use std::collections::BTreeMap;

use crate::config::ModelRef;

use super::model_routing;
use super::model_routing::{RouteDecisionRow, RouteTarget};
use super::reflex::{Complexity, ReflexAction, ReflexDecision, ReflexKeepReason, REFLEX_THRESHOLD};

/// One rung of the escalation ladder, cheapest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneRung {
    /// The bounded-change rung: rides the pre-prompt `[general.routing]
    /// cheap` target (the task-complexity router already owns that model).
    Cheap,
    /// A normal multi-file change: the `lane_medium` target.
    Medium,
    /// Capable-lane work: the `lane_high` target.
    High,
    /// Hand-it-upward work: the `escalate` target.
    Escalate,
}

impl LaneRung {
    /// The wire/log label (`"cheap"` / `"medium"` / `"high"` / `"escalate"`),
    /// identical to the [`RouteTarget`] label of the same name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Escalate => "escalate",
        }
    }

    /// The routing target this rung selects.
    pub fn to_route_target(self) -> RouteTarget {
        match self {
            Self::Cheap => RouteTarget::Cheap,
            Self::Medium => RouteTarget::Medium,
            Self::High => RouteTarget::High,
            Self::Escalate => RouteTarget::Escalate,
        }
    }
}

/// The rung a decided reflex decision selects.
///
/// Complexity picks the rung (`small` → cheap, `medium` → medium, `high` →
/// high, `escalate` → escalate), and an `escalate` ACTION overrides it
/// outright: the action question answers "what should happen to this step
/// next", and when the answer is "hand it upward" the lane follows even if the
/// step text itself looked small. The decision's `risk_flag` is recorded but
/// never routes here — risk colors the corpus, cost decisions stay with
/// complexity + action.
pub fn rung_from_decision(decision: &ReflexDecision) -> LaneRung {
    if decision.action == ReflexAction::Escalate {
        return LaneRung::Escalate;
    }
    match decision.complexity {
        Complexity::Small => LaneRung::Cheap,
        Complexity::Medium => LaneRung::Medium,
        Complexity::High => LaneRung::High,
        Complexity::Escalate => LaneRung::Escalate,
    }
}

/// The state text one step is classified from.
///
/// The first attempt classifies the step text alone. A retry — the loop bumps
/// its escalation epoch after a confident failure-triage verdict — appends the
/// verdict, so the re-ask can legitimately answer higher without inventing
/// facts the first attempt never had: the failure IS new information, and
/// suppressing it would make the second opinion identical to the first.
///
/// The text is handed to [`super::reflex::classify_reflex`] (which caps it) and
/// stored on the routing row through [`model_routing::cap_task_text`], so no
/// cap is applied here.
pub fn state_text(step_text: &str, failed_with: Option<&str>) -> String {
    match failed_with {
        None => step_text.to_string(),
        Some(verdict) => format!(
            "{step_text}\n\nThe previous attempt at this step failed ({verdict}). This is a \
             retry: judge the step for the second attempt, and answer higher when the failure \
             points at complexity or risk beyond what the step text alone shows."
        ),
    }
}

/// The label a fallback routing row carries: a stable marker naming why the
/// step kept its configured model.
pub fn keep_reason_label(reason: &ReflexKeepReason) -> &'static str {
    match reason {
        ReflexKeepReason::NoAnswer => "no_answer",
        ReflexKeepReason::LowConfidence(_) => "low_confidence",
        ReflexKeepReason::UnknownLabel(_) => "unknown_label",
    }
}

/// Whether a fallback reason is worth a routing row.
///
/// `NoAnswer` is not: there is no label to learn from, and the disabled path
/// is structurally the same shape — the pre-prompt routing contract writes
/// nothing there either. A below-gate confidence or a protocol deviation IS
/// the calibration signal the corpus exists to collect.
pub fn fallback_is_loggable(reason: &ReflexKeepReason) -> bool {
    !matches!(reason, ReflexKeepReason::NoAnswer)
}

/// The confidence a fallback reason carries (0.0 when it carries none).
fn reason_confidence(reason: &ReflexKeepReason) -> f64 {
    match reason {
        ReflexKeepReason::LowConfidence(confidence) => *confidence,
        _ => 0.0,
    }
}

/// The classification memo for one plan step: what the ladder decided, and
/// under which escalation epoch.
///
/// Stored per loop; [`needs_classify`] compares it with the step the agent is
/// on now to decide whether a new classifier call is owed.
#[derive(Debug, Clone, PartialEq)]
pub struct LaneState {
    /// The plan the classification was made for (a different plan re-asks).
    pub plan_title: String,
    /// The 0-based step index classified (the plan step's own index — the
    /// same value the routing row records).
    pub step_index: usize,
    /// The rung's route target; `None` for a fallback (no usable answer).
    pub target: Option<RouteTarget>,
    /// True while the ladder is shadow-first (`enforce` off): the decision was
    /// logged, the model is never switched.
    pub shadow: bool,
    /// The label behind the state: the answered complexity label, or the
    /// [`keep_reason_label`] marker for a fallback.
    pub label: String,
    /// The calibrated confidence behind the state (0.0 for a fallback that
    /// carries none).
    pub confidence: f64,
    /// The escalation epoch this classification was made under: when the loop
    /// bumps it (a failed cycle), the same step re-classifies.
    pub epoch: u64,
}

impl LaneState {
    /// Record a decided classification for one step.
    pub fn decided(
        plan_title: impl Into<String>,
        step_index: usize,
        decision: &ReflexDecision,
        shadow: bool,
        epoch: u64,
    ) -> Self {
        Self {
            plan_title: plan_title.into(),
            step_index,
            target: Some(rung_from_decision(decision).to_route_target()),
            shadow,
            label: decision.complexity.label().to_string(),
            confidence: decision.confidence,
            epoch,
        }
    }

    /// Record a fallback: no usable answer, so the step keeps its configured
    /// model.
    pub fn fallback(
        plan_title: impl Into<String>,
        step_index: usize,
        reason: &ReflexKeepReason,
        shadow: bool,
        epoch: u64,
    ) -> Self {
        Self {
            plan_title: plan_title.into(),
            step_index,
            target: None,
            shadow,
            label: keep_reason_label(reason).to_string(),
            confidence: reason_confidence(reason),
            epoch,
        }
    }

    /// The route target in effect for this step: `None` while the ladder is
    /// shadow-first (classified + logged, model never switched — the
    /// unvalidated default) and `None` for a fallback. The caller hands this
    /// to the resolver, whose own unset/dangling check is the last fail-safe.
    pub fn effective_target(&self) -> Option<RouteTarget> {
        if self.shadow {
            None
        } else {
            self.target
        }
    }
}

/// Why (and whether) a new classification is owed for the current step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassifyHow {
    /// No memo yet, or a different plan/step: classify this step's first
    /// attempt.
    Fresh,
    /// The same step, but a failed cycle has happened since (the escalation
    /// epoch moved): re-classify with the failure in the state text.
    ReClassify,
    /// The same step with no new event: reuse the memoized decision.
    Reuse,
}

/// What to do about the current step, given the memoized state.
///
/// `state`/`plan_title`/`step_index` describe the step the agent is on NOW, and
/// `epoch` is the loop's escalation epoch (bumped by a confident
/// failure-triage verdict). A `None` step (no active plan) is the caller's
/// concern — it never asks; this function assumes a step exists.
pub fn needs_classify(
    state: Option<&LaneState>,
    plan_title: &str,
    step_index: usize,
    epoch: u64,
) -> ClassifyHow {
    match state {
        None => ClassifyHow::Fresh,
        Some(current) if current.plan_title != plan_title || current.step_index != step_index => {
            ClassifyHow::Fresh
        }
        Some(current) if current.epoch != epoch => ClassifyHow::ReClassify,
        Some(_) => ClassifyHow::Reuse,
    }
}

/// Build the routing training row for a DECIDED lane classification.
///
/// `resolved` is the lane target as the resolver returned it (`None` = unset
/// or dangling): the row records the rung the decision selected AND whether it
/// actually armed, so the corpus keeps both halves of the story. Shadow mode
/// drops the resolution outright — nothing was armed, so nothing is claimed.
pub fn decided_row(
    turn_id: &str,
    agent_id: &str,
    step_index: usize,
    decision: &ReflexDecision,
    state_text: &str,
    shadow: bool,
    resolved: Option<&ModelRef>,
) -> RouteDecisionRow {
    let rung = rung_from_decision(decision);
    // Shadow never records a model as applied (the pre-prompt row contract):
    // with `enforce` off the decision is logged and the model is untouched.
    let resolved = if shadow { None } else { resolved };
    RouteDecisionRow {
        ts: model_routing::now_millis(),
        turn_id: turn_id.to_string(),
        agent_id: agent_id.to_string(),
        shadow,
        label: decision.complexity.label().to_string(),
        confidence: decision.confidence,
        threshold: REFLEX_THRESHOLD,
        target: Some(rung.to_route_target().label().to_string()),
        lane: Some(rung.as_str().to_string()),
        step_index: Some(step_index),
        enforced: resolved.is_some(),
        model: resolved.map(|m| format!("{}/{}", m.endpoint, m.model)),
        task_text: model_routing::cap_task_text(state_text),
        probabilities: BTreeMap::new(),
    }
}

/// Build the routing training row for a FALLBACK lane classification: the step
/// keeps its configured model, and the row carries why (the
/// [`fallback_is_loggable`] reasons only — a `NoAnswer` writes nothing).
pub fn fallback_row(
    turn_id: &str,
    agent_id: &str,
    step_index: usize,
    reason: &ReflexKeepReason,
    state_text: &str,
    shadow: bool,
) -> RouteDecisionRow {
    RouteDecisionRow {
        ts: model_routing::now_millis(),
        turn_id: turn_id.to_string(),
        agent_id: agent_id.to_string(),
        shadow,
        label: keep_reason_label(reason).to_string(),
        confidence: reason_confidence(reason),
        threshold: REFLEX_THRESHOLD,
        target: None,
        lane: None,
        step_index: Some(step_index),
        enforced: false,
        model: None,
        task_text: model_routing::cap_task_text(state_text),
        probabilities: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::reflex::RiskFlag;
    use super::*;

    /// A reflex decision with a fixed risk flag and confidence -- these tests
    /// care about complexity + action.
    fn decision(complexity: Complexity, action: ReflexAction) -> ReflexDecision {
        ReflexDecision {
            complexity,
            action,
            risk_flag: RiskFlag::None,
            confidence: 0.9,
        }
    }

    fn endpoint_ref() -> ModelRef {
        ModelRef {
            endpoint: "ep".into(),
            model: "m".into(),
            reasoning_effort: None,
        }
    }

    #[test]
    fn rung_follows_complexity_and_escalate_action_overrides() {
        for (complexity, expected) in [
            (Complexity::Small, LaneRung::Cheap),
            (Complexity::Medium, LaneRung::Medium),
            (Complexity::High, LaneRung::High),
            (Complexity::Escalate, LaneRung::Escalate),
        ] {
            assert_eq!(
                rung_from_decision(&decision(complexity, ReflexAction::Continue)),
                expected,
                "{complexity:?} must select {expected:?}"
            );
        }
        // The action question overrides the complexity read: an explicit
        // "hand it upward" answers the escalate rung whatever the step's size.
        assert_eq!(
            rung_from_decision(&decision(Complexity::Small, ReflexAction::Escalate)),
            LaneRung::Escalate
        );
        // No other action moves the rung.
        for action in [
            ReflexAction::Retry,
            ReflexAction::Verify,
            ReflexAction::Complete,
        ] {
            assert_eq!(
                rung_from_decision(&decision(Complexity::High, action)),
                LaneRung::High
            );
        }
    }

    #[test]
    fn lane_rungs_use_the_routing_vocabulary() {
        // One label per rung, and the same label the routing target carries --
        // the config keys, the rows and the log speak one vocabulary.
        for (rung, label) in [
            (LaneRung::Cheap, "cheap"),
            (LaneRung::Medium, "medium"),
            (LaneRung::High, "high"),
            (LaneRung::Escalate, "escalate"),
        ] {
            assert_eq!(rung.as_str(), label);
            assert_eq!(rung.to_route_target().label(), label);
        }
    }

    #[test]
    fn state_text_adds_the_failure_only_on_a_retry() {
        let first = state_text("wire the lane ladder", None);
        assert_eq!(first, "wire the lane ladder");

        let retry = state_text("wire the lane ladder", Some("permanent"));
        assert!(retry.starts_with("wire the lane ladder"));
        assert!(retry.contains("permanent"));
        assert!(retry.contains("second attempt"));
        assert_ne!(retry, first);

        let flaky = state_text("wire the lane ladder", Some("flaky_test"));
        assert!(flaky.contains("flaky_test"));
    }

    #[test]
    fn keep_reasons_have_stable_labels_and_loggability() {
        assert_eq!(keep_reason_label(&ReflexKeepReason::NoAnswer), "no_answer");
        assert_eq!(
            keep_reason_label(&ReflexKeepReason::LowConfidence(0.42)),
            "low_confidence"
        );
        assert_eq!(
            keep_reason_label(&ReflexKeepReason::UnknownLabel("odd".into())),
            "unknown_label"
        );
        // A no-answer writes nothing (the pre-prompt contract); the other two
        // ARE the calibration signal and get a row.
        assert!(!fallback_is_loggable(&ReflexKeepReason::NoAnswer));
        assert!(fallback_is_loggable(&ReflexKeepReason::LowConfidence(0.4)));
        assert!(fallback_is_loggable(&ReflexKeepReason::UnknownLabel(
            "odd".into()
        )));
    }

    #[test]
    fn needs_classify_is_fresh_then_reuse_and_reclassifies_after_a_failure() {
        // First sight of a step: fresh -- classified on its first attempt.
        assert_eq!(needs_classify(None, "plan", 0, 0), ClassifyHow::Fresh);

        let state = LaneState::decided(
            "plan",
            1,
            &decision(Complexity::Medium, ReflexAction::Continue),
            false,
            3,
        );
        // Same step, same epoch: reuse -- no second classifier call.
        assert_eq!(
            needs_classify(Some(&state), "plan", 1, 3),
            ClassifyHow::Reuse
        );
        // A different step, or a different plan, is a fresh step.
        assert_eq!(
            needs_classify(Some(&state), "plan", 2, 3),
            ClassifyHow::Fresh
        );
        assert_eq!(
            needs_classify(Some(&state), "other plan", 1, 3),
            ClassifyHow::Fresh
        );
        // A failed cycle moved the epoch: re-classify the same step.
        assert_eq!(
            needs_classify(Some(&state), "plan", 1, 4),
            ClassifyHow::ReClassify
        );
    }

    #[test]
    fn decided_state_carries_the_rung_and_the_row_both_halves() {
        let decided = decision(Complexity::High, ReflexAction::Verify);
        let state = LaneState::decided("plan", 2, &decided, false, 7);
        assert_eq!(state.plan_title, "plan");
        assert_eq!(state.step_index, 2);
        assert_eq!(state.target, Some(RouteTarget::High));
        assert_eq!(state.label, "high");
        assert!((state.confidence - 0.9).abs() < 1e-9);
        assert_eq!(state.epoch, 7);
        assert_eq!(state.effective_target(), Some(RouteTarget::High));

        let turn_id = "1700000000000-main";
        let resolved = endpoint_ref();
        let row = decided_row(turn_id, "main", 2, &decided, "wire it", false, Some(&resolved));
        assert_eq!(row.turn_id, turn_id);
        assert_eq!(row.agent_id, "main");
        assert_eq!(row.label, "high");
        assert!((row.confidence - 0.9).abs() < 1e-9);
        assert!((row.threshold - REFLEX_THRESHOLD).abs() < 1e-9);
        assert_eq!(row.target.as_deref(), Some("high"));
        assert_eq!(row.lane.as_deref(), Some("high"));
        assert_eq!(row.step_index, Some(2));
        assert!(!row.shadow);
        assert!(row.enforced);
        assert_eq!(row.model.as_deref(), Some("ep/m"));
        assert_eq!(row.task_text, "wire it");
        assert!(row.probabilities.is_empty());

        // An unresolved lane target (unset or dangling): the row records the
        // selected rung but nothing armed -- the step keeps its model.
        let unarmed = decided_row(turn_id, "main", 2, &decided, "wire it", false, None);
        assert_eq!(unarmed.target.as_deref(), Some("high"));
        assert!(!unarmed.enforced);
        assert!(unarmed.model.is_none());
    }

    #[test]
    fn shadow_state_never_yields_a_target_and_the_row_says_so() {
        let decided = decision(Complexity::Escalate, ReflexAction::Escalate);
        let shadow = LaneState::decided("plan", 0, &decided, true, 0);
        // The rung is recorded, but never in effect.
        assert_eq!(shadow.target, Some(RouteTarget::Escalate));
        assert_eq!(shadow.effective_target(), None);

        let resolved = endpoint_ref();
        let row = decided_row("t", "main", 0, &decided, "step", true, Some(&resolved));
        assert!(row.shadow);
        assert!(!row.enforced);
        // Shadow claims no model even when the target resolves: the decision
        // is logged, the model is not switched.
        assert!(row.model.is_none());
    }

    #[test]
    fn fallback_rows_keep_the_model_and_carry_the_reason() {
        let state = LaneState::fallback(
            "plan",
            1,
            &ReflexKeepReason::LowConfidence(0.42),
            false,
            5,
        );
        assert_eq!(state.target, None);
        assert_eq!(state.label, "low_confidence");
        assert!((state.confidence - 0.42).abs() < 1e-9);
        assert_eq!(state.effective_target(), None);

        let row = fallback_row(
            "t",
            "main",
            1,
            &ReflexKeepReason::LowConfidence(0.42),
            "wire it",
            false,
        );
        assert_eq!(row.label, "low_confidence");
        assert!((row.confidence - 0.42).abs() < 1e-9);
        assert!(row.target.is_none());
        assert!(row.lane.is_none());
        assert_eq!(row.step_index, Some(1));
        assert!(!row.enforced);
        assert!(row.model.is_none());
        assert_eq!(row.task_text, "wire it");

        // A protocol deviation carries no confidence of its own.
        let unknown = fallback_row(
            "t",
            "main",
            1,
            &ReflexKeepReason::UnknownLabel("odd".into()),
            "wire it",
            true,
        );
        assert_eq!(unknown.label, "unknown_label");
        assert!(unknown.confidence.abs() < f64::EPSILON);
        assert!(unknown.shadow);
    }
}
