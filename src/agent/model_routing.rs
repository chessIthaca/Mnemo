// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pre-prompt model routing: the Laya classifier's task-complexity decision
//! layer (backlog 091e694d, item 2 of the Laya chain).
//!
//! At the start of a main-agent user turn the task text is classified as
//! `trivial` (a small, local, mechanical change) or `architectural`
//! (design-level work) with a calibrated choice question
//! ([`routing_question`]). The decision is gated three ways:
//!
//! 1. The [`LayaConfig::routing`](crate::config::LayaConfig::routing) opt-in
//!    must be on and a classifier installed.
//! 2. The answer must clear the configured confidence gate
//!    ([`threshold`](crate::config::RoutingConfig::threshold), default
//!    `0.80`) with a label the question actually offered.
//! 3. A target must be configured for that label
//!    ([`cheap`](crate::config::RoutingConfig::cheap) /
//!    [`capable`](crate::config::RoutingConfig::capable)) and resolve to a
//!    live endpoint.
//!
//! Every miss keeps the turn's pre-classifier model exactly: no answer,
//! below-threshold, unknown label, unset or dangling target. **Shadow-first**:
//! while [`enforce`](crate::config::RoutingConfig::enforce) is off the
//! decision is still taken and logged, but the model is never switched.
//!
//! # The routing log
//!
//! Every classified turn writes two JSONL rows to [`routing_log_path`]
//! (`~/.mnemo/laya/training/routing.jsonl`): a [`RouteDecisionRow`] when the
//! turn starts (task text, label, confidence, threshold, the selected target
//! and whether it was enforced) and a [`RouteOutcomeRow`] when it ends
//! (terminal status, iterations, tool calls, the tool-error count), joined by
//! `turn_id`. That is
//! the labeled corpus the router needs before enforcement is defensible: base
//! Laya checkpoints are near-chance zero-shot on this task, so the flag ships
//! with enforcement off and the log collects
//! task text -> decision -> outcome until a fine-tune and a gate calibration
//! can be validated against it. Both writes are best-effort: a log failure
//! never fails a turn. A turn whose classifier gave no answer at all writes
//! nothing -- there is no label to learn from, and the classifier status
//! surface already reports the broken backend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::memory::classifier::{Answer, Classifier, Question};

use super::failure_triage::training_log_dir;

/// The label the routing question expects for a small, local, mechanical
/// task.
pub const ROUTING_LABEL_TRIVIAL: &str = "trivial";

/// The label the routing question expects for design-level work.
pub const ROUTING_LABEL_ARCHITECTURAL: &str = "architectural";

/// Maximum chars of the task text handed to the classifier and stored in the
/// routing log.
pub const ROUTING_TEXT_MAX_CHARS: usize = 2000;

/// The routing targets a confident classification selects between.
///
/// `Cheap` / `Capable` are the pre-prompt routing targets (backlog 091e694d);
/// `Medium` / `High` / `Escalate` are the escalation-lane rungs (backlog
/// ad56c7bd) selected step-granularly by the compound reflex call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTarget {
    /// A confidently-TRIVIAL task: run on the configured `cheap` model.
    Cheap,
    /// A confidently-ARCHITECTURAL task: run on the configured `capable`
    /// model.
    Capable,
    /// A medium-complexity plan step (reflex rung `medium`): run on the
    /// configured `lane_medium` model.
    Medium,
    /// A high-complexity plan step (reflex rung `high`): run on the
    /// configured `lane_high` model.
    High,
    /// An escalate-rung plan step (reflex rung `escalate`, typically after a
    /// failed cycle): run on the configured `escalate` model.
    Escalate,
}

impl RouteTarget {
    /// The wire/log label (`"cheap"` / `"capable"` / `"medium"` / `"high"` /
    /// `"escalate"`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Capable => "capable",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Escalate => "escalate",
        }
    }
}

/// The pre-prompt routing question: one calibrated choice between
/// [`ROUTING_LABEL_TRIVIAL`] and [`ROUTING_LABEL_ARCHITECTURAL`].
///
/// The labels are rendered verbatim by Laya, so they are plain class names
/// (never boolean-ish words the checkpoints can follow instead of the
/// descriptions).
pub fn routing_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        ROUTING_LABEL_TRIVIAL.to_string(),
        "A small, local, mechanical change: a typo, a rename, a version bump, a one-line \
         fix, or a doc tweak. Well understood, confined to one file or a couple of lines, \
         and needing no design decisions."
            .to_string(),
    );
    criteria.insert(
        ROUTING_LABEL_ARCHITECTURAL.to_string(),
        "Design-level work: a new feature, a cross-module refactor, a new dependency, a \
         data-model or concurrency change, or anything that needs multi-step planning and \
         review."
            .to_string(),
    );
    Question::Choice {
        instructions: "Classify the coding task the user is asking the agent to perform. \
                       Pick the ONE label that best describes the work the task requires."
            .to_string(),
        criteria,
    }
}

