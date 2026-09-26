// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The optional Laya tool-choice classifier — a confidence-gated "System 1"
//! decision for the search tools' auto-delegation.
//!
//! The `search` / `search_read` tools steer symbol-shaped queries to the graph
//! tools and memory-shaped ones to the memory store. That decision is
//! heuristic today (regex shape probes in [`crate::tool::agent::search`]).
//! This module offers an OPTIONAL replacement: ask a Laya [`Classifier`] which
//! of three classes a query belongs to — `SYMBOL` / `TEXT` / `MEMORY` — and
//! route the winning class into the EXISTING delegation emitters, leaving the
//! delegation mechanics untouched.
//!
//! Two guarantees make the swap safe:
//!
//! - **Off by default.** With Laya disabled, the handle unwired, or the
//!   `[general.laya] steer_tool_choice` flag false, [`steer`] never runs and
//!   the caller's regex heuristics run byte-identically to today.
//! - **Confidence-gated.** A usable answer must clear
//!   [`TOOL_CHOICE_THRESHOLD`]. Below it — or on any no-answer path (no
//!   backend, timeout, malformed response, an unknown label) — the caller
//!   falls back to the heuristics. The shipped checkpoints are near-chance
//!   zero-shot on custom tasks, so this steering is meant only against a
//!   fine-tuned checkpoint the user explicitly opted into.
//!
//! Every decision is best-effort and infallible by design, mirroring the
//! `auto_typing` module: nothing here can fail a tool call.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::memory::classifier::{Answer, Classifier, Question};

/// The calibrated confidence an answer must reach before it may steer the
/// auto-delegation decision. Below it the caller keeps its regex heuristics
/// (the pre-classifier behavior) — the gate the [`Classifier`] docs require,
/// since unvalidated probabilities must never gate a decision. Matches
/// `auto_typing::AUTO_TYPE_THRESHOLD`.
pub const TOOL_CHOICE_THRESHOLD: f64 = 0.80;

/// Cap on the state text handed to the classifier (the query + glob + mode).
/// The classifier only needs the gist of the query; the cap bounds latency
/// and request size for pathological patterns.
const STATE_TEXT_MAX_CHARS: usize = 2000;

/// The three classes a query can belong to — the routing target of the
/// auto-delegation decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolChoice {
    /// A symbol hunt — route to `graph_search` / `graph_context`.
    Symbol,
    /// A plain text search — no delegation; the file walk is the answer.
    Text,
    /// A memory hunt — route to the memory store.
    Memory,
}

impl ToolChoice {
    /// The classifier label for this class (the `criteria` key).
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolChoice::Symbol => "SYMBOL",
            ToolChoice::Text => "TEXT",
            ToolChoice::Memory => "MEMORY",
        }
    }

    /// Parse a classifier label back to a class; `None` for an unknown label.
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "SYMBOL" => Some(ToolChoice::Symbol),
            "TEXT" => Some(ToolChoice::Text),
            "MEMORY" => Some(ToolChoice::Memory),
            _ => None,
        }
    }
}

/// Why the classifier did not steer — surfaced so callers can note the
/// decision (calibration observability) without changing behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoiceFallback {
    /// The opt-in flag is false — the classifier was never consulted.
    Disabled,
    /// No classifier is wired (Laya off, or the slot is empty).
    NoClassifier,
    /// The backend returned no answer, or a non-choice answer to a choice
    /// question (a protocol deviation) — the strict fallback.
    NoAnswer,
    /// The answer's calibrated confidence was below [`TOOL_CHOICE_THRESHOLD`]
    /// (carries the confidence that was returned).
    LowConfidence(f64),
    /// The answer's label is not one of the three classes (carries the
    /// scrubbed label) — a protocol deviation, treated as no usable answer.
    UnknownLabel(String),
}

impl ToolChoiceFallback {
    /// A stable slug for the training log (the fine-tune corpus label).
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolChoiceFallback::Disabled => "disabled",
            ToolChoiceFallback::NoClassifier => "no_classifier",
            ToolChoiceFallback::NoAnswer => "no_answer",
            ToolChoiceFallback::LowConfidence(_) => "low_confidence",
            ToolChoiceFallback::UnknownLabel(_) => "unknown_label",
        }
    }
}

/// The outcome of one tool-choice pass.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolChoiceDecision {
    /// The classifier cleared the threshold and picked a class — the caller
    /// routes by it.
    Steered {
        /// The class the classifier picked.
        choice: ToolChoice,
        /// The calibrated confidence of the pick.
        confidence: f64,
    },
    /// No steer — the caller keeps its regex heuristics (with the reason).
    FellBack {
        /// Why the heuristics stand.
        reason: ToolChoiceFallback,
    },
}

