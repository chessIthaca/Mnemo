// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The plan-file format's parse side — [`PlanKind`], [`PlanFile`], [`Step`],
//! [`ReviewRoundStamp`], and [`PlanFile::parse`] with its private line
//! helpers.
//!
//! Kernel-owned (job 2b decision 5): the memory indexer and the
//! finish-capture digests are kernel consumers that must read a plan file
//! without importing the app's workflow types, so the single parser for the
//! `.coding/plans/<id>.md` format lives here. The app's
//! [`crate::app::workflow::plan_file`] re-exports these types and keeps the
//! serialize/write side — one parser, two module homes, zero drift.

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// What kind of plan this is — decides whether completion enters the review
/// closing sequence or skips straight to Complete.
///
/// - [`Implementation`](Self::Implementation) (the default): completing the
///   root plan's last step transitions to `Reviewing` — the test→review→commit
///   closing sequence runs, and `finish` is gated on a reviewer report. Use
///   this for any plan that may change source code.
/// - [`Research`](Self::Research): completing the root plan's last step
///   transitions directly to `Complete` — no reviewer, no report, no `finish`
///   gate. Use this for investigation/planning that produces no source-code
///   changes (reading code, drafting a design, exploring options).
///
/// The kind is set at `create_plan` time and persisted in the `.md` (a `## Kind`
/// section), so [`Workflow::complete_step`](crate::app::workflow::Workflow::complete_step)
/// reads it from the in-memory plan — the decision is by construction, not a
/// prompt instruction. Old plans without a `## Kind` section default to
/// `Implementation` (the historical behavior: every plan was reviewed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PlanKind {
    /// A plan that may change source code — completion enters `Reviewing`.
    #[default]
    Implementation,
    /// An investigation/planning plan that changes no source code — completion
    /// skips review and goes straight to `Complete`.
    Research,
    /// A bug-fix plan — completion enters `Reviewing` like `Implementation`,
    /// but with a locked 4-step skeleton (reproduce → root-cause → fix →
    /// verify), a required `bug:{symptom}` param at creation, and a BUG:
    /// memory auto-captured at finish. Bugs ALWAYS use this kind, never
    /// `Implementation` (compiled-prompt habit).
    #[serde(rename = "bug_fixing")]
    BugFixing,
}

impl PlanKind {
    /// Parse a kind from its lowercase serialized form. Unknown/empty values
    /// fall back to [`Implementation`](Self::Implementation) (the safe default
    /// — an unrecognized kind is reviewed, not silently skipped).
    fn from_str_ci(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "research" => Self::Research,
            "bug_fixing" => Self::BugFixing,
            _ => Self::Implementation,
        }
    }

    /// The lowercase token written to the `## Kind` section.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Implementation => "implementation",
            Self::Research => "research",
            Self::BugFixing => "bug_fixing",
        }
    }
}

