// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The compound Laya REFLEX call (backlog a8495cc1): ONE typed decision about
//! the work in front of the agent — how complex it is, what should happen next
//! and whether it carries risk — answered by a single `/v1/systemone` request.
//!
//! The existing Laya decision layers (tool-choice steering, model routing,
//! failure triage, memory auto-typing) each ask one question per round-trip. A
//! step-granular lane ladder wants all three answers for the same state at the
//! same moment, so [`classify_reflex`] rides
//! [`Classifier::classify_many`] and asks them together: one request, one
//! confidence gate, one decision.
//!
//! The CALL and the shadow LOG ship here; the **escalation lane ladder**
//! (backlog ad56c7bd, [`super::step_lanes`]) is the first consumer — it reads
//! the decision at each plan-step boundary and resolves the step's lane,
//! strictly shadow-first until `[general.routing] enforce` is on. The
//! **harness-run verify** layer (backlog 1f767466, [`super::step_verify`]) is
//! the second: a decided `verify` action re-runs `[general.verify]`'s command
//! harness-side and rides the compact evidence note on the next request's
//! volatile tail (the budget layer is the remaining consumer). The contract
//! is [`super::failure_triage`]'s: opt-in
//! (`[general.laya] reflex`, default off), a calibrated-confidence gate
//! ([`REFLEX_THRESHOLD`]), and a strict fallback — a missing answer, a
//! non-choice answer, a label outside its taxonomy, or a weakest confidence
//! below the gate leaves every caller on its pre-existing behavior. While the
//! flag is off, [`ReflexHandle::decide`] returns WITHOUT asking anything, so
//! disabled behavior is byte-identical (zero classifier calls).
//!
//! There is deliberately NO `scope_drift` question: step scope is checked
//! deterministically (`git diff --name-only` against the step's named file
//! paths), and a classifier must never gate a check a shell can make exactly.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::memory::classifier::{Answer, Classifier, Question};

/// The calibrated confidence every reflex answer must reach before a decision
/// may steer anything. Inclusive: exactly 0.80 decides — the same gate
/// contract as the failure-triage and auto-typing thresholds, since the
/// shipped checkpoints are near-chance zero-shot on custom tasks and
/// over-confident until a per-task temperature refit.
pub const REFLEX_THRESHOLD: f64 = 0.80;

/// The wire key of the complexity question — one `questions` map entry of the
/// `/v1/systemone` request, and the key the backend answers it under.
pub const REFLEX_KEY_COMPLEXITY: &str = "complexity";

/// The wire key of the action question. See [`REFLEX_KEY_COMPLEXITY`].
pub const REFLEX_KEY_ACTION: &str = "action";

/// The wire key of the risk question. See [`REFLEX_KEY_COMPLEXITY`].
pub const REFLEX_KEY_RISK: &str = "risk_flag";

/// Cap on the state text handed to the classifier. The classifier needs the
/// gist of the step; the cap bounds latency and request size for pathological
/// inputs (a whole file pasted into the state).
const STATE_TEXT_MAX_CHARS: usize = 2000;

/// Cap on the echoed unknown label in [`ReflexKeepReason::UnknownLabel`] (same
/// scrub contract as auto-typing: control characters stripped, echo bounded,
/// so an endpoint-controlled label carries no injection surface and no size
/// blowup).
const UNKNOWN_LABEL_MAX_CHARS: usize = 40;

/// Cap on the state text stored in a training row — the corpus keeps the gist
/// of the step, not a whole file pasted into it (the decision row is JSONL, and
/// one row per step adds up).
const LOG_STATE_TEXT_MAX_CHARS: usize = 500;

/// How complex the step in front of the agent is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Complexity {
    /// A bounded change one cheap step can carry: a small edit, a lookup, a
    /// one-line correction.
    Small,
    /// A normal multi-file change inside the design that already exists.
    Medium,
    /// Work that needs the capable lane: cross-module design, subtle
    /// invariants, a migration, an unfamiliar subsystem.
    High,
    /// The step cannot be sized by the classifier at all.
    Escalate,
}

impl Complexity {
    /// The wire/log label (lowercase, the same string the question's criteria
    /// keys carry).
    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Escalate => "escalate",
        }
    }

    /// Parse a label back from the endpoint's answer. `None` for any label
    /// outside the taxonomy — a protocol deviation handled as an unknown
    /// label, never as a usable class.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "small" => Some(Self::Small),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "escalate" => Some(Self::Escalate),
            _ => None,
        }
    }
}