impl ToolChoiceDecision {
    /// The class to route by, or `None` when the caller must keep its
    /// heuristics. This is the ONE accessor the call sites branch on.
    pub fn choice(&self) -> Option<ToolChoice> {
        match self {
            ToolChoiceDecision::Steered { choice, .. } => Some(*choice),
            ToolChoiceDecision::FellBack { .. } => None,
        }
    }
}

/// The choice question over the three classes. Labels are bare words (never
/// boolean-ish, per the classifier docs); descriptions carry the routing
/// taxonomy the fine-tuned checkpoint and the training corpus share.
pub fn tool_choice_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "SYMBOL".to_string(),
        "A question about a named code symbol — where it is defined, who \
         calls it, or what depends on it. The code graph answers it."
            .to_string(),
    );
    criteria.insert(
        "TEXT".to_string(),
        "A search for text in files — a literal string, a config key, a \
         comment, a log message. The file search answers it."
            .to_string(),
    );
    criteria.insert(
        "MEMORY".to_string(),
        "A hunt for stored knowledge — a memory record, a backlog item, a \
         decision, or a spec. The memory store answers it."
            .to_string(),
    );
    Question::Choice {
        instructions: "Decide which tool should answer this search query. \
Read the query, its glob, and its mode, then pick the ONE label naming the \
kind of answer it wants."
            .to_string(),
        criteria,
    }
}

/// The classifier state text for a search query: the pattern, the glob, and
/// the mode, capped at [`STATE_TEXT_MAX_CHARS`] chars.
pub fn state_text(pattern: &str, glob: Option<&str>, literal: bool) -> String {
    let mut text = format!("query: {pattern}");
    if let Some(glob) = glob {
        text.push_str(&format!("\nglob: {glob}"));
    }
    if literal {
        text.push_str("\nmode: literal");
    }
    if text.chars().count() > STATE_TEXT_MAX_CHARS {
        text = text.chars().take(STATE_TEXT_MAX_CHARS).collect();
    }
    text
}

/// Ask the classifier which class a query belongs to. Infallible by design —
/// every not-usable outcome (no backend, no answer, a non-choice answer, an
/// unknown label, confidence below [`TOOL_CHOICE_THRESHOLD`]) is a
/// [`ToolChoiceDecision::FellBack`].
pub async fn steer(
    classifier: Option<&dyn Classifier>,
    pattern: &str,
    glob: Option<&str>,
    literal: bool,
) -> ToolChoiceDecision {
    let Some(classifier) = classifier else {
        return ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::NoClassifier,
        };
    };
    let answer = classifier
        .classify(&state_text(pattern, glob, literal), &tool_choice_question())
        .await;
    let (label, confidence) = match answer {
        Some(Answer::Choice {
            label, confidence, ..
        }) => (label, confidence),
        _ => {
            return ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::NoAnswer,
            }
        }
    };
    let Some(choice) = ToolChoice::from_label(&label) else {
        // Bound + scrub the endpoint-controlled label before it rides the
        // decision: control characters stripped, echo capped at 40 chars —
        // the label may reach agent-visible text, so it must carry no
        // injection surface and no size blowup.
        let clean: String = label
            .chars()
            .filter(|c| !c.is_control())
            .take(40)
            .collect();
        return ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::UnknownLabel(clean),
        };
    };
    if confidence < TOOL_CHOICE_THRESHOLD {
        return ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::LowConfidence(confidence),
        };
    }
    ToolChoiceDecision::Steered { choice, confidence }
}

/// The shared classifier slot + opt-in flag the search tools read at call
/// time, mirroring `AutoTypingHandle` in `crate::tool::memory`.
///
/// Reading BOTH at call time means a Settings save takes effect on the next
/// search with no tool rebuild, and every existing swap site of the shared
/// slot (external rebuild on save, managed sidecar start, setup autostart)
/// feeds the tools untouched.
#[derive(Clone)]
pub struct ToolChoiceHandle {
    /// The app runtime's shared classifier slot — the same `Arc` the IPC
    /// layer swaps on rewire and managed-mode start.
    pub classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    /// The mirrored `[general.laya] steer_tool_choice` flag: while false the
    /// classifier is never consulted and the heuristics stand.
    pub enabled: Arc<AtomicBool>,
    /// Where this gate's training rows land — `None` means the default
    /// [`tool_choice_log_path`]. Tests (and the fine-tune tooling) point it
    /// somewhere else so a steering exercise never touches the real log.
    pub log_path: Option<PathBuf>,
}