impl std::fmt::Display for PlanKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A plan parsed from a markdown file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanFile {
    pub title: String,
    pub goal: String,
    pub context: String,
    /// Whether this plan is an implementation (reviewed on completion) or
    /// research (skips review). Defaults to `Implementation` for old plans
    /// that have no `## Kind` section.
    ///
    /// `#[serde(skip)]` keeps this off the frontend wire shape (`PlanFile` is
    /// serialized directly to the UI via `WorkflowStateInfo.plan` / `get_plan`)
    /// — `kind` is a workflow-internal concern that decides the completion
    /// path, not a display field. It is still persisted to the `.md` via the
    /// manual `serialize()`/`parse()` path (the `## Kind` section), which does
    /// not go through serde, so skipping it here does not affect on-disk
    /// storage or the in-memory workflow logic.
    #[serde(skip)]
    pub kind: PlanKind,
    /// The bug symptom for `kind = BugFixing` plans (the required `bug:`
    /// param at creation). `None` for other kinds. Persisted as the `## Bug`
    /// section; `#[serde(skip)]` keeps it off the frontend wire shape.
    #[serde(skip)]
    pub bug_symptom: Option<String>,
    /// The regression test name recorded by the verify step of a
    /// `BugFixing` plan (via `update_plan`'s `regression_test` field) —
    /// `finish` is blocked without it, and the name is validated against the
    /// code graph. `None` until recorded. Persisted as the
    /// `## Regression test` section; `#[serde(skip)]` keeps it off the
    /// frontend wire shape.
    #[serde(skip)]
    pub regression_test: Option<String>,
    /// Whether a `BugFixing` fix turned out FEATURE-SCALE mid-flight (new
    /// pub types / cross-module surface) and the agent recorded it via
    /// `update_plan`'s `landed_design` field (backlog e5a84ce9). When true,
    /// `finish` is blocked until the plan context carries a "Landed design"
    /// amendment (the constitution's documentation expectations). Persisted
    /// as the `## Landed design` section; `#[serde(skip)]` keeps it off the
    /// frontend wire shape.
    #[serde(skip)]
    pub landed_design: bool,
    /// The caller's detailed steps for `kind = BugFixing` plans — the
    /// step-level recipes create_plan carried (exact paths, line anchors,
    /// test designs) that the locked 4-step skeleton replaces as the
    /// checklist. Persisted as the `## Detailed steps` section so the plan
    /// file stays the COMPLETE crash-resumption document (backlog
    /// 77ff8f45): a restarted session resumes with the full recipe
    /// instead of re-deriving it from the bug + context alone. The bullets
    /// are CHECKABLE sub-items (`- [ ]`/`- [x]`, backlog 9441d776): the
    /// marks live in this raw body (round-tripping verbatim through
    /// serialize→parse) and are ticked via
    /// [`complete_detailed_step`](Self::complete_detailed_step), so a
    /// mid-fix restart sees exactly which sub-steps shipped. `None` for
    /// other kinds and for bug plans created without steps;
    /// `#[serde(skip)]` keeps it off the frontend wire shape.
    #[serde(skip)]
    pub detailed_steps: Option<String>,
    /// The reviewer rounds dispatched for this plan (backlog 85313a7e) — one
    /// [`ReviewRoundStamp`] per spawn, recording the round number and the
    /// BASE REVISION that round reviewed (`git rev-parse HEAD` at dispatch
    /// time, when the dispatcher suspends and the reviewer starts looking).
    /// A later round reads the last stamp to delta-scope itself:
    /// `git diff <base>` is exactly what changed since the previous round
    /// verified the tree. Persisted as the `## Reviews` section (one
    /// `<round> <base>` line per stamp); `#[serde(skip)]` keeps it off the
    /// frontend wire shape (`PlanFile` is serialized directly to the UI).
    #[serde(skip)]
    pub reviews: Vec<ReviewRoundStamp>,
    pub steps: Vec<Step>,
}

/// One reviewer round dispatched for a plan (backlog 85313a7e).
///
/// `round` is 1-based in dispatch order; `base` is the commit-ish the round
/// reviewed — `HEAD` at dispatch time. The NEXT round delta-scopes against it
/// (`git diff <base>`), so a re-review verifies only what changed instead of
/// re-reading the whole diff. Persisted as one `<round> <base>` line in the
/// plan file's `## Reviews` section, so the scope survives a restart.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewRoundStamp {
    /// The 1-based round number, in dispatch order.
    pub round: u32,
    /// The base revision this round reviewed (HEAD at dispatch time).
    pub base: String,
}