/// What should happen to the step next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReflexAction {
    /// Carry on with the current approach — no rework, no pause.
    Continue,
    /// Re-run the step: something transient got in the way.
    Retry,
    /// Prove the step before moving on (the harness verify step, a test run, a
    /// review).
    Verify,
    /// Hand the decision upward — the step exceeds what the current lane may
    /// decide on its own.
    Escalate,
    /// The step is done; nothing further is owed.
    Complete,
}

impl ReflexAction {
    /// The wire/log label (lowercase, the same string the question's criteria
    /// keys carry).
    pub fn label(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Retry => "retry",
            Self::Verify => "verify",
            Self::Escalate => "escalate",
            Self::Complete => "complete",
        }
    }

    /// Parse a label back from the endpoint's answer. `None` for any label
    /// outside the taxonomy.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "continue" => Some(Self::Continue),
            "retry" => Some(Self::Retry),
            "verify" => Some(Self::Verify),
            "escalate" => Some(Self::Escalate),
            "complete" => Some(Self::Complete),
            _ => None,
        }
    }
}

/// Whether the step touches something that must never be decided casually.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskFlag {
    /// Nothing beyond the step's own blast radius.
    None,
    /// Credentials, permissions, injection surfaces, auth or crypto paths.
    Security,
    /// Destructive or hard-to-reverse work: data migration, deletion, a
    /// rewrite of shared state.
    DataLoss,
}

impl RiskFlag {
    /// The wire/log label (lowercase, the same string the question's criteria
    /// keys carry).
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Security => "security",
            Self::DataLoss => "data_loss",
        }
    }

    /// Parse a label back from the endpoint's answer. `None` for any label
    /// outside the taxonomy.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "none" => Some(Self::None),
            "security" => Some(Self::Security),
            "data_loss" => Some(Self::DataLoss),
            _ => None,
        }
    }
}

/// The typed decision ONE reflex request yields — the compound answer the
/// step-routing, harness-verify and budget consumers read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflexDecision {
    /// How complex the step is.
    pub complexity: Complexity,
    /// What should happen next.
    pub action: ReflexAction,
    /// Whether the step carries a risk that overrides cost decisions.
    pub risk_flag: RiskFlag,
    /// The calibrated confidence of the WEAKEST of the three answers: a
    /// decision is only as trustworthy as its least certain part, so a
    /// confident complexity read can never carry an unsure risk read past the
    /// gate.
    pub confidence: f64,
}

/// The complexity question.
fn complexity_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "small".to_string(),
        "A bounded change one cheap step can carry: a small edit, a lookup, a \
         one-line correction, a mechanical rename."
            .to_string(),
    );
    criteria.insert(
        "medium".to_string(),
        "A normal multi-file change inside a design that already exists: adding \
         a field and its plumbing, a new test for known behavior."
            .to_string(),
    );
    criteria.insert(
        "high".to_string(),
        "Work that needs the capable lane: cross-module design, subtle \
         invariants, a migration, an unfamiliar subsystem, a change whose \
         blast radius must be reasoned about."
            .to_string(),
    );
    criteria.insert(
        "escalate".to_string(),
        "The step cannot be sized from this text at all — it is ambiguous, \
         contradictory, or missing the information needed to judge it."
            .to_string(),
    );
    Question::Choice {
        instructions: "Judge how complex this step is. Read the step text and \
                       pick the ONE label that best describes the work it \
                       requires."
            .to_string(),
        criteria,
    }
}

/// The action question.
fn action_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "continue".to_string(),
        "Carry on with the current approach — no rework and no pause needed."
            .to_string(),
    );
    criteria.insert(
        "retry".to_string(),
        "Re-run the step: something transient got in the way, and the same \
         attempt can succeed again."
            .to_string(),
    );
    criteria.insert(
        "verify".to_string(),
        "Prove the step before moving on: the work needs a test run, a review, \
         or another check before it can be trusted."
            .to_string(),
    );
    criteria.insert(
        "escalate".to_string(),
        "Hand the decision upward: the step exceeds what the current lane may \
         decide on its own, or needs a human."
            .to_string(),
    );
    criteria.insert(
        "complete".to_string(),
        "The step is done and nothing further is owed for it."
            .to_string(),
    );
    Question::Choice {
        instructions: "Choose what should happen to this step next. Read the \
                       step text and pick the ONE label that best describes the \
                       next move."
            .to_string(),
        criteria,
    }
}

/// The risk question.
fn risk_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "none".to_string(),
        "Nothing beyond the step's own blast radius: an ordinary code change \
         with no security or data consequences."
            .to_string(),
    );
    criteria.insert(
        "security".to_string(),
        "Credentials, permissions, API keys, injection surfaces, \
         authentication or cryptography are touched."
            .to_string(),
    );
    criteria.insert(
        "data_loss".to_string(),
        "Destructive or hard-to-reverse work: deleting or migrating data, \
         rewriting shared state, a change that cannot simply be reverted."
            .to_string(),
    );
    Question::Choice {
        instructions: "Judge the risk this step carries. Read the step text and \
                       pick the ONE label that best describes what could go \
                       wrong."
            .to_string(),
        criteria,
    }
}