impl ToolChoiceHandle {
    /// Override where this gate's training rows are appended (tests / the
    /// fine-tune dataset tooling). Returns `self` for chaining.
    pub fn with_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_path = Some(path.into());
        self
    }
}

/// Steer a search decision from a tool's handle: the flag gate, then the
/// shared slot, then [`steer`]. An absent handle, a false flag, or an empty
/// slot is a fallback with NO classifier call at all — so disabled behavior
/// stays byte-identical to the pre-classifier tools.
pub async fn steer_with_handle(
    handle: Option<&ToolChoiceHandle>,
    pattern: &str,
    glob: Option<&str>,
    literal: bool,
) -> ToolChoiceDecision {
    let Some(handle) = handle else {
        return ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::NoClassifier,
        };
    };
    if !handle.enabled.load(Ordering::Relaxed) {
        return ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::Disabled,
        };
    }
    // Clone the Arc out and drop the read guard BEFORE awaiting — holding the
    // slot lock across an await would let a rewire deadlock against a live
    // call (and the guard is not `Send`).
    let classifier = handle
        .classifier
        .read()
        .expect("tool-choice classifier slot lock poisoned")
        .clone();
    match classifier {
        Some(classifier) => steer(Some(classifier.as_ref()), pattern, glob, literal).await,
        None => ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::NoClassifier,
        },
    }
}

/// Whether the gate is live (wired AND the opt-in flag on) — the ONE gate for
/// both steering and the training log, so a disabled/unwired gate is provably
/// inert (no classifier call, no log write).
pub fn is_live(handle: Option<&ToolChoiceHandle>) -> bool {
    handle
        .map(|h| h.enabled.load(Ordering::Relaxed))
        .unwrap_or(false)
}

/// Cap on the logged pattern — a training row must never blow up the log.
const LOG_PATTERN_MAX_CHARS: usize = 500;

/// One training-log row: a search query and the tool-choice disposition that
/// answered it — the fine-tune corpus seed for a later Laya checkpoint (the
/// item's "log query → which tool answered").
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolChoiceLogRow {
    /// Unix seconds (0 before the epoch).
    pub ts: u64,
    /// The query pattern (capped at [`LOG_PATTERN_MAX_CHARS`] chars).
    pub pattern: String,
    /// The glob, when one was given.
    pub glob: Option<String>,
    /// Whether the call was a literal search.
    pub literal: bool,
    /// The steered class label, or `null` when the heuristics stood.
    pub choice: Option<String>,
    /// The classifier's calibrated confidence, when it steered — or when it
    /// answered below the threshold (the calibration signal a fine-tune
    /// wants). `None` when the classifier never answered at all.
    pub confidence: Option<f64>,
    /// Why the heuristics stood (a [`ToolChoiceFallback::as_str`] slug), when
    /// the classifier did not steer.
    pub fallback: Option<String>,
    /// Whether the search tool delegated — the graph or memory store answered
    /// instead of the file walk (its "which tool actually answered").
    pub delegated: bool,
    /// Whether the call was an escaped repeat: the delegation was re-issued
    /// as a plain search — the correction signal a fine-tune learns from.
    pub escaped: bool,
}

/// The tool-choice training log — `~/.mnemo/laya/training/tool_choice.jsonl`
/// (the same home as the failure-triage log).
pub fn tool_choice_log_path() -> PathBuf {
    crate::agent::failure_triage::training_log_dir().join("tool_choice.jsonl")
}

/// Record one search's tool-choice disposition. Best-effort by design — a log
/// write failure is swallowed, because training data must never fail a tool
/// call. `decision` is the steer outcome, or `None` for an escaped repeat (the
/// classifier was never asked); `log_path` overrides the default home.
pub fn log_decision(
    pattern: &str,
    glob: Option<&str>,
    literal: bool,
    decision: Option<&ToolChoiceDecision>,
    delegated: bool,
    escaped: bool,
    log_path: Option<&Path>,
) {
    let (choice, confidence, fallback) = match decision {
        Some(ToolChoiceDecision::Steered { choice, confidence }) => {
            (Some(choice.as_str().to_string()), Some(*confidence), None)
        }
        Some(ToolChoiceDecision::FellBack { reason }) => {
            // A below-threshold answer still carries the classifier's
            // calibrated confidence — the calibration signal a fine-tune
            // wants; the other fallbacks have no confidence to record.
            let confidence = match reason {
                ToolChoiceFallback::LowConfidence(c) => Some(*c),
                _ => None,
            };
            (None, confidence, Some(reason.as_str().to_string()))
        }
        None => (None, None, None),
    };
    let row = ToolChoiceLogRow {
        ts: now_unix_secs(),
        pattern: pattern.chars().take(LOG_PATTERN_MAX_CHARS).collect(),
        glob: glob.map(|g| g.to_string()),
        literal,
        choice,
        confidence,
        fallback,
        delegated,
        escaped,
    };
    let path = match log_path {
        Some(path) => path.to_path_buf(),
        None => tool_choice_log_path(),
    };
    append_row(&path, &row);
}

