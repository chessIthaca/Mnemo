// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Failure triage — the Laya classifier's decision layer for failure handling.
//!
//! At every failure-handling site (the tool-execution cap and the bad-JSON
//! repair loop in [`crate::agent::turn`], the provider turn-attempt ladder in
//! `crate::runtime::agent`) the error text is classified as one of four kinds
//! — `transient` / `permanent` / `needs_user` / `flaky_test` — and a
//! confident answer (≥ [`TRIAGE_THRESHOLD`]) lets the harness act instead of
//! leaving the model to guess: a transient READ-ONLY tool failure is re-run
//! by the harness without a model roundtrip ([`should_auto_retry`]), other
//! classes ride classified guidance on the fed-back error
//! ([`guidance_note`]), and a needs-user / permanent provider error skips
//! the backoff ladder outright.
//!
//! Same contract as auto-typing ([`crate::memory::auto_typing`]):
//! **confidence-gated and opt-in**. Below the threshold, or with no usable
//! answer (backend absent / unreachable / unknown label), or with the
//! `[general.laya] failure_triage` flag off, every caller keeps its
//! pre-classifier behavior — and with the flag off nothing is ever asked
//! ([`FailureTriageHandle::triage`] short-circuits), so disabled behavior is
//! byte-identical.
//!
//! Every classified failure is also logged to a JSONL training log
//! ([`log_failure`] / [`resolve_failure`]) with its true disposition (did
//! the retry succeed?) — the labeled corpus the startup fine-tune consumes.
//! Base Laya checkpoints are near-chance zero-shot and over-confident on
//! this task, so the log-then-fine-tune loop is what makes the gate
//! trustworthy; until a fine-tuned checkpoint validates, the flag stays off.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::config::global_config_dir;
use crate::memory::classifier::{Answer, Classifier, Question};

/// The calibrated confidence an answer must reach before its class may
/// steer failure handling. Below it (and with no answer at all) every
/// caller keeps its pre-classifier behavior — the same gate contract as
/// [`crate::memory::auto_typing::AUTO_TYPE_THRESHOLD`], since unvalidated
/// probabilities must never gate a decision. Inclusive: exactly 0.80
/// classifies.
pub const TRIAGE_THRESHOLD: f64 = 0.80;

/// Cap on the error text handed to the classifier. The classifier needs the
/// gist of the failure; the cap bounds latency and request size for
/// pathological outputs (a huge stack dump).
const STATE_TEXT_MAX_CHARS: usize = 2000;

/// Cap on the error text stored in the training log row — the corpus is for
/// style/class learning, not for reproducing whole transcripts.
const LOG_ERROR_TEXT_MAX_CHARS: usize = 500;

/// Cap on the echoed unknown label in [`FailureKeepReason::UnknownLabel`]
/// (same scrub contract as auto-typing: control characters stripped,
/// echo bounded, so an endpoint-controlled label carries no injection
/// surface and no size blowup).
const UNKNOWN_LABEL_MAX_CHARS: usize = 40;

/// How many times ONE tool call may be auto-retried by the harness.
pub const MAX_AUTO_RETRIES_PER_CALL: u32 = 1;

/// How many tool-call auto-retries one turn may perform in total — the
/// budget that keeps a pathologically "transient" endpoint from turning
/// into an unbounded retry loop.
pub const MAX_AUTO_RETRIES_PER_TURN: u32 = 4;

/// Backoff between the original tool call and its harness re-run. Deliberately
/// short: the auto-retry targets transient blips (a lock, a timeout) where
/// the model roundtrip the user is avoiding costs far more than this pause.
pub const AUTO_RETRY_BACKOFF_MS: u64 = 150;

/// The four failure kinds the triage question decides between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// The same call can succeed on a retry (timeout, rate limit, network
    /// blip, 5xx, truncated output, a temporary lock).
    Transient,
    /// The same call fails every time until something changes (invalid
    /// arguments, schema mismatch, missing file/symbol, a logic error).
    Permanent,
    /// Progress requires a human (credentials, permissions, approval,
    /// quota, a user-only decision).
    NeedsUser,
    /// An intermittent test failure unrelated to the change under test.
    FlakyTest,
}