/// The three questions ONE reflex request carries, each under its own wire key
/// ([`REFLEX_KEY_COMPLEXITY`], [`REFLEX_KEY_ACTION`], [`REFLEX_KEY_RISK`]).
///
/// Labels are bare words (never boolean-ish, per the classifier docs); the
/// descriptions carry the taxonomy so the fine-tuned checkpoint and the logged
/// corpus share one vocabulary.
pub fn reflex_questions() -> Vec<(&'static str, Question)> {
    vec![
        (REFLEX_KEY_COMPLEXITY, complexity_question()),
        (REFLEX_KEY_ACTION, action_question()),
        (REFLEX_KEY_RISK, risk_question()),
    ]
}

/// Why a reflex decision kept its pre-decision behavior — surfaced so callers
/// can note the outcome (calibration observability) without changing behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum ReflexKeepReason {
    /// The backend returned no answer, a non-choice answer, or fewer answers
    /// than questions — the strict fallback.
    NoAnswer,
    /// The weakest of the three answers was below [`REFLEX_THRESHOLD`]
    /// (carries that confidence).
    LowConfidence(f64),
    /// One answer's label is outside its taxonomy (carries the scrubbed
    /// label) — a protocol deviation, treated as no usable decision.
    UnknownLabel(String),
}

/// The outcome of one reflex pass.
#[derive(Debug, Clone, PartialEq)]
pub enum ReflexOutcome {
    /// All three answers cleared the gate together.
    Decided {
        /// The typed decision.
        decision: ReflexDecision,
    },
    /// No usable decision — the caller keeps its pre-existing behavior.
    Fallback {
        /// Why the decision was not applied.
        reason: ReflexKeepReason,
    },
}

/// The strict fallback: no usable answer.
fn no_answer() -> ReflexOutcome {
    ReflexOutcome::Fallback {
        reason: ReflexKeepReason::NoAnswer,
    }
}

/// Bound the state text handed to the classifier ([`STATE_TEXT_MAX_CHARS`]).
fn state_text(state: &str) -> String {
    if state.chars().count() > STATE_TEXT_MAX_CHARS {
        state.chars().take(STATE_TEXT_MAX_CHARS).collect()
    } else {
        state.to_string()
    }
}

/// Scrub an endpoint-controlled label before it rides a decision or a log row
/// (the auto-typing contract: no control characters, capped echo — the label
/// reaches logs and test output).
fn scrub_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| !c.is_control())
        .take(UNKNOWN_LABEL_MAX_CHARS)
        .collect()
}

/// A protocol-deviation fallback carrying the scrubbed offending label.
fn unknown_label(label: &str) -> ReflexOutcome {
    ReflexOutcome::Fallback {
        reason: ReflexKeepReason::UnknownLabel(scrub_label(label)),
    }
}

/// The label + confidence of one answer slot, or `None` for a missing or
/// non-choice answer.
fn choice_at(answers: &[Option<Answer>], index: usize) -> Option<(String, f64)> {
    match answers.get(index) {
        Some(Some(Answer::Choice {
            label, confidence, ..
        })) => Some((label.clone(), *confidence)),
        _ => None,
    }
}

/// Classify one state through the compound reflex question set: ONE request,
/// three answers, one gate.
///
/// Infallible by design — every not-usable outcome (a missing, short or
/// non-choice answer, a label outside its taxonomy, a weakest confidence below
/// [`REFLEX_THRESHOLD`]) is a [`ReflexOutcome::Fallback`], and callers act only
/// on [`ReflexOutcome::Decided`]. The three questions are ONE decision, not
/// three independent ones: a single unusable answer falls back for all three,
/// so a caller can never act on a partially understood step.
pub async fn classify_reflex(classifier: &dyn Classifier, state: &str) -> ReflexOutcome {
    let answers = classifier
        .classify_many(&state_text(state), &reflex_questions())
        .await;
    let Some((complexity_label, complexity_confidence)) = choice_at(&answers, 0) else {
        return no_answer();
    };
    let Some((action_label, action_confidence)) = choice_at(&answers, 1) else {
        return no_answer();
    };
    let Some((risk_label, risk_confidence)) = choice_at(&answers, 2) else {
        return no_answer();
    };
    let Some(complexity) = Complexity::from_label(&complexity_label) else {
        return unknown_label(&complexity_label);
    };
    let Some(action) = ReflexAction::from_label(&action_label) else {
        return unknown_label(&action_label);
    };
    let Some(risk_flag) = RiskFlag::from_label(&risk_label) else {
        return unknown_label(&risk_label);
    };
    // The weakest answer gates the decision: `LowConfidence` always carries the
    // minimum the three reported.
    let confidence = complexity_confidence
        .min(action_confidence)
        .min(risk_confidence);
    if confidence < REFLEX_THRESHOLD {
        return ReflexOutcome::Fallback {
            reason: ReflexKeepReason::LowConfidence(confidence),
        };
    }
    ReflexOutcome::Decided {
        decision: ReflexDecision {
            complexity,
            action,
            risk_flag,
            confidence,
        },
    }
}

