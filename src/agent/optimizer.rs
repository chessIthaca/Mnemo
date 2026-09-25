// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Pure helpers for the compaction-survival lever (token-optimizer lever 4,
//! backlog e4a50d22).
//!
//! Compaction today is one summarizer call plus a lossy mechanical fallback:
//! a bad summary loses the dropped region outright, with no recovery. These
//! three deterministic pieces make it survivable — no store and no LLM call,
//! so every function is unit-testable in isolation:
//!
//! - [`extract_decisions`] — the durable decisions made so far, injected into
//!   the summarizer as a must-preserve block;
//! - [`build_digest`] — a heuristic post-compaction digest (files touched,
//!   commands run, error-marked results, line volume);
//! - [`checkpoint_region`] — the serialized form of the region about to be
//!   dropped, archived BEFORE the summary replaces it.
//!
//! It also holds lever 6's grading types — [`QualityReport`] / [`QualityGrade`],
//! computed from [`OptimizerState`]'s running counters — so the score's
//! thresholds live next to the levers that feed them.

use crate::provider::{Message, Role};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

/// Cap on the decisions injected as must-preserve ("cap ~10").
pub const MAX_DECISIONS: usize = 10;

/// Longest decision line kept (chars); longer lines are cut with an ellipsis.
const MAX_DECISION_CHARS: usize = 240;

/// Cap on distinct file paths named in the digest (keeps the note bounded on
/// a wide exploration pass).
const MAX_DIGEST_FILES: usize = 12;

/// Cap on distinct shell commands named in the digest.
const MAX_DIGEST_COMMANDS: usize = 8;

/// Decision markers, matched case-insensitively against a line. `DECISION:`
/// is the typed-prefix convention the memory system itself uses; the others
/// are the natural-language forms a model reaches for when it commits.
const DECISION_MARKERS: &[&str] = &[
    "decision:",
    "decided",
    "we'll use",
    "we will use",
    "chose ",
    "instead of",
    "must not",
    "must ",
    "don't ",
    "do not ",
];

/// Extract the durable decisions recorded in the conversation so far —
/// chronological, deduplicated (whitespace- and case-insensitive), capped at
/// [`MAX_DECISIONS`].
///
/// Scans assistant and user text only: tool output is evidence, not a
/// decision, and a shell result echoing the word "decided" is noise.
pub fn extract_decisions(messages: &[Message]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for message in messages {
        if !matches!(message.role, Role::Assistant | Role::User) {
            continue;
        }
        for line in message.content.as_text().lines() {
            if !has_decision_marker(line) {
                continue;
            }
            let decision = clip(line.trim());
            if decision.is_empty() {
                continue;
            }
            let key = normalize_key(&decision);
            if out.iter().any(|existing| normalize_key(existing) == key) {
                continue;
            }
            out.push(decision);
            if out.len() >= MAX_DECISIONS {
                return out;
            }
        }
    }
    out
}

/// Whether a line carries any decision marker (case-insensitive substring).
fn has_decision_marker(line: &str) -> bool {
    let lower = line.to_lowercase();
    DECISION_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Cut a line to [`MAX_DECISION_CHARS`], marking the cut with an ellipsis.
fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_DECISION_CHARS {
        return line.to_string();
    }
    let mut clipped: String = line.chars().take(MAX_DECISION_CHARS).collect();
    clipped.push('…');
    clipped
}