impl FailureClass {
    /// The wire/log label for the class (lowercase, the same string the
    /// question's criteria keys carry).
    pub fn label(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Permanent => "permanent",
            Self::NeedsUser => "needs_user",
            Self::FlakyTest => "flaky_test",
        }
    }

    /// Parse a class label back from the endpoint's answer. `None` for any
    /// label outside the taxonomy — a protocol deviation handled as an
    /// unknown label, never as a usable class.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "transient" => Some(Self::Transient),
            "permanent" => Some(Self::Permanent),
            "needs_user" => Some(Self::NeedsUser),
            "flaky_test" => Some(Self::FlakyTest),
            _ => None,
        }
    }
}

/// The choice question over the four failure classes. Labels are bare words
/// (never boolean-ish, per the classifier docs); the descriptions carry the
/// decision taxonomy so the fine-tuned checkpoint and the logged corpus
/// share one vocabulary.
pub fn triage_question() -> Question {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        "transient".to_string(),
        "The same call can succeed on a retry: timeouts, rate limits, network blips, \
         server 5xx, truncated output, temporarily locked files."
            .to_string(),
    );
    criteria.insert(
        "permanent".to_string(),
        "The same call will fail every time until something changes: invalid or \
         malformed arguments, a schema mismatch, a missing file or symbol, a logic \
         error in the call itself."
            .to_string(),
    );
    criteria.insert(
        "needs_user".to_string(),
        "Progress requires a human: credentials, permissions, an approval, a quota, \
         or a decision only the user can make."
            .to_string(),
    );
    criteria.insert(
        "flaky_test".to_string(),
        "An intermittent test failure unrelated to the change under test — it passes \
         on re-run, or depends on order, timing, or parallelism."
            .to_string(),
    );
    Question::Choice {
        instructions: "Classify this agent failure by what should happen next. Read the \
                       error text and pick the ONE label that best describes it."
            .to_string(),
        criteria,
    }
}

/// Why a failure kept its pre-classifier handling — surfaced so callers can
/// note the decision (calibration observability) without changing behavior.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureKeepReason {
    /// The backend returned no answer (disabled, unreachable, timed out, or
    /// an unparseable response) — the strict fallback.
    NoAnswer,
    /// The answer's calibrated confidence was below [`TRIAGE_THRESHOLD`]
    /// (carries the confidence that was returned).
    LowConfidence(f64),
    /// The answer's label is not one of the four classes (carries the
    /// scrubbed label) — a protocol deviation, treated as no usable answer.
    UnknownLabel(String),
}

/// The outcome of one failure-triage pass.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureTriage {
    /// A confident classification (carries the class and the calibrated
    /// confidence that cleared [`TRIAGE_THRESHOLD`]).
    Classified {
        /// The classified failure kind.
        class: FailureClass,
        /// The calibrated confidence of the pick.
        confidence: f64,
    },
    /// No usable answer — the caller keeps its pre-classifier behavior.
    Fallback {
        /// Why the classification was not applied.
        reason: FailureKeepReason,
    },
}

/// Bound the error text handed to the classifier ([`STATE_TEXT_MAX_CHARS`]).
fn state_text(error_text: &str) -> String {
    if error_text.chars().count() > STATE_TEXT_MAX_CHARS {
        error_text.chars().take(STATE_TEXT_MAX_CHARS).collect()
    } else {
        error_text.to_string()
    }
}

/// Classify one failure. Infallible by design — every not-usable outcome
/// (no backend answer, a non-choice answer, an unknown label, a
/// below-threshold confidence) is a [`FailureTriage::Fallback`], and callers
/// act only on [`FailureTriage::Classified`].
pub async fn triage_failure(classifier: &dyn Classifier, error_text: &str) -> FailureTriage {
    let answer = classifier
        .classify(&state_text(error_text), &triage_question())
        .await;
    // No answer at all — or a non-choice answer to a choice question (a
    // protocol deviation) — is no usable answer.
    let (label, confidence) = match answer {
        Some(Answer::Choice {
            label, confidence, ..
        }) => (label, confidence),
        _ => {
            return FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            }
        }
    };
    let Some(class) = FailureClass::from_label(&label) else {
        // Bound + scrub the endpoint-controlled label before it rides the
        // decision (the auto-typing contract: no control characters, capped
        // echo — the label reaches logs and test output).
        let clean: String = label
            .chars()
            .filter(|c| !c.is_control())
            .take(UNKNOWN_LABEL_MAX_CHARS)
            .collect();
        return FailureTriage::Fallback {
            reason: FailureKeepReason::UnknownLabel(clean),
        };
    };
    if confidence < TRIAGE_THRESHOLD {
        return FailureTriage::Fallback {
            reason: FailureKeepReason::LowConfidence(confidence),
        };
    }
    FailureTriage::Classified { class, confidence }
}