/// Unix seconds now (0 when the clock is before the epoch — a log timestamp
/// must never panic).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Append one row to `path` (JSONL). Best-effort: a log write failure is
/// swallowed — training data must never fail a tool call.
fn append_row(path: &Path, row: &ToolChoiceLogRow) {
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

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;

    /// A classifier with a canned answer — `None` models every no-answer path
    /// (disabled backend, timeout, malformed response). Counts calls so the
    /// disabled/unwired paths can assert the classifier is never consulted.
    struct StubClassifier {
        answer: Option<Answer>,
        calls: Arc<AtomicUsize>,
    }

    impl StubClassifier {
        fn new(answer: Option<Answer>) -> Self {
            Self {
                answer,
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            self.calls.fetch_add(1, Ordering::Relaxed);
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

    fn handle(classifier: Arc<dyn Classifier>, enabled: bool) -> ToolChoiceHandle {
        ToolChoiceHandle {
            classifier: Arc::new(RwLock::new(Some(classifier))),
            enabled: Arc::new(AtomicBool::new(enabled)),
            log_path: None,
        }
    }

    #[test]
    fn tool_choice_question_covers_the_three_classes() {
        let Question::Choice {
            instructions,
            criteria,
        } = tool_choice_question()
        else {
            panic!("tool_choice_question must be a choice question");
        };
        assert!(!instructions.is_empty());
        let mut labels: Vec<_> = criteria.keys().cloned().collect();
        labels.sort();
        assert_eq!(labels, vec!["MEMORY", "SYMBOL", "TEXT"]);
        for description in criteria.values() {
            assert!(!description.is_empty());
        }
    }

    #[test]
    fn state_text_carries_the_glob_and_the_mode() {
        let text = state_text("watcher", Some(".coding/**"), true);
        assert!(text.contains("query: watcher"));
        assert!(text.contains("glob: .coding/**"));
        assert!(text.contains("mode: literal"));

        let plain = state_text("watcher", None, false);
        assert!(!plain.contains("glob:"), "no glob means no glob line");
        assert!(!plain.contains("mode:"), "regex mode is the default");
    }

    #[test]
    fn state_text_is_capped() {
        let long = "x".repeat(STATE_TEXT_MAX_CHARS + 500);
        let text = state_text(&long, None, false);
        assert_eq!(text.chars().count(), STATE_TEXT_MAX_CHARS);
    }

    #[tokio::test]
    async fn a_high_confidence_answer_steers() {
        let classifier = StubClassifier::new(choice("SYMBOL", 0.95));
        let decision = steer(Some(&classifier), "DeltaAccumulator", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::Steered {
                choice: ToolChoice::Symbol,
                confidence: 0.95,
            }
        );
    }

    #[tokio::test]
    async fn the_threshold_is_inclusive() {
        let classifier = StubClassifier::new(choice("TEXT", 0.80));
        let decision = steer(Some(&classifier), "anything", None, false).await;
        assert_eq!(decision.choice(), Some(ToolChoice::Text));
    }

    #[tokio::test]
    async fn below_the_threshold_falls_back() {
        let classifier = StubClassifier::new(choice("MEMORY", 0.79));
        let decision = steer(Some(&classifier), "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::LowConfidence(0.79),
            }
        );
        assert_eq!(decision.choice(), None);
    }

    #[tokio::test]
    async fn an_unknown_label_falls_back_with_a_scrubbed_echo() {
        let classifier = StubClassifier::new(choice("MAYBE\u{7}", 0.99));
        let decision = steer(Some(&classifier), "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::UnknownLabel("MAYBE".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn no_classifier_falls_back() {
        let decision = steer(None, "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::NoClassifier,
            }
        );
    }

    #[tokio::test]
    async fn no_answer_falls_back() {
        let classifier = StubClassifier::new(None);
        let decision = steer(Some(&classifier), "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn a_non_choice_answer_falls_back() {
        let classifier = StubClassifier::new(Some(Answer::Score {
            value: 0.5,
            confidence: 0.9,
        }));
        let decision = steer(Some(&classifier), "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn a_disabled_handle_never_consults_the_classifier() {
        let stub = Arc::new(StubClassifier::new(choice("SYMBOL", 0.99)));
        let calls = stub.calls.clone();
        let h = handle(stub, false);
        let decision = steer_with_handle(Some(&h), "DeltaAccumulator", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::Disabled,
            }
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0, "disabled never asks");
    }

    #[tokio::test]
    async fn an_enabled_handle_steers_through_the_shared_slot() {
        let stub = Arc::new(StubClassifier::new(choice("MEMORY", 0.9)));
        let calls = stub.calls.clone();
        let h = handle(stub, true);
        let decision = steer_with_handle(
            Some(&h),
            "plan ced9308a",
            Some(".coding/knowledge/**"),
            false,
        )
        .await;
        assert_eq!(decision.choice(), Some(ToolChoice::Memory));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn an_empty_slot_falls_back_without_a_call() {
        let h = ToolChoiceHandle {
            classifier: Arc::new(RwLock::new(None)),
            enabled: Arc::new(AtomicBool::new(true)),
            log_path: None,
        };
        let decision = steer_with_handle(Some(&h), "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::NoClassifier,
            }
        );
    }

    #[tokio::test]
    async fn an_unwired_handle_falls_back() {
        let decision = steer_with_handle(None, "anything", None, false).await;
        assert_eq!(
            decision,
            ToolChoiceDecision::FellBack {
                reason: ToolChoiceFallback::NoClassifier,
            }
        );
    }

    #[test]
    fn a_live_gate_is_live_and_a_disabled_or_unwired_one_is_not() {
        // The training log's gate: `false` for an absent handle or a false
        // flag — the structural reason a disabled gate writes nothing.
        let stub: Arc<dyn Classifier> = Arc::new(StubClassifier::new(None));
        assert!(!is_live(None));
        assert!(!is_live(Some(&handle(stub.clone(), false))));
        assert!(is_live(Some(&handle(stub, true))));
    }

    /// Read the JSONL rows a test logged (malformed lines skipped).
    fn read_rows(path: &Path) -> Vec<ToolChoiceLogRow> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    #[test]
    fn a_steered_decision_logs_one_row_with_the_class() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_choice.jsonl");
        let decision = ToolChoiceDecision::Steered {
            choice: ToolChoice::Symbol,
            confidence: 0.9,
        };
        log_decision(
            "DeltaAccumulator",
            None,
            false,
            Some(&decision),
            true,
            false,
            Some(&path),
        );
        let rows = read_rows(&path);
        assert_eq!(rows.len(), 1, "exactly one row per steered call");
        assert_eq!(rows[0].choice.as_deref(), Some("SYMBOL"));
        assert_eq!(rows[0].confidence, Some(0.9));
        assert_eq!(rows[0].fallback, None);
        assert!(rows[0].delegated);
        assert!(!rows[0].escaped);
    }

    #[test]
    fn a_fallback_row_carries_the_reason_slug() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_choice.jsonl");
        let decision = ToolChoiceDecision::FellBack {
            reason: ToolChoiceFallback::LowConfidence(0.5),
        };
        log_decision(
            "watcher",
            Some(".coding/**"),
            true,
            Some(&decision),
            false,
            true,
            Some(&path),
        );
        let rows = read_rows(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].choice, None);
        assert_eq!(rows[0].fallback.as_deref(), Some("low_confidence"));
        assert_eq!(
            rows[0].confidence,
            Some(0.5),
            "a below-threshold answer still records its confidence"
        );
        assert_eq!(rows[0].glob.as_deref(), Some(".coding/**"));
        assert!(rows[0].literal);
        assert!(rows[0].escaped, "the escaped signal rides the row");
    }

    #[test]
    fn an_escaped_repeat_with_no_decision_still_logs_the_correction() {
        // The escape hatch is the correction signal a fine-tune learns from,
        // so it must be logged even though the classifier was never asked.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_choice.jsonl");
        log_decision("anything", None, false, None, false, true, Some(&path));
        let rows = read_rows(&path);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].choice, None);
        assert_eq!(rows[0].fallback, None);
        assert!(rows[0].escaped);
        assert!(!rows[0].delegated);
    }

    #[test]
    fn the_logged_pattern_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool_choice.jsonl");
        let long = "x".repeat(LOG_PATTERN_MAX_CHARS + 100);
        log_decision(&long, None, false, None, false, false, Some(&path));
        let rows = read_rows(&path);
        assert_eq!(rows[0].pattern.chars().count(), LOG_PATTERN_MAX_CHARS);
    }
}
