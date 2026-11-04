// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pre-prompt model routing: the Laya classifier's task-complexity decision
//! layer (backlog 091e694d, item 2 of the Laya chain).
//!
//! On a main-agent session's first classifiable prompt the task text is
//! classified as `trivial`, `routine`, or `hard` with the shared calibrated
//! choice question ([`super::route_question::route_question`]). The decision is
//! gated three ways:
//!
//! 1. The [`LayaConfig::routing`](crate::config::LayaConfig::routing) opt-in
//!    must be on and a classifier installed.
//! 2. The answer must clear the first-prompt confidence gate
//!    ([`first_prompt_min_confidence`](crate::config::RoutingConfig::first_prompt_min_confidence),
//!    or the kernel default for the offered label count) with a known label.
//! 3. A target must be configured for that label
//!    ([`cheap`](crate::config::RoutingConfig::cheap) /
//!    [`routine`](crate::config::RoutingConfig::routine) /
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
//! Every classified first prompt writes two JSONL rows to [`routing_log_path`]
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

use crate::memory::classifier::{Answer, Classifier};

use super::failure_triage::training_log_dir;

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
    /// A confidently routine first prompt: run on the configured `routine`
    /// model, or keep the session's model when no target is configured.
    Routine,
    /// A confidently hard first prompt: run on the configured `capable`
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
    /// The wire/log label (`"cheap"` / `"routine"` / `"capable"` / `"medium"` / `"high"` /
    /// `"escalate"`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Cheap => "cheap",
            Self::Routine => "routine",
            Self::Capable => "capable",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Escalate => "escalate",
        }
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
    /// The confidence the gate reads (0.0-1.0): the top option's probability
    /// when the backend reported a distribution, else the pick's confidence
    /// (`route_question::answer_confidence`).
    pub confidence: f64,
    /// The per-option distribution, when the backend reported one.
    pub probabilities: BTreeMap<String, f64>,
    /// The target to route to: `Some` only for a known label at or above the
    /// configured threshold. `None` = keep today's model.
    pub target: Option<RouteTarget>,
}