/// One task-text classification: what the classifier answered and whether it
/// clears the confidence gate.
///
/// The answer is carried verbatim even when it selects no target -- a
/// below-threshold or unknown-label pick is exactly the calibration signal
/// the routing log exists to collect.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    /// The answered label (verbatim; an unknown label is kept as-is).
    pub label: String,
    /// The answer's calibrated confidence (0.0-1.0).
    pub confidence: f64,
    /// The per-option distribution, when the backend reported one.
    pub probabilities: BTreeMap<String, f64>,
    /// The target to route to: `Some` only for a known label at or above the
    /// configured threshold. `None` = keep today's model.
    pub target: Option<RouteTarget>,
}

/// Classify one turn's task text (capped at [`ROUTING_TEXT_MAX_CHARS`]) as
/// trivial vs architectural.
///
/// `None` = no usable answer (disabled backend, timeout, malformed response,
/// or a non-choice answer -- all treated as "no answer", mirroring the
/// failure-triage gate) -- the caller keeps today's model and logs nothing.
/// `Some(decision)` with `target: None` = an answer that did not clear the
/// gate (below `threshold`, or a label the question never offered): the
/// caller keeps today's model, but the decision IS logged.
pub async fn classify_task_text(
    classifier: &dyn Classifier,
    text: &str,
    threshold: f64,
) -> Option<RoutingDecision> {
    let capped = cap_task_text(text);
    let Answer::Choice {
        label,
        confidence,
        probabilities,
    } = classifier.classify(&capped, &routing_question()).await?
    else {
        return None;
    };
    let target = match label.as_str() {
        ROUTING_LABEL_TRIVIAL if confidence >= threshold => Some(RouteTarget::Cheap),
        ROUTING_LABEL_ARCHITECTURAL if confidence >= threshold => Some(RouteTarget::Capable),
        _ => None,
    };
    Some(RoutingDecision {
        label,
        confidence,
        probabilities,
        target,
    })
}

/// Cap the task text handed to the classifier (and stored in the log): a
/// pasted file dump is not a better routing signal than its opening lines.
pub fn cap_task_text(text: &str) -> String {
    if text.chars().count() > ROUTING_TEXT_MAX_CHARS {
        text.chars().take(ROUTING_TEXT_MAX_CHARS).collect()
    } else {
        text.to_string()
    }
}

/// One row of the routing training log, written when a turn starts and a
/// classification was obtained (`~/.mnemo/laya/training/routing.jsonl`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteDecisionRow {
    /// Unix milliseconds when the turn started.
    pub ts: u64,
    /// The turn this decision belongs to -- joins [`RouteOutcomeRow::turn_id`].
    pub turn_id: String,
    /// The agent the turn ran on (the main agent or a spawned one).
    pub agent_id: String,
    /// True while routing ran in shadow mode (`enforce` off): the decision was
    /// logged, the model was NOT switched.
    pub shadow: bool,
    /// The answered label (verbatim).
    pub label: String,
    /// The calibrated confidence of the answer.
    pub confidence: f64,
    /// The configured confidence gate the answer was compared against.
    pub threshold: f64,
    /// The selected target's label (`"cheap"` / `"capable"`); absent when the
    /// answer did not clear the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The escalation-lane rung a step-granular classification selected
    /// (backlog ad56c7bd): one of `"cheap"` / `"medium"` / `"high"` /
    /// `"escalate"`. Absent on pre-prompt (turn-granular) rows and on lane
    /// rows whose answer did not clear the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// The 0-based index of the plan step a lane row classified (backlog
    /// ad56c7bd; the same index [`Step`] carries). Absent on pre-prompt rows.
    ///
    /// [`Step`]: crate::workflow::plan_file::Step
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_index: Option<usize>,
    /// True when routing was IN EFFECT for this turn: enforcement was on (not
    /// shadow) and the selected target resolved to a live endpoint at turn
    /// start. Shadow, below-threshold and unknown-label decisions are all
    /// false, and so is an unset or dangling target.
    pub enforced: bool,
    /// The model the target resolved to at turn start (`endpoint/model`) --
    /// what this turn was routed onto. Absent when nothing resolved (shadow,
    /// below threshold, unset or dangling target). The per-iteration arm
    /// re-checks the same guard, so a mid-turn context switch (a skill
    /// starting, say) is the one case where a turn keeps its chain model
    /// despite a resolved target here. A target re-pointed mid-turn is
    /// re-resolved by later iterations, which can then run on the new model
    /// while this row keeps the turn-start fact; a target that did NOT resolve
    /// here is never armed, so it cannot start routing mid-turn either.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The task text handed to the classifier, capped at
    /// [`ROUTING_TEXT_MAX_CHARS`] -- the fine-tuning corpus.
    pub task_text: String,
    /// The per-option distribution, when the backend reported one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub probabilities: BTreeMap<String, f64>,
}