/// The shared failure-triage inputs for the agent loops: the app runtime's
/// live classifier slot plus the config-mirrored enable flag
/// (`[general.laya] failure_triage`, default off).
///
/// Reading BOTH at call time means a Settings save takes effect on the next
/// failure with no rebuild — and every existing swap site of the shared
/// slot (external rebuild on save, managed sidecar start, setup autostart)
/// feeds the triage sites untouched. The flag is the separate opt-in the
/// classifier docs require: base checkpoints are over-confident zero-shot,
/// so triage must only ever run against a **fine-tuned** endpoint the user
/// explicitly chose.
#[derive(Clone)]
pub struct FailureTriageHandle {
    /// The app runtime's shared classifier slot — the same `Arc` the IPC
    /// layer swaps on rewire and managed-mode start.
    pub classifier: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    /// The mirrored `[general.laya] failure_triage` flag: while false no
    /// classification is ever requested.
    pub enabled: Arc<AtomicBool>,
    /// Optional override for the training-log path (tests + the fine-tune
    /// dataset tooling). `None` uses the default
    /// `~/.mnemo/laya/training/failure_triage.jsonl`.
    pub log_path: Option<PathBuf>,
}

impl FailureTriageHandle {
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

    /// Override where this gate's training rows are appended (tests / the
    /// fine-tune dataset tooling). Returns `self` for chaining.
    pub fn with_log_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.log_path = Some(path.into());
        self
    }

    /// Record one classified failure through this gate. The returned pending
    /// row remembers the gate's log path, so [`resolve_failure`] needs no
    /// gate in scope.
    #[allow(clippy::too_many_arguments)]
    pub fn log_failure(
        &self,
        site: FailureSite,
        tool: Option<&str>,
        error_text: &str,
        class: FailureClass,
        confidence: f64,
        action: TriageAction,
    ) -> PendingFailureRow {
        match &self.log_path {
            Some(path) => log_failure_at(path, site, tool, error_text, class, confidence, action),
            None => log_failure(site, tool, error_text, class, confidence, action),
        }
    }

    /// Whether triage is currently enabled (the flag is read fresh on every
    /// call so a Settings save lands immediately).
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Clone the live classifier out of the shared slot. Any lock poisoning
    /// reads as "no classifier" (the classifier is optional machinery — it
    /// must never panic a turn). The read guard is dropped here, before any
    /// await, so a concurrent swap can never deadlock.
    pub fn snapshot(&self) -> Option<Arc<dyn Classifier>> {
        self.classifier
            .read()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Classify one failure through the live gate. While the flag is off (or
    /// the slot is empty) this returns [`FailureKeepReason::NoAnswer`]
    /// WITHOUT asking anything — the structural guarantee that disabled
    /// behavior is byte-identical (zero classifier calls).
    pub async fn triage(&self, error_text: &str) -> FailureTriage {
        if !self.is_enabled() {
            return FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            };
        }
        let Some(classifier) = self.snapshot() else {
            return FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            };
        };
        triage_failure(&*classifier, error_text).await
    }
}

/// The tool calls the harness may re-run BY ITSELF after a confident
/// `transient` classification: read-only tools with no side effects, so a
/// re-run can never double-apply a mutation. Mutating tools (`shell` and
/// every `file_*` writer, memory writers, plan/backlog mutations, `git`
/// writes) are deliberately absent — their failures take the classified-
/// guidance route instead, where the model decides.
const AUTO_RETRY_SAFE_TOOLS: &[&str] = &[
    "search",
    "search_read",
    "read_files",
    "file_read",
    "graph_search",
    "graph_context",
    "graph_path",
    "graph_impact",
    "memory_search",
    "backlog_list",
    "current_plan",
    "list_models",
    "git_read",
];