/// A single step in a plan.
///
/// A step has an optional short **header** (the bold punchline, shown in the
/// executing toolbar) plus the full **text** (the detailed body, shown expanded
/// in the PlanProgress view). A step authored as `**Header** — body` parses
/// with `header = Some("Header")` and `text` holding the full line; a plain
/// step (no bold) has `header = None` and `text` = the whole line. This is
/// backward-compatible: existing plans (no bold) parse unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// 0-indexed step number.
    pub index: usize,
    /// The step text (the full content — header + body).
    pub text: String,
    /// The short bold header extracted from the text, when the step starts
    /// with `**Header**`. `None` for plain steps. Shown alone in the
    /// executing toolbar; the full `text` is shown expanded in PlanProgress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// Whether the step is checked off.
    pub done: bool,
}impl PlanFile {
    /// Parse a plan from markdown text.
    pub fn parse(text: &str) -> Result<Self> {
        let mut title = String::new();
        let mut goal = String::new();
        let mut context = String::new();
        let mut kind = PlanKind::default();
        let mut bug_symptom: Option<String> = None;
        let mut regression_test: Option<String> = None;
        let mut landed_design = false;
        let mut detailed_steps = String::new();
        let mut reviews: Vec<ReviewRoundStamp> = Vec::new();
        let mut steps = Vec::new();

        let mut section = Section::Preamble;
        for line in text.lines() {
            let trimmed = line.trim();
            // Detect section headers.
            if trimmed.starts_with("# ") {
                let mut t = trimmed.trim_start_matches("# ").to_string();
                // Strip a leading "Plan: " prefix if present (our serialize adds it).
                if let Some(rest) = t.strip_prefix("Plan: ") {
                    t = rest.to_string();
                }
                title = t;
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("## ") {
                section = match rest.to_lowercase().as_str() {
                    "goal" => Section::Goal,
                    "kind" => Section::Kind,
                    "context" => Section::Context,
                    "bug" => Section::Bug,
                    "regression test" => Section::RegressionTest,
                    "landed design" => Section::LandedDesign,
                    "reviews" => Section::Reviews,
                    "steps" => Section::Steps,
                    "detailed steps" => Section::DetailedSteps,
                    _ => {
                        // Recognize "## Step N — Title" headers (free-form plan
                        // docs not authored via create_plan) as steps, so they
                        // display in the PlanProgress view.
                        if let Some(title) = parse_step_header(rest) {
                            let header = extract_bold_header(&title);
                            steps.push(Step {
                                index: steps.len(),
                                text: title,
                                header,
                                done: false,
                            });
                        }
                        Section::Other
                    }
                };
                continue;
            }
            // Parse checklist items in the Steps section.
            if matches!(section, Section::Steps) {
                if let Some((done, text)) = parse_checkbox(trimmed) {
                    let header = extract_bold_header(&text);
                    steps.push(Step {
                        index: steps.len(),
                        text,
                        header,
                        done,
                    });
                    continue;
                }
            }
            // Accumulate section content.
            match section {
                Section::Goal => {
                    if !goal.is_empty() {
                        goal.push('\n');
                    }
                    goal.push_str(line);
                }
                Section::Kind => {
                    // The kind is a single lowercase token on its own line.
                    // Take the first non-blank line encountered; ignore blanks.
                    let trimmed_line = line.trim();
                    if !trimmed_line.is_empty() {
                        kind = PlanKind::from_str_ci(trimmed_line);
                    }
                }
                Section::Context => {
                    if !context.is_empty() {
                        context.push('\n');
                    }
                    context.push_str(line);
                }
                Section::Bug => {
                    let trimmed_line = line.trim();
                    if !trimmed_line.is_empty() {
                        bug_symptom = Some(trimmed_line.to_string());
                    }
                }
                Section::RegressionTest => {
                    let trimmed_line = line.trim();
                    if !trimmed_line.is_empty() {
                        regression_test = Some(trimmed_line.to_string());
                    }
                }
                Section::LandedDesign => {
                    // The flag section body is "yes" (our serialize) — accept
                    // "true" too for hand-edited files.
                    if matches!(line.trim(), "yes" | "true") {
                        landed_design = true;
                    }
                }
                Section::DetailedSteps => {
                    // Accumulate the raw section body (the caller's
                    // step-level recipes) — trimmed at the end, like
                    // Context.
                    if !detailed_steps.is_empty() {
                        detailed_steps.push('\n');
                    }
                    detailed_steps.push_str(line);
                }
                Section::Reviews => {
                    // One `<round> <base>` line per stamp (our serialize).
                    // A malformed line is SKIPPED, never an error: the
                    // section is bookkeeping, and a hand-edited plan must
                    // still load (the stamps are advisory scope hints, not
                    // a safety gate).
                    let trimmed_line = line.trim();
                    if !trimmed_line.is_empty() {
                        if let Some((round, base)) = trimmed_line.split_once(' ') {
                            if let Ok(round) = round.trim().parse::<u32>() {
                                reviews.push(ReviewRoundStamp {
                                    round,
                                    base: base.trim().to_string(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        Ok(Self {
            title: title.trim().to_string(),
            goal: goal.trim().to_string(),
            context: context.trim().to_string(),
            kind,
            bug_symptom,
            regression_test,
            landed_design,
            detailed_steps: if detailed_steps.is_empty() {
                None
            } else {
                Some(detailed_steps.trim().to_string())
            },
            reviews,
            steps,
        })
    }
}#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Preamble,
    Goal,
    Kind,
    Context,
    Steps,
    DetailedSteps,
    Bug,
    RegressionTest,
    LandedDesign,
    Reviews,
    Other,
}

/// Parse a markdown checklist line: `- [ ] text` or `- [x] text`.
/// Returns (done, text).
pub(crate) fn parse_checkbox(line: &str) -> Option<(bool, String)> {
    // Match "- [ ] ..." or "- [x] ..." (case-insensitive for x).
    let line = line.trim_start();
    if !line.starts_with("- [") {
        return None;
    }
    let after_bracket = &line[3..];
    if after_bracket.is_empty() {
        return None;
    }
    let mark = after_bracket.as_bytes()[0];
    let done = match mark {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    let rest = &after_bracket[1..];
    if !rest.starts_with("] ") && rest != "]" {
        return None;
    }
    let text = if rest.len() > 2 {
        // Strip the leading "] " and any leading "N. " step number.
        let text = &rest[2..];
        // Strip a leading "N. " if present.
        let text = text.trim_start();
        strip_leading_number(text)
    } else {
        String::new()
    };
    Some((done, text))
}

/// Parse a "Step N — Title" header (the part after `## `).
///
/// Recognizes common separators: `—` (em dash), `–` (en dash), `-` (hyphen),
/// `:` (colon). Returns the title text (without the number and separator), or
/// `None` if the header doesn't match the `Step N` pattern.
///
/// Examples that match:
///   "Step 1 — Store: font family + font size state" → "Store: font family + font size state"
///   "Step 2 - CSS variables"                        → "CSS variables"
///   "Step 3: ConfigDialog component"                → "ConfigDialog component"
pub(crate) fn parse_step_header(header: &str) -> Option<String> {
    // Match "Step " prefix case-insensitively, but preserve the original
    // case of the title text that follows.
    let lower = header.to_lowercase();
    let rest = lower.strip_prefix("step ")?;
    // `rest` is lowercased — find how many chars the prefix consumed so we can
    // slice into the original (case-preserving) string.
    let prefix_len = header.len() - rest.len();
    let orig_after_prefix = &header[prefix_len..];
    // Consume the step number (one or more digits).
    let digits_end = rest
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    if digits_end == 0 {
        return None; // no digits after "Step "
    }
    let after_num = &rest[digits_end..];
    // Skip whitespace between the number and the separator.
    let after_ws = after_num.trim_start();
    let ws_skipped = after_num.len() - after_ws.len();
    // Slice into the original string to preserve case.
    let orig_after_ws = &orig_after_prefix[digits_end + ws_skipped..];
    // Find and consume the separator: —, –, -, or :.
    let title = orig_after_ws
        .strip_prefix("—")
        .or_else(|| orig_after_ws.strip_prefix("–"))
        .or_else(|| orig_after_ws.strip_prefix("-"))
        .or_else(|| orig_after_ws.strip_prefix(":"))
        .map(|t| t.trim().to_string());
    // If no separator is found, the header is just "Step N" with no title —
    // treat that as a step with an empty title rather than None, so the step
    // still appears in the list.
    match title {
        Some(t) if !t.is_empty() => Some(t),
        Some(_) => Some(String::new()),
        None => Some(String::new()),
    }
}

/// Extract a short bold header from the start of a step's text.
///
/// A step may start with `**Header**` (bold markdown) — the bold run is the
/// header (the punchline shown in the executing toolbar), and the rest is the
/// body. Returns the header text (without the `**`) when the text starts with
/// a bold run, or `None` for a plain step. The full `text` is preserved
/// unchanged so the PlanProgress view can show the header + body expanded.
///
/// Tolerant of longer marker runs: `****Header****` and the asymmetric
/// `****Header**` (the legacy double-wrapped form `StepInput::to_text` used
/// to produce when a model passed an already-bold header — backlog
/// 2026-08-20) parse as a single header, not an empty one.
///
/// Examples:
///   "**Add ask_user tool** — create src/tool/..."  → Some("Add ask_user tool")
///   "****Verify builds/tests** — run them"         → Some("Verify builds/tests")
///   "Create the ask_user tool"                     → None
///   "**Header**"                                   → Some("Header")
pub(crate) fn extract_bold_header(text: &str) -> Option<String> {
    extract_bold_header_impl(text)
}

fn extract_bold_header_impl(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    // The leading run of `*` must be at least 2 (bold). Runs longer than 2
    // (the legacy double-wrapped `****Header****`) are tolerated — measure
    // the run and skip all of it, so the closer search below never sees the
    // opening markers (the old `strip_prefix("**") + find("**")` found the
    // closer immediately after the opening pair and returned an EMPTY header
    // → None → the plan view showed the raw asterisks).
    let lead = trimmed.len() - trimmed.trim_start_matches('*').len();
    if lead < 2 {
        return None;
    }
    let rest = &trimmed[lead..];
    // Find the closing run: the first run of 2+ `*` after the header text.
    // A single `*` inside the header (e.g. `**a*b**`) is not a closer.
    let bytes = rest.as_bytes();
    let mut close = None; // (start, end) byte offsets of the closing run
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'*' {
            let run_start = i;
            while i < bytes.len() && bytes[i] == b'*' {
                i += 1;
            }
            if i - run_start >= 2 {
                close = Some((run_start, i));
                break;
            }
        } else {
            i += 1;
        }
    }
    // Consume the ENTIRE closing run (not just 2) so the text after it starts
    // clean — leaving stray `**` would fail the leading-token check below for
    // the asymmetric legacy form (`****Header**`).
    let (close_start, close_end) = close?;
    let header = rest[..close_start].trim();
    if header.is_empty() {
        return None;
    }
    // The bold run must be a LEADING token — the closing run is followed by
    // whitespace, a separator (—/–/-/:), or end-of-string. This avoids a false
    // positive on a step that merely *contains* bold mid-sentence (e.g.
    // "Use **bold** for emphasis" → not a header). The intended form is
    // "**Header** — body" or "**Header**".
    let after = &rest[close_end..];
    let after_trimmed = after.trim_start();
    let is_leading = after_trimmed.is_empty()
        || after_trimmed.starts_with('—')
        || after_trimmed.starts_with('–')
        || after_trimmed.starts_with('-')
        || after_trimmed.starts_with(':')
        || after_trimmed.starts_with('\n');
    if !is_leading {
        return None;
    }
    Some(header.to_string())
}

/// Strip a leading "N. " step number if present.
fn strip_leading_number(s: &str) -> String {
    let mut found_digit = false;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            found_digit = true;
        } else if c == '.' && found_digit {
            // Found "N." — skip the following space if any.
            let rest = &s[i + 1..];
            return rest.trim_start().to_string();
        } else {
            break;
        }
    }
    s.to_string()
}