/// One row of the reflex training log, written when a decision was obtained
/// (`~/.mnemo/laya/training/reflex.jsonl`).
///
/// Nothing acts on a reflex decision yet, so every row written today is a
/// SHADOW row: the labels and the confidence are recorded as the fine-tuning
/// corpus and the caller's behavior is untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflexDecisionRow {
    /// Unix milliseconds when the decision was taken.
    pub ts: u64,
    /// The turn this decision belongs to — joins [`ReflexOutcomeRow::turn_id`].
    pub turn_id: String,
    /// The agent the turn ran on (the main agent or a spawned one).
    pub agent_id: String,
    /// The answered complexity label (verbatim).
    pub complexity: String,
    /// The answered action label (verbatim).
    pub action: String,
    /// The answered risk label (verbatim).
    pub risk_flag: String,
    /// The calibrated confidence that cleared the gate — the weakest of the
    /// three answers.
    pub confidence: f64,
    /// The configured confidence gate the decision was compared against.
    pub threshold: f64,
    /// The step text handed to the classifier, capped at
    /// [`LOG_STATE_TEXT_MAX_CHARS`] — the fine-tuning corpus.
    pub state_text: String,
}

impl ReflexDecisionRow {
    /// Build the row for one decided state: the labels and the confidence come
    /// from `decision`, `ts` from the same clock the routing rows use (so the
    /// two corpora share a timeline), and the state text is capped for the
    /// corpus.
    pub fn new(
        turn_id: impl Into<String>,
        agent_id: impl Into<String>,
        state: &str,
        decision: &ReflexDecision,
    ) -> Self {
        Self {
            ts: super::model_routing::now_millis(),
            turn_id: turn_id.into(),
            agent_id: agent_id.into(),
            complexity: decision.complexity.label().to_string(),
            action: decision.action.label().to_string(),
            risk_flag: decision.risk_flag.label().to_string(),
            confidence: decision.confidence,
            threshold: REFLEX_THRESHOLD,
            state_text: log_state_text(state),
        }
    }
}

/// One row of the reflex training log, written when the step's turn ends: the
/// outcome half of the corpus (step text -> decision -> what actually
/// happened), joined to its decision row by `turn_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflexOutcomeRow {
    /// Unix milliseconds when the turn ended.
    pub ts: u64,
    /// The turn this outcome belongs to — joins
    /// [`ReflexDecisionRow::turn_id`].
    pub turn_id: String,
    /// The agent the turn ran on.
    pub agent_id: String,
    /// The terminal status the harness recorded: `ok` (no terminal failure
    /// seen), `aborted` (a hard internal abort), or `error` (the provider
    /// stream failed before producing output).
    pub outcome: String,
    /// Provider-request iterations the turn ran (its effort signal).
    pub iterations: usize,
    /// Tool calls the model requested this turn (its effort signal).
    pub tool_calls: usize,
    /// Consecutive-tool-error count as of the turn's last dispatch batch — the
    /// raw failure gradient behind `outcome`.
    pub tool_errors: u32,
}

/// The reflex log: `~/.mnemo/laya/training/reflex.jsonl`, beside the
/// failure-triage and routing logs so one directory holds every Laya
/// fine-tuning corpus.
pub fn reflex_log_path() -> PathBuf {
    super::failure_triage::training_log_dir().join("reflex.jsonl")
}

/// Cap `state` for a training row ([`LOG_STATE_TEXT_MAX_CHARS`]).
fn log_state_text(state: &str) -> String {
    if state.chars().count() > LOG_STATE_TEXT_MAX_CHARS {
        state.chars().take(LOG_STATE_TEXT_MAX_CHARS).collect()
    } else {
        state.to_string()
    }
}