/// Classify one session's first prompt (capped at
/// [`ROUTING_TEXT_MAX_CHARS`]) with the shared three-label question.
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
    offered: super::route_question::OfferedLabels,
    threshold: f64,
) -> Option<RoutingDecision> {
    let capped = cap_task_text(text);
    let question = super::route_question::route_question(offered)?;
    let answer = classifier.classify(&capped, &question).await?;
    let Answer::Choice { label, probabilities, .. } = &answer else {
        return None;
    };
    // The gate is calibrated on the top option's probability
    // (`route_question::answer_confidence`), not the pick's lower,
    // margin-like `confidence`.
    let confidence = super::route_question::answer_confidence(&answer);
    let target = match label.as_str() {
        super::route_question::LABEL_TRIVIAL if confidence >= threshold => Some(RouteTarget::Cheap),
        super::route_question::LABEL_ROUTINE if confidence >= threshold => Some(RouteTarget::Routine),
        super::route_question::LABEL_HARD if confidence >= threshold => Some(RouteTarget::Capable),
        _ => None,
    };
    Some(RoutingDecision {
        label: label.clone(),
        confidence,
        probabilities: probabilities.clone(),
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
    /// The gated confidence of the answer: the top option's probability when
    /// the backend reported a distribution, else the pick's confidence.
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
    /// ad56c7bd; the same index the plan-file `Step` type carries). Absent on
    /// pre-prompt rows.
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

/// The once-per-session routing log, beside the legacy per-turn log.
pub fn session_routing_log_path() -> PathBuf {
    training_log_dir().join("session_routing.jsonl")
}

/// Append-only calibration events for Mnemo1's route-once lifecycle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum SessionRoutingRow {
    /// The first classifiable prompt's answer and the action taken.
    Route {
        /// Unix milliseconds.
        ts: u64,
        /// The routed agent.
        agent_id: String,
        /// Laya's verbatim label.
        label: String,
        /// Laya's calibrated confidence.
        confidence: f64,
        /// The first-prompt gate.
        threshold: f64,
        /// Whether the label cleared the gate.
        cleared: bool,
        /// Whether the session's provider was actually replaced.
        enforced: bool,
        /// The selected `endpoint/model`, including a shadow-only pick.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Why no switch happened (`below_gate` / `unknown_label` / `shadow` /
        /// `kept` / `no_target` / `needs_compaction` / `superseded`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skip: Option<String>,
    },
    /// A one-per-session suggestion after drift or tool failures.
    Nudge {
        /// Unix milliseconds.
        ts: u64,
        /// The nudged agent.
        agent_id: String,
        /// `drift`, `tool_failures`, or `repeat_failure`.
        reason: String,
        /// The higher label whose configured model is suggested.
        to_label: String,
    },
    /// A human model pick, which ends routing for this session.
    Reroute {
        /// Unix milliseconds.
        ts: u64,
        /// The manually rerouted agent.
        agent_id: String,
        /// The previously routed model, or null when Laya had not switched it.
        #[serde(default)]
        from: Option<String>,
        /// The human's model spec.
        to: String,
        /// Whether a nudge preceded the pick.
        nudged: bool,
    },
    /// The session was cleared after a nudge without a manual pick.
    NudgeIgnored {
        /// Unix milliseconds.
        ts: u64,
        /// The agent whose nudge was ignored.
        agent_id: String,
        /// The label suggested by the ignored nudge.
        to_label: String,
    },
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
    /// The sibling route-once corpus, redirected with the legacy log in tests.
    session_log_path: PathBuf,
}

impl RoutingGate {
    /// Build a gate over the shared classifier slot, logging to
    /// [`routing_log_path`] (`~/.mnemo/laya/training/routing.jsonl`).
    pub fn new(classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>) -> Self {
        Self {
            classifier,
            log_path: routing_log_path(),
            session_log_path: session_routing_log_path(),
        }
    }

    /// Redirect this gate's routing log to `path` (tests; the app keeps the
    /// default). Replaces [`new`](Self::new)'s default path.
    pub fn with_log_path(mut self, path: PathBuf) -> Self {
        self.session_log_path = path.with_file_name("session_routing.jsonl");
        self.log_path = path;
        self
    }

    /// The routing log this gate appends to.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// The route-once session log this gate and its IPC callers append to.
    pub fn session_log_path(&self) -> &Path {
        &self.session_log_path
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
    /// The legacy step-lane gate; first-prompt routing uses
    /// `first_prompt_min_confidence` instead.
    pub threshold: f64,
    /// Whether a confident decision switches the model (`false` = shadow:
    /// classify + log only, model untouched).
    pub enforce: bool,
    /// Labels offered to the shared question after unset targets fold away.
    pub offered: super::route_question::OfferedLabels,
    /// The calibrated first-prompt gate for `offered.count()`.
    pub first_prompt_min_confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::memory::classifier::Question;

    use async_trait::async_trait;

    /// A classifier with a canned answer that records what it was asked --
    /// `None` models every no-answer path (disabled backend, timeout,
    /// malformed response).
    struct StubClassifier {
        answer: Option<Answer>,
        calls: Arc<AtomicUsize>,
        seen: Arc<Mutex<String>>,
        question: Arc<Mutex<Option<Question>>>,
    }

    impl StubClassifier {
        fn new(answer: Option<Answer>) -> Self {
            Self {
                answer,
                calls: Arc::new(AtomicUsize::new(0)),
                seen: Arc::new(Mutex::new(String::new())),
                question: Arc::new(Mutex::new(None)),
            }
        }
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, state: &str, question: &Question) -> Option<Answer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.seen.lock().expect("seen lock") = state.to_string();
            *self.question.lock().expect("question lock") = Some(question.clone());
            self.answer.clone()
        }
    }

    fn all_offered() -> crate::agent::route_question::OfferedLabels {
        crate::agent::route_question::OfferedLabels {
            trivial: true,
            routine: true,
            hard: true,
        }
    }

    fn choice(label: &str, confidence: f64) -> Option<Answer> {
        Some(Answer::Choice {
            label: label.to_string(),
            confidence,
            probabilities: BTreeMap::new(),
        })
    }

    #[tokio::test]
    async fn classify_task_text_uses_the_shared_question() {
        let offered = crate::agent::route_question::OfferedLabels {
            trivial: true,
            routine: true,
            hard: true,
        };
        let stub = StubClassifier::new(choice("hard", 0.91));
        let decision = classify_task_text(&stub, "design a storage engine", offered, 0.475)
            .await
            .expect("decision");
        assert_eq!(decision.target, Some(RouteTarget::Capable));
        let actual = stub.question.lock().expect("question lock").clone();
        let expected = crate::agent::route_question::route_question(offered);
        assert_eq!(actual, expected, "Mnemo1 asks the kernel question byte-for-byte");
    }

    #[tokio::test]
    async fn confident_answers_pick_their_target() {
        let trivial = StubClassifier::new(choice(crate::agent::route_question::LABEL_TRIVIAL, 0.9));
        let decision = classify_task_text(&trivial, "bump the version", all_offered(), 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.label, crate::agent::route_question::LABEL_TRIVIAL);
        assert!((decision.confidence - 0.9).abs() < 1e-9);
        assert_eq!(decision.target, Some(RouteTarget::Cheap));

        let architectural = StubClassifier::new(choice(crate::agent::route_question::LABEL_HARD, 0.95));
        let decision = classify_task_text(&architectural, "add a new feature", all_offered(), 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.label, crate::agent::route_question::LABEL_HARD);
        assert_eq!(decision.target, Some(RouteTarget::Capable));
    }

    #[tokio::test]
    async fn the_gate_reads_the_top_probability_not_the_margin() {
        // A live-shaped answer: the pick's margin-like `confidence` (0.30)
        // sits below the gate, the top option's probability (0.91) above it —
        // routing gates on the latter (`route_question::answer_confidence`).
        let answer = Answer::Choice {
            label: crate::agent::route_question::LABEL_HARD.to_string(),
            confidence: 0.30,
            probabilities: BTreeMap::from([
                (crate::agent::route_question::LABEL_TRIVIAL.to_string(), 0.03),
                (crate::agent::route_question::LABEL_ROUTINE.to_string(), 0.06),
                (crate::agent::route_question::LABEL_HARD.to_string(), 0.91),
            ]),
        };
        let stub = StubClassifier::new(Some(answer));
        let decision = classify_task_text(&stub, "fix a subtle race", all_offered(), 0.475)
            .await
            .expect("decision");
        assert_eq!(decision.label, crate::agent::route_question::LABEL_HARD);
        assert!((decision.confidence - 0.91).abs() < 1e-9);
        assert_eq!(decision.target, Some(RouteTarget::Capable));
    }

    #[tokio::test]
    async fn threshold_is_inclusive_and_below_keeps_no_target() {
        let at = StubClassifier::new(choice(crate::agent::route_question::LABEL_TRIVIAL, 0.80));
        let decision = classify_task_text(&at, "typo", all_offered(), 0.80).await.expect("decision");
        assert_eq!(decision.target, Some(RouteTarget::Cheap));

        let below = StubClassifier::new(choice(crate::agent::route_question::LABEL_TRIVIAL, 0.79));
        let decision = classify_task_text(&below, "typo", all_offered(), 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.target, None);
        // The answer still rides the log: a below-threshold pick is the
        // calibration signal the corpus exists to collect.
        assert_eq!(decision.label, crate::agent::route_question::LABEL_TRIVIAL);
        assert!((decision.confidence - 0.79).abs() < 1e-9);
    }

    #[tokio::test]
    async fn no_answer_and_non_choice_answers_yield_none() {
        let none = StubClassifier::new(None);
        assert!(classify_task_text(&none, "typo", all_offered(), 0.80).await.is_none());

        let scored = StubClassifier::new(Some(Answer::Score {
            value: 1.0,
            confidence: 0.99,
        }));
        assert!(classify_task_text(&scored, "typo", all_offered(), 0.80).await.is_none());
    }

    #[tokio::test]
    async fn unknown_label_records_the_answer_but_picks_no_target() {
        let odd = StubClassifier::new(choice("medium", 0.99));
        let decision = classify_task_text(&odd, "something", all_offered(), 0.80)
            .await
            .expect("decision");
        assert_eq!(decision.label, "medium");
        assert_eq!(decision.target, None);
    }

    #[tokio::test]
    async fn task_text_is_capped_before_the_classifier_sees_it() {
        let stub = StubClassifier::new(choice(crate::agent::route_question::LABEL_TRIVIAL, 0.9));
        let long = "x".repeat(ROUTING_TEXT_MAX_CHARS + 500);
        classify_task_text(&stub, &long, all_offered(), 0.80).await.expect("decision");
        let seen = stub.seen.lock().expect("seen lock").clone();
        assert_eq!(seen.chars().count(), ROUTING_TEXT_MAX_CHARS);
        assert_eq!(stub.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn decision_and_outcome_rows_round_trip_through_the_log() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("routing.jsonl");
        let mut probabilities = BTreeMap::new();
        probabilities.insert(crate::agent::route_question::LABEL_TRIVIAL.to_string(), 0.93);
        probabilities.insert(crate::agent::route_question::LABEL_HARD.to_string(), 0.07);
        let decision = RouteDecisionRow {
            ts: 1_700_000_000_000,
            turn_id: "1700000000000-main".into(),
            agent_id: "main".into(),
            shadow: true,
            label: crate::agent::route_question::LABEL_TRIVIAL.into(),
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
        assert_eq!(RouteTarget::Routine.label(), "routine");
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