/// Whether `name` is a read-only tool the harness may auto-retry.
pub fn is_auto_retry_safe_tool(name: &str) -> bool {
    AUTO_RETRY_SAFE_TOOLS.contains(&name)
}

/// The auto-retry policy: only a confident `transient` classification of a
/// read-only tool, within both budgets ([`MAX_AUTO_RETRIES_PER_CALL`],
/// [`MAX_AUTO_RETRIES_PER_TURN`] — the counts are the retries already
/// performed). Everything else keeps the pre-classifier behavior.
pub fn should_auto_retry(
    class: FailureClass,
    tool_name: &str,
    retries_this_call: u32,
    retries_this_turn: u32,
) -> bool {
    class == FailureClass::Transient
        && is_auto_retry_safe_tool(tool_name)
        && retries_this_call < MAX_AUTO_RETRIES_PER_CALL
        && retries_this_turn < MAX_AUTO_RETRIES_PER_TURN
}

/// The classified guidance appended to a fed-back failure, chosen by class.
/// This is the tier-2 route: the model still decides, but with a stated
/// reading of the failure instead of a bare error string. Wording keeps the
/// failure recoverable — guidance never forbids a retry the model judges
/// necessary, it names what the classification suggests.
pub fn guidance_note(class: FailureClass) -> &'static str {
    match class {
        FailureClass::Transient => {
            "This failure looks TRANSIENT — retrying the same call is reasonable."
        }
        FailureClass::Permanent => {
            "This failure looks PERMANENT — do NOT repeat this call unchanged; \
             change your approach or fix the call's arguments."
        }
        FailureClass::NeedsUser => {
            "This failure needs USER input — ask the user (ask_user) instead of retrying."
        }
        FailureClass::FlakyTest => {
            "This failure looks like a FLAKY test — re-run it once; if it keeps failing, \
             treat it as a real failure (or record the flaky signature and continue)."
        }
    }
}

/// The actionable hint that rides a needs-user / permanent provider SKIP into
/// the error text — written by the inner retry layer (`complete_with_retry`,
/// which appends `classified needs_user` / `classified permanent` so both
/// retry layers skip through `Error::is_non_retryable`), read by the
/// turn-level layer's terminal note and the UI.
pub fn skip_hint(class: FailureClass) -> &'static str {
    match class {
        FailureClass::NeedsUser => "credentials, permissions, or a quota need the user",
        FailureClass::Permanent => "the request itself must change",
        FailureClass::Transient | FailureClass::FlakyTest => "retrying cannot help",
    }
}

/// Which failure-handling site classified the failure — the training log's
/// first separator (the same class means different things per site).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureSite {
    /// The tool-execution path in `execute_tool_batch` (per-call failures,
    /// the batch-level [`crate::agent::MAX_RETRIES`] accounting).
    ToolBatch,
    /// The malformed-arguments (bad JSON) repair loop in `run_turn`.
    BadJsonRepair,
    /// The provider turn-attempt ladder in `run_turn_attempt`.
    ProviderTurn,
}

impl FailureSite {
    /// The wire/log label for the site.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolBatch => "tool_batch",
            Self::BadJsonRepair => "bad_json_repair",
            Self::ProviderTurn => "provider_turn",
        }
    }
}

/// What the harness DID for a classified failure — logged so the training
/// corpus records the action the class was mapped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageAction {
    /// The harness re-ran the identical read-only call (tier 1, no model
    /// roundtrip).
    AutoRetry,
    /// Classified guidance rode the fed-back failure; the model decides
    /// (tier 2).
    Guidance,
    /// The recorded classification kept the site's EXISTING retry ladder —
    /// no behavior change (the provider site's transient / flaky-test path,
    /// where the backoff ladder was already the right answer).
    Ladder,
    /// The provider retry ladder was skipped — immediate final failure
    /// (tier 3).
    SkipRetry,
    /// A confident permanent bad-JSON failure aborted the repair loop
    /// before its cap.
    AbortEarly,
}