/// The shared reflex inputs for the agent loops: the app runtime's live
/// classifier slot and the config-mirrored enable flag (`[general.laya]
/// reflex`, default off).
///
/// Reading both at call time means a Settings save takes effect on the next
/// call with no rebuild — and every existing swap site of the shared slot
/// (rebuild on save, managed sidecar start, setup autostart) feeds the reflex
/// site untouched. The flag is the separate opt-in the classifier docs
/// require: base checkpoints are over-confident zero-shot, so a compound
/// decision must only ever run against a **fine-tuned** endpoint the user
/// explicitly chose.
#[derive(Clone)]
pub struct ReflexHandle {
    /// The app runtime's shared classifier slot — the same `Arc` the IPC layer
    /// swaps on rewire and managed-runtime start.
    pub classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    /// The mirrored `[general.laya] reflex` flag: while false no reflex
    /// request is ever made.
    pub enabled: Arc<AtomicBool>,
    /// Optional override for the training-log path (tests + the fine-tune
    /// dataset tooling). `None` uses the default
    /// `~/.mnemo/laya/training/reflex.jsonl`.
    pub log_path: Option<PathBuf>,
}

impl ReflexHandle {
    /// Build a handle over the shared classifier slot + enable flag (training
    /// rows go to the default log path).
    pub fn new(
        classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
        enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            classifier,
            enabled,
            log_path: None,
        }
    }

    /// Override where this handle's training rows are appended (tests / the
    /// fine-tune dataset tooling). Returns `self` for chaining.
    pub fn with_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_path = Some(path.into());
        self
    }

    /// Whether reflex is currently enabled (the flag is read fresh on every
    /// call, so a Settings save lands immediately).
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Clone the live classifier out of the shared slot. Any lock poisoning
    /// reads as "no classifier" (the classifier is optional machinery — it
    /// must never panic a turn). The read guard is dropped here, before any
    /// await, so a concurrent swap can never deadlock.
    pub fn snapshot(&self) -> Option<Arc<dyn Classifier>> {
        self.classifier.read().ok().and_then(|slot| slot.clone())
    }

    /// Decide one state through the live gate. While the flag is off (or the
    /// slot is empty) this returns [`ReflexKeepReason::NoAnswer`] WITHOUT
    /// asking anything — the structural guarantee that disabled behavior is
    /// byte-identical (zero classifier calls).
    pub async fn decide(&self, state: &str) -> ReflexOutcome {
        if !self.is_enabled() {
            return no_answer();
        }
        let Some(classifier) = self.snapshot() else {
            return no_answer();
        };
        classify_reflex(&*classifier, state).await
    }

    /// Append one decision row to this handle's reflex log. Best-effort by
    /// design: a log write failure is swallowed — training data must never
    /// fail a turn.
    pub fn log_decision(&self, row: &ReflexDecisionRow) {
        super::model_routing::append_row(&self.resolved_log_path(), row);
    }

    /// Append one outcome row to this handle's reflex log. Best-effort, like
    /// [`Self::log_decision`].
    pub fn log_outcome(&self, row: &ReflexOutcomeRow) {
        super::model_routing::append_row(&self.resolved_log_path(), row);
    }

    /// This handle's log: the [`Self::with_log_path`] override when one is set
    /// (tests / the fine-tune dataset tooling), else [`reflex_log_path`].
    fn resolved_log_path(&self) -> PathBuf {
        self.log_path.clone().unwrap_or_else(reflex_log_path)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;

    /// A choice answer with no distribution — these tests care about the label
    /// and the confidence, not the probabilities.
    fn choice(label: &str, confidence: f64) -> Option<Answer> {
        Some(Answer::Choice {
            label: label.to_string(),
            confidence,
            probabilities: BTreeMap::new(),
        })
    }

    /// A backend that answers the reflex questions from a canned slot list and
    /// records the keys (and state) it was asked with. `classify` panics: the
    /// reflex call must ride ONE compound request, never a per-question loop.
    struct CannedReflex {
        answers: Vec<Option<Answer>>,
        calls: AtomicUsize,
        keys: Mutex<Vec<String>>,
        state: Mutex<String>,
    }

    impl CannedReflex {
        /// Three labels + confidences, in question order.
        fn triple(labels: [&str; 3], confidences: [f64; 3]) -> Self {
            Self::raw(vec![
                choice(labels[0], confidences[0]),
                choice(labels[1], confidences[1]),
                choice(labels[2], confidences[2]),
            ])
        }

        /// Raw slots — a short list or a non-choice answer expresses a protocol
        /// deviation.
        fn raw(answers: Vec<Option<Answer>>) -> Self {
            Self {
                answers,
                calls: AtomicUsize::new(0),
                keys: Mutex::new(Vec::new()),
                state: Mutex::new(String::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }

        fn keys(&self) -> Vec<String> {
            self.keys.lock().expect("keys lock").clone()
        }

        fn state(&self) -> String {
            self.state.lock().expect("state lock").clone()
        }
    }

    #[async_trait]
    impl Classifier for CannedReflex {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            panic!("the reflex call must ask every question in ONE compound request");
        }

        async fn classify_many(
            &self,
            state: &str,
            questions: &[(&str, Question)],
        ) -> Vec<Option<Answer>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            *self.keys.lock().expect("keys lock") =
                questions.iter().map(|(key, _)| (*key).to_string()).collect();
            *self.state.lock().expect("state lock") = state.to_string();
            self.answers.clone()
        }
    }

    /// A backend that counts every request — the flag-off proof.
    struct CountingClassifier {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl Classifier for CountingClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// A handle over `classifier` with the mirrored flag set as given.
    fn handle(classifier: Option<Arc<dyn Classifier>>, enabled: bool) -> ReflexHandle {
        ReflexHandle::new(
            Arc::new(RwLock::new(classifier)),
            Arc::new(AtomicBool::new(enabled)),
        )
    }

    #[tokio::test]
    async fn above_the_gate_decides_all_three_labels_in_one_ask() {
        let backend = CannedReflex::triple(["medium", "verify", "security"], [0.91, 0.88, 0.95]);
        assert_eq!(
            classify_reflex(&backend, "step 3: add the log row").await,
            ReflexOutcome::Decided {
                decision: ReflexDecision {
                    complexity: Complexity::Medium,
                    action: ReflexAction::Verify,
                    risk_flag: RiskFlag::Security,
                    // The minimum of the three, not the first or the last.
                    confidence: 0.88,
                }
            }
        );
        assert_eq!(backend.calls(), 1);
        assert_eq!(backend.keys(), vec!["complexity", "action", "risk_flag"]);
    }

    #[tokio::test]
    async fn the_weakest_answer_gates_the_whole_decision() {
        // Two strong answers cannot carry an unsure third: the decision is only
        // as trustworthy as its least certain part.
        let backend = CannedReflex::triple(["small", "continue", "none"], [0.95, 0.95, 0.79]);
        assert_eq!(
            classify_reflex(&backend, "state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::LowConfidence(0.79)
            }
        );
    }

    #[tokio::test]
    async fn exactly_the_threshold_decides() {
        // The gate is inclusive: exactly 0.80 decides — the same boundary
        // contract as the failure-triage gate.
        let backend = CannedReflex::triple(["high", "escalate", "data_loss"], [0.80, 0.93, 0.99]);
        assert_eq!(
            classify_reflex(&backend, "state").await,
            ReflexOutcome::Decided {
                decision: ReflexDecision {
                    complexity: Complexity::High,
                    action: ReflexAction::Escalate,
                    risk_flag: RiskFlag::DataLoss,
                    confidence: 0.80,
                }
            }
        );
    }

    #[tokio::test]
    async fn missing_answer_slots_fall_back() {
        // A slot the backend left out, and a short response, are both no usable
        // decision.
        let missing = CannedReflex::raw(vec![choice("small", 0.9), None, choice("none", 0.9)]);
        assert_eq!(
            classify_reflex(&missing, "state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::NoAnswer
            }
        );
        let short = CannedReflex::raw(vec![choice("small", 0.9)]);
        assert_eq!(
            classify_reflex(&short, "state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::NoAnswer
            }
        );
    }

    #[tokio::test]
    async fn non_choice_answers_fall_back() {
        // A score answer to a choice question is a protocol deviation.
        let backend = CannedReflex::raw(vec![
            choice("small", 0.9),
            Some(Answer::Score {
                value: 1.2,
                confidence: 0.9,
            }),
            choice("none", 0.9),
        ]);
        assert_eq!(
            classify_reflex(&backend, "state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::NoAnswer
            }
        );
    }

    #[tokio::test]
    async fn unknown_labels_carry_a_scrubbed_echo() {
        // The label reaches logs and test output, so control characters are
        // stripped and the echo is capped — and an out-of-taxonomy label is a
        // protocol deviation, not a low-confidence one.
        let backend = CannedReflex::triple(["hu\u{7}ge", "verify", "none"], [0.5, 0.5, 0.5]);
        assert_eq!(
            classify_reflex(&backend, "state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::UnknownLabel("huge".to_string())
            }
        );
        let long = "x".repeat(80);
        let backend = CannedReflex::triple([&long, "verify", "none"], [0.9, 0.9, 0.9]);
        assert_eq!(
            classify_reflex(&backend, "state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::UnknownLabel("x".repeat(40))
            }
        );
    }

    #[tokio::test]
    async fn flag_off_never_asks_the_classifier() {
        // The structural guarantee: while the flag is off no request is made at
        // all, so the caller's behavior is byte-identical to pre-reflex.
        let backend = Arc::new(CountingClassifier {
            calls: AtomicUsize::new(0),
        });
        let gate = handle(Some(backend.clone()), false);
        assert_eq!(
            gate.decide("state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::NoAnswer
            }
        );
        assert_eq!(backend.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn flag_on_with_an_empty_slot_falls_back() {
        let gate = handle(None, true);
        assert_eq!(
            gate.decide("state").await,
            ReflexOutcome::Fallback {
                reason: ReflexKeepReason::NoAnswer
            }
        );
    }

    #[tokio::test]
    async fn flag_on_decides_through_the_shared_slot() {
        let backend = Arc::new(CannedReflex::triple(
            ["small", "complete", "none"],
            [0.9, 0.9, 0.9],
        ));
        let gate = handle(Some(backend.clone()), true);
        assert!(matches!(
            gate.decide("state").await,
            ReflexOutcome::Decided { .. }
        ));
        assert_eq!(backend.calls(), 1);
    }

    #[tokio::test]
    async fn the_flag_is_read_fresh_on_every_call() {
        // A Settings save flips the mirrored flag with no rebuild.
        let backend = Arc::new(CannedReflex::triple(
            ["small", "complete", "none"],
            [0.9, 0.9, 0.9],
        ));
        let gate = handle(Some(backend.clone()), false);
        assert!(matches!(
            gate.decide("state").await,
            ReflexOutcome::Fallback { .. }
        ));
        gate.enabled.store(true, Ordering::Relaxed);
        assert!(matches!(
            gate.decide("state").await,
            ReflexOutcome::Decided { .. }
        ));
        assert_eq!(backend.calls(), 1);
    }

    #[tokio::test]
    async fn the_state_text_is_capped_before_it_is_sent() {
        // A pathological state (a whole file pasted in) must not blow up the
        // request: the backend sees the first [`STATE_TEXT_MAX_CHARS`] chars.
        let backend = CannedReflex::triple(["small", "continue", "none"], [0.9, 0.9, 0.9]);
        let long_state = "a".repeat(5000);
        let _ = classify_reflex(&backend, &long_state).await;
        assert_eq!(backend.state().chars().count(), 2000);
    }

    #[test]
    fn the_question_set_pins_the_three_keys_and_plain_labels() {
        let questions = reflex_questions();
        assert_eq!(
            questions.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            vec!["complexity", "action", "risk_flag"]
        );
        let mut expected = vec![
            vec!["escalate", "high", "medium", "small"],
            vec!["complete", "continue", "escalate", "retry", "verify"],
            vec!["data_loss", "none", "security"],
        ];
        for (index, (key, question)) in questions.iter().enumerate() {
            let Question::Choice { criteria, .. } = question else {
                panic!("question {key} must be a choice");
            };
            let mut labels: Vec<&str> = criteria.keys().map(String::as_str).collect();
            labels.sort_unstable();
            expected[index].sort_unstable();
            assert_eq!(labels, expected[index], "labels for {key}");
            for label in &labels {
                // The classifier docs' verbatim-label rule: never boolean-ish.
                assert!(
                    !matches!(*label, "true" | "false" | "yes" | "no" | "0" | "1"),
                    "label {label} of question {key} is boolean-ish"
                );
                // The descriptions carry the taxonomy the corpus learns.
                assert!(
                    criteria[*label].trim().len() > 10,
                    "label {label} of question {key} carries no description"
                );
            }
        }
    }

    #[test]
    fn labels_round_trip() {
        for complexity in [
            Complexity::Small,
            Complexity::Medium,
            Complexity::High,
            Complexity::Escalate,
        ] {
            assert_eq!(Complexity::from_label(complexity.label()), Some(complexity));
        }
        for action in [
            ReflexAction::Continue,
            ReflexAction::Retry,
            ReflexAction::Verify,
            ReflexAction::Escalate,
            ReflexAction::Complete,
        ] {
            assert_eq!(ReflexAction::from_label(action.label()), Some(action));
        }
        for risk in [RiskFlag::None, RiskFlag::Security, RiskFlag::DataLoss] {
            assert_eq!(RiskFlag::from_label(risk.label()), Some(risk));
        }
        assert_eq!(Complexity::from_label("HUGE"), None);
        assert_eq!(ReflexAction::from_label("true"), None);
        assert_eq!(RiskFlag::from_label(""), None);
    }

    // ---- The shadow training log ------------------------------------------

    /// A unique temp log path for one test — a suite run never touches the
    /// real training corpus.
    fn temp_log(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mnemo-reflex-{}-{name}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// A representative decision for the log tests.
    fn decision() -> ReflexDecision {
        ReflexDecision {
            complexity: Complexity::Small,
            action: ReflexAction::Continue,
            risk_flag: RiskFlag::None,
            confidence: 0.91,
        }
    }

    #[test]
    fn the_decision_and_outcome_rows_join_by_turn_id() {
        // The corpus contract: one decision row when the decision is taken, one
        // outcome row when the turn ends, joined by turn_id.
        let path = temp_log("join");
        let gate = handle(None, true).with_log_path(&path);
        let picked = ReflexDecision {
            complexity: Complexity::Medium,
            action: ReflexAction::Verify,
            risk_flag: RiskFlag::DataLoss,
            confidence: 0.87,
        };
        let turn_id = crate::agent::model_routing::new_turn_id("main");
        let decision_row = ReflexDecisionRow::new(&turn_id, "main", "step 2", &picked);
        gate.log_decision(&decision_row);
        gate.log_outcome(&ReflexOutcomeRow {
            ts: crate::agent::model_routing::now_millis(),
            turn_id: turn_id.clone(),
            agent_id: "main".to_string(),
            outcome: "ok".to_string(),
            iterations: 3,
            tool_calls: 7,
            tool_errors: 0,
        });

        let text = std::fs::read_to_string(&path).expect("log file");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one decision row and one outcome row");
        let read_decision: ReflexDecisionRow =
            serde_json::from_str(lines[0]).expect("decision row");
        let read_outcome: ReflexOutcomeRow = serde_json::from_str(lines[1]).expect("outcome row");
        assert_eq!(read_decision, decision_row);
        assert_eq!(read_decision.turn_id, read_outcome.turn_id);
        assert_eq!(read_decision.turn_id, turn_id);
        assert_eq!(read_decision.threshold, REFLEX_THRESHOLD);
        assert_eq!(read_decision.complexity, "medium");
        assert_eq!(read_decision.action, "verify");
        assert_eq!(read_decision.risk_flag, "data_loss");
        assert_eq!(read_decision.confidence, 0.87);
        assert!(read_decision.ts > 0);
        assert_eq!(read_outcome.agent_id, "main");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_state_carrying_quotes_and_newlines_survives_the_log() {
        // JSONL must round-trip a state that carries the separators itself.
        let path = temp_log("escapes");
        let gate = handle(None, true).with_log_path(&path);
        let state = "line one \"quoted\"\nline two\\slash";
        let row = ReflexDecisionRow::new("turn-1", "main", state, &decision());
        gate.log_decision(&row);

        let text = std::fs::read_to_string(&path).expect("log file");
        assert_eq!(text.lines().count(), 1, "one row, one physical line");
        let read: ReflexDecisionRow = serde_json::from_str(&text).expect("row");
        assert_eq!(read.state_text, state);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_broken_log_path_never_panics_nor_fails_the_caller() {
        // Best-effort contract: the parent is a FILE, so create_dir_all and the
        // append both fail — the write is swallowed and the caller continues.
        let blocker = std::env::temp_dir().join(format!(
            "mnemo-reflex-blocker-{}",
            std::process::id()
        ));
        std::fs::write(&blocker, "not a directory").expect("blocker file");
        let gate = handle(None, true).with_log_path(blocker.join("reflex.jsonl"));
        gate.log_decision(&ReflexDecisionRow::new("turn-1", "main", "state", &decision()));
        gate.log_outcome(&ReflexOutcomeRow {
            ts: 1,
            turn_id: "turn-1".to_string(),
            agent_id: "main".to_string(),
            outcome: "ok".to_string(),
            iterations: 1,
            tool_calls: 0,
            tool_errors: 0,
        });
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn the_decision_row_is_the_shadow_corpus_shape() {
        let row = ReflexDecisionRow::new("turn-1", "main", &"x".repeat(900), &decision());
        assert_eq!(row.threshold, REFLEX_THRESHOLD);
        assert_eq!(row.complexity, "small");
        assert_eq!(row.action, "continue");
        assert_eq!(row.risk_flag, "none");
        assert_eq!(row.confidence, 0.91);
        // The state is capped for the corpus, not for the classifier.
        assert_eq!(row.state_text.chars().count(), LOG_STATE_TEXT_MAX_CHARS);
        // The JSON carries numbers where numbers belong.
        let json = serde_json::to_value(&row).expect("json");
        assert!(json["confidence"].is_number());
        assert!(json["threshold"].is_number());
        assert_eq!(json["turn_id"], "turn-1");
    }

    #[test]
    fn the_reflex_log_sits_beside_the_other_laya_logs() {
        let path = reflex_log_path();
        assert_eq!(path.file_name().and_then(|name| name.to_str()), Some("reflex.jsonl"));
        assert_eq!(
            path,
            crate::agent::failure_triage::training_log_dir().join("reflex.jsonl")
        );
    }
}