/// One row of the routing training log, written when the turn ends: the
/// outcome half of the corpus (task text -> decision -> what actually
/// happened).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteOutcomeRow {
    /// Unix milliseconds when the turn ended.
    pub ts: u64,
    /// The turn this outcome belongs to -- joins
    /// [`RouteDecisionRow::turn_id`].
    pub turn_id: String,
    /// The agent the turn ran on.
    pub agent_id: String,
    /// The terminal status the harness recorded: `"ok"` (no terminal failure
    /// seen), `"aborted"` (a hard internal abort: repeated tool errors, or the
    /// review-report failure cap) or `"error"` (the provider stream failed
    /// before producing output -- the `Err` the caller retries).
    pub outcome: String,
    /// Provider-request iterations the turn ran (its effort signal).
    pub iterations: usize,
    /// Tool calls the model requested this turn (its effort signal).
    pub tool_calls: usize,
    /// Consecutive-tool-error count as of the turn's last dispatch batch --
    /// the raw failure gradient behind `outcome`.
    pub tool_errors: u32,
}

/// The routing log: `~/.mnemo/laya/training/routing.jsonl`, beside the
/// failure-triage log so one directory holds every Laya fine-tuning corpus.
pub fn routing_log_path() -> PathBuf {
    training_log_dir().join("routing.jsonl")
}

/// A unique-enough id joining one turn's decision and outcome rows
/// (`<unix-millis>-<agent_id>`).
pub fn new_turn_id(agent_id: &str) -> String {
    format!("{}-{agent_id}", now_millis())
}