impl TriageAction {
    /// The wire/log label for the action.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoRetry => "auto_retry",
            Self::Guidance => "guidance",
            Self::Ladder => "ladder",
            Self::SkipRetry => "skip_retry",
            Self::AbortEarly => "abort_early",
        }
    }
}

/// The TRUE disposition of a logged failure — the label the fine-tune
/// trains on. `RetrySucceeded`/`RetryFailed` come from actually observing
/// the retried attempt; `Escalated` marks a classification that skipped the
/// retry entirely (the counterfactual is unobserved — off-policy rows the
/// trainer must weight accordingly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriageDisposition {
    /// The retry (harness or model) succeeded.
    RetrySucceeded,
    /// The retry was attempted and failed again.
    RetryFailed,
    /// The retry budget / cap was reached with the failure standing.
    CapReached,
    /// The classification skipped the retry (needs-user / permanent skip,
    /// bad-JSON early abort) — disposition observed as "escalated without a
    /// retry attempt".
    Escalated,
}

impl TriageDisposition {
    /// The wire/log label for the disposition.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RetrySucceeded => "retry_succeeded",
            Self::RetryFailed => "retry_failed",
            Self::CapReached => "cap_reached",
            Self::Escalated => "escalated",
        }
    }
}

/// One row of the failure-triage training log
/// (`~/.mnemo/laya/training/failure_triage.jsonl`). Written ONCE per
/// classified failure, when its disposition is known (the pending row is
/// held in memory until then), so every row is complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureLogRow {
    /// Unix seconds when the failure was classified.
    pub ts: u64,
    /// The [`FailureSite`] label.
    pub site: String,
    /// The failing tool's name (tool-batch / bad-JSON sites; `None` for
    /// provider errors).
    pub tool: Option<String>,
    /// The error text handed to the classifier (capped, verbatim).
    pub error_text: String,
    /// The [`FailureClass`] label.
    pub class: String,
    /// The calibrated confidence of the classification.
    pub confidence: f64,
    /// The [`TriageAction`] label.
    pub action: String,
    /// The [`TriageDisposition`] label, once resolved.
    pub disposition: Option<String>,
}

/// A classified failure whose disposition is not known yet — created by
/// [`log_failure`] / [`FailureTriageHandle::log_failure`], completed by
/// [`resolve_failure`]. Holding the row until the outcome is observable is
/// what keeps the log one-complete-row-per-failure (a turn that dies
/// mid-flight simply writes nothing). The row also carries the log path it
/// was created for, so the resolving sites never need the gate in scope.
#[derive(Debug, Clone)]
pub struct PendingFailureRow {
    row: FailureLogRow,
    path: PathBuf,
}

impl PendingFailureRow {
    /// The classified class label (for observability at the call sites).
    pub fn class(&self) -> &str {
        &self.row.class
    }

    /// The underlying row (tests / the fine-tune reader).
    pub fn row(&self) -> &FailureLogRow {
        &self.row
    }
}

/// Unix seconds now (0 when the clock is before the epoch — a log
/// timestamp must never panic).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Record one classified failure (in memory — the row is appended by
/// [`resolve_failure`] once the disposition is known). `error_text` is
/// capped at [`LOG_ERROR_TEXT_MAX_CHARS`] chars. Rows go to the default
/// training log; [`log_failure_at`] takes an explicit path.
pub fn log_failure(
    site: FailureSite,
    tool: Option<&str>,
    error_text: &str,
    class: FailureClass,
    confidence: f64,
    action: TriageAction,
) -> PendingFailureRow {
    log_failure_at(
        &training_log_path(),
        site,
        tool,
        error_text,
        class,
        confidence,
        action,
    )
}