/// Collapse whitespace + lowercase — the dedup key, so a decision restated
/// with different spacing or casing counts once.
fn normalize_key(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// A compact, LLM-free digest of a region that compaction dropped: the files
/// it touched, the shell commands it ran, its error-marked tool results, and
/// its line volume. Injected as one note after compaction so the model keeps a
/// map of what the summary elided (and knows the checkpoint exists).
pub fn build_digest(messages: &[Message]) -> String {
    let mut files: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut errors = 0usize;
    let mut lines = 0usize;
    for message in messages {
        lines += message.content.as_text().lines().count();
        if message.role == Role::Tool && is_error_result(&message.content.as_text()) {
            errors += 1;
        }
        for call in &message.tool_calls {
            match call.name.as_str() {
                "shell" => {
                    if let Some(command) = arg_str(&call.arguments, "command") {
                        push_bounded(&mut commands, first_line(&command), MAX_DIGEST_COMMANDS);
                    }
                }
                "read_files" | "search" | "search_read" => {
                    for path in paths_from_call(&call.name, &call.arguments) {
                        push_bounded(&mut files, path, MAX_DIGEST_FILES);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = format!(
        "post-compaction digest: {lines} lines dropped; {} file(s) touched, {} shell command(s) \
         run, {errors} error-marked result(s)",
        files.len(),
        commands.len(),
    );
    if !files.is_empty() {
        out.push_str(&format!("\nfiles: {}", files.join(", ")));
    }
    if !commands.is_empty() {
        out.push_str(&format!("\ncommands: {}", commands.join(" | ")));
    }
    out
}

/// The slice compaction is about to drop: `messages[1..len - keep_recent]`
/// (index 0 is the system prompt, which compaction never drops). Empty when
/// there is nothing to drop. The single definition of the drop boundary —
/// [`checkpoint_region`] archives exactly this slice and [`build_digest`]
/// describes it, so the two can never disagree.
pub fn dropped_region(messages: &[Message], keep_recent: usize) -> &[Message] {
    if messages.len() <= 1 {
        return &[];
    }
    let end = messages.len().saturating_sub(keep_recent);
    if end <= 1 {
        return &[];
    }
    &messages[1..end]
}

/// Serialize the region compaction is about to drop (see [`dropped_region`]).
/// This is what gets archived, so a summary that loses the region still leaves
/// it recoverable.
pub fn checkpoint_region(messages: &[Message], keep_recent: usize) -> String {
    let region = dropped_region(messages, keep_recent);
    if region.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (offset, message) in region.iter().enumerate() {
        out.push_str(&format!(
            "#{} [{:?}] {}\n",
            offset + 1,
            message.role,
            format_message(message)
        ));
    }
    out
}

/// Render one message for the checkpoint: its text, then any tool calls it
/// made (name + raw arguments), so the archived form is self-describing.
fn format_message(message: &Message) -> String {
    let mut out = message.content.as_text();
    for call in &message.tool_calls {
        out.push_str(&format!("\n  -> {}({})", call.name, call.arguments));
    }
    out
}

/// Whether a tool result carries an error marker (`[tool error]` from the turn
/// loop's FAILED wrap, or an error-flavored first line).
fn is_error_result(text: &str) -> bool {
    let head = first_line(text);
    text.contains("[tool error]")
        || head.to_lowercase().contains("error")
        || head.to_lowercase().contains("failed")
}

/// The first line of `text` (trimmed).
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// Read a string field out of a tool call's JSON arguments.
fn arg_str(arguments: &str, field: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(arguments).ok()?;
    value.get(field)?.as_str().map(str::to_string)
}

/// The file paths a read/search call names: `read_files`'s `files[].path` (or
/// the single-path shorthand), and `search`/`search_read`'s `path` scope.
fn paths_from_call(tool: &str, arguments: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    if let Some(files) = value.get("files").and_then(|f| f.as_array()) {
        for spec in files {
            if let Some(path) = spec.get("path").and_then(|p| p.as_str()) {
                out.push(path.to_string());
            }
        }
    }
    if let Some(path) = value.get("path").and_then(|p| p.as_str()) {
        out.push(path.to_string());
    }
    // A memory/graph hunt carries no path; it is not a file touch.
    let _ = tool;
    out
}

/// Push `item` if absent, stopping once `cap` distinct entries are held.
fn push_bounded(out: &mut Vec<String>, item: String, cap: usize) {
    if item.is_empty() || out.len() >= cap || out.contains(&item) {
        return;
    }
    out.push(item);
}

// ---------------------------------------------------------------------------
// Lever 6 — S-F quality score (backlog e4a50d22)
// ---------------------------------------------------------------------------

/// The letter grade of a [`QualityReport`], best (`S`) to worst (`F`).
///
/// Ordered by quality — `S < A < ... < F` — so "a drop of two bands" is a plain
/// numeric comparison (lever 7's quality-drop nudge). Serializes as the bare
/// letter, which is what the frontend's grade badge renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum QualityGrade {
    /// Healthy: room to spare, almost no wasted tokens.
    S,
    /// Good: comfortable fill, little waste.
    A,
    /// Fair: filling up, or some redundant work.
    B,
    /// Poor: the window is tight, or the session is repeating itself.
    C,
    /// Bad: close to the ceiling, or waste-dominated.
    D,
    /// Critical: nearly full and/or burning tokens on repeats.
    F,
}

impl QualityGrade {
    /// The grade for a 0–100 quality score. Bands are deliberately coarse so a
    /// grade never flickers on a rounding change.
    pub fn from_score(score: i32) -> Self {
        match score {
            95.. => Self::S,
            85..=94 => Self::A,
            70..=84 => Self::B,
            55..=69 => Self::C,
            40..=54 => Self::D,
            _ => Self::F,
        }
    }

    /// The bare letter (`"S"` ... `"F"`) — the wire form the UI badge shows.
    pub fn letter(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::F => "F",
        }
    }
}

/// A graded snapshot of how healthy the conversation's context is
/// (token-optimizer lever 6, backlog e4a50d22). Rides
/// `AgentEvent::ContextUsage` to the frontend; the event carries `None` when
/// the quality lever is off, so the wire stays backward compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityReport {
    /// The letter grade.
    pub grade: QualityGrade,
    /// Window fill at the time of the report (0–100).
    pub fill_pct: u8,
    /// Cumulative tokens spent on redundant serves (skeletons).
    pub waste_tokens: u64,
    /// Share of lever-served reads that were stale re-reads (0–100).
    pub stale_read_rate: u8,
    /// Decisions seen per 100 messages (0–100) — a low value means the session
    /// is churning without committing to anything.
    pub decision_density: u8,
}

/// The raw counters [`QualityReport::compute`] grades — a snapshot of the
/// session's [`OptimizerState`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QualitySignals {
    /// Reads served through a lever (skeleton or delta).
    pub reads_total: u64,
    /// Of those, the ones served as a skeleton (a stale re-read).
    pub skeleton_serves: u64,
    /// Cumulative tokens every lever handed the model.
    pub served_tokens: u64,
    /// Cumulative tokens spent on redundant serves.
    pub waste_tokens: u64,
    /// Decisions the compaction-survival lever has preserved.
    pub decisions_seen: u64,
}

impl QualityReport {
    /// Grade the context from the current fill level and the session's
    /// optimizer counters (token-optimizer lever 6, backlog e4a50d22).
    ///
    /// Deterministic by construction: the same inputs always yield the same
    /// grade, every band boundary is unit-tested, and there is no model call —
    /// the report is a UI signal and a nudge trigger, never a decision.
    pub fn compute(fill_pct: u8, messages: usize, signals: QualitySignals) -> Self {
        let fill_pct = fill_pct.min(100);
        let waste_ratio_pct = ratio_pct(signals.waste_tokens, signals.served_tokens);
        let stale_read_rate = ratio_pct(signals.skeleton_serves, signals.reads_total);
        let decision_density = ratio_pct(signals.decisions_seen, messages.max(1) as u64);
        let score = 100
            - fill_penalty(fill_pct) as i32
            - waste_penalty(waste_ratio_pct) as i32
            - stale_penalty(stale_read_rate) as i32;
        Self {
            grade: QualityGrade::from_score(score),
            fill_pct,
            waste_tokens: signals.waste_tokens,
            stale_read_rate,
            decision_density,
        }
    }
}

/// `part / whole` as a 0–100 percentage (`0` when the whole is `0`).
fn ratio_pct(part: u64, whole: u64) -> u8 {
    if whole == 0 {
        return 0;
    }
    (part.saturating_mul(100) / whole).min(100) as u8
}

/// Fill is the primary risk: a full window forces compaction, and compaction is
/// where detail is lost.
fn fill_penalty(fill_pct: u8) -> u8 {
    match fill_pct {
        95.. => 70,
        90..=94 => 62,
        75..=89 => 35,
        60..=74 => 18,
        40..=59 => 6,
        _ => 0,
    }
}

/// Waste is the secondary signal: tokens the model paid for twice.
fn waste_penalty(waste_ratio_pct: u8) -> u8 {
    match waste_ratio_pct {
        50.. => 30,
        25..=49 => 18,
        10..=24 => 8,
        5..=9 => 3,
        _ => 0,
    }
}

/// Stale re-reads are the tertiary signal — a symptom of the same waste.
fn stale_penalty(stale_read_rate: u8) -> u8 {
    match stale_read_rate {
        50.. => 15,
        25..=49 => 8,
        10..=24 => 3,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Lever 6 — session counters
// ---------------------------------------------------------------------------

/// The session's running optimizer counters (token-optimizer lever 6, backlog
/// e4a50d22), fed by `AgentLoop::record_savings_event` — one call site updates
/// the ledger row AND these counters, so the two can never disagree.
///
/// Atomics rather than a lock: `record_savings_event` runs on `&self` from the
/// turn loop and the tool-ingestion path, and the counters are a cheap monotone
/// summary of what the levers already logged.
#[derive(Debug)]
pub struct OptimizerState {
    /// Reads served through a lever (skeleton or delta).
    reads_total: AtomicU64,
    /// Of those, the ones served as a skeleton (a stale re-read).
    skeleton_serves: AtomicU64,
    /// Cumulative tokens every lever handed the model.
    served_tokens: AtomicU64,
    /// Cumulative tokens spent on redundant serves.
    waste_tokens: AtomicU64,
    /// Decisions the compaction-survival lever has preserved.
    decisions_seen: AtomicU64,
    /// Requests seen so far — the clock lever 7's nudge cooldown is measured
    /// against (ticked once per turn-loop iteration).
    requests: AtomicU64,
    /// The `requests` value at which the last nudge fired (`0` = never).
    last_nudge_request: AtomicU64,
    /// The best (lowest-index) grade seen this session, or [`Self::NO_GRADE`]
    /// before the first observation — the baseline lever 7's quality-drop
    /// trigger compares against.
    best_grade: AtomicU8,
}

impl Default for OptimizerState {
    /// Hand-written rather than derived: `AtomicU8::default()` is `0`, which is
    /// the *best* grade (`S`) — a derived default would make a fresh session
    /// look like it had already seen an `S`, firing a spurious quality-drop
    /// nudge on the first turn. The baseline must start at [`NO_GRADE`].
    ///
    /// [`NO_GRADE`]: OptimizerState::NO_GRADE
    fn default() -> Self {
        Self {
            reads_total: AtomicU64::new(0),
            skeleton_serves: AtomicU64::new(0),
            served_tokens: AtomicU64::new(0),
            waste_tokens: AtomicU64::new(0),
            decisions_seen: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            last_nudge_request: AtomicU64::new(0),
            best_grade: AtomicU8::new(Self::NO_GRADE),
        }
    }
}

impl OptimizerState {
    /// Record one lever event, mirroring the `savings_events` row written
    /// alongside it. `kind` is the ledger kind; `after` the tokens the model
    /// was actually handed.
    pub fn record(&self, kind: &str, after: i64) {
        let after = after.max(0) as u64;
        self.served_tokens.fetch_add(after, Ordering::Relaxed);
        match kind {
            // A skeleton is a stale re-read: the model already had the file, so
            // whatever it pays for the second serve is waste.
            "skeleton" => {
                self.reads_total.fetch_add(1, Ordering::Relaxed);
                self.skeleton_serves.fetch_add(1, Ordering::Relaxed);
                self.waste_tokens.fetch_add(after, Ordering::Relaxed);
            }
            "delta_read" => {
                self.reads_total.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Note the decisions the compaction-survival lever preserved.
    pub fn note_decisions(&self, count: usize) {
        self.decisions_seen
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    /// A snapshot for [`QualityReport::compute`].
    pub fn snapshot(&self) -> QualitySignals {
        QualitySignals {
            reads_total: self.reads_total.load(Ordering::Relaxed),
            skeleton_serves: self.skeleton_serves.load(Ordering::Relaxed),
            served_tokens: self.served_tokens.load(Ordering::Relaxed),
            waste_tokens: self.waste_tokens.load(Ordering::Relaxed),
            decisions_seen: self.decisions_seen.load(Ordering::Relaxed),
        }
    }

    /// The "no grade observed yet" sentinel for [`best_grade`](Self::best_grade).
    const NO_GRADE: u8 = u8::MAX;

    /// Count one request (once per turn-loop iteration) and return the new
    /// count — the clock lever 7's nudge cooldown is measured against.
    pub fn tick_request(&self) -> u64 {
        self.requests.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The best (lowest-index) grade seen this session, if any was observed.
    pub fn best_grade(&self) -> Option<QualityGrade> {
        match self.best_grade.load(Ordering::Relaxed) {
            Self::NO_GRADE => None,
            index => grade_from_index(index),
        }
    }

    /// Fold a fresh grade into the session's best-seen grade and return the
    /// PREVIOUS best, so the caller can detect a drop. `S` is the best grade,
    /// so a lower declaration index wins.
    pub fn observe_grade(&self, grade: QualityGrade) -> Option<QualityGrade> {
        let previous = self.best_grade();
        let index = grade as u8;
        let current = self.best_grade.load(Ordering::Relaxed);
        if current == Self::NO_GRADE || index < current {
            self.best_grade.store(index, Ordering::Relaxed);
        }
        previous
    }

    /// True when no nudge has fired within the last `cooldown` requests.
    pub fn nudge_allowed(&self, cooldown: u32) -> bool {
        let last = self.last_nudge_request.load(Ordering::Relaxed);
        if last == 0 {
            return true;
        }
        self.requests.load(Ordering::Relaxed).saturating_sub(last) >= cooldown as u64
    }

    /// Record that a nudge fired at request `at`.
    pub fn mark_nudge(&self, at: u64) {
        self.last_nudge_request.store(at, Ordering::Relaxed);
    }
}

/// Inverse of a grade's declaration index (see [`QualityGrade`]).
fn grade_from_index(index: u8) -> Option<QualityGrade> {
    match index {
        0 => Some(QualityGrade::S),
        1 => Some(QualityGrade::A),
        2 => Some(QualityGrade::B),
        3 => Some(QualityGrade::C),
        4 => Some(QualityGrade::D),
        5 => Some(QualityGrade::F),
        _ => None,
    }
}

/// How many bands `current` fell below `best` (0 when it did not fall).
fn grade_delta(best: QualityGrade, current: QualityGrade) -> u8 {
    (current as u8).saturating_sub(best as u8)
}

/// The lever-7 steering note (backlog e4a50d22), or `None` when no trigger
/// fires. Two triggers, one note, one cooldown (applied by the caller):
///
///  * **lean-output** — the window is at or past `fill_threshold_pct` full, so
///    the model is steered to spend fewer tokens on its own output;
///  * **quality-drop** — the grade fell two or more bands below the session's
///    best, which the fill bar alone cannot show.
///
/// The note rides the volatile tail, which is popped right after the request,
/// so it never mutates the cached conversation prefix.
pub fn nudge_note(
    report: &QualityReport,
    best: Option<QualityGrade>,
    lean_output: bool,
    fill_threshold_pct: u8,
) -> Option<String> {
    let mut lines: Vec<String> = Vec::new();
    if lean_output && report.fill_pct >= fill_threshold_pct {
        lines.push(format!(
            "[context guidance] context is {}% full; output tokens cost ~5× input — keep \
             visible output lean (no preamble, no restating context)",
            report.fill_pct,
        ));
    }
    if let Some(best) = best {
        if grade_delta(best, report.grade) >= 2 {
            lines.push(format!(
                "Context quality dropped from {} to {} ({}% of the window used, {}% stale \
                 re-reads, ~{} tokens spent on repeated serves) — consider compacting or \
                 narrowing the task before continuing.",
                best.letter(),
                report.grade.letter(),
                report.fill_pct,
                report.stale_read_rate,
                report.waste_tokens,
            ));
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

/// Append lever 7's steering note to the volatile tail. `None` (and an empty
/// note) leave the tail byte-identical to its pre-lever form, which is what
/// keeps the flag-off path — and the provider prefix cache — untouched.
pub fn append_nudge(tail: &mut String, nudge: Option<&str>) {
    if let Some(note) = nudge {
        if !note.is_empty() {
            tail.push_str("\n\n");
            tail.push_str(note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolCall;

    /// A tool-call assistant turn (the shape `build_digest` reads).
    fn assistant_call(name: &str, arguments: &str) -> Message {
        Message {
            tool_calls: vec![ToolCall::new("c1", name, arguments)],
            ..Message::assistant_text("")
        }
    }

    #[test]
    fn extract_decisions_collects_marker_lines_chronologically() {
        let messages = vec![
            Message::system("system prompt"),
            Message::user_text("we will use rusqlite for storage"),
            Message::assistant_text("DECISION: keep the store free of the optimizer\nnoise"),
            Message::user_text("must not commit to main"),
        ];
        let decisions = extract_decisions(&messages);
        assert_eq!(decisions.len(), 3, "{decisions:?}");
        assert_eq!(decisions[0], "we will use rusqlite for storage");
        assert_eq!(decisions[1], "DECISION: keep the store free of the optimizer");
        assert_eq!(decisions[2], "must not commit to main");
    }

    #[test]
    fn extract_decisions_dedups_case_and_whitespace_insensitively() {
        let messages = vec![
            Message::user_text("We will use   rusqlite"),
            Message::assistant_text("we will use rusqlite"),
        ];
        let decisions = extract_decisions(&messages);
        assert_eq!(decisions.len(), 1, "{decisions:?}");
    }

    #[test]
    fn extract_decisions_caps_at_the_maximum_keeping_the_earliest() {
        let messages: Vec<Message> = (0..MAX_DECISIONS + 5)
            .map(|i| Message::assistant_text(format!("decided option number {i}")))
            .collect();
        let decisions = extract_decisions(&messages);
        assert_eq!(decisions.len(), MAX_DECISIONS);
        // Chronological: the cap keeps the FIRST ten, not a sample.
        assert_eq!(decisions[0], "decided option number 0");
    }

    #[test]
    fn extract_decisions_ignores_tool_output() {
        // A shell result echoing the word "decided" is evidence, not a decision.
        let messages = vec![
            Message::tool_result("c1", "shell", "the test decided to fail"),
            Message::user_text("decided to ship"),
        ];
        let decisions = extract_decisions(&messages);
        assert_eq!(
            decisions,
            vec!["decided to ship".to_string()],
            "tool output is not a decision"
        );
    }

    #[test]
    fn build_digest_counts_files_commands_and_errors() {
        let messages = vec![
            Message::user_text("hi"),
            assistant_call("read_files", r#"{"files":[{"path":"src/a.rs"},{"path":"src/a.rs"}]}"#),
            assistant_call("read_files", r#"{"files":[{"path":"src/b.rs"}]}"#),
            assistant_call("shell", r#"{"command":"cargo test\n--all"}"#),
            Message::tool_result("c2", "shell", "[tool error] boom"),
        ];
        let digest = build_digest(&messages);
        assert!(digest.contains("2 file(s) touched"), "{digest}");
        assert!(digest.contains("src/a.rs, src/b.rs"), "{digest}");
        assert!(digest.contains("1 shell command(s) run"), "{digest}");
        assert!(digest.contains("1 error-marked result(s)"), "{digest}");
        // A multi-line command contributes its first line only.
        assert!(digest.contains("commands: cargo test"), "{digest}");
        assert!(!digest.contains("--all"), "{digest}");
        // A duplicate path is named once (bounded disclosure, not a repeat list).
        assert_eq!(digest.matches("src/a.rs").count(), 1, "{digest}");
    }

    #[test]
    fn dropped_region_excludes_the_system_prompt_and_the_kept_tail() {
        let messages = vec![
            Message::system("sys"),
            Message::user_text("dropped 1"),
            Message::assistant_text("dropped 2"),
            Message::user_text("kept 1"),
            Message::assistant_text("kept 2"),
        ];
        let region = dropped_region(&messages, 2);
        assert_eq!(region.len(), 2, "{}", region.len());
        assert_eq!(region[0].content.as_text(), "dropped 1");
        assert_eq!(region[1].content.as_text(), "dropped 2");
        // Nothing to drop -> empty, never a panic (short conversations, or a
        // keep_recent that already covers everything).
        assert!(dropped_region(&messages, 10).is_empty());
        assert!(dropped_region(&messages[..1], 2).is_empty());
    }

    #[test]
    fn checkpoint_region_serializes_exactly_the_dropped_region() {
        let messages = vec![
            Message::system("sys prompt"),
            Message::user_text("DROPPED_BODY"),
            Message::assistant_text("KEPT_BODY"),
        ];
        let checkpoint = checkpoint_region(&messages, 1);
        assert!(checkpoint.contains("DROPPED_BODY"), "{checkpoint}");
        assert!(
            !checkpoint.contains("KEPT_BODY"),
            "the kept tail is never archived: {checkpoint}"
        );
        assert!(
            !checkpoint.contains("sys prompt"),
            "the system prompt is never archived: {checkpoint}"
        );
        // Tool calls are part of the archived form (the checkpoint is
        // self-describing: what was called, with which arguments).
        let with_call = vec![
            Message::system("s"),
            assistant_call("shell", r#"{"command":"ls"}"#),
            Message::user_text("tail"),
        ];
        let checkpoint = checkpoint_region(&with_call, 1);
        assert!(checkpoint.contains(r#"-> shell({"command":"ls"})"#), "{checkpoint}");
    }

    #[test]
    fn quality_grade_bands_are_pinned_at_their_boundaries() {
        // Lever 6: the grade is a UI signal and a nudge trigger, so every band
        // boundary is pinned — shifting a threshold has to be deliberate.
        assert_eq!(QualityGrade::from_score(100), QualityGrade::S);
        assert_eq!(QualityGrade::from_score(95), QualityGrade::S);
        assert_eq!(QualityGrade::from_score(94), QualityGrade::A);
        assert_eq!(QualityGrade::from_score(85), QualityGrade::A);
        assert_eq!(QualityGrade::from_score(84), QualityGrade::B);
        assert_eq!(QualityGrade::from_score(70), QualityGrade::B);
        assert_eq!(QualityGrade::from_score(69), QualityGrade::C);
        assert_eq!(QualityGrade::from_score(55), QualityGrade::C);
        assert_eq!(QualityGrade::from_score(54), QualityGrade::D);
        assert_eq!(QualityGrade::from_score(40), QualityGrade::D);
        assert_eq!(QualityGrade::from_score(39), QualityGrade::F);
        assert_eq!(QualityGrade::from_score(0), QualityGrade::F);
    }

    #[test]
    fn quality_grade_orders_best_to_worst() {
        // Ord is the declaration order (S first): lever 7's "dropped two bands"
        // nudge is a numeric comparison on this, so pin it.
        assert!(QualityGrade::S < QualityGrade::A);
        assert!(QualityGrade::A < QualityGrade::F);
        assert!(QualityGrade::D < QualityGrade::F);
        assert_eq!(QualityGrade::S.letter(), "S");
        assert_eq!(QualityGrade::F.letter(), "F");
        // `as u8` must stay the same order (it is what a future
        // best-seen-grade tracker stores).
        assert_eq!(QualityGrade::S as u8, 0);
        assert_eq!(QualityGrade::F as u8, 5);
    }

    #[test]
    fn quality_report_is_s_for_a_roomy_session_with_no_waste() {
        let report = QualityReport::compute(10, 4, QualitySignals::default());
        assert_eq!(report.grade, QualityGrade::S, "{report:?}");
        assert_eq!(report.fill_pct, 10);
        assert_eq!(report.waste_tokens, 0);
        assert_eq!(report.stale_read_rate, 0);
    }

    #[test]
    fn fill_is_the_primary_risk_and_waste_the_secondary() {
        // Fill alone can reach F (>90% of the window gone).
        let full = QualityReport::compute(92, 4, QualitySignals::default());
        assert_eq!(full.grade, QualityGrade::F, "{full:?}");
        // Waste alone can only reach B: a busy-but-roomy session must never
        // read as critical, or the nudge would fire on healthy long turns.
        let wasteful = QualityReport::compute(
            10,
            4,
            QualitySignals {
                served_tokens: 1000,
                waste_tokens: 800,
                ..Default::default()
            },
        );
        assert_eq!(wasteful.grade, QualityGrade::B, "{wasteful:?}");
        assert_eq!(wasteful.waste_tokens, 800);
        // Stale re-reads alone: every lever-served read was a repeat -> A.
        let stale = QualityReport::compute(
            10,
            4,
            QualitySignals {
                reads_total: 10,
                skeleton_serves: 10,
                ..Default::default()
            },
        );
        assert_eq!(stale.grade, QualityGrade::A, "{stale:?}");
        assert_eq!(stale.stale_read_rate, 100);
    }

    #[test]
    fn quality_report_is_deterministic_and_zero_safe() {
        // No window reported (max == 0) must not divide by zero, and identical
        // inputs must always grade identically.
        let empty = QualitySignals::default();
        assert_eq!(
            QualityReport::compute(0, 0, empty),
            QualityReport::compute(0, 0, empty)
        );
        assert_eq!(QualityReport::compute(0, 0, empty).grade, QualityGrade::S);
        // fill_pct is clamped: the local estimate can overshoot the window.
        assert_eq!(
            QualityReport::compute(200, 4, QualitySignals::default()).fill_pct,
            100
        );
        assert_eq!(
            QualityReport::compute(200, 4, QualitySignals::default()).grade,
            QualityGrade::F
        );
    }

    #[test]
    fn decision_density_is_decisions_per_hundred_messages() {
        let signals = QualitySignals {
            decisions_seen: 3,
            ..Default::default()
        };
        assert_eq!(QualityReport::compute(10, 6, signals).decision_density, 50);
        // Never divides by zero on an empty conversation (capped at 100).
        assert_eq!(QualityReport::compute(0, 0, signals).decision_density, 100);
    }

    #[test]
    fn optimizer_state_counts_serves_waste_and_decisions() {
        // Lever 6: one `record` call site feeds the ledger row and these
        // counters, so the score is graded from exactly what the levers logged.
        let state = OptimizerState::default();
        state.record("skeleton", 400);
        state.record("delta_read", 100);
        state.record("compression", 50);
        state.note_decisions(3);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.reads_total, 2, "skeleton + delta are both reads");
        assert_eq!(snapshot.skeleton_serves, 1);
        assert_eq!(
            snapshot.served_tokens, 550,
            "every kind contributes what it served"
        );
        assert_eq!(snapshot.waste_tokens, 400, "only the skeleton is redundant");
        assert_eq!(snapshot.decisions_seen, 3);
        // A negative `after` (a re-expansion row) never underflows a counter.
        state.record("archive_expand", -100);
        assert_eq!(state.snapshot().served_tokens, 550);
    }

    /// A report built from the given fill / served / waste signals.
    fn report(fill_pct: u8, served: u64, waste: u64) -> QualityReport {
        QualityReport::compute(
            fill_pct,
            4,
            QualitySignals {
                served_tokens: served,
                waste_tokens: waste,
                ..Default::default()
            },
        )
    }

    #[test]
    fn nudge_note_is_none_when_no_trigger_fires() {
        // Below the fill threshold with no baseline grade -> no note at all, so
        // the volatile tail stays untouched.
        assert_eq!(nudge_note(&report(10, 0, 0), None, true, 75), None);
        // A lean-output flag that is off never fires, however full the window.
        assert_eq!(nudge_note(&report(99, 0, 0), None, false, 75), None);
    }

    #[test]
    fn lean_output_nudge_fires_at_and_above_the_threshold() {
        let note = nudge_note(&report(75, 0, 0), None, true, 75)
            .expect("exactly at the threshold must fire");
        assert!(note.contains("[context guidance] context is 75% full"), "{note}");
        // One point below the threshold stays quiet.
        assert_eq!(nudge_note(&report(74, 0, 0), None, true, 75), None);
        assert!(nudge_note(&report(90, 0, 0), None, true, 75).is_some());
    }

    #[test]
    fn quality_drop_nudge_names_the_drop_and_its_components() {
        // Two bands (A -> C) fires, and the note carries both letters plus the
        // signals the model can act on.
        let dropped = report(70, 1000, 300);
        assert_eq!(dropped.grade, QualityGrade::C, "{dropped:?}");
        let note = nudge_note(&dropped, Some(QualityGrade::A), false, 75)
            .expect("a two-band drop must fire");
        assert!(note.contains("dropped from A to C"), "{note}");
        assert!(note.contains("70% of the window used"), "{note}");
        assert!(note.contains("~300 tokens spent on repeated serves"), "{note}");
        // It never speaks for the lean-output lever.
        assert!(!note.contains("[context guidance]"), "{note}");
    }

    #[test]
    fn a_single_band_drop_stays_quiet() {
        // S -> A is ONE band: below the two-band bar, so no note.
        let a_grade = QualityReport::compute(
            5,
            4,
            QualitySignals {
                reads_total: 10,
                skeleton_serves: 10,
                ..Default::default()
            },
        );
        assert_eq!(a_grade.grade, QualityGrade::A, "{a_grade:?}");
        assert_eq!(nudge_note(&a_grade, Some(QualityGrade::S), false, 75), None);
        // A three-band drop from the same baseline does fire.
        let c_grade = QualityReport::compute(
            5,
            4,
            QualitySignals {
                served_tokens: 1000,
                waste_tokens: 300,
                reads_total: 10,
                skeleton_serves: 10,
                ..Default::default()
            },
        );
        assert_eq!(c_grade.grade, QualityGrade::C, "{c_grade:?}");
        assert!(nudge_note(&c_grade, Some(QualityGrade::S), false, 75).is_some());
    }

    #[test]
    fn append_nudge_leaves_the_tail_byte_identical_for_none() {
        // The flag-off path: the tail must not change by even one byte, or the
        // nudge machinery would invalidate the provider prefix cache.
        let base = "workflow: Executing\nmemories: none".to_string();
        let mut untouched = base.clone();
        append_nudge(&mut untouched, None);
        assert_eq!(untouched, base);
        let mut empty = base.clone();
        append_nudge(&mut empty, Some(""));
        assert_eq!(empty, base, "an empty note is not a note");
        // A real note lands after a blank line, exactly once.
        let mut nudged = base.clone();
        append_nudge(&mut nudged, Some("be terse"));
        assert_eq!(nudged, format!("{base}\n\nbe terse"));
    }

    #[test]
    fn nudge_cooldown_suppresses_the_next_nudges() {
        let state = OptimizerState::default();
        let first = state.tick_request();
        assert!(state.nudge_allowed(10), "nothing has fired yet");
        state.mark_nudge(first);
        assert!(!state.nudge_allowed(10), "the same request is suppressed");
        for _ in 0..9 {
            state.tick_request();
        }
        assert!(!state.nudge_allowed(10), "nine requests later is too soon");
        state.tick_request(); // the tenth request since the nudge
        assert!(state.nudge_allowed(10), "the cooldown has elapsed");
    }

    #[test]
    fn best_grade_tracks_the_session_best_and_reports_the_previous() {
        let state = OptimizerState::default();
        // The first observation has no baseline to compare against.
        assert_eq!(state.best_grade(), None);
        assert_eq!(state.observe_grade(QualityGrade::A), None);
        assert_eq!(state.best_grade(), Some(QualityGrade::A));
        // A worse grade leaves the baseline at the best seen...
        assert_eq!(state.observe_grade(QualityGrade::C), Some(QualityGrade::A));
        assert_eq!(state.best_grade(), Some(QualityGrade::A));
        // ...and a better one raises it.
        assert_eq!(state.observe_grade(QualityGrade::S), Some(QualityGrade::A));
        assert_eq!(state.best_grade(), Some(QualityGrade::S));
    }
}