/// Append one row to `path` (JSONL). Best-effort by design: a log write
/// failure is swallowed -- training data must never fail a turn.
pub fn append_row<T: Serialize>(path: &Path, row: &T) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(line) = serde_json::to_string(row) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Unix milliseconds now (0 when the clock is before the epoch -- a log
/// timestamp must never panic). Public so the turn stamps its decision and
/// outcome rows from the same clock.
pub fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The shared pre-prompt routing gate: the app runtime's ONE classifier slot
/// (the same `Arc` the other Laya consumers hold), read at turn start.
///
/// There is no enable-flag mirror here, unlike the failure-triage handle: the
/// `[general.laya] routing` opt-in and the `[general.routing]` threshold /
/// `enforce` live in the LIVE config, which the model resolver reads per turn
/// ([`ModelResolver::routing_policy`](crate::model_resolver::ModelResolver::routing_policy)) --
/// so a Settings save lands on the next turn with no rewire. An empty slot
/// (Laya disabled or unconfigured) means the turn never classifies.
#[derive(Clone)]
pub struct RoutingGate {
    classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    /// Where this gate's decision + outcome rows are appended. The app keeps
    /// [`routing_log_path`]; tests point it at a temp file, so a suite run
    /// never touches the real training corpus.
    log_path: PathBuf,
}

impl RoutingGate {
    /// Build a gate over the shared classifier slot, logging to
    /// [`routing_log_path`] (`~/.mnemo/laya/training/routing.jsonl`).
    pub fn new(classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>) -> Self {
        Self {
            classifier,
            log_path: routing_log_path(),
        }
    }

    /// Redirect this gate's routing log to `path` (tests; the app keeps the
    /// default). Replaces [`new`](Self::new)'s default path.
    pub fn with_log_path(mut self, path: PathBuf) -> Self {
        self.log_path = path;
        self
    }

    /// The routing log this gate appends to.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// The classifier installed in the shared slot right now (`None` while
    /// Laya is disabled or unconfigured) -- a clone of the `Arc`, so the call
    /// site can await the classification without holding the lock.
    pub fn classifier(&self) -> Option<Arc<dyn Classifier>> {
        self.classifier
            .read()
            .expect("RoutingGate classifier lock poisoned")
            .clone()
    }
}

/// The live routing policy, read from `[general.routing]` per turn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoutingPolicy {
    /// The calibrated-probability gate a classification must clear to route.
    pub threshold: f64,
    /// Whether a confident decision switches the model (`false` = shadow:
    /// classify + log only, model untouched).
    pub enforce: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;

    /// A classifier with a canned answer that records what it was asked --
    /// `None` models every no-answer path (disabled backend, timeout,
    /// malformed response).
    struct StubClassifier {
        answer: Option<Answer>,
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<String>>,
    }

    impl StubClassifier {
        fn new(answer: Option<Answer>) -> Self {
            Self {
                answer,
                calls: Arc::new(AtomicUsize::new(0)),
                seen: Arc::new(Mutex::new(String::new())),
            }
        }
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, state: &str, _question: &Question) -> Option<Answer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.seen.lock().expect("seen lock") = state.to_string();
            self.answer.clone()
        }
    }

    fn choice(label: &str, confidence: f64) -> Option<Answer> {
        Some(Answer::Choice {
            label: label.to_string(),
            confidence,
            probabilities: BTreeMap::new(),
        })
    }

    #[test]
    fn routing_question_offers_both_labels_with_descriptions() {
        let Question::Choice {
            instructions,
            criteria,
        } = routing_question()
        else {
            panic!("routing_question must be a choice question");
        };
        assert!(!instructions.is_empty());
        let labels: Vec<_> = criteria.keys().cloned().collect();
        assert_eq!(
            labels,
            vec![
                ROUTING_LABEL_ARCHITECTURAL.to_string(),
                ROUTING_LABEL_TRIVIAL.to_string()
            ]
        );
        for description in criteria.values() {
            assert!(!description.is_empty());
        }
    }

    #[tokio::test]
    async fn confident_answers_pick_their_target() {
        let trivial = StubClassifier::new(choice(ROUTING_LABEL_TRIVIAL, 0.9));
        let decision = classify_task_text(&trivial, "bump the version", 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.label, ROUTING_LABEL_TRIVIAL);
        assert!((decision.confidence - 0.9).abs() < 1e-9);
        assert_eq!(decision.target, Some(RouteTarget::Cheap));

        let architectural = StubClassifier::new(choice(ROUTING_LABEL_ARCHITECTURAL, 0.95));
        let decision = classify_task_text(&architectural, "add a new feature", 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.label, ROUTING_LABEL_ARCHITECTURAL);
        assert_eq!(decision.target, Some(RouteTarget::Capable));
    }

    #[tokio::test]
    async fn threshold_is_inclusive_and_below_keeps_no_target() {
        let at = StubClassifier::new(choice(ROUTING_LABEL_TRIVIAL, 0.80));
        let decision = classify_task_text(&at, "typo", 0.80).await.expect("decision");
        assert_eq!(decision.target, Some(RouteTarget::Cheap));

        let below = StubClassifier::new(choice(ROUTING_LABEL_TRIVIAL, 0.79));
        let decision = classify_task_text(&below, "typo", 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.target, None);
        // The answer still rides the log: a below-threshold pick is the
        // calibration signal the corpus exists to collect.
        assert_eq!(decision.label, ROUTING_LABEL_TRIVIAL);
        assert!((decision.confidence - 0.79).abs() < 1e-9);
    }

    #[tokio::test]
    async fn no_answer_and_non_choice_answers_yield_none() {
        let none = StubClassifier::new(None);
        assert!(classify_task_text(&none, "typo", 0.80).await.is_none());

        let scored = StubClassifier::new(Some(Answer::Score {
            value: 1.0,
            confidence: 0.99,
        }));
        assert!(classify_task_text(&scored, "typo", 0.80).await.is_none());
    }

    #[tokio::test]
    async fn unknown_label_records_the_answer_but_picks_no_target() {
        let odd = StubClassifier::new(choice("medium", 0.99));
        let decision = classify_task_text(&odd, "something", 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.label, "medium");
        assert_eq!(decision.target, None);
    }

    #[tokio::test]
    async fn task_text_is_capped_before_the_classifier_sees_it() {
        let stub = StubClassifier::new(choice(ROUTING_LABEL_TRIVIAL, 0.9));
        let long = "x".repeat(ROUTING_TEXT_MAX_CHARS + 500);
        classify_task_text(&stub, &long, 0.80).await.expect("decision");
        let seen = stub.seen.lock().expect("seen lock").clone();
        assert_eq!(seen.chars().count(), ROUTING_TEXT_MAX_CHARS);
        assert_eq!(stub.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn decision_and_outcome_rows_round_trip_through_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("routing.jsonl");
        let mut probabilities = BTreeMap::new();
        probabilities.insert(ROUTING_LABEL_TRIVIAL.to_string(), 0.93);
        probabilities.insert(ROUTING_LABEL_ARCHITECTURAL.to_string(), 0.07);
        let decision = RouteDecisionRow {
            ts: 1_700_000_000_000,
            turn_id: "1700000000000-main".into(),
            agent_id: "main".into(),
            shadow: true,
            label: ROUTING_LABEL_TRIVIAL.into(),
            confidence: 0.93,
            threshold: 0.80,
            target: Some(RouteTarget::Cheap.label().to_string()),
            lane: None,
            step_index: None,
            enforced: false,
            model: None,
            task_text: "bump the version to 1.1.1".into(),
            probabilities,
        };
        let outcome = RouteOutcomeRow {
            ts: 1_700_000_000_500,
            turn_id: decision.turn_id.clone(),
            agent_id: "main".into(),
            outcome: "ok".into(),
            iterations: 1,
            tool_calls: 2,
            tool_errors: 0,
        };
        append_row(&path, &decision);
        append_row(&path, &outcome);

        let text = std::fs::read_to_string(&path).expect("log readable");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let back: RouteDecisionRow = serde_json::from_str(lines[0]).expect("decision row");
        let back_outcome: RouteOutcomeRow = serde_json::from_str(lines[1]).expect("outcome row");
        assert_eq!(back, decision);
        assert_eq!(back_outcome, outcome);
        assert_eq!(back_outcome.turn_id, back.turn_id);
    }

    #[test]
    fn lane_labels_are_the_plain_rung_names() {
        // The escalation-lane rungs (backlog ad56c7bd) use the same plain
        // class labels the other Laya consumers do; the pre-prompt labels
        // stay byte-identical (the shipped corpus depends on them).
        assert_eq!(RouteTarget::Medium.label(), "medium");
        assert_eq!(RouteTarget::High.label(), "high");
        assert_eq!(RouteTarget::Escalate.label(), "escalate");
        assert_eq!(RouteTarget::Cheap.label(), "cheap");
        assert_eq!(RouteTarget::Capable.label(), "capable");
    }

    #[test]
    fn lane_rows_round_trip_and_old_shape_rows_still_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("routing.jsonl");
        let lane_row = RouteDecisionRow {
            ts: 1_700_000_001_000,
            turn_id: "1700000001000-main".into(),
            agent_id: "main".into(),
            shadow: false,
            label: "high".into(),
            confidence: 0.91,
            threshold: 0.80,
            target: Some(RouteTarget::High.label().to_string()),
            lane: Some(RouteTarget::High.label().to_string()),
            step_index: Some(2),
            enforced: true,
            model: Some("ep/model".into()),
            task_text: "wire the lane ladder".into(),
            probabilities: BTreeMap::new(),
        };
        append_row(&path, &lane_row);
        let text = std::fs::read_to_string(&path).expect("log readable");
        let back: RouteDecisionRow =
            serde_json::from_str(text.lines().next().expect("one line")).expect("lane row");
        assert_eq!(back, lane_row);

        // A row written BEFORE the lane fields existed (no `lane`, no
        // `step_index`) still parses: both default to None.
        let old = r#"{"ts":1,"turn_id":"t","agent_id":"main","shadow":true,"label":"trivial","confidence":0.9,"threshold":0.8,"target":"cheap","enforced":false,"task_text":"x"}"#;
        let back_old: RouteDecisionRow = serde_json::from_str(old).expect("old row parses");
        assert_eq!(back_old.lane, None);
        assert_eq!(back_old.step_index, None);
    }

    #[test]
    fn routing_log_path_sits_beside_the_failure_triage_log() {
        // One directory holds every Laya fine-tuning corpus.
        let path = routing_log_path();
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some("routing.jsonl"));
        assert_eq!(path.parent(), Some(training_log_dir().as_path()));
    }
}