/// [`log_failure`] against an explicit log path — the seam the gate's
/// optional override and the tests use.
#[allow(clippy::too_many_arguments)]
pub fn log_failure_at(
    path: &Path,
    site: FailureSite,
    tool: Option<&str>,
    error_text: &str,
    class: FailureClass,
    confidence: f64,
    action: TriageAction,
) -> PendingFailureRow {
    PendingFailureRow {
        row: FailureLogRow {
            ts: now_unix_secs(),
            site: site.as_str().to_string(),
            tool: tool.map(|t| t.to_string()),
            error_text: error_text.chars().take(LOG_ERROR_TEXT_MAX_CHARS).collect(),
            class: class.label().to_string(),
            confidence,
            action: action.as_str().to_string(),
            disposition: None,
        },
        path: path.to_path_buf(),
    }
}

/// The training log's home: `~/.mnemo/laya/training/`.
pub fn training_log_dir() -> PathBuf {
    global_config_dir().join("laya").join("training")
}

/// The training log file: `~/.mnemo/laya/training/failure_triage.jsonl`.
pub fn training_log_path() -> PathBuf {
    training_log_dir().join("failure_triage.jsonl")
}

/// Append one row to `path` (JSONL). Best-effort by design: a log write
/// failure is swallowed — training data must never fail a turn.
pub fn append_row(path: &Path, row: &FailureLogRow) {
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

/// Complete a pending failure row with its true disposition and append it to
/// the log path the row was created for (the gate's override, or the default
/// training log `~/.mnemo/laya/training/failure_triage.jsonl`).
pub fn resolve_failure(mut pending: PendingFailureRow, disposition: TriageDisposition) {
    pending.row.disposition = Some(disposition.as_str().to_string());
    append_row(&pending.path, &pending.row);
}

/// Read every parseable row of the training log at `path` in file order;
/// malformed lines and a missing file yield an empty/short list (the reader
/// is used by the startup fine-tune check and must never fail on a
/// half-written tail line).
pub fn read_rows(path: &Path) -> Vec<FailureLogRow> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter_map(|line| serde_json::from_str::<FailureLogRow>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;

    /// A classifier with a canned answer — `None` models every no-answer
    /// path (disabled backend, timeout, malformed response).
    struct StubClassifier {
        answer: Option<Answer>,
    }

    #[async_trait]
    impl Classifier for StubClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            self.answer.clone()
        }
    }

    /// A classifier that counts calls — the proof that a disabled gate asks
    /// nothing at all.
    struct CountingClassifier {
        calls: Arc<AtomicUsize>,
        answer: Option<Answer>,
    }

    #[async_trait]
    impl Classifier for CountingClassifier {
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

    fn handle_with(classifier: Arc<dyn Classifier>, enabled: bool) -> FailureTriageHandle {
        FailureTriageHandle::new(
            Arc::new(RwLock::new(Some(classifier))),
            Arc::new(AtomicBool::new(enabled)),
        )
    }

    #[test]
    fn triage_question_covers_the_four_classes_with_descriptions() {
        let Question::Choice {
            instructions,
            criteria,
        } = triage_question()
        else {
            panic!("triage_question must be a choice question");
        };
        assert!(!instructions.is_empty());
        let labels: Vec<_> = criteria.keys().cloned().collect();
        assert_eq!(
            labels,
            vec!["flaky_test", "needs_user", "permanent", "transient"]
        );
        for description in criteria.values() {
            assert!(!description.is_empty());
        }
    }

    #[test]
    fn class_labels_round_trip() {
        for class in [
            FailureClass::Transient,
            FailureClass::Permanent,
            FailureClass::NeedsUser,
            FailureClass::FlakyTest,
        ] {
            assert_eq!(FailureClass::from_label(class.label()), Some(class));
        }
        assert_eq!(FailureClass::from_label("other"), None);
        assert_eq!(FailureClass::from_label("Transient"), None);
    }

    #[tokio::test]
    async fn high_confidence_classifies_each_class() {
        for (label, expected) in [
            ("transient", FailureClass::Transient),
            ("permanent", FailureClass::Permanent),
            ("needs_user", FailureClass::NeedsUser),
            ("flaky_test", FailureClass::FlakyTest),
        ] {
            let classifier = StubClassifier {
                answer: choice(label, 0.9),
            };
            assert_eq!(
                triage_failure(&classifier, "boom").await,
                FailureTriage::Classified {
                    class: expected,
                    confidence: 0.9,
                }
            );
        }
    }

    #[tokio::test]
    async fn threshold_is_inclusive() {
        let at = StubClassifier {
            answer: choice("transient", TRIAGE_THRESHOLD),
        };
        assert!(matches!(
            triage_failure(&at, "boom").await,
            FailureTriage::Classified {
                class: FailureClass::Transient,
                ..
            }
        ));
        let below = StubClassifier {
            answer: choice("transient", TRIAGE_THRESHOLD - 0.01),
        };
        assert_eq!(
            triage_failure(&below, "boom").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::LowConfidence(TRIAGE_THRESHOLD - 0.01),
            }
        );
    }

    #[tokio::test]
    async fn no_answer_and_non_choice_answers_fall_back() {
        let none = StubClassifier { answer: None };
        assert_eq!(
            triage_failure(&none, "boom").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            }
        );
        let scored = StubClassifier {
            answer: Some(Answer::Score {
                value: 1.0,
                confidence: 0.99,
            }),
        };
        assert_eq!(
            triage_failure(&scored, "boom").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn unknown_label_is_scrubbed_and_bounded() {
        let dirty = StubClassifier {
            answer: choice("bogus\u{7}label", 0.99),
        };
        assert_eq!(
            triage_failure(&dirty, "boom").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::UnknownLabel("boguslabel".to_string()),
            }
        );
        let long = StubClassifier {
            answer: choice(&"x".repeat(100), 0.99),
        };
        let FailureTriage::Fallback {
            reason: FailureKeepReason::UnknownLabel(clean),
        } = triage_failure(&long, "boom").await
        else {
            panic!("a 100-char unknown label must fall back");
        };
        assert_eq!(clean.chars().count(), UNKNOWN_LABEL_MAX_CHARS);
    }

    #[tokio::test]
    async fn disabled_gate_never_asks_the_classifier() {
        let calls = Arc::new(AtomicUsize::new(0));
        let classifier = CountingClassifier {
            calls: calls.clone(),
            answer: choice("transient", 0.99),
        };
        let gate = handle_with(Arc::new(classifier), false);
        assert!(!gate.is_enabled());
        assert_eq!(
            gate.triage("boom").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            }
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn enabled_gate_with_an_empty_slot_falls_back() {
        let gate = FailureTriageHandle::new(
            Arc::new(RwLock::new(None)),
            Arc::new(AtomicBool::new(true)),
        );
        assert_eq!(
            gate.triage("boom").await,
            FailureTriage::Fallback {
                reason: FailureKeepReason::NoAnswer,
            }
        );
    }

    #[tokio::test]
    async fn enabled_gate_classifies_through_the_shared_slot() {
        let stub = StubClassifier {
            answer: choice("flaky_test", 0.85),
        };
        let gate = handle_with(Arc::new(stub), true);
        assert_eq!(
            gate.triage("test X failed").await,
            FailureTriage::Classified {
                class: FailureClass::FlakyTest,
                confidence: 0.85,
            }
        );
    }

    #[test]
    fn auto_retry_gate_allows_only_confident_transient_read_only_within_budget() {
        // The happy path: confident transient + read-only tool + budget.
        assert!(should_auto_retry(FailureClass::Transient, "search", 0, 0));
        assert!(should_auto_retry(FailureClass::Transient, "read_files", 0, 0));
        assert!(should_auto_retry(FailureClass::Transient, "file_read", 0, 0));
        // Every other class keeps the pre-classifier behavior.
        assert!(!should_auto_retry(FailureClass::Permanent, "search", 0, 0));
        assert!(!should_auto_retry(FailureClass::NeedsUser, "search", 0, 0));
        assert!(!should_auto_retry(FailureClass::FlakyTest, "search", 0, 0));
        // Mutating / side-effecting tools are never auto-retried.
        for tool in ["shell", "file_write", "file_edit", "memory_write", "git"] {
            assert!(!should_auto_retry(FailureClass::Transient, tool, 0, 0));
        }
        // Budgets: one retry per call, four per turn.
        assert!(!should_auto_retry(
            FailureClass::Transient,
            "search",
            MAX_AUTO_RETRIES_PER_CALL,
            0
        ));
        assert!(!should_auto_retry(
            FailureClass::Transient,
            "search",
            0,
            MAX_AUTO_RETRIES_PER_TURN
        ));
    }

    #[test]
    fn log_labels_are_stable() {
        assert_eq!(FailureSite::ToolBatch.as_str(), "tool_batch");
        assert_eq!(FailureSite::BadJsonRepair.as_str(), "bad_json_repair");
        assert_eq!(FailureSite::ProviderTurn.as_str(), "provider_turn");
        assert_eq!(TriageAction::AutoRetry.as_str(), "auto_retry");
        assert_eq!(TriageAction::Guidance.as_str(), "guidance");
        assert_eq!(TriageAction::Ladder.as_str(), "ladder");
        assert_eq!(TriageAction::SkipRetry.as_str(), "skip_retry");
        assert_eq!(TriageAction::AbortEarly.as_str(), "abort_early");
        assert_eq!(
            TriageDisposition::RetrySucceeded.as_str(),
            "retry_succeeded"
        );
        assert_eq!(TriageDisposition::RetryFailed.as_str(), "retry_failed");
        assert_eq!(TriageDisposition::CapReached.as_str(), "cap_reached");
        assert_eq!(TriageDisposition::Escalated.as_str(), "escalated");
    }

    #[test]
    fn resolve_writes_one_complete_row() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("failure_triage.jsonl");
        let pending = log_failure_at(
            &path,
            FailureSite::ToolBatch,
            Some("search"),
            "[tool error] connection reset",
            FailureClass::Transient,
            0.91,
            TriageAction::AutoRetry,
        );
        assert_eq!(pending.class(), "transient");
        resolve_failure(pending, TriageDisposition::RetrySucceeded);

        let rows = read_rows(&path);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.site, "tool_batch");
        assert_eq!(row.tool.as_deref(), Some("search"));
        assert_eq!(row.error_text, "[tool error] connection reset");
        assert_eq!(row.class, "transient");
        assert_eq!(row.confidence, 0.91);
        assert_eq!(row.action, "auto_retry");
        assert_eq!(row.disposition.as_deref(), Some("retry_succeeded"));
        assert!(row.ts > 0);
    }

    #[test]
    fn log_caps_the_error_text() {
        let long = "e".repeat(LOG_ERROR_TEXT_MAX_CHARS + 120);
        let pending = log_failure(
            FailureSite::ProviderTurn,
            None,
            &long,
            FailureClass::NeedsUser,
            0.99,
            TriageAction::SkipRetry,
        );
        assert_eq!(
            pending.row().error_text.chars().count(),
            LOG_ERROR_TEXT_MAX_CHARS
        );
        assert_eq!(pending.row().tool, None);
    }

    #[test]
    fn read_rows_skips_malformed_lines_and_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("failure_triage.jsonl");
        let good = serde_json::to_string(&FailureLogRow {
            ts: 1,
            site: "provider_turn".to_string(),
            tool: None,
            error_text: "boom".to_string(),
            class: "permanent".to_string(),
            confidence: 0.9,
            action: "guidance".to_string(),
            disposition: Some("retry_failed".to_string()),
        })
        .expect("serialize");
        std::fs::write(&path, format!("{good}\nnot json\n{good}\n")).expect("write");
        let rows = read_rows(&path);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].class, "permanent");
        assert!(read_rows(&dir.path().join("absent.jsonl")).is_empty());
    }

    #[test]
    fn guidance_notes_are_class_specific() {
        let notes = [
            FailureClass::Transient,
            FailureClass::Permanent,
            FailureClass::NeedsUser,
            FailureClass::FlakyTest,
        ]
        .map(guidance_note);
        for note in notes {
            assert!(!note.is_empty());
        }
        let unique: std::collections::BTreeSet<_> = notes.iter().collect();
        assert_eq!(unique.len(), 4);
        assert!(notes[2].contains("ask_user"));
    }

    #[test]
    fn skip_hints_are_class_specific() {
        assert!(skip_hint(FailureClass::NeedsUser).contains("user"));
        assert!(skip_hint(FailureClass::Permanent).contains("request"));
        assert!(skip_hint(FailureClass::Transient).contains("cannot help"));
    }
}
