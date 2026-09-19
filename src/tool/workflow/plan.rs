// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `create_plan` + `update_plan` + `complete_step` + `abandon_plan` workflow tools.
//!
//! Plan resumability gate (2027-01-09): `create_plan` and `update_plan` reject
//! plan content too thin to resume from after a recompile+restart — substantive
//! context (≥40 chars: symptom/root-cause, file anchors, verification
//! commands), every step naming concrete file path(s) (or an explicit no-code
//! marker, the same vocabulary as the backlog detail bar via
//! [`crate::backlog::body_carries_detail_marker`]), and `bug_fixing` plans
//! naming the regression-test design. One actionable error names every
//! violation so the retry fixes all of them in one shot.
//!
//! `create_plan` is `AutoRun` — creating a plan document is non-destructive and
//! is the prerequisite for all further work, so it must never block on approval.
//! `update_plan` is `AutoRun` for the same reason — it edits the active plan in
//! place (preserving completed steps) and is the non-destructive way to adjust
//! scope. `complete_step` and `abandon_plan` are also `AutoRun` — they
//! only rewrite the project's own `.coding/plans/` bookkeeping (sandboxed,
//! non-destructive to user code), so they never block on approval either. All
//! four are gated by workflow state.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::memory::MemoryStoreTrait;
use crate::project::git_ops::{
    ensure_work_branch_with, prepare_branch_with, valid_branch_name, GitRunner, RealGit,
};
use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};
use crate::workflow::{PlanKind, Workflow, WorkflowState};

/// The locked 4-step skeleton for `kind = bug_fixing` plans — the bug
/// protocol: reproduce with a failing regression test → document the root
/// cause → minimal fix → verify. `create_plan` FORCES these steps for bug
/// plans and `update_plan` refuses to replace them (the skeleton lock), so
/// the discipline cannot slip. The caller's provided steps are NOT
/// discarded (backlog 77ff8f45): they persist as CHECKABLE sub-items under
/// `## Detailed steps` (ticked via complete_step's detailed_step_index,
/// backlog 9441d776) — the crash-resumption detail — so the plan file
/// carries the full step-level recipe alongside the locked checklist,
/// with per-sub-step completion state that survives restarts.
const BUG_FIXING_SKELETON: [&str; 4] = [
    "**Reproduce with failing regression test** — Write a regression test that \
     reproduces the defect (constitution: every defect gets a regression test \
     that fails without the fix and passes with it). Run it and confirm it FAILS.",
    "**Document root cause** — Investigate and document the root cause. \
     memory_write a BUG: record (symptom → root cause → fix + regression test \
     name, ≤600 chars).",
    "**Minimal fix** — Apply the minimal fix that makes the regression test \
     pass. Do not refactor unrelated code.",
    "**Verify** — Run the regression test + the full test suite (cargo test \
     unpiped, warning-free). Record the regression test name via update_plan \
     (regression_test field) — finish is blocked without it. If the fix \
     turned out FEATURE-SCALE (new pub types or cross-module surface): \
     amend the plan context via update_plan append with a paragraph \
     starting \"Landed design —\" (architecture, deviations from the \
     sketch, measured numbers, invariants to preserve) and record \
     landed_design=true BEFORE completing this step; at finish write a \
     SPEC memory and amend the BUG record with the landed outcome \
     (agent.md 'Documentation expectations'; the finish gate blocks \
     landed_design without the context amendment).",
];

/// Arguments for `create_plan`.
#[derive(Debug, Deserialize)]
struct CreatePlanArgs {
    title: String,
    goal: String,
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    context: String,
    /// Optional for `kind = bug_fixing` (the locked skeleton is forced;
    /// provided steps persist as checkable `## Detailed steps` sub-items —
    /// the crash-resumption detail); required otherwise.
    #[serde(default)]
    steps: Vec<StepInput>,
    /// The plan kind — `implementation` (default, reviewed on completion),
    /// `research` (skips review, goes straight to Complete), or `bug_fixing`
    /// (locked 4-step skeleton + required `bug` param + BUG: memory at
    /// finish). Defaults to `implementation` when omitted. A research plan may
    /// still write its own `.coding/**` artifacts with the file tools
    /// (dispatch allows artifact paths only), and an implementation/bug_fixing
    /// SUB-plan completing under a research root forces that root through
    /// Reviewing — see `Workflow::review_required` and
    /// `research_write_verdict`.
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    kind: PlanKind,
    /// The bug symptom for `kind = bug_fixing` plans — REQUIRED (the tool
    /// errors without it). Ignored for other kinds.
    #[serde(default)]
    bug: Option<String>,
    /// Optional: a git feature branch to create + switch to BEFORE the plan
    /// file is written (forked from `base`, default "main"). Replaces the
    /// manual 5-6 command checkout dance an agent otherwise runs at the start
    /// of every backlog item, where `.coding/plans/stack.json` + the
    /// `backlog.json` InFlight flip block `git checkout main`.
    #[serde(default)]
    branch: Option<String>,
    /// Optional: the branch to fork `branch` from (default `"main"`).
    #[serde(default)]
    base: Option<String>,
}

/// A single plan step as accepted from the model.
///
/// The documented step format is a bold header followed by a body recipe
/// (`**header** — body`). Models naturally emit that structure two ways:
/// - a plain string already in the documented form (`"**Add X** — do it"`), or
/// - a structured object `{"header": "Add X", "body": "do it"}`.
///
/// Without this, the structured form failed deserialization with
/// `invalid type: map, expected a string` (backlog #38). The untagged enum
/// accepts either; [`StepInput::to_text`] normalizes a map step into the
/// documented string form so the workflow layer (which stores `Vec<String>`)
/// is unchanged. The untagged order matters: `Text` (string) is tried before
/// `Map` (object), so a plain string never accidentally parses as a map.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StepInput {
    /// A step given as a plain string (already in the documented format).
    Text(String),
    /// A step given as a structured `{header, body}` object.
    Map {
        /// The short header text, PLAIN — do not include `**` markers (they
        /// are added automatically; an already-bold header is unwrapped to a
        /// single layer). The punchline shown in the executing toolbar.
        header: String,
        /// The body recipe (self-contained instructions). May be empty —
        /// or null (strict mode's representation of "no body", plan
        /// 21118961 review HIGH 1).
        #[serde(default)]
        body: Option<String>,
    },
}

impl StepInput {
    /// Normalize into the stored step text. A `Text` step passes through
    /// verbatim; a `Map` step is joined into the documented
    /// `**header** — body` form (or `**header**` alone when the body is
    /// empty). A header that already carries its own `**` markers is
    /// unwrapped first: models routinely pass `"**Add X**"` (the schema's
    /// string-step example shows the bold form), and double-wrapping wrote
    /// `****Add X****` into plan files — which the header extractor then
    /// failed to parse, so the plan view showed raw asterisks instead of a
    /// bold header (backlog 2026-08-20, "****Verify builds/tests**** should
    /// be rendered bold").
    fn to_text(self) -> String {
        match self {
            StepInput::Text(s) => s,
            StepInput::Map { header, body } => {
                let header = unwrap_bold(&header);
                let body = body.unwrap_or_default();
                if body.is_empty() {
                    format!("**{header}**")
                } else {
                    format!("**{header}** — {body}")
                }
            }
        }
    }
}

/// Strip ALL layers of `**` markers from a header that already carries them
/// (`"**Add X**"` → `"Add X"`, and the double-wrapped `"****Add X****"` →
/// `"Add X"`), so the wrap in [`StepInput::to_text`] is always exactly one
/// layer (review L1, 2026-08-20).
///
/// Anything else passes through unchanged — single `*italics*` markers,
/// `**` appearing only mid-text, or a header without any markers — so this
/// never mangles a legitimately plain header. The unwrapped value is
/// trimmed; an all-marker header (`"**"` / `"****"`, empty inner) is left
/// as-is (degenerate caller input).
fn unwrap_bold(header: &str) -> &str {
    let mut current = header.trim();
    // Unwrap to a fixpoint: a single pass left `"****X****"` → `"**X**"`,
    // and the re-wrap in to_text restored the broken `****X****`.
    while let Some(inner) = current
        .strip_prefix("**")
        .and_then(|rest| rest.strip_suffix("**"))
        .map(str::trim)
    {
        if inner.is_empty() {
            break;
        }
        current = inner;
    }
    current
}

/// Normalize a list of [`StepInput`] steps into stored step text.
fn steps_to_text(steps: Vec<StepInput>) -> Vec<String> {
    steps.into_iter().map(StepInput::to_text).collect()
}

/// Collapse a value to a single line for the plan file's one-line sections
/// (`## Bug` / `## Regression test`): trim, then replace every run of
/// whitespace (including newlines) with a single space. The plan-file parser
/// keeps only the LAST non-blank line of each section, and an embedded
/// `\n\n## Kind\nresearch` would re-parse as a real section header — so a
/// multi-line value must never reach the file verbatim (section injection /
/// silent truncation, review HIGH 2).
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Escape lines that would re-parse as plan-file structure (review L3,
/// backlog 77ff8f45): the parser switches sections on any line whose
/// trimmed form starts with `## ` and takes the title from `# ` — a
/// detailed step quoting that format (e.g. a bug plan about the plan
/// format itself) would corrupt the re-parsed plan (inject checklist
/// items, overwrite the symptom/kind/regression test, or the title). A
/// leading backslash defuses the marker while keeping the recipe
/// readable; the escape alters the line's own leading character, so the
/// parser's trim-before-match cannot bypass it.
fn escape_section_markers(text: &str) -> String {
    text.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("## ") || trimmed.starts_with("# ") {
                format!("\\{line}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The minimum substance for a plan's `context` field, in characters — the
/// plan resumability gate (2027-01-09): the plan file is the
/// crash-resumption document, and a thin context ("fix crash", "see
/// above") leaves a restarted session re-deriving what the planning phase
/// already knew. Field-sized next to the backlog detail bar's
/// `MIN_BODY_CHARS` (160, `src/backlog.rs`).
const MIN_CONTEXT_CHARS: usize = 40;

/// Word-boundary matcher for the `bug_fixing` regression-test design
/// signal — `\btest` / `\bregression` (case-insensitive) so substring
/// lookalikes ("latest", "contested") don't satisfy the design check
/// while "test:", "(regression)", and "tests.rs" do (review L4,
/// 2027-01-09).
static TEST_DESIGN_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(?:test|regression)").unwrap()
});

/// Backlog 51ee41c1: count the DISTINCT module directories referenced by
/// path-like tokens in `texts` — every `src/<module>/` prefix (including
/// the inner `src/` of `src-tauri/src/<module>/...` and
/// `frontend/src/<dir>/...`), so files in the same module count once
/// (`src/codegraph/mod.rs` + `src/codegraph/store.rs` → 1) while a bare
/// `src/lib.rs` (no module segment) counts none. The feature-scale
/// steering note fires when a bug_fixing payload BOTH carries more
/// detailed steps than the locked 4-step skeleton AND spans ≥3 modules —
/// the shape of a bug-triggered feature (cf. plan c87093c1: 10 grammars +
/// 7 visitors + new crate deps).
fn referenced_module_dirs(texts: &[&str]) -> usize {
    let mut dirs = std::collections::HashSet::new();
    for text in texts {
        let mut rest = *text;
        while let Some(idx) = rest.find("src/") {
            let after = &rest[idx + 4..];
            if let Some(seg_end) = after.find('/') {
                dirs.insert(after[..seg_end].to_string());
            }
            rest = after;
        }
    }
    dirs.len()
}

/// The context-side resumability issues — substance (symptom/root-cause,
/// file anchors, verification commands) plus, for `bug_fixing` plans, the
/// regression-test design. Shared by create_plan (via
/// [`validate_plan_resumability`]) and update_plan (which validates the
/// effective post-update context). Returns the first violation, if any.
fn context_issue(
    context: &str,
    kind: PlanKind,
    detailed_steps: Option<&str>,
) -> Option<String> {
    let ctx = context.trim();
    if ctx.chars().count() < MIN_CONTEXT_CHARS {
        return Some(format!(
            "context is too thin to resume from ({} chars, min {MIN_CONTEXT_CHARS}) — the \
             plan file is the crash-resumption document: context must carry the \
             symptom/root-cause, exact file paths + symbol anchors, and the verification \
             commands (e.g. 'cargo test')",
            ctx.chars().count(),
        ));
    }
    if kind == PlanKind::BugFixing {
        // The constitution requires every defect to get a regression test,
        // so the plan must say which test and where — the design may live
        // in the context or the detailed steps.
        let design = format!("{ctx} {}", detailed_steps.unwrap_or(""));
        if !TEST_DESIGN_RE.is_match(&design) {
            return Some(
                "bug_fixing plans must carry the regression-test design — name the \
                 regression test in the context (or detailed steps): which test, which \
                 file it lives in, and how it fails without the fix (constitution: every \
                 defect gets a regression test)"
                    .to_string(),
            );
        }
    }
    None
}

/// The step-side resumability issue — a step that names no file path (or
/// no-code marker). Shared by create_plan (via
/// [`validate_plan_resumability`]) and update_plan (which validates only
/// the steps being written). `bug_fixing` plans pass `bug_skeleton_exempt =
/// true` for the forced skeleton (path-free by design — the locked
/// checklist); update_plan's Reviewing append window (backlog 37f8631a)
/// passes false so appended follow-on steps meet the same bar as every
/// other step. Returns the first violation, if any.
fn step_issue(steps: &[String], bug_skeleton_exempt: bool) -> Option<String> {
    if bug_skeleton_exempt {
        return None;
    }
    let path_free: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| !crate::backlog::body_carries_detail_marker(step))
        .map(|(i, _)| i)
        .collect();
    let first = *path_free.first()?;
    let preview: String = steps[first].chars().take(60).collect();
    Some(format!(
        "step {} names no file path ({} of {} steps path-free) — every step must \
         name the concrete file path(s) it touches (or carry an explicit \
         'no-code research' marker for research-only steps): '{preview}'",
        first + 1,
        path_free.len(),
        steps.len(),
    ))
}

/// Structural resumability validation for a plan document — the plan
/// resumability gate (2027-01-09). A plan too thin to resume from after a
/// recompile+restart is rejected at the tool boundary with one actionable
/// error naming everything missing (the `backlog_add` detail-bar UX): the
/// context must be substantive (symptom/root-cause, file anchors,
/// verification commands), every step must name concrete file path(s) (or
/// carry an explicit no-code marker — the same vocabulary as the backlog
/// detail bar via [`crate::backlog::body_carries_detail_marker`]), and
/// `bug_fixing` plans must carry the regression-test design (the forced
/// skeleton is path-free by design, so the per-step check does not apply;
/// the design requirement takes its place). Returns every violation found
/// (empty = the plan passes) so the retry fixes all of them in one shot.
fn validate_plan_resumability(
    context: &str,
    steps: &[String],
    kind: PlanKind,
    detailed_steps: Option<&str>,
) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(issue) = context_issue(context, kind, detailed_steps) {
        issues.push(issue);
    }
    if let Some(issue) = step_issue(steps, kind == PlanKind::BugFixing) {
        issues.push(issue);
    }
    issues
}

/// Format resumability-gate issues into the single actionable error the
/// tools return — every violation numbered in one message so the retry
/// fixes all of them at once (the `backlog_add` UX).
fn resumability_gate_error(prefix: &str, issues: &[String]) -> String {
    let numbered = issues
        .iter()
        .enumerate()
        .map(|(i, issue)| format!("({}) {issue}", i + 1))
        .collect::<Vec<_>>()
        .join(" ");
    format!("{prefix} — {numbered}")
}

/// A step number as accepted from the model: a JSON integer or a numeric
/// string (`"3"` — models occasionally quote the number). The untagged enum
/// accepts either (the same tolerant-shape precedent as [`StepInput`],
/// backlog #38); a non-numeric string fails with an actionable error.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StepNumber {
    /// A step number given as a JSON integer.
    Number(u32),
    /// A step number given as a (numeric) string.
    Text(String),
}

impl StepNumber {
    /// Parse into the 1-indexed step number, rejecting non-numeric strings
    /// with an actionable message.
    fn parse(self) -> Result<u32, String> {
        match self {
            StepNumber::Number(n) => Ok(n),
            StepNumber::Text(s) => s
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("step_index must be a step number, got '{s}'")),
        }
    }
}

/// Arguments for `complete_step`.
#[derive(Debug, Deserialize)]
struct CompleteStepArgs {
    /// The step number, 1-INDEXED (1 = the first step — matching the numbers
    /// shown in the plan document, the PlanProgress view, and this tool's own
    /// result echo). Accepted as a JSON integer or a numeric string. Exactly
    /// one of step_index / detailed_step_index is required.
    #[serde(default)]
    step_index: Option<StepNumber>,
    /// The detailed sub-step number, 1-INDEXED (1 = the first bullet under
    /// the active plan's `## Detailed steps` section) — ticks that sub-item's
    /// checkbox WITHOUT touching the skeleton steps or the workflow state
    /// (a sub-step tick never completes the plan; only the skeleton steps
    /// drive state). Accepted as a JSON integer or a numeric string.
    #[serde(default)]
    detailed_step_index: Option<StepNumber>,
    /// Optional: the id of the plan this step belongs to. When provided, the
    /// tool verifies it matches the active plan's id and errors on a mismatch
    /// (so the agent can't silently complete a step in the wrong stacked plan).
    /// When omitted, the step is completed against the active plan.
    #[serde(default)]
    plan_id: Option<String>,
}

/// Arguments for `update_plan`.
#[derive(Debug, Default, Deserialize)]
struct UpdatePlanArgs {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    steps: Option<Vec<StepInput>>,
    /// When true, `steps` are APPENDED after the remaining (not-yet-completed)
    /// steps instead of replacing them, and a provided non-empty `context` is
    /// APPENDED to the existing context instead of replacing it — the
    /// chunked-write protocol for very long plans (backlog 0085ccc0): create
    /// with the first few steps, then extend per chunk without resending.
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    append: bool,
    /// The regression test name recorded by a bug_fixing plan's verify step —
    /// `finish` is blocked without it (and the name is validated against the
    /// code graph).
    #[serde(default)]
    regression_test: Option<String>,
    /// Set true when a bug_fixing fix turned out FEATURE-SCALE (new pub
    /// types / cross-module surface — backlog e5a84ce9): the finish gate
    /// then requires a "Landed design" context amendment (the
    /// constitution's documentation expectations).
    #[serde(default)]
    landed_design: Option<bool>,
}

/// The `create_plan` workflow tool.
pub struct CreatePlanTool {
    workflow: Arc<Mutex<Workflow>>,
    /// Optional memory store for the RECALLED CONTEXT rider: a best-effort
    /// PASSIVE recall (`recall_peek` — never bumps access) over the plan's
    /// title + goal (+ bug symptom) whose top hits ride the create result.
    /// Structurally enforces the "memory_search before planning non-trivial
    /// work" rule — relevant prior knowledge arrives WITH the plan instead
    /// of depending on the agent remembering to look for it. `None` (tests /
    /// no store wired) → silent skip.
    memory: Option<Arc<dyn MemoryStoreTrait>>,
    /// The git runner — production uses [`RealGit`] (spawns `git`); tests
    /// inject a mock so the suite doesn't depend on git being installed.
    git: Arc<dyn GitRunner + Send + Sync>,
}

impl CreatePlanTool {
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self {
            workflow,
            memory: None,
            git: Arc::new(RealGit),
        }
    }

    /// Wire the memory store for the RECALLED CONTEXT rider (mirrors
    /// `AbandonPlanTool::with_memory` / `FinishTool::with_memory`).
    pub fn with_memory(mut self, store: Arc<dyn MemoryStoreTrait>) -> Self {
        self.memory = Some(store);
        self
    }

    /// Inject a custom [`GitRunner`] (tests use a mock; production uses
    /// [`RealGit`] via [`new`](Self::new)).
    pub fn with_git(mut self, git: Arc<dyn GitRunner + Send + Sync>) -> Self {
        self.git = git;
        self
    }
}

#[async_trait]
impl Tool for CreatePlanTool {
    fn name(&self) -> &str {
        "create_plan"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "create_plan",
            "Create a plan (title, goal, context, ordered steps) and enter Executing. Use \
             before writing any code; available in every state. The plan file is the \
             CRASH-RESUMPTION document — bug + context + steps together must carry \
             everything a fresh session needs to resume mid-work. Called while a plan is \
             executing, it pushes a SUB-PLAN: it becomes active and the parent resumes where \
             it left off when it completes (sub-plans always pop to the parent regardless of \
             `kind`); abandon_plan discards the active plan back to the parent. For a VERY \
             long plan, create it with the first few steps and extend via update_plan with \
             append=true — several small calls instead of one huge body.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "A short title for the plan. MUST be non-blank (whitespace-only is rejected) — write the full title/goal/steps content in your reply FIRST, then emit the call carrying it; the call body is never where content gets drafted."},
                    "goal": {"type": "string", "description": "What we're building. MUST be non-blank (whitespace-only is rejected)."},
                    "context": {"type": ["string", "null"], "description": "Relevant findings from the planning phase. MUST be substantive (≥40 chars): symptom/root-cause, file anchors, verification commands."},
                    "steps": {
                        "type": "array",
                        "items": {
                            "oneOf": [
                                {"type": "string"},
                                {"type": "object", "properties": {"header": {"type": "string"}, "body": {"type": "string"}}, "required": ["header"]}
                            ]
                        },
                        "description": "Ordered steps. Each is a plain string ('**bold header** — body') or an object {header, body}, normalized to that same form; an object's header is PLAIN text (** markers are stripped, leaving exactly one bold layer). The header is the one-liner shown in the executing toolbar. The body is a self-contained recipe a less-powerful model executes alone: explicit file paths (not 'the relevant file'), explicit actions (not 'handle the case'), and any context it needs — never assume it remembers an earlier step. Keep it to a few lines. Example: '**Add ask_user tool** — create src/tool/workflow/ask_user.rs with schema {question, options:[{label,description?}]}, SafetyLevel::AutoRun, category Workflow. Register it in factory.rs register_workflow_tools.' For kind=bug_fixing the locked skeleton is the checklist; provided steps persist as CHECKABLE '## Detailed steps' sub-items (tick each via complete_step detailed_step_index as it ships — the crash-resumption detail). Resumability gate: every step names file path(s) or a no-code marker."
                    },
                    "kind": {
                        "type": ["string", "null"],
                        "enum": ["implementation", "research", "bug_fixing"],
                        "description": "Decides what happens when the ROOT plan's last step completes. 'implementation' (default) enters Reviewing — a code review before Complete. 'research' skips review, straight to Complete — only for work producing no source-code changes (its own .coding/ artifacts stay writable with the file tools; source writes are dispatch-denied, and a completed implementation/bug_fixing sub-plan forces this root's review). 'bug_fixing' is for CONTAINED bug fixes — never 'implementation' for a contained defect: locked reproduce→root-cause→fix→verify skeleton, requires `bug`, auto-captures a BUG: memory at finish; context must name the regression-test design; provided steps persist as checkable '## Detailed steps' sub-items (the crash-resumption detail). A bug-triggered FEATURE (the fix adds capabilities, new dependencies, or spans multiple modules) belongs in kind=implementation with the bug documented as motivation in goal/context."
                    },
                    "bug": {
                        "type": ["string", "null"],
                        "description": "The bug symptom — REQUIRED when kind=bug_fixing (errors without it). Ignored for other kinds."
                    },
                    "branch": {
                        "type": ["string", "null"],
                        "description": "Optional git working branch — pass this ONLY when the user explicitly requests a specific branch name. With NO `branch` arg, create_plan auto-forks/reuses a single per-directory wt/* working branch (forked from main when on main, reused when already on one) so work never lands on main; the agent never passes `branch` automatically. An existing branch is reused (resume flow); only a new name forks from `base`. Skipped with a note when the tree has changes outside .coding/ or the workflow is mid-executing."
                    },
                    "base": {
                        "type": ["string", "null"],
                        "description": "Optional branch to fork `branch` from. Default 'main'."
                    }
                },
                "required": ["title", "goal", "steps"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Creating a plan document is non-destructive (writes to the project's
        // own `.coding/plans/` dir, sandboxed) and is the prerequisite for all
        // further work. It must never block on approval — otherwise the agent
        // is stuck unable to act.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: CreatePlanArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Non-blank content guard: serde enforces PRESENCE of title/goal but a
        // whitespace-only string deserializes fine — and a blank title/goal is
        // the signature of a call emitted before its content was drafted (the
        // content-first rule). Reject with an actionable error instead of
        // persisting an empty-headed plan.
        if args.title.trim().is_empty() {
            return ToolResult::error(
                "title must be non-blank — a create_plan with an empty/blank title \
                 means the plan content was not written; draft title/goal/steps in \
                 your reply FIRST, then emit the call carrying that exact text",
            );
        }
        if args.goal.trim().is_empty() {
            return ToolResult::error(
                "goal must be non-blank — a create_plan with an empty/blank goal \
                 means the plan content was not written; draft title/goal/steps in \
                 your reply FIRST, then emit the call carrying that exact text",
            );
        }
        // BugFixing plans: the bug symptom is REQUIRED and the 4-step
        // skeleton is FORCED (the bug protocol is not negotiable). The
        // symptom is collapsed to a single line before persisting: the
        // plan file's `## Bug` section is one line, and an embedded
        // newline would re-parse as a section header or truncate on
        // reload (section injection — see the plan_file parser).
        // Backlog 77ff8f45: the caller's detailed steps are NOT discarded
        // — they persist as the `## Detailed steps` section (the
        // crash-resumption detail) alongside the locked skeleton, so a
        // restarted session resumes with the full recipe instead of
        // re-deriving it from the bug + context alone.
        let (steps, bug_symptom, detailed_steps, detailed_step_count, feature_scale) =
            if args.kind == PlanKind::BugFixing {
                let symptom =
                    match args.bug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        Some(s) => s,
                        None => {
                            return ToolResult::error(
                                "kind=bug_fixing requires the bug param (the symptom) — \
                                 bugs always use kind=bug_fixing with bug: '<symptom>'",
                            )
                        }
                    };
                let symptom = collapse_whitespace(symptom);
                let detail_texts = if args.steps.is_empty() {
                    Vec::new()
                } else {
                    steps_to_text(args.steps)
                };
                let detailed_step_count = detail_texts.len();
                // Backlog 51ee41c1: steer bug-triggered FEATURES to
                // implementation plans. Advisory only — a note, never a
                // blocking question (unattended run-alls must not stall):
                // the payload looks feature-scale when it BOTH carries
                // more detailed steps than the locked 4-step skeleton AND
                // spans ≥3 modules, because the locked skeleton + BUG:
                // auto-capture mislabel features (cf. plan c87093c1).
                let feature_scale = detail_texts.len() > 4 && {
                    let mut texts: Vec<&str> =
                        detail_texts.iter().map(String::as_str).collect();
                    texts.push(args.context.as_str());
                    texts.push(args.goal.as_str());
                    referenced_module_dirs(&texts) >= 3
                };
                let detailed_steps = if detail_texts.is_empty() {
                    None
                } else {
                    let body = detail_texts
                        .into_iter()
                        .map(|s| format!("- [ ] {s}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    // A rendered step line always starts `- [ ] ` — any
                    // OTHER `- `-leading line is a continuation of a
                    // multi-line step text (e.g. a nested list). Escape it
                    // so the tick's line-oriented counting can't mistake it
                    // for a checkable sub-step (review L3): the leading
                    // backslash defuses the bullet exactly the way
                    // escape_section_markers defuses a `## ` marker, and
                    // the raw body round-trips it verbatim.
                    let body = body
                        .lines()
                        .map(|l| {
                            if l.starts_with("- [ ] ") {
                                l.to_string()
                            } else {
                                let trimmed = l.trim_start();
                                if trimmed.starts_with("- ") {
                                    let indent_len = l.len() - trimmed.len();
                                    let (indent, rest) = l.split_at(indent_len);
                                    format!("{indent}\\{rest}")
                                } else {
                                    l.to_string()
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    // Review L3 (backlog 77ff8f45): escape lines that
                    // would re-parse as plan-file structure — a step
                    // quoting the plan format must not corrupt the
                    // re-parsed plan.
                    Some(escape_section_markers(&body))
                };
                (
                    BUG_FIXING_SKELETON.iter().map(|s| s.to_string()).collect(),
                    Some(symptom),
                    detailed_steps,
                    detailed_step_count,
                    feature_scale,
                )
            } else {
                (steps_to_text(args.steps), None, None, 0, false)
            };
        if steps.is_empty() {
            return ToolResult::error("plan must have at least one step");
        }
        // Plan resumability gate (2027-01-09): a plan too thin to resume
        // from after a recompile+restart is rejected here with one
        // actionable error naming everything missing — the plan file is the
        // crash-resumption document, so the gate enforces what a fresh
        // session needs (substantive context, per-step file anchors, the
        // regression-test design for bug plans). Runs after the shape
        // checks above so those keep their specific errors.
        let issues = validate_plan_resumability(
            &args.context,
            &steps,
            args.kind,
            detailed_steps.as_deref(),
        );
        if !issues.is_empty() {
            return ToolResult::error(resumability_gate_error(
                "plan rejected by the resumability gate",
                &issues,
            ));
        }
        let mut wf = self.workflow.lock().await;
        if !wf.plan_mutations_allowed() {
            return ToolResult::error(
                "create_plan is restricted to the main agent under the multi-agent plan ownership policy"
            );
        }
        // Optional branch fork BEFORE the plan is persisted (see the schema).
        // Deliberately run while holding the workflow lock: plan mutations
        // already serialize on it (main agent only), and keeping it held
        // guarantees no other plan write can land between the branch switch
        // and this plan's persistence.
        let branch_note = match (&args.branch, wf.state()) {
            // A sub-plan (or skill) created mid-execution must never move
            // branches — the parent plan's in-flight work lives on this one.
            // This holds for BOTH an explicit `branch` arg (ignored with a
            // note) and the auto-fork (silently skipped — the parent's branch
            // IS the one-branch-per-directory reuse).
            (Some(branch), WorkflowState::Executing | WorkflowState::Skill) => Some(format!(
                "git: branch '{branch}' ignored — the workflow is mid-executing \
                 (sub-plan); create the branch manually after this plan completes"
            )),
            (None, WorkflowState::Executing | WorkflowState::Skill) => None,
            (Some(branch), _) => {
                // An EXPLICIT branch request (the user asked for a specific
                // branch). wt/* branches fork straight from `main`; an
                // existing branch is reused (the resume flow), only new names
                // fork. The agent never passes `branch` automatically —
                // run_all relies on the auto-fork below instead.
                let base = args.base.clone().unwrap_or_else(|| "main".to_string());
                // A bad name is a hard error BEFORE the plan is created — the
                // agent retries with a fixed name instead of orphaning a plan
                // on the wrong branch.
                if !valid_branch_name(branch) || !valid_branch_name(&base) {
                    return ToolResult::error(format!(
                        "invalid branch/base name (branch: '{branch}', base: '{base}') — \
                         must be non-empty, must not start with '-', and contain no whitespace"
                    ));
                }
                match wf.project_root() {
                    None => Some(
                        "git: branch prep skipped — the plans dir does not imply a project \
                         root; create the feature branch manually"
                            .to_string(),
                    ),
                    Some(root) => {
                        match prepare_branch_with(self.git.clone(), root, branch.clone(), base)
                            .await
                        {
                            Ok(outcome) => Some(outcome.describe()),
                            Err(reason) => Some(format!(
                                "git: branch prep skipped — {reason}; the plan is created on the \
                             current branch (create the feature branch manually)"
                            )),
                        }
                    }
                }
            }
            // Auto-fork: one branch per agent directory. With no explicit
            // branch requested, ensure the directory is on its single stable
            // wt/* working branch — fork it from main when on main, reuse the
            // current branch when already on one. Ok(None) (not a git repo, or
            // a branch that could not be probed) is a silent skip — planning
            // is never blocked on git.
            (None, _) => match wf.project_root() {
                None => None,
                Some(root) => match ensure_work_branch_with(self.git.clone(), root).await {
                    Ok(None) => None,
                    Ok(Some(outcome)) => Some(outcome.describe()),
                    Err(reason) => Some(format!(
                        "git: auto-branch skipped — {reason}; the plan is created on the \
                         current branch (create the feature branch manually)"
                    )),
                },
            },
        };
        let mut result = match wf.create_plan_with_kind_and_detail(
            &args.title,
            &args.goal,
            &args.context,
            steps.clone(),
            args.kind,
            bug_symptom.as_deref(),
            detailed_steps.as_deref(),
        ) {
            Ok(id) => {
                let mut output = format!(
                    "created plan '{title}' with {n} steps (id: {id}, kind: {kind})",
                    title = args.title,
                    n = steps.len(),
                    id = id,
                    kind = args.kind
                );
                if let Some(note) = &branch_note {
                    output.push_str(&format!(" [{note}]"));
                }
                // Backlog 77ff8f45: state plainly what happened to the
                // caller's steps — the plan file is the crash-resumption
                // document, and the caller must not discover the step
                // handling only after a crash.
                if detailed_step_count > 0 {
                    output.push_str(&format!(
                        " — kind=bug_fixing locks the 4-step skeleton; your {m} detailed \
                         steps persist as CHECKABLE sub-items under '## Detailed steps' \
                         (tick each via complete_step detailed_step_index as it ships, so a \
                         restart shows exactly what landed)",
                        m = detailed_step_count
                    ));
                }
                if feature_scale {
                    // Backlog 51ee41c1: the advisory steering note — the
                    // payload looks like a bug-triggered FEATURE, which
                    // belongs in kind=implementation with the bug as
                    // motivation (the locked skeleton + BUG: auto-capture
                    // mislabel features). Non-gating: the caller judges.
                    output.push_str(
                        " — NOTE (backlog 51ee41c1): this bug_fixing payload looks \
                         FEATURE-SCALE (more detailed steps than the locked skeleton, \
                         spanning multiple modules — new capabilities/dependencies?): \
                         re-file it as kind=implementation with the bug documented as \
                         motivation in goal/context — bug_fixing is for contained \
                         defect fixes. This is a NOTE, not a gate.",
                    );
                }
                ToolResult {
                    success: true,
                    output,
                    data: Some(json!({
                        "plan_id": id,
                        "step_count": steps.len(),
                        "kind": args.kind,
                        "branch_note": branch_note,
                        "detailed_step_count": detailed_step_count,
                    })),
                }
            }
            Err(e) => ToolResult::error(format!("failed to create plan: {e}")),
        };
        // The RECALLED CONTEXT rider does store round-trips (recall_peek —
        // embedding, potentially a REMOTE embedder call) and needs NOTHING
        // under the workflow lock: the plan is already persisted. Drop the
        // guard BEFORE awaiting it — the codebase rule is to never hold the
        // workflow mutex across store round-trips (see abandon_plan's
        // rationale for the same shape). Unlike the branch-prep awaits
        // above (order-critical, held deliberately), the rider has no
        // reason to keep the lock.
        drop(wf);
        if result.success {
            if let Some(rider) = recall_rider(
                self.memory.as_ref(),
                &args.title,
                &args.goal,
                bug_symptom.as_deref(),
            )
            .await
            {
                result.output.push_str(&rider);
            }
        }
        result
    }
}

/// Best-effort passive recall riding the create_plan result (see
/// [`CreatePlanTool`]'s `memory` field). Delegates to the shared
/// [`crate::tool::steering::recalled_context_block`] — `spawn_agent` rides
/// the same block onto spawned agents' first prompts — with the
/// create_plan query (title + goal, + symptom for bug plans). Returns
/// `None` on no store, no hits, or any error — the rider must never block
/// or fail plan creation; the shared builder holds the recall_peek-only
/// (passive, no access bump) contract.
async fn recall_rider(
    store: Option<&Arc<dyn MemoryStoreTrait>>,
    title: &str,
    goal: &str,
    bug_symptom: Option<&str>,
) -> Option<String> {
    let query = match bug_symptom {
        Some(symptom) => format!("{title} {goal} {symptom}"),
        None => format!("{title} {goal}"),
    };
    crate::tool::steering::recalled_context_block(store, Some(title), &query, bug_symptom).await
}

/// The `update_plan` workflow tool.
///
/// Edits the active plan in place — the non-destructive alternative to
/// abandoning and re-planning. Completed steps are preserved; remaining
/// (not-yet-done) steps can be replaced and/or new steps appended.
pub struct UpdatePlanTool {
    workflow: Arc<Mutex<Workflow>>,
}

impl UpdatePlanTool {
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self { workflow }
    }
}

#[async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "update_plan",
            "Edit the active plan in place WITHOUT abandoning it — use this to \
             adjust a plan whose scope changed, and use it whenever the plan is \
             still broadly correct (abandon_plan is a destructive last resort). Completed \
             steps are preserved verbatim; only the remaining steps can be replaced or \
             appended to. Omit a field to leave it unchanged. Executing and Reviewing \
             (full field set in both for the main agent — the reviewer never sees this \
             tool; complete_step stays hidden mid-review, so appended steps stay \
             unchecked through the exit). bug_fixing \
             plans have a LOCKED skeleton (steps cannot be replaced; appends are allowed \
             only in the Reviewing state — follow-on fix work in the closing sequence) — record \
             the verify step's test name via regression_test, and landed_design=true when \
             the fix turned out feature-scale (the finish gate then requires a 'Landed \
             design' context amendment).",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": ["string", "null"], "description": "New title (omit or empty to keep current)."},
                    "goal": {"type": ["string", "null"], "description": "New goal (omit or empty to keep current)."},
                    "context": {"type": ["string", "null"], "description": "New context (omit or empty to keep current); with append=true it is EXTENDED instead of replaced. The gate checks the effective post-update text (≥40 chars)."},
                    "regression_test": {"type": ["string", "null"], "description": "The regression test name recorded by a bug_fixing plan's verify step (finish is blocked without it; validated against the code graph)."},
                    "landed_design": {"type": "boolean", "description": "Set true when a bug_fixing fix turned out feature-scale (new pub types / cross-module surface) — the finish gate then requires a 'Landed design' context amendment (agent.md 'Documentation expectations')."},
                    "steps": {
                        "type": "array",
                        "items": {
                            "oneOf": [
                                {"type": "string"},
                                {"type": "object", "properties": {"header": {"type": "string"}, "body": {"type": "string"}}, "required": ["header"]}
                            ]
                        },
                        "description": "Replacement for the remaining (not-yet-completed) steps; \
                            completed steps are prepended automatically. Same shape as \
                            create_plan's steps. More entries than remain appends work, \
                            fewer trims. Must be non-empty; omit to keep current steps. \
                            With append=true the entries are ADDED AFTER the remaining \
                            steps (they need not be resent) — the chunked protocol for \
                            very long plans: create_plan with the first few steps, then \
                            update_plan(append=true) per chunk. Resumability gate: every \
                            step names file path(s) or a no-code marker."
                    },
                    "append": {
                        "type": "boolean",
                        "description": "Default false. When true, steps are appended after the remaining ones (not replacing them) and context is appended (not replaced). For bug_fixing plans, appends are allowed only in the Reviewing state (follow-on fix work in the closing sequence); steps replacement is always refused (locked skeleton)."
                    }
                }
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Adjusting the active plan in place is non-destructive (rewrites the
        // sandboxed plan doc, preserves completed steps, keeps the same plan
        // id) — the same posture as create_plan. Keep it frictionless so the
        // agent prefers this over the destructive abandon_plan + re-plan path.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: UpdatePlanArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Normalize map steps into the stored string form (same as create_plan).
        let steps = args.steps.map(steps_to_text);
        let mut wf = self.workflow.lock().await;
        if !wf.plan_mutations_allowed() {
            return ToolResult::error(
                "update_plan is restricted to the main agent under the multi-agent plan ownership policy"
            );
        }
        // Plan resumability gate (2027-01-09): the same bar as create_plan,
        // applied to what this call writes. New steps are checked per-step
        // (bug_fixing plans are exempt — the engine's locked-skeleton
        // refusal owns that path — EXCEPT appended follow-on steps in the
        // Reviewing window, backlog 37f8631a, which are checked like any
        // other step); a context write is checked as the
        // EFFECTIVE post-update text (replace → the new text; append → the
        // existing context plus the new text), so a short hardening chunk
        // appended onto a substantive context passes while a thin
        // replacement fails. Title-only / regression_test-only calls are
        // ungated.
        let active = wf.plan();
        let kind = active.map(|p| p.kind).unwrap_or(PlanKind::Implementation);
        // The bug-skeleton exemption flips off in the Reviewing append
        // window (backlog 37f8631a): appended follow-on steps are real
        // checklist items (they surface as the active step after a crash),
        // so they meet the same path-or-marker resumability bar as every
        // other step. Everywhere else the engine's lock refuses bug-plan
        // steps anyway — the exemption stands.
        let reviewing_append = args.append && wf.state() == WorkflowState::Reviewing;
        let mut issues = Vec::new();
        if let Some(new_steps) = &steps {
            if let Some(issue) =
                step_issue(new_steps, kind == PlanKind::BugFixing && !reviewing_append)
            {
                issues.push(issue);
            }
        }
        if let Some(new_ctx) = args.context.as_deref().filter(|c| !c.trim().is_empty()) {
            // Mirror the engine's append semantics (the separator is only
            // inserted when the existing context is non-blank — an append
            // onto an empty context REPLACES), so the simulated text is
            // exactly what the engine will persist (review L3).
            let existing = active.map(|p| p.context.as_str()).unwrap_or("");
            let effective = if args.append && !existing.trim().is_empty() {
                format!("{existing}\n\n{new_ctx}")
            } else {
                new_ctx.to_string()
            };
            let detailed = active.and_then(|p| p.detailed_steps.clone());
            if let Some(issue) = context_issue(&effective, kind, detailed.as_deref()) {
                issues.push(issue);
            }
        }
        if !issues.is_empty() {
            return ToolResult::error(resumability_gate_error(
                "update rejected by the resumability gate",
                &issues,
            ));
        }
        let before = wf.plan().map(|p| p.steps.len()).unwrap_or(0);
        // Captured before the call — `steps` is moved into update_plan, so
        // the Ok arm cannot borrow it (the changes enumeration needs the
        // count).
        let steps_len = steps.as_ref().map(|s| s.len());
        match wf.update_plan(
            args.title.as_deref(),
            args.goal.as_deref(),
            args.context.as_deref(),
            steps,
            args.append,
            args.regression_test.as_deref(),
            args.landed_design,
        ) {
            Ok(()) => {
                let plan = wf.plan().unwrap();
                let completed = plan.completed_count();
                let total = plan.steps.len();
                let title = plan.title.clone();
                let plan_id = wf.plan_id().map(str::to_string).unwrap_or_default();
                // Self-describing result (backlog d779060a): enumerate WHAT
                // changed so the transcript alone shows the amendment's
                // effect — the call site knows which fields were passed.
                // update_plan errors on a no-op, so ≥1 change always lands.
                let mut changes: Vec<String> = Vec::new();
                // Mirror the engine's blank filter (blank = "keep current",
                // silently skipped) so the enumeration never claims a change
                // the engine didn't apply (review round 1, finding 2).
                if args
                    .title
                    .as_deref()
                    .is_some_and(|t| !t.trim().is_empty())
                {
                    changes.push("title set".into());
                }
                if args
                    .goal
                    .as_deref()
                    .is_some_and(|g| !g.trim().is_empty())
                {
                    changes.push("goal set".into());
                }
                if let Some(ctx) = args
                    .context
                    .as_deref()
                    .filter(|c| !c.trim().is_empty())
                {
                    if args.append {
                        changes.push(format!(
                            "context appended (+{} chars)",
                            ctx.chars().count()
                        ));
                    } else {
                        changes.push(format!(
                            "context replaced ({} chars)",
                            ctx.chars().count()
                        ));
                    }
                }
                if let Some(new_steps_len) = steps_len {
                    if args.append {
                        changes.push(format!("steps appended (+{new_steps_len})"));
                    } else {
                        // The engine preserves the completed prefix, so the
                        // post-update total is completed + passed — report
                        // the true size transition, not the passed count
                        // (review round 1, finding 1).
                        changes.push(format!(
                            "steps replaced ({before} → {total} steps)"
                        ));
                    }
                }
                if let Some(rt) = args
                    .regression_test
                    .as_deref()
                    .filter(|r| !r.trim().is_empty())
                {
                    changes.push(format!("regression_test={rt}"));
                }
                if let Some(ld) = args.landed_design {
                    changes.push(format!("landed_design={ld}"));
                }
                // The id is echoed so the model always holds the current
                // handle without an extra current_plan call.
                ToolResult {
                    success: true,
                    output: format!(
                        "updated plan '{title}' (id: {plan_id}): {changes} — {completed}/{total} done — state: {state}",
                        changes = changes.join(", "),
                        state = wf.state(),
                    ),
                    data: Some(json!({
                        "title": title,
                        "plan_id": plan_id,
                        "completed": completed,
                        "total": total,
                        "before": before,
                        "state": wf.state().to_string(),
                    })),
                }
            }
            Err(e) => ToolResult::error(format!("failed to update plan: {e}")),
        }
    }
}

/// The `complete_step` workflow tool.
/// The plan_id guard shared by both complete_step paths (skeleton steps
/// and detailed sub-steps, backlog 9441d776): when provided, verify it
/// matches the active plan's id — forgiving of a ≥4-char unique PREFIX
/// (accepted with a note) — else error with a did-you-mean hint (parent
/// plan / older plan file) instead of a bare error. Returns the
/// prefix-match note ("" when the id matched exactly).
fn complete_step_plan_id_guard(
    wf: &Workflow,
    provided_id: &str,
    what: &str,
) -> Result<String, String> {
    let provided_id = provided_id.trim();
    let active_id = wf.plan_id();
    let matches_active = active_id == Some(provided_id)
        || (provided_id.len() >= 4 && active_id.is_some_and(|a| a.starts_with(provided_id)));
    if !matches_active {
        let active_title = wf.plan().map(|p| p.title.clone()).unwrap_or_default();
        let hint = plan_id_hint(provided_id, wf.plan_stack(), wf.plans_dir())
            .map(|h| format!("{h} "))
            .unwrap_or_default();
        return Err(format!(
            "complete_step plan_id mismatch: {what} is in plan '{provided_id}', but the \
             active plan is '{active_title}' (id: {}). {hint}Call current_plan to confirm \
             the active plan, or omit plan_id to complete against the active plan.",
            active_id.unwrap_or("(none)")
        ));
    }
    if active_id != Some(provided_id) {
        return Ok(format!(
            " (plan_id '{provided_id}' matched the active id by prefix)"
        ));
    }
    Ok(String::new())
}

pub struct CompleteStepTool {
    workflow: Arc<Mutex<Workflow>>,
}

impl CompleteStepTool {
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self { workflow }
    }
}

#[async_trait]
impl Tool for CompleteStepTool {
    fn name(&self) -> &str {
        "complete_step"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "complete_step",
            "Mark a plan step as complete by its 1-indexed step_index (1 = the first step; \
             matches the numbers shown in the plan document and in this tool's result). \
             Checks off the step on the active plan and transitions to Complete when all \
             steps are done. The result names the active plan so you stay oriented. Pass \
             plan_id (the id returned by create_plan / current_plan) to guard against \
             completing a step in the wrong stacked plan — it errors on a mismatch. Omit \
             plan_id to complete against the active plan. For bug_fixing plans, pass \
             detailed_step_index instead to tick a '## Detailed steps' sub-item's checkbox \
             (1-indexed) — sub-step progress only; it never completes the plan or changes \
             the workflow state.",
            json!({
                "type": "object",
                "properties": {
                    "step_index": {"type": "integer", "description": "The 1-indexed step number to mark complete (1 = the first step; matches the plan document's numbering). A numeric string is also accepted. Exactly one of step_index / detailed_step_index is required."},
                    "detailed_step_index": {"type": "integer", "description": "The 1-indexed '## Detailed steps' sub-item to tick (bug_fixing plans) — records sub-step progress without touching the skeleton steps or the workflow state. A numeric string is also accepted."},
                    "plan_id": {"type": "string", "description": "Optional: the id of the plan this step belongs to; errors on a mismatch with the active plan."}
                },
                "required": []
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Checking off a step only rewrites the project's own `.coding/plans/`
        // bookkeeping (sandboxed, non-destructive) — never prompts.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: CompleteStepArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Exactly one of step_index / detailed_step_index (backlog
        // 9441d776): the skeleton-step tick or the detailed sub-step tick.
        let has_step = args.step_index.is_some();
        match (has_step, args.detailed_step_index.is_some()) {
            (true, true) => {
                return ToolResult::error(
                    "pass either step_index or detailed_step_index, not both — \
                     step_index ticks a skeleton step, detailed_step_index ticks a \
                     '## Detailed steps' sub-item",
                )
            }
            (false, false) => {
                return ToolResult::error(
                    "complete_step requires step_index (the 1-indexed skeleton step) \
                     or detailed_step_index (the 1-indexed '## Detailed steps' sub-item)",
                )
            }
            _ => {}
        }
        // The model-facing numbers are 1-indexed (1 = the first — matching
        // the plan document, PlanProgress, and the echo below). The skeleton
        // path converts to the 0-indexed index the workflow layer stores.
        let step_number = match args.step_index {
            Some(s) => match s.parse() {
                Ok(n) => n,
                Err(msg) => return ToolResult::error(msg),
            },
            None => 0,
        };
        if has_step && step_number == 0 {
            return ToolResult::error(
                "step_index is 1-indexed: step numbers start at 1 (1 = the \
                 first step), got 0",
            );
        }
        let detailed_number = match args.detailed_step_index {
            Some(d) => match d.parse() {
                Ok(n) => n,
                Err(msg) => return ToolResult::error(msg),
            },
            None => 0,
        };
        if !has_step && detailed_number == 0 {
            return ToolResult::error(
                "detailed_step_index is 1-indexed: sub-step numbers start at 1 \
                 (1 = the first '## Detailed steps' bullet), got 0",
            );
        }
        let step_index = step_number.saturating_sub(1) as usize;
        let mut wf = self.workflow.lock().await;
        if !wf.plan_mutations_allowed() {
            return ToolResult::error(
                "complete_step is restricted to the main agent under the multi-agent plan ownership policy"
            );
        }
        // plan_id guard: when provided, verify it matches the active plan's
        // id. This catches a wrong-plan completion (the agent completing a step
        // in a stacked plan that isn't the active one) — a silent miscount
        // risk when two stacked plans share an index range. The match is
        // forgiving of transcription noise: a provided id that is a unique
        // PREFIX of the active id (≥4 chars) is accepted with a note, and a
        // mismatch gets a did-you-mean hint (parent plan / older plan file)
        // instead of a bare error.
        let mut plan_id_note = String::new();
        if let Some(provided_id) = &args.plan_id {
            let what = if has_step {
                format!("step {step_number}")
            } else {
                format!("detailed step {detailed_number}")
            };
            match complete_step_plan_id_guard(&wf, provided_id, &what) {
                Ok(note) => plan_id_note = note,
                Err(err) => return ToolResult::error(err),
            }
        }
        // The detailed sub-step tick (backlog 9441d776): flips the nth
        // `## Detailed steps` checkbox on the active plan — sub-step
        // progress only, NEVER a workflow-state transition.
        if !has_step {
            return match wf.complete_detailed_step(detailed_number as usize) {
                Ok(summary) => {
                    let title = wf.plan().map(|p| p.title.clone()).unwrap_or_default();
                    let id = wf.plan_id().map(str::to_string).unwrap_or_default();
                    ToolResult {
                        success: true,
                        output: format!(
                            "Detailed step {detailed_number} ticked ({summary}) — \
                             plan '{title}' (id: {id}){plan_id_note}"
                        ),
                        data: Some(json!({
                            "detailed_step": detailed_number,
                            "summary": summary,
                            "plan_id": id,
                        })),
                    }
                }
                Err(e) => ToolResult::error(format!("failed to tick detailed step: {e}")),
            };
        }
        // Pre-validate the range in the model-facing 1-indexed convention
        // (review L1): PlanFile's internal error would echo the converted
        // 0-indexed value — one less than the number the model passed, which
        // can look like it lies INSIDE the stated valid range.
        if let Some(plan) = wf.plan() {
            let len = plan.steps.len();
            if step_index as usize >= len {
                return ToolResult::error(format!(
                    "step {step_number} out of range (plan has {len} steps; step \
                     numbers are 1-indexed, valid 1..={len})"
                ));
            }
        }
        // Capture the active plan's id BEFORE completing — a sub-plan's last
        // step pops the stack, so plan_id() afterwards is the parent's.
        let active_plan_id = wf.plan_id().map(str::to_string);
        // Detect out-of-order completion BEFORE the call: completing a step
        // after the first not-done one leaves a done step behind an undone
        // one — update_plan later REFUSES to replace the remaining steps in
        // that state, so warn now, at the cause (not at the far-away failure).
        let first_unchecked = wf.plan().and_then(|p| p.steps.iter().position(|s| !s.done));
        let out_of_order = first_unchecked.is_some_and(|split| step_index as usize > split);
        // Capture the completed step's text BEFORE the call too (review
        // round 1, finding 3): after a sub-plan's last-step pop the plan is
        // the PARENT's, so a post-call read would name the wrong work item.
        let step_text = wf
            .plan()
            .and_then(|p| p.steps.get(step_index as usize))
            .map(|s| {
                let text = &s.text;
                if text.chars().count() > 80 {
                    let mut t: String = text.chars().take(80).collect();
                    t.push('…');
                    t
                } else {
                    text.clone()
                }
            })
            .unwrap_or_default();
        match wf.complete_step(step_index as usize) {
            Ok(()) => {
                let state = wf.state();
                let plan = wf.plan();
                let completed = plan.map(|p| p.completed_count()).unwrap_or(0);
                let total = plan.map(|p| p.steps.len()).unwrap_or(0);
                let title = plan.map(|p| p.title.clone()).unwrap_or_default();
                // step_text was captured BEFORE the call (a sub-plan's
                // last-step pop swaps the plan for the parent's).
                ToolResult {
                    success: true,
                    output: format!(
                        "Complete Step {n} '{step_text}' ({completed}/{total}, {title}) — state: {state}{plan_id_note}{out_of_order_warning}",
                        // step_number is already the 1-indexed display form —
                        // the same numbers the plan document + PlanProgress
                        // view show.
                        n = step_number,
                        out_of_order_warning = if out_of_order {
                            "\nwarning: completed out of order — update_plan will refuse \
                             to replace the remaining steps until steps are completed \
                             in order"
                        } else {
                            ""
                        },
                    ),
                    data: Some(json!({
                        "completed": completed,
                        "total": total,
                        "state": state.to_string(),
                        "plan_title": title,
                        "plan_id": active_plan_id,
                        "out_of_order": out_of_order,
                    })),
                }
            }
            Err(e) => ToolResult::error(format!("failed to complete step: {e}")),
        }
    }
}

/// Build the did-you-mean hint for a `plan_id` that does not match the
/// active plan. Two resemblance sources are checked, in order:
/// 1. the plan stack — a truncated or hedged transcription of a PARENT (or
///    other stacked) plan id, the classic stacked-plan slip;
/// 2. the plans directory — an id belonging to an OLDER, finished plan.
/// Returns `None` when nothing resembles the provided id (the caller then
/// falls back to the generic "call current_plan" advice).
fn plan_id_hint(
    provided: &str,
    stack: &[crate::workflow::PlanFrame],
    plans_dir: &std::path::Path,
) -> Option<String> {
    // 1. Another plan on the stack (exact or prefix match, either direction).
    // The ACTIVE frame (the stack's last) is EXCLUDED — the guard already
    // rejected an active match, and claiming "not the active plan" for the
    // active plan's own id would be factually wrong (review L2: the over-long
    // transcription edge, a provided id that starts with the active id).
    for frame in &stack[..stack.len().saturating_sub(1)] {
        let id = frame.id.as_str();
        let resembles = id == provided
            || (provided.len() >= 4 && id.starts_with(provided))
            || (id.len() >= 4 && provided.starts_with(id));
        if resembles {
            return Some(format!(
                "'{provided}' is the id of plan '{}' on the plan stack — that is not \
                 the active plan.",
                frame.plan.title
            ));
        }
    }
    // 2. An older plan file under .coding/plans/.
    let mut stems: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(plans_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    stems.push(stem.to_string());
                }
            }
        }
    }
    if let Some(stem) = stems.iter().find(|s| s.as_str() == provided) {
        return Some(format!(
            "'{provided}' is an older plan file ('{stem}.md') — not the active plan."
        ));
    }
    if provided.len() >= 4 {
        let prefix_matches: Vec<&String> =
            stems.iter().filter(|s| s.starts_with(provided)).collect();
        if prefix_matches.len() == 1 {
            let stem = prefix_matches[0];
            return Some(format!(
                "'{provided}' matches older plan file '{stem}' — not the active plan."
            ));
        }
    }
    None
}

/// The `abandon_plan` workflow tool.
///
/// Pops the active plan off the stack without completing it and resumes the
/// parent plan (or returns to Planning if the stack empties). The escape hatch
/// for a stale/wrong plan.
pub struct AbandonPlanTool {
    workflow: Arc<Mutex<Workflow>>,
    /// The shared memory store — when wired, abandoning a plan supersedes the
    /// plan's lingering ACTIVE:/STEP MARKER: crash markers (Phase 4 hygiene:
    /// a plan that stopped being the active plan must stop recalling as
    /// "resume me"). `None` (tests / no store) skips the supersede.
    memory: Option<Arc<dyn MemoryStoreTrait>>,
}

impl AbandonPlanTool {
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self {
            workflow,
            memory: None,
        }
    }

    /// Wire the shared memory store so `abandon_plan` supersedes the
    /// abandoned plan's lingering crash markers. Omitted when no store is
    /// wired.
    pub fn with_memory(mut self, store: Arc<dyn MemoryStoreTrait>) -> Self {
        self.memory = Some(store);
        self
    }
}

#[async_trait]
impl Tool for AbandonPlanTool {
    fn name(&self) -> &str {
        "abandon_plan"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "abandon_plan",
            "Abandon the active plan WITHOUT completing it: pop it off the plan stack and \
             resume the parent plan (or return to Planning if none remains). DESTRUCTIVE LAST \
             RESORT. Use update_plan whenever the plan is still broadly \
             correct (it edits the active plan in place, preserving completed steps). Only use \
             abandon_plan when the plan is fundamentally wrong or stale and can't be salvaged \
             by editing. The plan file stays on disk for reference.",
            json!({
                "type": "object",
                "properties": {},
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Pops the active plan off the stack and rewrites the project's own
        // `.coding/plans/` bookkeeping (sandboxed; the plan file stays on
        // disk) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        // Snapshot-and-drop: the workflow lock is held ONLY for the
        // mutations + the plan-id snapshot — never across the supersede
        // awaits below (the codebase rule: never hold a lock across an
        // await; the supersede does store round-trips, and a concurrent
        // workflow-lock acquirer must not stall behind SQLite latency).
        let (abandoned, plan_id) = {
            let mut wf = self.workflow.lock().await;
            if !wf.plan_mutations_allowed() {
                return ToolResult::error(
                    "abandon_plan is restricted to the main agent under the multi-agent plan ownership policy"
                );
            }
            // Snapshot the ACTIVE plan's id BEFORE abandoning — after the pop,
            // plan_id() is the parent's (or None), and the marker supersede
            // must target the abandoned plan, not its parent.
            let plan_id = wf.plan_id().map(str::to_string);
            let title = match wf.abandon_plan() {
                Ok(title) => title,
                Err(e) => return ToolResult::error(format!("failed to abandon plan: {e}")),
            };
            (title, plan_id)
        };

        // Crash-marker supersede: an abandoned plan's ACTIVE:/STEP MARKER:
        // working-tier markers are stale — supersede them (never delete) so
        // they stop recalling as "resume me". Non-blocking best-effort: a
        // failure logs + notes in the output, never blocks the abandon
        // (mirrors finish's capture posture).
        let mut marker_note: Option<String> = None;
        if let (Some(store), Some(plan_id)) = (&self.memory, &plan_id) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let n = crate::memory::finish_capture::supersede_plan_markers(
                store.as_ref(),
                &plan_id,
                "ABANDONED",
                now,
            )
            .await;
            if n > 0 {
                marker_note = Some(format!(" — superseded {n} stale marker(s)"));
            }
        }

        let wf = self.workflow.lock().await;
        let state = wf.state();
        let depth = wf.plan_depth();
        let parent = wf.plan().map(|p| p.title.clone());
        let mut output = match &parent {
            Some(p) => format!(
                "abandoned plan '{abandoned}' — resumed parent '{p}' (depth {depth}, state: {state})"
            ),
            None => format!("abandoned plan '{abandoned}' — no parent plan (state: {state})"),
        };
        if let Some(note) = marker_note {
            output.push_str(&note);
        }
        ToolResult {
            success: true,
            output,
            data: Some(json!({
                "abandoned": abandoned,
                "parent": parent,
                "depth": depth,
                "state": state.to_string(),
            })),
        }
    }
}

/// Arguments for `finish`.
#[derive(Debug, Deserialize)]
struct FinishArgs {
    /// Path to the review report file under `.coding/reviews/`.
    review_report: String,
    /// Optional: the regression test name for bug_fixing plans. Recorded on
    /// the plan frame right before the gate check — an alternative to calling
    /// update_plan separately (the two paths exist because update_plan was
    /// historically blocked in Reviewing, creating a deadlock).
    #[serde(default)]
    regression_test: Option<String>,
}

/// The parsed review verdict — the report's first non-blank line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// `## Verdict: PASS` — the review found nothing blocking.
    Pass,
    /// `## Verdict: FINDINGS (n high, n low)` — findings must be fixed and
    /// the review re-run before finish.
    Findings,
    /// Anything else — an unparseable verdict counts as FINDINGS (fail
    /// closed): a report that can't be parsed must never pass.
    Unparseable,
}

/// Parse the verdict from a review report's first non-blank line.
fn parse_verdict(report: &str) -> Verdict {
    let first = report
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if first.starts_with("## Verdict: PASS") {
        Verdict::Pass
    } else if first.starts_with("## Verdict: FINDINGS") {
        Verdict::Findings
    } else {
        Verdict::Unparseable
    }
}

/// The `finish` workflow tool — the only path from `Reviewing` to `Complete`.
///
/// Closes out the review→fix→commit closing sequence. Gated on a non-empty
/// review report file under `.coding/reviews/` (written by a reviewer
/// subagent), so the review is unskippable by construction. `AutoRun` — it
/// only rewrites the project's own `.coding/` bookkeeping (the `reviewed` flag
/// in `stack.json`), never user code.
///
/// The reviews dir is an explicit field (not derived from the workflow's
/// `plans_dir()`) so a per-agent plans dir (`.coding/plans/agents/<id>/`)
/// doesn't redirect `finish` to `.coding/plans/agents/reviews/` — the
/// reviewer subagent always writes to `.coding/reviews/` (via
/// `WriteReviewReportTool`), and `finish` must look in the same place.
pub struct FinishTool {
    workflow: Arc<Mutex<Workflow>>,
    reviews_dir: PathBuf,
    /// The shared memory store — when wired, `finish` auto-captures the
    /// PLAN:/BUG: digests (Phase 4). `None` (tests / no store) skips the
    /// capture with a note.
    memory: Option<Arc<dyn MemoryStoreTrait>>,
    /// The knowledge-file backing — when wired, the BUG: capture lands in a
    /// knowledge FILE (the truth) instead of an authored row. `None`
    /// (tests / no store) keeps the historical authored-row capture.
    knowledge: Option<Arc<crate::memory::knowledge::KnowledgeStore>>,
    /// The shared code knowledge graph — when wired, the bug_fixing
    /// regression-test gate resolves the recorded test name against it.
    /// `None` (tests / no graph) skips the gate with a note.
    codegraph: Option<Arc<crate::codegraph::CodeGraph>>,
}

impl FinishTool {
    /// Create the tool bound to the workflow + the reviews directory
    /// (`.coding/reviews/`). The reviews dir MUST be the main-derived path
    /// (from the factory's `reviews_dir()`), NOT the per-agent workflow's
    /// `plans_dir().parent().join("reviews")`.
    pub fn new(workflow: Arc<Mutex<Workflow>>, reviews_dir: impl Into<PathBuf>) -> Self {
        Self {
            workflow,
            reviews_dir: reviews_dir.into(),
            memory: None,
            knowledge: None,
            codegraph: None,
        }
    }

    /// Wire the shared memory store so `finish` auto-captures the PLAN:/BUG:
    /// digests (Phase 4). Omitted when no store is wired.
    pub fn with_memory(mut self, store: Arc<dyn MemoryStoreTrait>) -> Self {
        self.memory = Some(store);
        self
    }

    /// Wire the knowledge-file backing so the BUG: capture lands in a
    /// knowledge file (the truth) instead of an authored row. Pass the same
    /// store the memory tools use; omitted when no store is wired.
    pub fn with_knowledge(
        mut self,
        knowledge: Arc<crate::memory::knowledge::KnowledgeStore>,
    ) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Wire the shared code knowledge graph so the bug_fixing regression-test
    /// gate can resolve the recorded test name. Omitted when no graph is
    /// wired (the gate then skips with a note).
    pub fn with_codegraph(mut self, graph: Arc<crate::codegraph::CodeGraph>) -> Self {
        self.codegraph = Some(graph);
        self
    }
}

#[async_trait]
impl Tool for FinishTool {
    fn name(&self) -> &str {
        "finish"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "finish",
            "The only path from Reviewing to Complete. Requires a non-empty review report \
             under .coding/reviews/ opening with \"## Verdict: PASS\" — a FINDINGS or \
             unparseable verdict blocks finish (fail closed). So: reviewer subagent writes \
             its report, you fix every finding and commit, then call this with the report \
             path. bug_fixing plans also need the regression test name recorded (either \
             via update_plan regression_test, or pass it directly here), the symbol \
             present in the code graph, and — when landed_design was recorded — a \
             'Landed design' context amendment (agent.md 'Documentation expectations'). \
             Auto-captures the PLAN:/BUG: digests and supersedes \
             the plan's crash markers.",
            json!({
                "type": "object",
                "properties": {
                    "review_report": {
                        "type": "string",
                        "description": "Path to the review report file under .coding/reviews/ (written by the reviewer subagent; must open with \"## Verdict: PASS\")."
                    },
                    "regression_test": {
                        "type": "string",
                        "description": "Optional: the regression test name for bug_fixing plans. Recorded on the plan frame right before the gate check — an alternative to calling update_plan separately (useful when the test name is only known after running tests at the end of the verify step)."
                    }
                },
                "required": ["review_report"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Only flips the persisted `reviewed` flag in the project's own
        // `.coding/plans/stack.json` sidecar (sandboxed, non-destructive to
        // user code) — never prompts for approval.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: FinishArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // Snapshot-and-drop: the workflow lock is held ONLY for the state
        // check + the plan snapshot — never across the capture awaits below
        // (the codebase rule: never hold a lock across an await; the capture
        // does multiple store round-trips, and a concurrent workflow-lock
        // acquirer must not stall behind SQLite latency).
        let (plan, plan_id, plans_dir, repo_root) = {
            let mut wf = self.workflow.lock().await;
            if wf.state() != WorkflowState::Reviewing {
                return ToolResult::error(format!(
                    "finish is only available in the Reviewing state (current state: {}). \
                     Complete all plan steps first.",
                    wf.state()
                ));
            }
            // Record the regression test if provided (relaxes the
            // finish↔update_plan deadlock: finish can accept the test name
            // directly instead of requiring a separate update_plan call that
            // was historically blocked in Reviewing). The workflow method
            // allows regression_test-only updates in Reviewing.
            if let Some(rt) = args.regression_test.as_deref() {
                if !rt.trim().is_empty() {
                    // Log (don't silently drop) errors: the gate below
                    // catches a missing test, but a real disk-write failure
                    // (the in-memory frame still gets the test, but the plan
                    // file isn't persisted) shouldn't be fully silent —
                    // mirrors the capture-failure logging below (plan.rs:1375).
                    if let Err(e) =
                        wf.update_plan(None, None, None, None, false, Some(rt), None)
                    {
                        eprintln!("finish: regression_test update_plan failed: {e}");
                    }
                }
            }
            (
                wf.plan().cloned(),
                wf.plan_id().map(str::to_string),
                wf.plans_dir().to_path_buf(),
                wf.project_root(),
            )
        };
        // The review report must live under .coding/reviews/ and be non-empty —
        // this is what makes the review unskippable: the agent must have run a
        // reviewer subagent that wrote a report there before it can finish.
        // The reviews dir is the factory's main-derived path (NOT the per-agent
        // workflow's plans_dir), so a per-agent plans dir doesn't redirect
        // finish to the wrong place.
        let reviews_dir = self.reviews_dir.clone();
        // Resolve the report path (backlog 5b46674d): the finish
        // notification now carries the PROJECT-RELATIVE path
        // (".coding/reviews/<file>.md"), so finish accepts it — as-given
        // first (absolute or CWD-relative, the old form), then the basename
        // under the reviews dir (which covers both the project-relative form
        // and a bare filename).
        let report_path = std::path::Path::new(&args.review_report);
        let resolved_report = if report_path.is_file() {
            report_path.to_path_buf()
        } else {
            reviews_dir.join(report_path.file_name().unwrap_or_default())
        };
        let under_reviews = resolved_report
            .canonicalize()
            .ok()
            .and_then(|c| reviews_dir.canonicalize().ok().map(|rd| c.starts_with(rd)))
            .unwrap_or(false);
        let report_text = std::fs::read_to_string(&resolved_report).unwrap_or_default();
        let non_empty = !report_text.trim().is_empty();
        if !under_reviews || !non_empty {
            return ToolResult::error(format!(
                "no review report found at '{}' — spawn a reviewer subagent (spawn_agent) and \
                 let it write its report to .coding/reviews/ first, then call finish with that \
                 path",
                args.review_report
            ));
        }
        // The verdict line is the report's contract (fail closed): PASS
        // proceeds; FINDINGS blocks until every finding is fixed and the
        // review re-run; an unparseable verdict counts as FINDINGS.
        match parse_verdict(&report_text) {
            Verdict::Pass => {}
            Verdict::Findings => {
                return ToolResult::error(
                    "review verdict is FINDINGS — fix every finding, re-run the reviewer \
                     subagent, and re-finish with a PASS report",
                );
            }
            Verdict::Unparseable => {
                return ToolResult::error(
                    "unparseable review verdict — the report must open with \
                     \"## Verdict: PASS\" or \"## Verdict: FINDINGS (n high, n low)\" \
                     (fail closed)",
                );
            }
        }

        // ── Phase 4 gates + auto-capture (all UNLOCKED) ───────────────────
        // (a) BugFixing regression-test gate: the verify step must have
        // recorded the test name via update_plan — finish is blocked without
        // it (the fix is unverifiable).
        let mut notes: Vec<String> = Vec::new();
        if let Some(plan) = &plan {
            if plan.kind == crate::workflow::PlanKind::BugFixing {
                let test = plan
                    .regression_test
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let Some(test) = test else {
                    return ToolResult::error(
                        "bug_fixing plan has no regression test recorded — the verify step \
                         must call update_plan with regression_test before finish",
                    );
                };
                // (a2) Landed-design gate (backlog e5a84ce9): a feature-scale
                // fix records landed_design=true — the plan context must then
                // carry a "Landed design" amendment (the constitution's
                // documentation expectations: architecture, deviations from
                // the sketch, measured numbers, invariants). The marker is a
                // prose paragraph, not a "## " header (the plan-file parser
                // treats those as section starts).
                if plan.landed_design {
                    if !plan.context.contains("Landed design") {
                        return ToolResult::error(
                            "landed_design is set but the plan context carries no \
                             'Landed design' amendment — update_plan append a paragraph \
                             starting \"Landed design —\" (architecture, deviations from \
                             the sketch, measured numbers, invariants to preserve) before \
                             finish (agent.md 'Documentation expectations')",
                        );
                    }
                    notes.push(
                        "landed_design recorded with its context amendment — at finish, \
                         also write a SPEC memory (architecture + invariants) and amend \
                         the BUG record with the landed outcome (agent.md 'Documentation \
                         expectations')"
                            .to_string(),
                    );
                }
                // (b) Code-graph gate: the recorded test name must resolve to
                // a real symbol. Skipped (with a note) when no graph is wired.
                if let Some(graph) = &self.codegraph {
                    // Resolve against the last-indexed snapshot; on a miss,
                    // self-freshen before failing: one incremental index pass
                    // (content-hash based — cheap when the graph is fresh)
                    // and one retry. A regression test written moments ago
                    // (no file watcher running, or finish racing the
                    // watcher's debounce) must not block on a stale index
                    // (backlog #91 — no more manual reindex dance). A
                    // still-miss escalates to a forced re-index below
                    // (backlog 648e0ad5 — the incremental pass can no-op on
                    // fresh meta rows with missing symbol rows).
                    let found_in_snapshot = graph
                        .view()
                        .ok()
                        .map(|view| !view.resolve(test).is_empty())
                        .unwrap_or(false);
                    let (mut found, refreshed) = if found_in_snapshot {
                        (true, false)
                    } else {
                        let g = graph.clone();
                        // No lock is held across this await (the view
                        // snapshot above was dropped; a reindex failure or
                        // panic fails closed below).
                        let reindexed = tokio::task::spawn_blocking(move || g.index(None))
                            .await
                            .ok()
                            .and_then(|r| r.ok())
                            .is_some();
                        let found_after = reindexed
                            && graph
                                .view()
                                .ok()
                                .map(|view| !view.resolve(test).is_empty())
                                .unwrap_or(false);
                        (found_after, found_after)
                    };
                    if !found {
                        // (backlog 648e0ad5) The incremental pass above can
                        // NO-OP: it is mtime/hash-based, so a DB whose meta
                        // rows are fresh but whose SYMBOL rows are stale or
                        // missing (a partial write, schema drift, or a prior
                        // best-effort skip) never re-parses the file — the
                        // retry misses and the gate would error on a symbol
                        // that exists on disk (live case 2027-01-08:
                        // 'checkFillContract'), sending the agent after a
                        // phantom rename. Escalate to a GUARANTEED re-index:
                        // force a re-parse of every source file whose
                        // on-disk content contains the test name, then
                        // re-resolve once more. Only a symbol absent after
                        // THAT errors — with a message naming the case.
                        let g = graph.clone();
                        let t = test.to_string();
                        let forced = tokio::task::spawn_blocking(move || {
                            g.reindex_files_containing(&t)
                        })
                        .await
                        .ok()
                        .and_then(|r| r.ok());
                        match forced {
                            Some(outcome) => {
                                found = graph
                                    .view()
                                    .ok()
                                    .map(|view| !view.resolve(test).is_empty())
                                    .unwrap_or(false);
                                if !found {
                                    if outcome.matched == 0
                                        && outcome.matched_unparsed == 0
                                    {
                                        // The name is absent from every code
                                        // file on disk — the wrong-test-name
                                        // case, verified after the forced
                                        // re-index.
                                        return ToolResult::error(format!(
                                            "regression test symbol '{test}' not found in \
                                             the code graph and no code file on disk \
                                             contains the name (verified after a forced \
                                             re-index) — the verify step must record the \
                                             actual test function name"
                                        ));
                                    }
                                    if outcome.matched_unparsed > 0 {
                                        // The name is on disk in code file(s)
                                        // whose language no grammar parses
                                        // — the symbol can never resolve in
                                        // the graph. Word-bounded disk
                                        // containment is the evidence the
                                        // recorded name is real (a
                                        // hallucinated name appears in no
                                        // code file; a comment mention in
                                        // a non-indexed language can still
                                        // pass — the transparent note
                                        // carries that residual): pass
                                        // instead of erroring on a test
                                        // that exists and is runnable.
                                        found = true;
                                        notes.push(format!(
                                            "regression test symbol '{test}' not \
                                             graph-indexed — found on disk in {} code \
                                             file(s) in languages the graph does not \
                                             parse; symbol check satisfied by disk \
                                             containment",
                                            outcome.matched_unparsed
                                        ));
                                    } else if outcome.reparsed == 0 {
                                        // (review LOW-1, 2026-09-08) Files
                                        // contain the name but every re-parse
                                        // failed (a store write failure) —
                                        // inconclusive, NOT absent: erroring
                                        // "no source file contains the name"
                                        // here would steer the agent to the
                                        // phantom rename this fix exists to
                                        // prevent.
                                        return ToolResult::error(format!(
                                            "code graph could not be refreshed to verify \
                                             the regression test symbol '{test}' — the \
                                             symbol check is inconclusive ({} file(s) \
                                             contain the name but their re-parse failed); \
                                             re-run finish, do not rename the test",
                                            outcome.matched
                                        ));
                                    } else {
                                        return ToolResult::error(format!(
                                            "regression test symbol '{test}' not found in \
                                             the code graph even after a forced re-index \
                                             of {} file(s) containing the name{} — the verify \
                                             step must record the actual test function name",
                                            outcome.reparsed,
                                            if outcome.capped {
                                                format!(
                                                    " (re-parse budget capped — {} of {} \
                                                     matching file(s) re-parsed; the absence \
                                                     verdict is inconclusive for the rest)",
                                                    outcome.reparsed, outcome.matched
                                                )
                                            } else {
                                                String::new()
                                            }
                                        ));
                                    }
                                }
                                if found && outcome.reparsed > 0 {
                                    notes.push(format!(
                                        "code graph symbol rows were stale — {} \
                                         file(s) force re-parsed before the symbol check",
                                        outcome.reparsed
                                    ));
                                }
                            }
                            None => {
                                return ToolResult::error(format!(
                                    "code graph could not be refreshed to verify the \
                                     regression test symbol '{test}' — the symbol check is \
                                     inconclusive (the forced re-index failed); re-run \
                                     finish, do not rename the test"
                                ));
                            }
                        }
                    }
                    if refreshed {
                        notes.push(
                            "code graph was stale — refreshed before the symbol check".into(),
                        );
                    }
                } else {
                    notes.push("code graph unwired — regression-test symbol check skipped".into());
                }
            }
        }

        // (c) Finish auto-capture: deterministic PLAN:/BUG: digests + crash-
        // marker supersede. Non-blocking — a capture failure logs and notes,
        // never blocks the state transition (the review gate already passed).
        if let (Some(store), Some(plan), Some(plan_id)) = (&self.memory, &plan, &plan_id) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            match crate::memory::finish_capture::capture_finish(
                store.as_ref(),
                plan,
                plan_id,
                &plans_dir,
                repo_root,
                now,
                self.knowledge.clone(),
            )
            .await
            {
                Ok(report) => {
                    notes.push(format!(
                        "captured PLAN: {} ({} marker(s) superseded)",
                        report.plan_memory_id, report.markers_superseded
                    ));
                    if let Some(bug_id) = &report.bug_memory_id {
                        notes.push(format!("captured BUG: {bug_id}"));
                    }
                }
                Err(e) => {
                    eprintln!("finish capture failed: {e}");
                    notes.push(format!("capture failed: {e}"));
                }
            }
        } else {
            notes.push("no memory store — finish capture skipped".into());
        }
        // (d) The behavior-change nudge: if the plan changed intended
        // behavior, the new behavior deserves a SPEC:/DECISION: record.
        notes.push(
            "if this plan changed intended behavior, write a SPEC:/DECISION: memory \
             (memory_write) so the new behavior is recorded"
                .into(),
        );

        // Re-lock for the state transition. The state is re-checked: the
        // unlocked window above must not let finish fire from a state that
        // changed underneath it (e.g. an abandon_plan racing in).
        let mut wf = self.workflow.lock().await;
        if wf.state() != WorkflowState::Reviewing {
            return ToolResult::error(format!(
                "finish is only available in the Reviewing state (current state: {}). \
                 Complete all plan steps first.",
                wf.state()
            ));
        }
        match wf.finish() {
            Ok(()) => ToolResult {
                success: true,
                output: format!(
                    "Finish review step — review passed, state → Complete (report: {}){}",
                    args.review_report,
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", notes.join(" · "))
                    }
                ),
                data: Some(json!({
                    "state": wf.state().to_string(),
                    "review_report": args.review_report,
                    "notes": notes,
                })),
            },
            Err(e) => ToolResult::error(format!("failed to finish: {e}")),
        }
    }
}

/// The `current_plan` workflow tool — a read-only query returning the active
/// plan's id + title + step count + completed count.
///
/// Used for recovery when the agent is disoriented about which plan is active
/// (e.g. after a long stacked-plan session). `AutoRun` + available in every
/// state (it's a read, not a mutation). Returns `null`-equivalent (a clear
/// "no active plan" message) when the workflow is in Planning with no plan.
pub struct CurrentPlanTool {
    workflow: Arc<Mutex<Workflow>>,
    /// The project's `.coding/plans/` dir — the active plan's on-disk file
    /// (`<id>.md`) is reported when it exists (F2), so an id hunt goes
    /// straight to the file instead of a zero-hit content search.
    plans_dir: PathBuf,
}

impl CurrentPlanTool {
    /// Create the tool, bound to a workflow handle + the project's plans dir.
    pub fn new(workflow: Arc<Mutex<Workflow>>, plans_dir: impl Into<PathBuf>) -> Self {
        Self {
            workflow,
            plans_dir: plans_dir.into(),
        }
    }
}

#[async_trait]
impl Tool for CurrentPlanTool {
    fn name(&self) -> &str {
        "current_plan"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "current_plan",
            "Read-only query of the active plan. Returns the active plan's id, title, total \
             steps, completed count, and workflow state — use this when you're unsure which plan \
             is active (e.g. after stacked sub-plans). Returns 'no active plan' when the workflow \
             is in Planning with no plan. Available in every workflow state.",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // A read-only query — never approval-gated.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let wf = self.workflow.lock().await;
        let state = wf.state();
        let depth = wf.plan_depth();
        match (wf.plan_id(), wf.plan()) {
            (Some(id), Some(plan)) => {
                let completed = plan.completed_count();
                let total = plan.steps.len();
                // F2: the active plan's on-disk file when it exists — an id
                // hunt goes straight to the file instead of a zero-hit
                // content search.
                let plan_path = self.plans_dir.join(format!("{id}.md"));
                let plan_file = plan_path
                    .exists()
                    .then(|| plan_path.to_string_lossy().replace('\\', "/"));
                ToolResult {
                    success: true,
                    output: format!(
                        "active plan: '{}' (id: {id}) — {completed}/{total} steps done, \
                         depth {depth}, state: {state}{}",
                        plan.title,
                        plan_file
                            .as_deref()
                            .map(|p| format!(", plan file: {p}"))
                            .unwrap_or_default()
                    ),
                    data: Some(json!({
                        "id": id,
                        "title": plan.title,
                        "completed": completed,
                        "total": total,
                        "depth": depth,
                        "state": state.to_string(),
                        "plan_file": plan_file,
                    })),
                }
            }
            _ => ToolResult {
                success: true,
                output: format!(
                    "no active plan (state: {state}, depth: {depth}) — the workflow is in Planning"
                ),
                data: Some(json!({
                    "id": null,
                    "title": null,
                    "completed": 0,
                    "total": 0,
                    "depth": depth,
                    "state": state.to_string(),
                })),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::{Memory, MemoryStore, MemoryStoreTrait, MemoryTier};
    use crate::project::git_ops::mock_git::MockGit;
    use tempfile::tempdir;

    fn make_workflow(dir: &std::path::Path) -> Arc<Mutex<Workflow>> {
        Arc::new(Mutex::new(Workflow::new(dir.join("plans"))))
    }

    #[test]
    fn strict_mode_shape_all_keys_null_optionals_deserializes() {
        // Plan 21118961 regression (review HIGH 1): strict mode forces
        // every schema key present with null as "no value" — the
        // non-Option optional fields (context, kind, append, and a map
        // step's body) must accept it.
        let args: CreatePlanArgs = serde_json::from_value(serde_json::json!({
            "title": "t",
            "goal": "g",
            "context": null,
            "steps": [{"header": "h", "body": null}],
            "kind": null,
            "bug": null,
            "branch": null,
            "base": null
        }))
        .expect("create_plan strict-mode shape must deserialize");
        assert_eq!(args.title, "t");
        assert_eq!(args.context, "");
        assert_eq!(args.kind, PlanKind::Implementation);
        assert!(args.bug.is_none());
        match &args.steps[0] {
            StepInput::Map { header, body } => {
                assert_eq!(header, "h");
                assert!(body.is_none());
            }
            other => panic!("expected a Map step, got {other:?}"),
        }

        let upd: UpdatePlanArgs = serde_json::from_value(serde_json::json!({
            "title": null,
            "goal": null,
            "context": null,
            "steps": null,
            "append": null,
            "regression_test": null,
            "landed_design": null
        }))
        .expect("update_plan strict-mode shape must deserialize");
        assert!(!upd.append);
        assert!(upd.title.is_none());
        assert!(upd.steps.is_none());
    }

    fn make_store() -> Arc<MemoryStore> {
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        Arc::new(MemoryStore::open_in_memory(embedder).unwrap())
    }

    // ---- plan resumability gate (pure validator) ----

    /// A context that passes the gate: symptom + root cause + file anchor +
    /// verification command.
    const GOOD_CTX: &str = "Crash on open when config.toml is missing; root cause: unwrap \
         on None in src/config.rs:42. Verify with cargo test.";

    #[test]
    fn validator_accepts_complete_plan() {
        let issues = validate_plan_resumability(
            GOOD_CTX,
            &["edit src/config.rs to handle the missing file".to_string()],
            PlanKind::Implementation,
            None,
        );
        assert!(issues.is_empty(), "issues: {issues:?}");
    }

    #[test]
    fn validator_rejects_thin_context() {
        let issues = validate_plan_resumability(
            "fix crash",
            &["edit src/config.rs".to_string()],
            PlanKind::Implementation,
            None,
        );
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].contains("context is too thin"),
            "issue: {}",
            issues[0]
        );
        assert!(issues[0].contains("min 40"), "issue: {}", issues[0]);
    }

    #[test]
    fn validator_rejects_blank_context() {
        let issues = validate_plan_resumability(
            "   ",
            &["edit src/config.rs".to_string()],
            PlanKind::Implementation,
            None,
        );
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("0 chars"), "issue: {}", issues[0]);
    }

    #[test]
    fn validator_context_length_boundary() {
        let steps = ["edit src/config.rs".to_string()];
        let short = "a".repeat(MIN_CONTEXT_CHARS - 1);
        let issues = validate_plan_resumability(&short, &steps, PlanKind::Implementation, None);
        assert_eq!(issues.len(), 1, "39 chars must be rejected: {issues:?}");
        let exact = "a".repeat(MIN_CONTEXT_CHARS);
        let issues = validate_plan_resumability(&exact, &steps, PlanKind::Implementation, None);
        assert!(issues.is_empty(), "40 chars must pass: {issues:?}");
    }

    #[test]
    fn validator_rejects_path_free_step_and_names_it() {
        let issues = validate_plan_resumability(
            GOOD_CTX,
            &[
                "edit src/config.rs to handle the missing file".to_string(),
                "ask the user what to do".to_string(),
                "another vague step".to_string(),
            ],
            PlanKind::Implementation,
            None,
        );
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("step 2"), "issue: {}", issues[0]);
        assert!(
            issues[0].contains("2 of 3 steps path-free"),
            "issue: {}",
            issues[0]
        );
        assert!(issues[0].contains("ask the user"), "issue: {}", issues[0]);
    }

    #[test]
    fn validator_path_detection_variants() {
        // Windows drive path, bare filename with a code-ish extension, a
        // URL, and the no-code markers all count; prose with a lone
        // separator does not (the backlog detail-bar vocabulary).
        for step in [
            "fix C:/repo/src/config.rs handling",
            "update README.md",
            "see https://example.com/docs/guide for the API",
            "no-code research — compare provider retry policies",
            "no code changes — findings only",
            "research only: interview the user",
        ] {
            let issues = validate_plan_resumability(
                GOOD_CTX,
                &[step.to_string()],
                PlanKind::Implementation,
                None,
            );
            assert!(issues.is_empty(), "step '{step}' must pass: {issues:?}");
        }
        let issues = validate_plan_resumability(
            GOOD_CTX,
            &["and/or something else".to_string()],
            PlanKind::Implementation,
            None,
        );
        assert_eq!(issues.len(), 1, "prose must not count as a path");
    }

    #[test]
    fn validator_bug_fixing_requires_regression_test_design() {
        // The skeleton is path-free by design — no step issue for bug plans,
        // but the regression-test design is required.
        let issues = validate_plan_resumability(
            "Crash on open when config.toml is missing; root cause: unwrap on None in \
             src/config.rs:42.",
            &[],
            PlanKind::BugFixing,
            None,
        );
        assert_eq!(issues.len(), 1, "no test design: {issues:?}");
        assert!(
            issues[0].contains("regression-test design"),
            "issue: {}",
            issues[0]
        );

        let issues = validate_plan_resumability(
            "Crash on open when config.toml is missing; root cause: unwrap on None in \
             src/config.rs:42. Regression test: config_missing_crash in \
             src/config/tests.rs fails without the fix.",
            &[],
            PlanKind::BugFixing,
            None,
        );
        assert!(issues.is_empty(), "with the design: {issues:?}");
    }

    #[test]
    fn validator_bug_design_may_live_in_detailed_steps() {
        let issues = validate_plan_resumability(
            "Crash on open when config.toml is missing; root cause: unwrap on None in \
             src/config.rs:42.",
            &[],
            PlanKind::BugFixing,
            Some("- Write the regression test in src/config/tests.rs"),
        );
        assert!(issues.is_empty(), "design in detailed steps: {issues:?}");
    }

    #[test]
    fn validator_bug_design_word_boundary() {
        // "latest" contains the substring but is not the word — the design
        // check must not be satisfied by lookalikes (review L4).
        let issues = validate_plan_resumability(
            "Reproduce with the latest driver; root cause: unwrap in src/foo.rs:42, \
             verify by hand.",
            &[],
            PlanKind::BugFixing,
            None,
        );
        assert_eq!(issues.len(), 1, "substring lookalike must not pass: {issues:?}");
        assert!(
            issues[0].contains("regression-test design"),
            "issue: {}",
            issues[0]
        );
    }

    #[test]
    fn gate_error_numbers_every_issue() {
        let issues = vec!["issue one".to_string(), "issue two".to_string()];
        let err = resumability_gate_error("plan rejected by the resumability gate", &issues);
        assert!(err.contains("(1) issue one"), "error: {err}");
        assert!(err.contains("(2) issue two"), "error: {err}");
        assert!(
            err.starts_with("plan rejected by the resumability gate"),
            "error: {err}"
        );
    }

    #[tokio::test]
    async fn create_plan_works() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "Build it",
                "goal": "Make it work",
                "context": GOOD_CTX,
                "steps": ["edit src/widget.rs", "edit src/config.rs"]
            }))
            .await;
        assert!(result.success);
        assert!(result.output.contains("created plan"));
        // Workflow should now be Executing.
        let wf = wf.lock().await;
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Executing);
    }

    #[tokio::test]
    async fn create_plan_rides_recalled_context() {
        // The RECALLED CONTEXT rider: prior knowledge matched to the plan's
        // goal rides the create result — structural enforcement of the
        // "memory_search before planning" rule (it arrives WITH the plan
        // even if the agent forgot to look).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let store = make_store();
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "BUG: kettle crashes when water is empty",
                "SYMPTOM: kettle crashes on empty water → ROOT CAUSE: divide-by-zero in boil() → \
                 fixed in boil(); regression test kettle_boil_empty_guard",
                1_000_000_000,
            ))
            .await
            .unwrap();

        let tool = CreatePlanTool::new(wf.clone()).with_memory(store.clone());
        let r = tool
            .execute(json!({
                "title": "Fix the kettle crash",
                "goal": "stop the kettle crashing when the water is empty",
                "context": "Kettle crashes on empty water; root cause: divide-by-zero in boil() at src/kettle.rs:42. Regression test: kettle_boil_empty_guard fails without the fix.",
                "bug": "kettle crashes when water is empty",
                "kind": "bug_fixing",
                "steps": ["ignored for bug_fixing"]
            }))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("RECALLED CONTEXT"),
            "rider rides the create result: {}",
            r.output
        );
        assert!(
            r.output.contains("BUG: kettle crashes"),
            "the matched hit is surfaced: {}",
            r.output
        );
        assert!(
            r.output.contains("memory_search"),
            "rider points at the full-detail tool"
        );
        // Passive by construction: recall_peek must NOT bump access counts.
        let rows = store.list_by_tier(MemoryTier::Semantic).await.unwrap();
        assert_eq!(rows[0].access_count, 0, "rider is passive (no access bump)");
    }

    #[tokio::test]
    async fn create_plan_without_store_has_no_rider() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        // No store wired (tests / NeedsProject) → plain result, silent skip.
        let tool = CreatePlanTool::new(wf.clone());
        let r = tool
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("RECALLED CONTEXT"));
    }

    #[tokio::test]
    async fn create_plan_defaults_to_implementation_kind() {
        // Omitting `kind` defaults to implementation (reviewed on completion).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]
            }))
            .await;
        assert!(result.success);
        assert!(result.output.contains("kind: implementation"));
        let wf = wf.lock().await;
        assert_eq!(
            wf.plan().unwrap().kind,
            crate::workflow::PlanKind::Implementation
        );
    }

    #[tokio::test]
    async fn create_plan_with_research_kind_skips_review() {
        // A research plan created via the tool skips Reviewing on completion.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Investigate",
                "goal": "G",
                "context": GOOD_CTX,
                "steps": ["read src/widget.rs"],
                "kind": "research"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("kind: research"));
        let wf_guard = wf.lock().await;
        assert_eq!(
            wf_guard.plan().unwrap().kind,
            crate::workflow::PlanKind::Research
        );
        drop(wf_guard);

        // Complete the last step → Complete directly (not Reviewing).
        let complete = CompleteStepTool::new(wf.clone());
        complete.execute(json!({"step_index": 1})).await;
        let wf_guard = wf.lock().await;
        assert_eq!(wf_guard.state(), crate::workflow::WorkflowState::Complete);
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_requires_bug_param() {
        // kind=bug_fixing without the bug param errors — the symptom is the
        // whole point of the kind.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "steps": ["a"],
                "kind": "bug_fixing"
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("requires the bug param"),
            "{}",
            result.output
        );
        // The workflow stays in Planning (no plan was created).
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Planning
        );
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_forces_skeleton_and_persists_symptom() {
        // kind=bug_fixing with the bug param: the 4-step skeleton is FORCED
        // (provided steps persist as the `## Detailed steps` section —
        // see the sibling test below) and the symptom is persisted.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "steps": ["my own step"],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("kind: bug_fixing"),
            "{}",
            result.output
        );
        let wf_guard = wf.lock().await;
        let plan = wf_guard.plan().unwrap();
        assert_eq!(plan.kind, crate::workflow::PlanKind::BugFixing);
        assert_eq!(plan.steps.len(), 4, "the skeleton is forced");
        assert!(
            plan.steps[0]
                .text
                .contains("Reproduce with failing regression test"),
            "{}",
            plan.steps[0].text
        );
        assert!(
            plan.steps[2].text.contains("Minimal fix")
                && plan.steps[2].text.contains("regression test pass"),
            "fix step is a direct minimal fix: {}",
            plan.steps[2].text
        );
        assert!(
            plan.steps[3].text.contains("regression_test"),
            "verify step names the gate: {}",
            plan.steps[3].text
        );
        assert!(
            plan.steps[3].text.contains("landed_design")
                && plan.steps[3].text.contains("Landed design"),
            "verify step carries the feature-scale documentation instruction: {}",
            plan.steps[3].text
        );
        assert_eq!(plan.bug_symptom.as_deref(), Some("the app crashes on open"));
        // The symptom round-trips through the on-disk plan file.
        let id = wf_guard.plan_id().unwrap().to_string();
        let text = std::fs::read_to_string(wf_guard.plans_dir().join(format!("{id}.md"))).unwrap();
        assert!(text.contains("## Bug\nthe app crashes on open"), "{text}");
    }

    #[test]
    fn referenced_module_dirs_counts_distinct_modules() {
        // Backlog 51ee41c1: same-module files count once, a bare
        // src/<file> (no module segment) counts none, and the inner src/
        // of src-tauri/src/... + frontend/src/... matches.
        assert_eq!(
            referenced_module_dirs(&[
                "fix src/codegraph/mod.rs and src/codegraph/store.rs",
                "then src/workflow/mod.rs",
            ]),
            2
        );
        assert_eq!(referenced_module_dirs(&["see src/lib.rs"]), 0);
        assert_eq!(
            referenced_module_dirs(&[
                "src-tauri/src/ipc/projects.rs",
                "frontend/src/components/projects/ProjectPicker.tsx",
            ]),
            2
        );
        assert_eq!(
            referenced_module_dirs(&["src/agent/a.rs", "src/agent/b.rs", "src/agent/c.rs"]),
            1,
            "repeats of the same module count once"
        );
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_feature_scale_notes_steering() {
        // Backlog 51ee41c1: a bug-triggered FEATURE filed under bug_fixing
        // (more detailed steps than the locked 4-step skeleton AND
        // spanning ≥3 modules — cf. plan c87093c1's all-language coverage)
        // gets the advisory steering NOTE: re-file as kind=implementation
        // with the bug as motivation. Advisory only — the plan still files
        // as bug_fixing (the caller judges).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Gate rejects non-indexed languages",
                "goal": "Cover all languages",
                "context": "The finish gate errors on regression tests in non-indexed \
                           languages. The fix spans src/codegraph/walk.rs, \
                           src/codegraph/extract.rs, src/agent/prompt.rs, and \
                           src/tool/workflow/plan.rs, and adds tree-sitter crate \
                           dependencies. Regression test: finish_passes_when_test_is_python.",
                "steps": [
                    "Add the grammars to src/codegraph/walk.rs",
                    "Add the visitors in src/codegraph/extract.rs",
                    "Broaden the gate disk check in src/tool/workflow/plan.rs",
                    "Wire the prompt guidance in src/agent/prompt.rs",
                    "Add the regression test finish_passes_when_test_is_python"
                ],
                "kind": "bug_fixing",
                "bug": "the finish gate falsely errors on regression tests in non-indexed languages"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("NOTE (backlog 51ee41c1)"),
            "the feature-scale steering note must fire: {}",
            result.output
        );
        assert!(
            result.output.contains("kind=implementation"),
            "the note must name the implementation alternative: {}",
            result.output
        );
        // Advisory only: the plan still filed as bug_fixing.
        let wf_guard = wf.lock().await;
        assert_eq!(
            wf_guard.plan().unwrap().kind,
            crate::workflow::PlanKind::BugFixing
        );
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_contained_fix_stays_silent() {
        // Backlog 51ee41c1: small true bugs go through bug_fixing
        // UNCHANGED — a contained payload (≤4 detailed steps, ≤2 modules)
        // must not carry the steering note (the acceptance criterion).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at \
                           src/app.rs:42 when the config lacks the key. Regression test: \
                           crashy.",
                "steps": [
                    "Reproduce — run `cargo test crashy` and observe the panic in src/app.rs:42",
                    "Root-cause — the None unwrap at src/app.rs:42 fires when the config lacks the key",
                    "Fix — guard the unwrap with a default in src/app.rs:42",
                    "Verify — cargo test green"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("NOTE (backlog 51ee41c1)"),
            "a contained fix must not carry the feature-scale note: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_and_legs_pin_independently() {
        // Backlog 51ee41c1 (review LOW-4): the AND heuristic's legs must
        // each suppress the note on their own — the plan's own
        // must-not-warn shapes. Case 1 is the 9441d776 shape (4 steps
        // across 3 modules: the STEP leg fails — 4 > 4 is false — while
        // the module leg would fire); case 2 is the 85368a1f shape
        // generalized (6 steps in 1 module: the MODULE leg fails — 1 >= 3
        // is false — while the step leg would fire).
        // Case 1 — the step leg: 4 steps across 3 modules → no note.
        {
            let dir = tempdir().unwrap();
            let wf = make_workflow(dir.path());
            let create = CreatePlanTool::new(wf.clone());
            let result = create
                .execute(json!({
                    "title": "Sub-step progress lost",
                    "goal": "Persist sub-step state",
                    "context": "Bug-fix plans lose sub-step progress across restarts. The \
                               fix spans src/workflow/plan_file.rs, src/workflow/mod.rs, \
                               src/tool/workflow/plan.rs, and src/agent/factory.rs. \
                               Regression test: bug_fixing_detailed_step_ticks_survive_restart.",
                    "steps": [
                        "Persist the ticks in src/workflow/plan_file.rs",
                        "Wire the workflow in src/workflow/mod.rs",
                        "Route the tool in src/tool/workflow/plan.rs",
                        "Raise the budgets in src/agent/factory.rs"
                    ],
                    "kind": "bug_fixing",
                    "bug": "bug-fix plans lose sub-step progress across restarts"
                }))
                .await;
            assert!(result.success, "{}", result.output);
            assert!(
                !result.output.contains("NOTE (backlog 51ee41c1)"),
                "the 9441d776 shape (4 steps, 3 modules) must stay silent: {}",
                result.output
            );
        }
        // Case 2 — the module leg: 6 steps in 1 module → no note.
        {
            let dir = tempdir().unwrap();
            let wf = make_workflow(dir.path());
            let create = CreatePlanTool::new(wf.clone());
            let result = create
                .execute(json!({
                    "title": "Parser edge cases",
                    "goal": "Fix the parser edge cases",
                    "context": "Several parser edge cases panic on malformed input in \
                               src/codegraph/extract.rs. Regression test: extract_edge_cases.",
                    "steps": [
                        "Case A in src/codegraph/extract.rs",
                        "Case B in src/codegraph/extract.rs",
                        "Case C in src/codegraph/extract.rs",
                        "Case D in src/codegraph/extract.rs",
                        "Case E in src/codegraph/extract.rs",
                        "Case F in src/codegraph/extract.rs"
                    ],
                    "kind": "bug_fixing",
                    "bug": "parser edge cases panic on malformed input"
                }))
                .await;
            assert!(result.success, "{}", result.output);
            assert!(
                !result.output.contains("NOTE (backlog 51ee41c1)"),
                "the single-module shape (6 steps, 1 module) must stay silent: {}",
                result.output
            );
        }
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_persists_detailed_steps_section() {
        // Backlog (plan 4405d82d): a bug_fixing create_plan carrying
        // detailed, self-contained steps must NOT discard them — the plan
        // file is the crash-resumption document. The locked 4-step
        // skeleton stays the checklist; the caller's steps persist as the
        // "## Detailed steps" section so a restarted session resumes with
        // the full recipe (paths, anchors, test designs) instead of
        // re-deriving it from the bug + context alone.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at src/app.rs:42 when the config lacks the key.",
                "steps": [
                    "Reproduce — run `cargo test crashy` and observe the panic in src/app.rs:42",
                    "Root-cause — the None unwrap at src/app.rs:42 fires when the config lacks the key",
                    "Fix — guard the unwrap with a default in src/app.rs:42"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        // The result must state plainly what was persisted — the caller
        // must not discover the step handling after a crash.
        assert!(
            result.output.contains("detailed steps"),
            "the result must mention the detailed steps: {}",
            result.output
        );
        let wf_guard = wf.lock().await;
        let id = wf_guard.plan_id().unwrap().to_string();
        let text = std::fs::read_to_string(wf_guard.plans_dir().join(format!("{id}.md"))).unwrap();
        // The skeleton stays the locked checklist…
        assert!(text.contains("## Steps"), "{text}");
        // …and the caller's detailed steps persist as their own section.
        assert!(
            text.contains("## Detailed steps"),
            "the detailed steps must be persisted as a section: {text}"
        );
        assert!(
            text.contains("run `cargo test crashy` and observe the panic in src/app.rs:42"),
            "the step-level recipe must survive: {text}"
        );
        assert!(
            text.contains("guard the unwrap with a default in src/app.rs:42"),
            "every detailed step must survive: {text}"
        );
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_renders_detailed_steps_as_checkable_substeps() {
        // Backlog (plan 9441d776): the "## Detailed steps" section must
        // render as CHECKABLE sub-items — `- [ ] {text}` bullets — so a
        // restarted session can see which sub-steps shipped (each mark is
        // ticked via complete_step's detailed_step_index as the sub-step
        // lands). Plain `- {text}` bullets carry no completion state: a
        // mid-fix restart resumes against an unchecked mega-step plus a
        // wall of detail text (live case: plan c87093c1, ~2000 lines
        // across 5 files collapsed into one skeleton step).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at src/app.rs:42 when the config lacks the key.",
                "steps": [
                    "Reproduce — run `cargo test crashy` and observe the panic in src/app.rs:42",
                    "Root-cause — the None unwrap at src/app.rs:42 fires when the config lacks the key",
                    "Fix — guard the unwrap with a default in src/app.rs:42"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf_guard = wf.lock().await;
        let id = wf_guard.plan_id().unwrap().to_string();
        let text = std::fs::read_to_string(wf_guard.plans_dir().join(format!("{id}.md"))).unwrap();
        assert!(text.contains("## Detailed steps"), "{text}");
        // Every detailed step renders as an UNCHECKED checkbox bullet —
        // the mark is the sub-step's completion state.
        assert!(
            text.contains(
                "- [ ] Reproduce — run `cargo test crashy` and observe the panic in src/app.rs:42"
            ),
            "each detailed step must render as a checkable sub-item: {text}"
        );
        assert!(
            text.contains("- [ ] Fix — guard the unwrap with a default in src/app.rs:42"),
            "every detailed step must carry the checkbox: {text}"
        );
    }

    #[tokio::test]
    async fn complete_step_detailed_step_index_validations() {
        // Backlog 9441d776: the tick param is validated like the skeleton
        // step param — exactly one of the two, 1-indexed, in range.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at src/app.rs:42.",
                "steps": [
                    "Reproduce — run `cargo test crashy` (src/app.rs:42)",
                    "Fix — guard the unwrap in src/app.rs:42"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let complete = CompleteStepTool::new(wf.clone());
        // Neither param.
        let neither = complete.execute(json!({})).await;
        assert!(!neither.success);
        assert!(
            neither
                .output
                .contains("requires step_index (the 1-indexed skeleton step)"),
            "{}",
            neither.output
        );
        // Both params.
        let both = complete
            .execute(json!({"step_index": 1, "detailed_step_index": 1}))
            .await;
        assert!(!both.success);
        assert!(both.output.contains("not both"), "{}", both.output);
        // Zero is not 1-indexed.
        let zero = complete.execute(json!({"detailed_step_index": 0})).await;
        assert!(!zero.success);
        assert!(zero.output.contains("1-indexed"), "{}", zero.output);
        // Out of range (the plan has 2 detailed steps).
        let oor = complete.execute(json!({"detailed_step_index": 3})).await;
        assert!(!oor.success);
        assert!(oor.output.contains("1-indexed"), "{}", oor.output);
        assert!(oor.output.contains("valid 1..=2"), "{}", oor.output);
    }

    #[tokio::test]
    async fn bug_fixing_detailed_step_ticks_survive_restart() {
        // Backlog 9441d776 acceptance: a bug_fixing plan restarted mid-fix
        // shows exactly which detailed sub-steps shipped — the marks live
        // in the plan file (the crash-resumption document), so a fresh
        // Workflow::load_latest over the same directory resumes with the
        // sub-step progress intact and the remaining work completes
        // without redoing the finished sub-steps.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at src/app.rs:42.",
                "steps": [
                    "Reproduce — run `cargo test crashy` and observe the panic in src/app.rs:42",
                    "Root-cause — the None unwrap at src/app.rs:42",
                    "Fix — guard the unwrap with a default in src/app.rs:42"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let complete = CompleteStepTool::new(wf.clone());
        // Mid-fix state: skeleton steps 1-2 done, sub-steps 1-2 ticked.
        assert!(complete.execute(json!({"step_index": 1})).await.success);
        assert!(complete.execute(json!({"step_index": 2})).await.success);
        let tick = complete.execute(json!({"detailed_step_index": 1})).await;
        assert!(tick.success, "{}", tick.output);
        assert!(tick.output.contains("1/3"), "{}", tick.output);
        let tick2 = complete.execute(json!({"detailed_step_index": 2})).await;
        assert!(tick2.success, "{}", tick2.output);
        assert!(tick2.output.contains("2/3"), "{}", tick2.output);

        // RESTART: a fresh Workflow over the same directory re-derives the
        // plan from disk — the sub-step marks must survive.
        let wf2 = Arc::new(Mutex::new({
            let mut w = Workflow::new(dir.path().join("plans"));
            w.load_latest().unwrap();
            w
        }));
        {
            let guard = wf2.lock().await;
            let plan = guard.plan().unwrap();
            assert_eq!(
                plan.steps.iter().filter(|s| s.done).count(),
                2,
                "skeleton progress survived the restart"
            );
            let body = plan.detailed_steps.as_deref().unwrap();
            assert!(
                body.contains(
                    "- [x] Reproduce — run `cargo test crashy` and observe the panic in src/app.rs:42"
                ),
                "{body}"
            );
            assert!(
                body.contains("- [x] Root-cause — the None unwrap at src/app.rs:42"),
                "{body}"
            );
            assert!(
                body.contains("- [ ] Fix — guard the unwrap with a default in src/app.rs:42"),
                "{body}"
            );
        }
        // The resumed session finishes the remaining sub-step + skeleton
        // steps — no redo of the finished sub-work.
        let complete2 = CompleteStepTool::new(wf2.clone());
        let tick3 = complete2.execute(json!({"detailed_step_index": 3})).await;
        assert!(tick3.success, "{}", tick3.output);
        assert!(tick3.output.contains("3/3"), "{}", tick3.output);
        assert!(complete2.execute(json!({"step_index": 3})).await.success);
        assert!(complete2.execute(json!({"step_index": 4})).await.success);
        // All four skeleton steps done → the root bug_fixing plan left
        // Executing for Reviewing.
        let guard = wf2.lock().await;
        assert!(matches!(guard.state(), WorkflowState::Reviewing));
    }

    #[tokio::test]
    async fn detailed_step_marks_survive_step_ticks_and_update_plan() {
        // The marks must survive every other plan write: skeleton-step
        // ticks and update_plan (regression_test recording) both rewrite
        // the plan file — the raw section round-trips verbatim.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at src/app.rs:42.",
                "steps": [
                    "Reproduce — run `cargo test crashy` (src/app.rs:42)",
                    "Fix — guard the unwrap in src/app.rs:42"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let complete = CompleteStepTool::new(wf.clone());
        assert!(
            complete
                .execute(json!({"detailed_step_index": 1}))
                .await
                .success
        );
        assert!(complete.execute(json!({"step_index": 1})).await.success);
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"regression_test": "crash_on_open_regression"}))
            .await;
        assert!(result.success, "{}", result.output);
        // The ticked mark + the unchecked sibling survive both writes.
        let guard = wf.lock().await;
        let id = guard.plan_id().unwrap().to_string();
        let text = std::fs::read_to_string(guard.plans_dir().join(format!("{id}.md"))).unwrap();
        assert!(
            text.contains("- [x] Reproduce — run `cargo test crashy` (src/app.rs:42)"),
            "{text}"
        );
        assert!(
            text.contains("- [ ] Fix — guard the unwrap in src/app.rs:42"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn multi_line_step_continuations_do_not_become_sub_steps() {
        // Review L3: a step text carrying a nested plain-dash list must
        // not inflate the checkable sub-step count — the render escapes
        // `- `-leading continuation lines (a rendered step line always
        // starts `- [ ] `), so the tick's line walk sees only real steps.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap at src/app.rs:42.",
                "steps": [
                    "Fix — two edits:\n- guard the unwrap in src/app.rs:42\n- add the default key",
                    "Verify — run `cargo test crashy` (src/app.rs:42)"
                ],
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let complete = CompleteStepTool::new(wf.clone());
        // Ticking sub-step 1 flips the FIRST step's checkbox — the nested
        // bullets are escaped continuations, not sub-steps.
        let tick = complete.execute(json!({"detailed_step_index": 1})).await;
        assert!(tick.success, "{}", tick.output);
        assert!(tick.output.contains("1/2"), "{}", tick.output);
        let guard = wf.lock().await;
        let id = guard.plan_id().unwrap().to_string();
        let text = std::fs::read_to_string(guard.plans_dir().join(format!("{id}.md"))).unwrap();
        assert!(
            text.contains("- [x] Fix — two edits:"),
            "the first step's checkbox flipped: {text}"
        );
        assert!(
            text.contains("\\- guard the unwrap in src/app.rs:42"),
            "the nested bullet is escaped as a continuation: {text}"
        );
        assert!(
            text.contains("- [ ] Verify — run `cargo test crashy` (src/app.rs:42)"),
            "the second step is untouched: {text}"
        );
    }

    #[tokio::test]
    async fn detailed_steps_cannot_inject_sections() {
        // Review L3 (backlog 77ff8f45): a detailed step quoting the
        // plan-file format must not corrupt the re-parsed plan — the
        // render escapes lines whose trimmed form starts with a section
        // or title marker.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "T",
                "goal": "G",
                "context": "The real symptom is a crash on open; root cause: unguarded unwrap in src/app.rs:42. The regression-test design lives in the detailed steps.",
                "steps": [
                    "Fix — the serialize() output looks like:\n## Bug\ninjected symptom\n## Regression test\ninjected_test\n# Plan: fake title"
                ],
                "kind": "bug_fixing",
                "bug": "the real symptom"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf_guard = wf.lock().await;
        let id = wf_guard.plan_id().unwrap().to_string();
        let text =
            std::fs::read_to_string(wf_guard.plans_dir().join(format!("{id}.md"))).unwrap();
        // Re-parse: the injected markers must NOT have taken effect.
        let parsed = crate::workflow::PlanFile::parse(&text).unwrap();
        assert_eq!(parsed.bug_symptom.as_deref(), Some("the real symptom"));
        assert_eq!(parsed.regression_test, None);
        assert_eq!(parsed.title, "T");
        assert_eq!(parsed.kind, crate::workflow::PlanKind::BugFixing);
        // The escaped lines survive in the section body.
        let detail = parsed.detailed_steps.as_deref().unwrap();
        assert!(detail.contains("\\## Bug"), "{detail}");
        assert!(detail.contains("\\# Plan: fake title"), "{detail}");
    }

    #[tokio::test]
    async fn multi_line_bug_symptom_cannot_inject_sections() {
        // A symptom containing newlines + fake section headers must be
        // collapsed to a single line before persisting — otherwise the
        // plan-file parser would re-read `## Kind\nresearch` as a real
        // section (turning the bug plan into a research plan on resume,
        // skipping the review gate) or truncate the symptom (review HIGH 2).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "kind": "bug_fixing",
                "bug": "line one\nline two\n\n## Kind\nresearch\n- [x] 1. fake step"
            }))
            .await;
        assert!(result.success, "{}", result.output);

        // The persisted plan file must NOT contain a second `## Kind` or a
        // fake step — the symptom is one collapsed line.
        let (id, plans_dir) = {
            let wf_guard = wf.lock().await;
            (
                wf_guard.plan_id().unwrap().to_string(),
                wf_guard.plans_dir().to_path_buf(),
            )
        };
        let text = std::fs::read_to_string(plans_dir.join(format!("{id}.md"))).unwrap();
        assert!(
            text.contains("## Bug\nline one line two ## Kind research - [x] 1. fake step"),
            "symptom collapsed to one line: {text}"
        );
        // Only ONE real `## Kind` section header (the plan's own) — the
        // collapsed symptom line contains the substring but is not a header.
        let kind_headers = text.lines().filter(|l| l.trim() == "## Kind").count();
        assert_eq!(kind_headers, 1, "no injected Kind section: {text}");
        // No real checklist line was injected (the collapsed symptom line
        // contains the text mid-line, but no line STARTS with it).
        assert!(
            !text.lines().any(|l| l.starts_with("- [x] 1. fake step")),
            "no injected step: {text}"
        );

        // Re-parsing the file keeps kind = bug_fixing and the 4-step
        // skeleton — the injection is neutralized.
        let reparsed =
            crate::workflow::PlanFile::read_from_file(&plans_dir.join(format!("{id}.md"))).unwrap();
        assert_eq!(reparsed.kind, crate::workflow::PlanKind::BugFixing);
        assert_eq!(reparsed.steps.len(), 4);
        assert_eq!(
            reparsed.bug_symptom.as_deref(),
            Some("line one line two ## Kind research - [x] 1. fake step")
        );
    }

    #[tokio::test]
    async fn multi_line_regression_test_cannot_inject_sections() {
        // Same injection guard on the regression_test field (update_plan).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "kind": "bug_fixing",
                "bug": "crash"
            }))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"regression_test": "test_one\ntest_two\n\n## Kind\nresearch"}))
            .await;
        assert!(result.success, "{}", result.output);
        let wf_guard = wf.lock().await;
        assert_eq!(
            wf_guard.plan().unwrap().regression_test.as_deref(),
            Some("test_one test_two ## Kind research"),
            "collapsed to one line"
        );
        assert_eq!(
            wf_guard.plan().unwrap().kind,
            crate::workflow::PlanKind::BugFixing,
            "kind unchanged"
        );
    }

    #[tokio::test]
    async fn update_plan_records_regression_test_via_tool() {
        // The verify step records the regression test name through the tool —
        // the field lands on the plan (and the on-disk file).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "kind": "bug_fixing",
                "bug": "crash"
            }))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"regression_test": "crash_on_open_regression"}))
            .await;
        assert!(result.success, "{}", result.output);
        let wf_guard = wf.lock().await;
        assert_eq!(
            wf_guard.plan().unwrap().regression_test.as_deref(),
            Some("crash_on_open_regression")
        );
    }

    #[tokio::test]
    async fn create_plan_requires_steps() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(json!({"title": "T", "goal": "G", "steps": []}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("at least one step"));
    }

    // ── Plan resumability gate (tool-level) ─────────────────────────────────
    // A plan too thin to resume from after a recompile+restart is rejected
    // at the tool boundary with one actionable error naming everything
    // missing (the backlog_add detail-bar UX).

    #[tokio::test]
    async fn create_plan_rejects_sparse_plan() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(json!({"title": "T", "goal": "G", "steps": ["ask the user"]}))
            .await;
        assert!(!result.success);
        assert!(
            result
                .output
                .starts_with("plan rejected by the resumability gate"),
            "gate error: {}",
            result.output
        );
        // Both issues named in one shot so the retry fixes everything.
        assert!(
            result.output.contains("context is too thin"),
            "names the thin context: {}",
            result.output
        );
        assert!(
            result.output.contains("names no file path"),
            "names the path-free step: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn create_plan_accepts_complete_plan() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "Build it",
                "goal": "Make it work",
                "context": GOOD_CTX,
                "steps": ["edit src/config.rs to handle the missing file"]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf = wf.lock().await;
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Executing);
    }

    #[tokio::test]
    async fn create_plan_accepts_no_code_marker_steps() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(json!({
                "title": "Survey providers",
                "goal": "Compare retry policies across providers",
                "context": "No code exists yet for provider retry; survey the docs and report findings.",
                "kind": "research",
                "steps": ["no-code research — compare provider retry policies"]
            }))
            .await;
        assert!(result.success, "{}", result.output);
    }

    #[tokio::test]
    async fn create_plan_names_the_path_free_step() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(json!({
                "title": "T",
                "goal": "G",
                "context": GOOD_CTX,
                "steps": ["edit src/config.rs", "ask the user what to do"]
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("step 2 names no file path"),
            "names the offending step: {}",
            result.output
        );
        assert!(
            result.output.contains("ask the user"),
            "quotes the step text: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn create_plan_bug_fixing_requires_regression_test_design() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        // Substantive context but no regression-test design → rejected.
        let result = tool
            .execute(json!({
                "title": "Fix the crash",
                "goal": "Stop the crash on open",
                "kind": "bug_fixing",
                "bug": "crashes on open when config.toml is missing",
                "context": "Crash on open when config.toml is missing; root cause: unwrap on None in src/config.rs:42."
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("regression-test design"),
            "names the missing design: {}",
            result.output
        );
        // With the design → passes.
        let result = tool
            .execute(json!({
                "title": "Fix the crash",
                "goal": "Stop the crash on open",
                "kind": "bug_fixing",
                "bug": "crashes on open when config.toml is missing",
                "context": "Crash on open when config.toml is missing; root cause: unwrap on None in src/config.rs:42. Regression test: config_missing_crash in src/config/tests.rs fails without the fix."
            }))
            .await;
        assert!(result.success, "{}", result.output);
    }

    #[tokio::test]
    async fn update_plan_rejects_path_free_steps() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let r = create
            .execute(json!({
                "title": "Build it",
                "goal": "Make it work",
                "context": GOOD_CTX,
                "steps": ["edit src/config.rs to handle the missing file"]
            }))
            .await;
        assert!(r.success, "{}", r.output);
        let update = UpdatePlanTool::new(wf.clone());
        let r = update
            .execute(json!({"steps": ["figure it out later"]}))
            .await;
        assert!(!r.success);
        assert!(
            r.output
                .starts_with("update rejected by the resumability gate"),
            "gate error: {}",
            r.output
        );
        assert!(
            r.output.contains("names no file path"),
            "names the path-free step: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn update_plan_rejects_thin_context_replace() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let r = create
            .execute(json!({
                "title": "Build it",
                "goal": "Make it work",
                "context": GOOD_CTX,
                "steps": ["edit src/config.rs to handle the missing file"]
            }))
            .await;
        assert!(r.success, "{}", r.output);
        let update = UpdatePlanTool::new(wf.clone());
        let r = update.execute(json!({"context": "see notes"})).await;
        assert!(!r.success);
        assert!(
            r.output.contains("context is too thin"),
            "names the thin context: {}",
            r.output
        );
    }

    #[tokio::test]
    async fn update_plan_append_context_onto_substantive_passes() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let r = create
            .execute(json!({
                "title": "Build it",
                "goal": "Make it work",
                "context": GOOD_CTX,
                "steps": ["edit src/config.rs to handle the missing file"]
            }))
            .await;
        assert!(r.success, "{}", r.output);
        let update = UpdatePlanTool::new(wf.clone());
        // A short hardening chunk appended onto the substantive context —
        // the COMBINED text is what must be resumable, and it is.
        let r = update
            .execute(json!({"append": true, "context": "Baseline: cargo test green at HEAD."}))
            .await;
        assert!(r.success, "{}", r.output);
    }

    // ── Non-blank content guard (content-first rule) ────────────────────────
    // serde enforces PRESENCE of title/goal, but a whitespace-only string
    // deserializes fine — which is the signature of a call emitted before its
    // content was drafted. The tool rejects it with the drafting protocol.

    #[tokio::test]
    async fn create_plan_rejects_blank_title() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(json!({"title": "", "goal": "G", "steps": ["s"]}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("title must be non-blank"),
            "blank title rejected with the drafting protocol: {}",
            result.output
        );
        assert!(
            result.output.contains("in your reply FIRST"),
            "error teaches content-first: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn create_plan_rejects_blank_goal() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(json!({"title": "T", "goal": "   ", "steps": ["s"]}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("goal must be non-blank"),
            "blank goal rejected: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn create_plan_accepts_whitespace_padded_non_blank_content() {
        // The guard trims: surrounding whitespace is fine, only all-blank is
        // rejected — normal calls keep working.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf);
        let result = tool
            .execute(
                json!({"title": "  Build it  ", "goal": " G ", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}),
            )
            .await;
        assert!(
            result.success,
            "padded-but-non-blank content succeeds: {}",
            result.output
        );
    }

    // ── Optional branch fork (backlog: "git checkout always fails on stack
    //    json after creating a plan — 5-6 wasted git commands") ────────────

    /// A temp git repo laid out like a real project: `.coding/plans/` +
    /// `.coding/backlog.jsonl` committed on `main`, a `feat/prev` branch that
    /// diverges `stack.json` (the exact post-item state a run-all batch
    /// starts the next item from), and `backlog.jsonl` left dirty to mimic the
    /// harness's InFlight flip. Returns (repo dir, workflow handle) — keep
    /// A mock project: temp dir with real `.coding/` files (for the carry
    /// mechanism) + a MockGit on `feat/prev` with a dirty backlog flip.
    /// Replaces the old `git_project()` which spawned ~10 git subprocesses.
    fn mock_git_project() -> (tempfile::TempDir, Arc<Mutex<Workflow>>, Arc<MockGit>) {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".coding/plans")).unwrap();
        std::fs::write(root.join(".coding/backlog.jsonl"), "{\"items\":[]}").unwrap();
        std::fs::write(root.join("file.txt"), "original\n").unwrap();
        // stack.json (untracked, local — never carried)
        std::fs::write(
            root.join(".coding/plans/stack.json"),
            "{\"stack\":[\"prev\"]}",
        )
        .unwrap();
        // Dirty backlog (InFlight flip)
        std::fs::write(
            root.join(".coding/backlog.jsonl"),
            "{\"id\":\"in-flight\",\"text\":\"x\",\"images\":[],\"status\":\"in_flight\",\"created_at\":1,\"note\":null}\n",
        )
        .unwrap();

        let git = Arc::new(MockGit::new());
        git.set_branch("feat/prev");
        git.add_branch("feat/prev");
        git.set_porcelain(" M .coding/backlog.jsonl\n");

        let plans_dir = root.join(".coding/plans");
        (dir, Arc::new(Mutex::new(Workflow::new(plans_dir))), git)
    }

    #[tokio::test]
    async fn create_plan_with_branch_forks_the_feature_branch() {
        let (dir, wf, git) = mock_git_project();
        let root = dir.path().to_path_buf();
        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let result = tool
            .execute(json!({
                "title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"],
                "branch": "fix/new-thing"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("created branch 'fix/new-thing'"),
            "output reports the fork: {}",
            result.output
        );
        // The mock switched to the new branch.
        assert_eq!(git.current_branch(), "fix/new-thing");
        // The dirty backlog flip was carried across and rides this item.
        let backlog = std::fs::read_to_string(root.join(".coding/backlog.jsonl")).unwrap();
        assert!(backlog.contains("in_flight"), "carried: {backlog}");
        // The plan itself was persisted and is executing on the new branch.
        assert_eq!(wf.lock().await.state(), WorkflowState::Executing);
    }

    #[tokio::test]
    async fn create_plan_stringified_null_branch_skips_branch_prep() {
        // Backlog 9118714a: the transport stringifies JSON null (and
        // auto-fills omitted optional properties with it) — create_plan
        // ran "git checkout null" and failed. The dispatch seam drops the
        // artifact from OPTIONAL properties; composed here with the tool
        // exactly as the seam composes them, the dropped branch means the
        // default auto-fork/reuse flow and no checkout of "null" happens.
        let (_dir, wf, git) = mock_git_project();
        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let mut args = json!({
            "title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"],
            "branch": "null"
        });
        crate::tool::drop_stringified_nulls(&tool.schema().parameters, &mut args);
        let result = tool.execute(args).await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("git checkout null"),
            "no checkout of the stringified artifact: {}",
            result.output
        );
        // The mock starts on feat/prev (not main): the default flow reuses
        // the current branch — no checkout ran at all.
        assert_eq!(git.current_branch(), "feat/prev");
        assert_eq!(wf.lock().await.state(), WorkflowState::Executing);
    }

    #[tokio::test]
    async fn create_plan_with_branch_skips_when_source_is_dirty() {
        let (dir, wf, git) = mock_git_project();
        let root = dir.path().to_path_buf();
        // Work in progress on a tracked file — real work must never ride the
        // automatic switch.
        std::fs::write(root.join("file.txt"), "wip\n").unwrap();
        git.set_porcelain(" M file.txt\n M .coding/backlog.jsonl\n");
        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let result = tool
            .execute(json!({
                "title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"], "branch": "fix/new"
            }))
            .await;
        assert!(result.success, "the plan must still be created");
        assert!(
            result.output.contains("branch prep skipped"),
            "output explains the skip: {}",
            result.output
        );
        assert_eq!(git.current_branch(), "feat/prev", "branch unchanged");
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "wip\n"
        );
    }

    #[tokio::test]
    async fn create_plan_invalid_branch_name_is_a_hard_error() {
        let (_dir, wf, git) = mock_git_project();
        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let result = tool
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"], "branch": "-x"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid branch"),
            "{}",
            result.output
        );
        // No plan was created — the agent retries with a fixed name rather
        // than orphaning a plan on the wrong branch.
        assert_eq!(wf.lock().await.state(), WorkflowState::Planning);
    }

    #[tokio::test]
    async fn create_plan_branch_ignored_mid_execution() {
        let (dir, wf, git) = mock_git_project();
        let _root = dir.path().to_path_buf();
        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let first = tool
            .execute(json!({"title": "Parent", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        assert!(first.success);

        // A sub-plan created mid-execution must NOT move branches — the
        // parent plan's work lives on the current one.
        let second = tool
            .execute(json!({
                "title": "Sub", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/config.rs"], "branch": "fix/sub"
            }))
            .await;
        assert!(second.success, "{}", second.output);
        assert!(
            second.output.contains("ignored"),
            "output says the branch was ignored: {}",
            second.output
        );
        assert_eq!(git.current_branch(), "feat/prev");
        assert_eq!(wf.lock().await.plan_depth(), 2, "the sub-plan still pushed");
    }

    // ── Auto-fork: one branch per agent directory (no `branch` arg) ────────

    #[tokio::test]
    async fn create_plan_auto_forks_when_on_main_and_no_branch_given() {
        // No `branch` arg + on main → create_plan auto-forks the stable
        // per-directory wt/* branch from main (one branch per agent
        // directory; work never silently stays on main).
        let (dir, wf, git) = mock_git_project();
        let root = dir.path().to_path_buf();
        // mock_git_project starts on feat/prev; switch to main.
        git.set_branch("main");
        let expected = crate::project::git_ops::work_branch_name(&root);

        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let result = tool
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            git.current_branch(),
            expected,
            "auto-forked the stable branch"
        );
        assert!(
            result.output.contains("created branch"),
            "output notes the auto-fork: {}",
            result.output
        );
        assert_eq!(wf.lock().await.state(), WorkflowState::Executing);
    }

    #[tokio::test]
    async fn create_plan_auto_reuses_current_branch_when_not_on_main() {
        // No `branch` arg + already on a non-main branch → reuse it (no fork,
        // no mutation). One branch per directory.
        let (dir, wf, git) = mock_git_project();
        let _root = dir.path().to_path_buf();
        // mock_git_project starts on feat/prev.
        let tool = CreatePlanTool::new(wf.clone()).with_git(git.clone());
        let result = tool
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            git.current_branch(),
            "feat/prev",
            "reused the current branch"
        );
        assert_eq!(wf.lock().await.state(), WorkflowState::Executing);
    }

    // ── StepInput (string-or-map) normalization (backlog #38) ────────────────

    #[test]
    fn step_input_to_text_passes_strings_through() {
        let step: StepInput = serde_json::from_value(json!("**Add X** — do it")).unwrap();
        assert_eq!(step.to_text(), "**Add X** — do it");
    }

    #[test]
    fn step_input_to_text_joins_header_and_body() {
        let step: StepInput =
            serde_json::from_value(json!({"header": "Add X", "body": "do it"})).unwrap();
        assert_eq!(step.to_text(), "**Add X** — do it");
    }

    #[test]
    fn step_input_to_text_header_only_when_body_empty() {
        // A map step with an empty (or omitted) body becomes just the bold header.
        let step: StepInput =
            serde_json::from_value(json!({"header": "Add X", "body": ""})).unwrap();
        assert_eq!(step.to_text(), "**Add X**");
        let step: StepInput = serde_json::from_value(json!({"header": "Add X"})).unwrap();
        assert_eq!(step.to_text(), "**Add X**");
    }

    #[test]
    fn step_input_to_text_unwraps_already_bold_header() {
        // Regression (backlog 2026-08-20, "****Verify builds/tests**** should
        // be rendered bold"): models pass the header already bold (the
        // schema's string-step example shows the bold form), and
        // double-wrapping stored `****X****` in the plan file — which the
        // header extractor then failed to parse, so the plan view showed raw
        // asterisks. Exactly ONE layer of markers must survive.
        let step: StepInput = serde_json::from_value(
            json!({"header": "**Verify builds/tests**", "body": "run tests"}),
        )
        .unwrap();
        assert_eq!(step.to_text(), "**Verify builds/tests** — run tests");
        // Header-only form too.
        let step: StepInput = serde_json::from_value(json!({"header": "**Verify**"})).unwrap();
        assert_eq!(step.to_text(), "**Verify**");
        // Already-DOUBLE-wrapped headers lose BOTH layers (fixpoint unwrap —
        // review L1: a single pass left `****X****` re-wrapped to the exact
        // broken form the backlog reported).
        let step: StepInput =
            serde_json::from_value(json!({"header": "****Add X****", "body": "do it"})).unwrap();
        assert_eq!(step.to_text(), "**Add X** — do it");
        // Single `*italics*` markers are NOT unwrapped (only `**…**` is).
        let step: StepInput = serde_json::from_value(json!({"header": "*Add X*"})).unwrap();
        assert_eq!(step.to_text(), "***Add X***");
        // `**` only mid-text stays as-is.
        let step: StepInput =
            serde_json::from_value(json!({"header": "Use **bold** here"})).unwrap();
        let text = step.to_text();
        assert!(text.starts_with("**Use **bold** here**"), "{text}");
    }

    #[tokio::test]
    async fn create_plan_normalizes_already_bold_map_header() {
        // End-to-end regression for the same defect: the STORED step text
        // (what the plan .md file gets) must be single-wrapped, and the
        // parsed header must extract cleanly for the toolbar/PlanProgress.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "T",
                "goal": "G",
                "context": GOOD_CTX,
                "steps": [{"header": "**Verify builds/tests**", "body": "run tests in src/widget.rs"}]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf = wf.lock().await;
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps[0].text, "**Verify builds/tests** — run tests in src/widget.rs");
        assert_eq!(plan.steps[0].header.as_deref(), Some("Verify builds/tests"));
    }

    #[tokio::test]
    async fn create_plan_accepts_map_step() {
        // Regression (backlog #38): a structured {header, body} step used to
        // fail with "invalid type: map, expected a string". It must now parse
        // and store the normalized "**header** — body" text.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "T",
                "goal": "G",
                "context": GOOD_CTX,
                "steps": [{"header": "Add X", "body": "do the thing in src/widget.rs"}]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf = wf.lock().await;
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps[0].text, "**Add X** — do the thing in src/widget.rs");
    }

    #[tokio::test]
    async fn create_plan_accepts_string_step_verbatim() {
        // Back-compat: plain-string steps must pass through unchanged.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "T",
                "goal": "G",
                "context": GOOD_CTX,
                "steps": ["**Do Y** — the recipe in src/widget.rs"]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf = wf.lock().await;
        assert_eq!(wf.plan().unwrap().steps[0].text, "**Do Y** — the recipe in src/widget.rs");
    }

    #[tokio::test]
    async fn create_plan_accepts_mixed_string_and_map_steps() {
        // A steps array mixing both forms normalizes each independently.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CreatePlanTool::new(wf.clone());
        let result = tool
            .execute(json!({
                "title": "T",
                "goal": "G",
                "context": GOOD_CTX,
                "steps": [
                    "**Plain step** — as a string in src/widget.rs",
                    {"header": "Map step", "body": "as an object in src/widget.rs"}
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let wf = wf.lock().await;
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].text, "**Plain step** — as a string in src/widget.rs");
        assert_eq!(plan.steps[1].text, "**Map step** — as an object in src/widget.rs");
    }

    #[tokio::test]
    async fn update_plan_accepts_map_step() {
        // update_plan shares the same steps input — map steps normalize there too.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        CreatePlanTool::new(wf.clone())
            .execute(json!({"title": "P", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"steps": [{"header": "New step", "body": "the recipe in src/widget.rs"}]}))
            .await;
        assert!(result.success, "{}", result.output);
        let wf = wf.lock().await;
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].text, "**New step** — the recipe in src/widget.rs");
    }

    #[tokio::test]
    async fn complete_step_works() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]
            }))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        let result = complete.execute(json!({"step_index": 1})).await;
        assert!(result.success);
        let wf = wf.lock().await;
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Executing);
        assert_eq!(wf.plan().unwrap().completed_count(), 1);
    }

    #[tokio::test]
    async fn complete_step_transitions_to_reviewing() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        complete.execute(json!({"step_index": 1})).await;
        let wf = wf.lock().await;
        // Final step → Reviewing (not Complete); finish is the only onward path.
        assert_eq!(wf.state(), crate::workflow::WorkflowState::Reviewing);
    }

    #[tokio::test]
    async fn finish_tool_closes_out_review() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        complete.execute(json!({"step_index": 1})).await;

        // Write a review report under the reviews dir (plans_dir.parent()
        // joined with "reviews" — i.e. dir/reviews here).
        let reviews_dir = dir.path().join("reviews");
        std::fs::create_dir_all(&reviews_dir).unwrap();
        let report = reviews_dir.join("review.md");
        std::fs::write(&report, "## Verdict: PASS\nno findings").unwrap();

        // finish with a valid report → Complete.
        let finish = FinishTool::new(wf.clone(), reviews_dir.clone());
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_blocks_on_findings_verdict() {
        // A FINDINGS verdict blocks finish — the agent must fix every finding
        // and re-run the reviewer before finishing.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        CompleteStepTool::new(wf.clone())
            .execute(json!({"step_index": 1}))
            .await;

        let reviews_dir = dir.path().join("reviews");
        std::fs::create_dir_all(&reviews_dir).unwrap();
        let report = reviews_dir.join("review.md");
        std::fs::write(
            &report,
            "## Verdict: FINDINGS (1 high, 0 low)\n\n- high: crash in x",
        )
        .unwrap();

        let finish = FinishTool::new(wf.clone(), reviews_dir);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(!res.success);
        assert!(res.output.contains("verdict is FINDINGS"), "{}", res.output);
        // Still Reviewing — finish did not proceed.
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    #[tokio::test]
    async fn finish_blocks_on_unparseable_verdict() {
        // A report without a parseable verdict line counts as FINDINGS (fail
        // closed) — it can never pass.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        CompleteStepTool::new(wf.clone())
            .execute(json!({"step_index": 1}))
            .await;

        let reviews_dir = dir.path().join("reviews");
        std::fs::create_dir_all(&reviews_dir).unwrap();
        let report = reviews_dir.join("review.md");
        std::fs::write(&report, "# Review\nno findings").unwrap();

        let finish = FinishTool::new(wf.clone(), reviews_dir);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(!res.success);
        assert!(
            res.output.contains("unparseable review verdict"),
            "{}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    #[tokio::test]
    async fn finish_resolves_project_relative_report_path() {
        // Backlog 5b46674d: the finish notification now carries the
        // project-relative path (".coding/reviews/<file>.md") — finish must
        // resolve it (as-given first, then the basename under the reviews
        // dir), not only the absolute path the notification used to carry.
        let dir = tempdir().unwrap();
        // Mirror the real layout: plans at <root>/.coding/plans → reviews
        // at <root>/.coding/reviews (the factory's derivation).
        let wf = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join(".coding").join("plans"),
        )));
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        CompleteStepTool::new(wf.clone())
            .execute(json!({"step_index": 1}))
            .await;

        let reviews_dir = dir.path().join(".coding").join("reviews");
        std::fs::create_dir_all(&reviews_dir).unwrap();
        // A test-unique filename: the as-given branch resolves relative
        // paths against the test-process CWD (the crate root), so a generic
        // name like "review.md" would depend on no such file existing
        // there — a unique probe name removes that environmental coupling
        // (review LOW 4).
        std::fs::write(
            reviews_dir.join("finish-relpath-probe-review.md"),
            "## Verdict: PASS\n\nno findings",
        )
        .unwrap();

        // The project-relative form the notification now carries.
        let finish = FinishTool::new(wf.clone(), reviews_dir);
        let res = finish
            .execute(json!({"review_report": ".coding/reviews/finish-relpath-probe-review.md"}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[test]
    fn parse_verdict_classifies_report_openers() {
        // The verdict contract: PASS / FINDINGS proceed as parsed; anything
        // else is Unparseable (fail closed).
        assert_eq!(
            parse_verdict("## Verdict: PASS\nno findings"),
            Verdict::Pass
        );
        assert_eq!(
            parse_verdict("## Verdict: FINDINGS (2 high, 1 low)\n- x"),
            Verdict::Findings
        );
        assert_eq!(
            parse_verdict("## Verdict: FINDINGS\n- x"),
            Verdict::Findings
        );
        // Leading blank lines are skipped; the first non-blank line decides.
        assert_eq!(parse_verdict("\n\n## Verdict: PASS\nx"), Verdict::Pass);
        // Anything else — including a legacy "# Review" opener — is
        // unparseable.
        assert_eq!(parse_verdict("# Review\nno findings"), Verdict::Unparseable);
        assert_eq!(parse_verdict("no findings"), Verdict::Unparseable);
        assert_eq!(parse_verdict(""), Verdict::Unparseable);
        assert_eq!(parse_verdict("## Verdict: maybe"), Verdict::Unparseable);
    }

    #[tokio::test]
    async fn finish_without_review_report_errors() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        CompleteStepTool::new(wf.clone())
            .execute(json!({"step_index": 1}))
            .await;

        // No report written — finish must error and stay in Reviewing.
        let reviews_dir = dir.path().join("reviews");
        let res = FinishTool::new(wf.clone(), reviews_dir)
            .execute(json!({"review_report": "missing.md"}))
            .await;
        assert!(!res.success);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    #[tokio::test]
    async fn finish_from_executing_errors() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        // Still Executing (step not done) — finish must error.
        let reviews_dir = dir.path().join("reviews");
        let res = FinishTool::new(wf.clone(), reviews_dir)
            .execute(json!({"review_report": "x"}))
            .await;
        assert!(!res.success);
    }

    #[tokio::test]
    async fn complete_step_out_of_range_errors() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": 99})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("1-indexed"),
            "out-of-range error should teach the 1-indexed convention, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_out_of_range_echoes_the_passed_number() {
        // Review L1: the error must echo the number the MODEL passed (1-indexed),
        // not the converted internal 0-indexed value — otherwise "step 3 out of
        // range ... valid 1..=3" reads like the passed 4 was valid.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs", "edit src/lib.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": 4})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("step 4 out of range"),
            "the error echoes the passed 1-indexed number, got: {}",
            result.output
        );
        assert!(
            result.output.contains("valid 1..=3"),
            "the error states the valid range, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_plan_id_overlong_prefix_gets_no_false_hint() {
        // Review L2: a provided id LONGER than the active id that starts with
        // it must not be hinted as "another plan on the stack ... not the
        // active plan" — that plan IS the active one. The hint falls back to
        // the generic advice.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let active_id = wf.lock().await.plan_id().unwrap().to_string();
        let overlong = format!("{active_id}ff00");
        let complete = CompleteStepTool::new(wf);
        let result = complete
            .execute(json!({"step_index": 1, "plan_id": overlong}))
            .await;
        assert!(!result.success, "an over-long id must error");
        assert!(
            !result.output.contains("on the plan stack"),
            "no false stack hint for the ACTIVE plan, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_rejects_zero_with_1_indexed_hint() {
        // Regression: step_index is 1-INDEXED (1 = the first step — the
        // numbers the plan document and the echo show). 0 must be rejected
        // with the convention stated, not silently mis-map to the first step.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        let result = complete.execute(json!({"step_index": 0})).await;
        assert!(!result.success, "step 0 must be rejected");
        assert!(
            result.output.contains("1-indexed"),
            "error should state the 1-indexed convention, got: {}",
            result.output
        );
        assert_eq!(
            wf.lock().await.plan().unwrap().completed_count(),
            0,
            "the rejected call must not complete anything"
        );
    }

    #[tokio::test]
    async fn update_plan_result_enumerates_applied_changes() {
        // Self-describing result (backlog d779060a): the output enumerates
        // WHAT was updated — the appended context with its char count, the
        // recorded regression test — not just the plan title/id.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let result = create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        assert!(result.success, "{}", result.output);
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({
                "context": "landed design — waves of 512 on scoped workers",
                "append": true,
                "regression_test": "large_fixture_indexes_on_worker_threads"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("context appended (+"),
            "output must enumerate the appended context with its char count, got: {}",
            result.output
        );
        assert!(
            result.output.contains("regression_test=large_fixture_indexes_on_worker_threads"),
            "output must name the recorded regression test, got: {}",
            result.output
        );
        // Steps clauses (review round 1, finding 1): the append arm counts
        // the passed steps; the replace arm reports the TRUE post-update
        // size (the engine preserves the completed prefix), not the passed
        // count.
        let result = update
            .execute(json!({"steps": ["edit src/second.rs"], "append": true}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("steps appended (+1)"),
            "append must count the passed steps, got: {}",
            result.output
        );
        let complete = CompleteStepTool::new(wf.clone());
        let result = complete.execute(json!({"step_index": 1})).await;
        assert!(result.success, "{}", result.output);
        let result = update
            .execute(json!({"steps": ["edit src/third.rs", "edit src/fourth.rs"]}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("steps replaced (2 → 3 steps)"),
            "replace must report the true post-update size (completed prefix \
             preserved + passed), got: {}",
            result.output
        );
        // Blank fields are "keep current" — never enumerated as changes
        // (review round 1, finding 2). An all-blank call is a no-op error.
        let result = update.execute(json!({"title": "", "goal": "  "})).await;
        assert!(!result.success, "an all-blank call must be a no-op error");
        let result = update
            .execute(json!({"title": "", "context": "more real text — long enough to clear the resumability gate"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("title set"),
            "a blank title is 'keep current', not a change, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_uses_1_indexed_numbering() {
        // Regression: step_index 1 completes the FIRST step and 2 the second,
        // matching the plan document's `- [x] 1.` numbering (previously
        // 0-indexed while every read surface was 1-indexed — the off-by-one).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        let result = complete.execute(json!({"step_index": 1})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Complete Step 1 'edit src/widget.rs' (1/2"),
            "echo must show the 1-indexed number and the step's text, got: {}",
            result.output
        );
        {
            let w = wf.lock().await;
            let plan = w.plan().unwrap();
            assert!(plan.steps[0].done, "step 1 = the first step");
            assert!(!plan.steps[1].done);
        }
        let result = complete.execute(json!({"step_index": 2})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("Complete Step 2 'edit src/config.rs' (2/2"),
            "echo must show the 1-indexed number and the step's text, got: {}",
            result.output
        );
        let w = wf.lock().await;
        assert!(w.plan().unwrap().steps[1].done, "step 2 = the second step");
    }

    #[tokio::test]
    async fn complete_step_accepts_numeric_string_step_index() {
        // Regression: models occasionally quote the number ("step_index":
        // "1") — the untagged StepNumber accepts it instead of failing with a
        // raw serde type error (same tolerant-shape precedent as StepInput).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": "1"})).await;
        assert!(
            result.success,
            "a numeric string step number must be accepted, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_rejects_non_numeric_string_step_index() {
        // A non-numeric string fails with an actionable message, not a raw
        // serde type error.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": "second"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("step_index must be a step number"),
            "error should be actionable, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn abandon_plan_pops_to_parent() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Main", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]}))
            .await;
        create
            .execute(json!({"title": "Sub", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        {
            let w = wf.lock().await;
            assert_eq!(w.plan_depth(), 2);
        }
        let abandon = AbandonPlanTool::new(wf.clone());
        let result = abandon.execute(json!({})).await;
        assert!(result.success);
        assert!(result.output.contains("resumed parent 'Main'"));
        let w = wf.lock().await;
        assert_eq!(w.plan_depth(), 1);
        assert_eq!(w.plan().unwrap().title, "Main");
    }

    #[tokio::test]
    async fn abandon_last_plan_returns_to_planning() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Main", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let abandon = AbandonPlanTool::new(wf.clone());
        let result = abandon.execute(json!({})).await;
        assert!(result.success);
        let w = wf.lock().await;
        assert_eq!(w.state(), crate::workflow::WorkflowState::Planning);
    }

    #[tokio::test]
    async fn abandon_without_plan_errors() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let abandon = AbandonPlanTool::new(wf);
        let result = abandon.execute(json!({})).await;
        assert!(!result.success);
    }

    #[tokio::test]
    async fn abandon_supersedes_lingering_step_markers() {
        // An abandoned plan's ACTIVE:/STEP MARKER: crash markers must be
        // superseded (never deleted) so they stop recalling as "resume me" —
        // previously they lingered until a later finish (regression for the
        // lingering-marker gap, user 2026-09-04).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Doomed", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let plan_id = wf.lock().await.plan_id().unwrap().to_string();

        // Seed the store with two markers mentioning the plan + one unrelated.
        let store = make_store();
        for (title, content) in [
            ("ACTIVE: doomed plan", format!("plan {plan_id} in progress")),
            ("STEP MARKER: doomed plan", format!("resume plan {plan_id}")),
            (
                "STEP MARKER: other plan",
                "plan other-999 in progress".to_string(),
            ),
        ] {
            store
                .write(crate::memory::Memory::new(
                    crate::memory::MemoryTier::Working,
                    title,
                    content,
                    500,
                ))
                .await
                .unwrap();
        }

        let abandon = AbandonPlanTool::new(wf.clone()).with_memory(store.clone());
        let result = abandon.execute(json!({})).await;
        assert!(result.success);
        assert!(
            result.output.contains("superseded 2 stale marker(s)"),
            "{}",
            result.output
        );

        // The markers are superseded (excluded from default recall); the
        // unrelated one stays live.
        let filter = crate::memory::MemoryFilter::new()
            .tier(crate::memory::MemoryTier::Working)
            .include_superseded();
        let all = store.list_filtered(&filter).await.unwrap();
        let superseded: Vec<_> = all.iter().filter(|m| m.superseded_by.is_some()).collect();
        assert_eq!(superseded.len(), 2, "both plan markers superseded");
        let live: Vec<_> = all.iter().filter(|m| m.superseded_by.is_none()).collect();
        assert!(
            live.iter().any(|m| m.title == "STEP MARKER: other plan"),
            "the unrelated marker stays live"
        );
        // The plan itself is still abandoned (state back to Planning).
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Planning
        );
    }

    #[tokio::test]
    async fn update_plan_appends_steps_preserving_completed() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "P", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs", "edit src/lib.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        complete.execute(json!({"step_index": 1})).await; // step 1 done
        complete.execute(json!({"step_index": 2})).await; // step 2 done
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"steps": ["edit src/main.rs", "edit src/util.rs"]}))
            .await;
        assert!(result.success);
        let w = wf.lock().await;
        let plan = w.plan().unwrap();
        // The two completed steps preserved done; remaining replaced with
        // the two new steps.
        assert_eq!(plan.steps.len(), 4);
        assert_eq!(plan.steps[0].text, "edit src/widget.rs");
        assert!(plan.steps[0].done);
        assert_eq!(plan.steps[1].text, "edit src/config.rs");
        assert!(plan.steps[1].done);
        assert_eq!(plan.steps[2].text, "edit src/main.rs");
        assert!(!plan.steps[2].done);
        assert_eq!(plan.steps[3].text, "edit src/util.rs");
        assert!(!plan.steps[3].done);
    }

    #[tokio::test]
    async fn update_plan_tool_append_adds_after_remaining_steps() {
        // The chunked-write protocol at the tool layer: append=true adds the
        // entries AFTER the remaining steps without resending them, and the
        // output says so.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "P", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]}))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"steps": ["edit src/main.rs"], "context": "extra", "append": true}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let w = wf.lock().await;
        let plan = w.plan().unwrap();
        // The two created steps kept; the appended step added after them;
        // context extended.
        assert_eq!(plan.steps.len(), 3);
        assert_eq!(plan.steps[0].text, "edit src/widget.rs");
        assert_eq!(plan.steps[1].text, "edit src/config.rs");
        assert_eq!(plan.steps[2].text, "edit src/main.rs");
        assert_eq!(plan.context, format!("{GOOD_CTX}\n\nextra"));
    }

    #[tokio::test]
    async fn update_plan_tool_append_refused_for_bug_fixing_skeleton() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "Fix",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "bug": "crash on open",
                "kind": "bug_fixing",
                "steps": []
            }))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"steps": ["extra"], "append": true}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("locked 4-step skeleton"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn update_plan_tool_appends_follow_on_in_reviewing_for_bug_fixing() {
        // Backlog 37f8631a: the skeleton lock's follow-on window, through the
        // tool — after the forced 4-step skeleton completes (state
        // Reviewing), update_plan(append=true) succeeds; the appended step
        // lands after the done skeleton, unchecked.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "Fix",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "bug": "crash on open",
                "kind": "bug_fixing",
                "steps": []
            }))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        for i in 1..=4 {
            let res = complete.execute(json!({"step_index": i})).await;
            assert!(res.success, "step {i}: {}", res.output);
        }
        {
            let w = wf.lock().await;
            assert_eq!(w.state(), WorkflowState::Reviewing);
        }
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"steps": ["fix the reviewer finding in src/widget.rs"], "append": true}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let w = wf.lock().await;
        assert_eq!(w.state(), WorkflowState::Reviewing);
        let plan = w.plan().unwrap();
        assert_eq!(plan.steps.len(), 5);
        for step in &plan.steps[..4] {
            assert!(step.done);
        }
        assert!(!plan.steps[4].done);
        assert_eq!(plan.steps[4].text, "fix the reviewer finding in src/widget.rs");
    }

    #[tokio::test]
    async fn update_plan_tool_gates_appended_bug_steps_for_resumability() {
        // Backlog 37f8631a: appended follow-on steps meet the same
        // path-or-marker resumability bar as every other step — the
        // bug-skeleton exemption flips off inside the Reviewing append
        // window (a path-free step is rejected with the actionable error;
        // a path-bearing one lands).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "Fix",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "bug": "crash on open",
                "kind": "bug_fixing",
                "steps": []
            }))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        for i in 1..=4 {
            let res = complete.execute(json!({"step_index": i})).await;
            assert!(res.success, "step {i}: {}", res.output);
        }
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"steps": ["re-run the suite"], "append": true}))
            .await;
        assert!(
            !result.success,
            "path-free step must be rejected: {}",
            result.output
        );
        assert!(
            result.output.contains("names no file path"),
            "{}",
            result.output
        );
        let result = update
            .execute(json!({"steps": ["fix the finding in src/widget.rs"], "append": true}))
            .await;
        assert!(result.success, "output: {}", result.output);
        let w = wf.lock().await;
        assert_eq!(w.plan().unwrap().steps.len(), 5);
    }

    #[tokio::test]
    async fn update_plan_edits_title_goal() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Old", "goal": "Old goal", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let result = update
            .execute(json!({"title": "New", "goal": "New goal"}))
            .await;
        assert!(result.success);
        let w = wf.lock().await;
        let plan = w.plan().unwrap();
        assert_eq!(plan.title, "New");
        assert_eq!(plan.goal, "New goal");
    }

    #[tokio::test]
    async fn update_plan_stringified_null_fields_leave_them_unchanged() {
        // Backlog 9118714a: the transport stringifies JSON null for string
        // params into the literal string "null" — update_plan(title:"null",
        // goal:"null") REPLACED the plan's title and goal with the string
        // "null" (data corruption, repaired manually). The dispatch seam
        // drops the artifact from OPTIONAL properties; composed here with
        // the tool exactly as the seam composes them, a dropped field means
        // "leave unchanged" while a real field still applies.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Old", "goal": "Old goal", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let update = UpdatePlanTool::new(wf.clone());
        let mut args = json!({"title": "null", "goal": "New goal"});
        crate::tool::drop_stringified_nulls(&update.schema().parameters, &mut args);
        let result = update.execute(args).await;
        assert!(result.success, "output: {}", result.output);
        let w = wf.lock().await;
        let plan = w.plan().unwrap();
        assert_eq!(plan.title, "Old", "the dropped title means unchanged");
        assert_eq!(plan.goal, "New goal", "the real goal still applies");
    }

    #[test]
    fn plan_tools_optional_string_params_advertise_nullable() {
        // Backlog 9118714a: optional string/enum params advertise
        // ["string", "null"] so explicit JSON null is legal end-to-end —
        // the spawn_agent.model precedent mirrored onto the plan tools.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        for field in ["bug", "branch", "base", "context", "kind"] {
            let ty = &create.schema().parameters["properties"][field]["type"];
            assert!(
                ty.as_array()
                    .is_some_and(|t| t.contains(&json!("string")) && t.contains(&json!("null"))),
                "create_plan.{field} must advertise [\"string\", \"null\"], got: {ty}"
            );
        }
        let update = UpdatePlanTool::new(wf);
        for field in ["title", "goal", "context", "regression_test"] {
            let ty = &update.schema().parameters["properties"][field]["type"];
            assert!(
                ty.as_array()
                    .is_some_and(|t| t.contains(&json!("string")) && t.contains(&json!("null"))),
                "update_plan.{field} must advertise [\"string\", \"null\"], got: {ty}"
            );
        }
    }

    #[tokio::test]
    async fn update_plan_noop_errors() {
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        CreatePlanTool::new(wf.clone())
            .execute(json!({"title": "P", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let update = UpdatePlanTool::new(wf);
        let result = update.execute(json!({})).await;
        assert!(!result.success);
        assert!(result.output.contains("changed nothing"));
        assert!(
            result.output.contains("in your reply FIRST"),
            "no-op error teaches the content-first rule: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn update_plan_blank_title_errors_as_noop() {
        // A whitespace-only title is treated as "leave unchanged" — which makes
        // the whole call a no-op, rejected with the same teach-back. Blank
        // fields mean the update content was never written (content-first).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        CreatePlanTool::new(wf.clone())
            .execute(json!({"title": "P", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let update = UpdatePlanTool::new(wf);
        let result = update.execute(json!({"title": "   "})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("changed nothing"),
            "blank title alone is a no-op: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn current_plan_returns_active_plan() {
        // current_plan returns the active plan's id + title + step counts.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "My Plan", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs", "edit src/lib.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        complete.execute(json!({"step_index": 1})).await; // 1 done
        let tool = CurrentPlanTool::new(wf.clone(), dir.path().join(".coding/plans"));
        let result = tool.execute(json!({})).await;
        assert!(result.success);
        assert!(result.output.contains("My Plan"));
        assert!(result.output.contains("1/3 steps done"));
        let data = result.data.unwrap();
        assert_eq!(data["title"], "My Plan");
        assert_eq!(data["completed"], 1);
        assert_eq!(data["total"], 3);
        assert!(data["id"].is_string(), "id is the plan's short hex handle");
    }

    #[tokio::test]
    async fn current_plan_on_subagent_state_returns_mirrored_plan() {
        // Round-3 warning #1 pin (backlog c5ded15d): a parented sub-agent's
        // workflow keeps the plan stack loaded (the read-only mirror) even
        // though its state is Subagent — current_plan gates on plan PRESENCE,
        // so a reviewer can still read the plan under review; the state is
        // only echoed.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Under Review", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]}))
            .await;
        {
            let mut w = wf.lock().await;
            w.enter_subagent_state();
        }
        let tool = CurrentPlanTool::new(wf.clone(), dir.path().join(".coding/plans"));
        let result = tool.execute(json!({})).await;
        assert!(result.success);
        assert!(
            result.output.contains("Under Review"),
            "the mirrored plan resolves for a Subagent-state workflow"
        );
        assert!(
            result.output.contains("state: Subagent"),
            "the state is echoed as Subagent: {}",
            result.output
        );
        let data = result.data.unwrap();
        assert_eq!(data["title"], "Under Review");
        assert_eq!(data["state"], "Subagent");
    }

    #[tokio::test]
    async fn current_plan_reports_the_plan_file_path() {
        // F2: the active plan's on-disk file is reported (existence-checked)
        // so an id hunt goes straight to the file instead of a zero-hit
        // content search.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let plans_dir = dir.path().join(".coding/plans");
        let tool = CurrentPlanTool::new(wf.clone(), &plans_dir);
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "My Plan", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        // The plan's short hex id comes from current_plan's own data shape.
        let id = tool.execute(json!({})).await.data.unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        std::fs::create_dir_all(&plans_dir).unwrap();
        std::fs::write(plans_dir.join(format!("{id}.md")), "# Plan\n").unwrap();
        let result = tool.execute(json!({})).await;
        assert!(result.success, "{}", result.output);
        let expected = format!(
            "{}/{}.md",
            plans_dir.to_string_lossy().replace('\\', "/"),
            id
        );
        assert!(result.output.contains(&expected), "{}", result.output);
        assert_eq!(result.data.unwrap()["plan_file"], expected);
        // Without the file on disk, the path is absent (null).
        std::fs::remove_file(plans_dir.join(format!("{id}.md"))).unwrap();
        let result = tool.execute(json!({})).await;
        assert!(result.success);
        assert!(result.data.unwrap()["plan_file"].is_null());
    }

    #[tokio::test]
    async fn current_plan_returns_no_active_plan_in_planning() {
        // With no plan created, current_plan reports "no active plan".
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let tool = CurrentPlanTool::new(wf, dir.path().join(".coding/plans"));
        let result = tool.execute(json!({})).await;
        assert!(result.success);
        assert!(result.output.contains("no active plan"));
        let data = result.data.unwrap();
        assert!(data["id"].is_null());
        assert!(data["title"].is_null());
    }

    #[tokio::test]
    async fn complete_step_echo_includes_plan_title() {
        // The result names the active plan so the agent stays oriented.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Build Feature X", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": 1})).await;
        assert!(result.success);
        assert!(
            result.output.contains("(1/1, Build Feature X)"),
            "result should echo the plan title compactly, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_plan_id_mismatch_errors() {
        // A wrong plan_id errors loudly instead of silently completing.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Active", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete
            .execute(json!({"step_index": 1, "plan_id": "wrong-id"}))
            .await;
        assert!(!result.success, "a mismatched plan_id must error");
        assert!(
            result.output.contains("mismatch"),
            "error should mention the mismatch, got: {}",
            result.output
        );
        assert!(
            result.output.contains("Active"),
            "error should name the active plan, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_plan_id_match_succeeds() {
        // The correct plan_id (from create_plan's result) completes normally.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let create_result = create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        // Extract the plan id from the create_plan result data.
        let plan_id = create_result.data.unwrap()["plan_id"]
            .as_str()
            .unwrap()
            .to_string();
        let complete = CompleteStepTool::new(wf);
        let result = complete
            .execute(json!({"step_index": 1, "plan_id": plan_id}))
            .await;
        assert!(result.success, "a matching plan_id must complete normally");
    }

    #[tokio::test]
    async fn complete_step_plan_id_prefix_match_succeeds() {
        // Regression: a truncated transcription of the plan id (>= 4 chars)
        // is accepted with a note instead of erroring — the guard should
        // tolerate transcription noise, not punish it.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let create_result = create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let plan_id = create_result.data.unwrap()["plan_id"]
            .as_str()
            .unwrap()
            .to_string();
        let prefix = plan_id[..4].to_string();
        let complete = CompleteStepTool::new(wf);
        let result = complete
            .execute(json!({"step_index": 1, "plan_id": prefix}))
            .await;
        assert!(
            result.success,
            "a unique-prefix plan_id must complete normally, got: {}",
            result.output
        );
        assert!(
            result.output.contains("matched the active id by prefix"),
            "the success echo should note the prefix normalization, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_plan_id_parent_hint_names_the_stack_plan() {
        // A parent-plan id passed while a sub-plan is active errors with a
        // did-you-mean hint naming the stacked plan — not a bare mismatch.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Main", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]}))
            .await;
        let parent_id = wf.lock().await.plan_id().unwrap().to_string();
        create
            .execute(json!({"title": "Sub", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete
            .execute(json!({"step_index": 1, "plan_id": parent_id}))
            .await;
        assert!(!result.success, "a parent plan_id must error");
        assert!(
            result.output.contains("on the plan stack"),
            "error should hint the id belongs to a stacked plan, got: {}",
            result.output
        );
        assert!(
            result.output.contains("Main"),
            "error should name the stacked plan, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_plan_id_stale_file_hint_names_the_old_plan() {
        // An id that matches an older plan FILE under .coding/plans/ (but no
        // stack frame) gets the older-plan hint.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "Active", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        // A finished plan file with a known stem sits in the plans dir.
        let plans_dir = dir.path().join("plans");
        std::fs::write(plans_dir.join("deadbeef.md"), "# Plan: Old\n").unwrap();
        let complete = CompleteStepTool::new(wf);
        let result = complete
            .execute(json!({"step_index": 1, "plan_id": "deadbeef"}))
            .await;
        assert!(!result.success, "an older plan's id must error");
        assert!(
            result.output.contains("older plan file"),
            "error should hint the id belongs to an older plan, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn complete_step_warns_on_out_of_order_completion() {
        // Regression: completing a step after the first not-done one warned
        // only later, when update_plan refused to replace the remaining
        // steps. The warning now fires at completion time — at the cause.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs", "edit src/lib.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": 3})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("completed out of order"),
            "the warning should fire at completion time, got: {}",
            result.output
        );
        assert_eq!(
            result.data.unwrap()["out_of_order"],
            json!(true),
            "data should flag the out-of-order completion"
        );
    }

    #[tokio::test]
    async fn complete_step_in_order_has_no_out_of_order_warning() {
        // The normal in-order path carries no warning noise.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs", "edit src/config.rs"]}))
            .await;
        let complete = CompleteStepTool::new(wf);
        let result = complete.execute(json!({"step_index": 1})).await;
        assert!(result.success, "{}", result.output);
        assert!(
            !result.output.contains("out of order"),
            "in-order completion must not warn, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn update_plan_echoes_the_plan_id() {
        // Regression: update_plan's result omits the plan id, forcing an
        // extra current_plan call to learn the active handle.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        let create_result = create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        let plan_id = create_result.data.unwrap()["plan_id"]
            .as_str()
            .unwrap()
            .to_string();
        let update = UpdatePlanTool::new(wf);
        let result = update.execute(json!({"title": "T2"})).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            result.data.unwrap()["plan_id"],
            json!(plan_id),
            "update_plan must echo the plan id in data"
        );
        assert!(
            result.output.contains(&format!("id: {plan_id}")),
            "update_plan must echo the plan id in the output, got: {}",
            result.output
        );
    }

    // ── Phase 4: finish gates + auto-capture ─────────────────────────────

    /// Drive a bug_fixing plan to Reviewing (create → complete the skeleton).
    /// When `test` is Some, the regression test is recorded (via update_plan)
    /// BEFORE the final step completes — update_plan only works in Executing.
    async fn bug_plan_to_reviewing(wf: &Arc<Mutex<Workflow>>, test: Option<&str>) {
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({
                "title": "Fix crash",
                "goal": "G",
                "context": "The app crashes on open; root cause: unguarded unwrap in src/app.rs:42. Regression test: app_crash_on_open in src/app/tests.rs fails without the fix.",
                "kind": "bug_fixing",
                "bug": "the app crashes on open"
            }))
            .await;
        let complete = CompleteStepTool::new(wf.clone());
        for i in 1..=3 {
            complete.execute(json!({"step_index": i})).await;
        }
        if let Some(test) = test {
            let update = UpdatePlanTool::new(wf.clone());
            let res = update.execute(json!({"regression_test": test})).await;
            assert!(res.success, "{}", res.output);
        }
        complete.execute(json!({"step_index": 4})).await;
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    /// Write a PASS-verdict review report under `dir/reviews`.
    fn write_pass_report(dir: &std::path::Path) -> std::path::PathBuf {
        let reviews_dir = dir.join("reviews");
        std::fs::create_dir_all(&reviews_dir).unwrap();
        let report = reviews_dir.join("review.md");
        std::fs::write(&report, "## Verdict: PASS\nno findings").unwrap();
        report
    }

    #[tokio::test]
    async fn finish_blocks_bug_plan_without_regression_test() {
        // The verify step must record the regression test name — finish is
        // blocked without it (the fix is unverifiable).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, None).await;
        let report = write_pass_report(dir.path());

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews"));
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(!res.success);
        assert!(
            res.output.contains("no regression test recorded"),
            "{}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    #[tokio::test]
    async fn finish_blocks_landed_design_without_context_amendment() {
        // Backlog e5a84ce9: a feature-scale bug fix records landed_design=true
        // — the finish gate then requires a "Landed design" context amendment
        // (the constitution's documentation expectations). Without it,
        // finish blocks with the amendment instruction.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("some_regression_test")).await;
        let update = UpdatePlanTool::new(wf.clone());
        let res = update.execute(json!({"landed_design": true})).await;
        assert!(res.success, "{}", res.output);
        let report = write_pass_report(dir.path());

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews"));
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(!res.success);
        assert!(
            res.output.contains("no 'Landed design' amendment")
                && res.output.contains("Documentation expectations"),
            "{}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    #[tokio::test]
    async fn finish_accepts_landed_design_with_amendment() {
        // With the context amended (a "Landed design —" paragraph), finish
        // proceeds and the output carries the SPEC-memory + BUG-amendment
        // reminder note (backlog e5a84ce9).
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("some_regression_test")).await;
        let update = UpdatePlanTool::new(wf.clone());
        let res = update
            .execute(json!({
                "landed_design": true,
                "context": "Landed design — waves of 512 on scoped workers; measured 67.5s mean.",
                "append": true
            }))
            .await;
        assert!(res.success, "{}", res.output);
        let report = write_pass_report(dir.path());

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews"));
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(
            res.output.contains("SPEC memory") && res.output.contains("BUG record"),
            "{}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_accepts_regression_test_param() {
        // The finish↔update_plan deadlock (2026-12-04): finish requires the
        // regression_test recorded via update_plan, but update_plan was blocked
        // in Reviewing. Relaxation part B: finish accepts an optional
        // regression_test param that records the test on the plan frame right
        // before the gate check. Before the fix the param is ignored by serde
        // (unknown field) and the gate blocks with "no regression test
        // recorded"; after, finish records it and proceeds.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        // NO pre-recorded regression test — we pass it via the finish param.
        bug_plan_to_reviewing(&wf, None).await;
        let report = write_pass_report(dir.path());

        // The test symbol must exist in the code graph (the gate validates it).
        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        std::fs::write(
            dir.path().join("tests.rs"),
            "fn deadlock_regression_test() {}\n",
        )
        .unwrap();
        graph.index(None).unwrap();

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews")).with_codegraph(graph);
        let res = finish
            .execute(json!({
                "review_report": report.to_string_lossy(),
                "regression_test": "deadlock_regression_test"
            }))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_blocks_bug_plan_when_test_symbol_missing_from_graph() {
        // With the code graph wired, the recorded test name must resolve to a
        // real symbol — a name that doesn't exist blocks finish.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("no_such_test")).await;
        let report = write_pass_report(dir.path());

        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews")).with_codegraph(graph);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(!res.success);
        assert!(
            res.output.contains("not found in the code graph"),
            "{}",
            res.output
        );
        // (backlog 648e0ad5) The error must name the case: no code file
        // on disk contains the name — the wrong-test-name case, verified
        // after the forced re-index (not a refresh failure).
        assert!(
            res.output.contains("no code file on disk contains the name"),
            "{}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing
        );
    }

    #[tokio::test]
    async fn finish_refreshes_stale_graph_and_finds_new_regression_test() {
        // Backlog #91: the gate resolved the test name against the
        // last-indexed snapshot only, so a regression test written after the
        // last index pass (no file watcher running, or finish racing the
        // watcher's debounce) blocked finish — forcing the manual
        // helper-crate reindex dance. The gate must self-freshen: one
        // incremental index pass on miss, then retry the resolve.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("late_added_regression")).await;
        let report = write_pass_report(dir.path());

        // A graph whose index is stale by construction: opened BEFORE the
        // test file exists, never indexed — exactly the no-watcher scenario.
        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        std::fs::write(
            dir.path().join("tests.rs"),
            "fn late_added_regression() {}\n",
        )
        .unwrap();

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews")).with_codegraph(graph);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_force_reindexes_when_meta_fresh_but_symbols_missing() {
        // Backlog 648e0ad5 (live case 2027-01-08: 'checkFillContract'): the
        // gate's incremental refresh is mtime/hash-based — a DB whose meta
        // rows are fresh but whose SYMBOL rows are missing (a partial
        // write) never re-parses the file, so the gate errored on a
        // regression-test symbol that exists on disk, and the dispatched
        // agent chased a phantom rename. The gate must escalate to a
        // forced re-index of the files containing the name and find the
        // symbol.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("phantom_row_regression")).await;
        let report = write_pass_report(dir.path());

        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        std::fs::write(
            dir.path().join("tests.rs"),
            "fn phantom_row_regression() {}\n",
        )
        .unwrap();
        graph.index(None).unwrap();
        // Corrupt: meta rows fresh (hash + mtime + content), symbol rows
        // gone — the partial-write scenario. The incremental refresh
        // no-ops (the hash matches), so only the forced escalation can
        // find the symbol.
        graph.simulate_partial_write_for_test("tests.rs").unwrap();

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews")).with_codegraph(graph);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_passes_when_regression_test_is_in_non_indexed_language() {
        // The gate's disk check used to filter to code-graph extensions
        // (.rs/.ts/.tsx only): a regression test written in ANY other
        // programming language (.js, .py, .swift, …) was invisible to the
        // containment scan, so finish errored "no source file on disk
        // contains the name" on a test that exists and is runnable — a
        // false error that blocked the agent and steered it toward a
        // phantom rename. The gate must scan every code file: a name found
        // on disk in a language the graph does not parse satisfies the
        // symbol check by disk containment (with a transparent note), and
        // only a name in NO code file errors.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("swift_gate_regression")).await;
        let report = write_pass_report(dir.path());

        // The regression test lives in a language no grammar parses (.swift):
        // the file is on disk (and in the content index), but the symbol
        // graph can never resolve the name.
        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        std::fs::write(
            dir.path().join("tests.swift"),
            "func swift_gate_regression() {\n    assert(true)\n}\n",
        )
        .unwrap();
        graph.index(None).unwrap();

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews")).with_codegraph(graph);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(
            res.output.contains("disk containment"),
            "the pass must carry the disk-containment note: {}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_resolves_arrow_const_regression_test_in_js() {
        // HIGH-1 (review 2026-09-11): `const testFoo = () => {}` — the
        // dominant jest/vitest idiom — produced no symbol (visit_ts had
        // no lexical_declaration arm), so the gate dead-ended in "not
        // found even after a forced re-index" — the exact false-error
        // class this plan fixes, for the shape modern JS tests use.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("js_arrow_gate_regression")).await;
        let report = write_pass_report(dir.path());

        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        std::fs::write(
            dir.path().join("tests.js"),
            "const js_arrow_gate_regression = () => {\n  assert(true);\n};\n",
        )
        .unwrap();
        graph.index(None).unwrap();

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews")).with_codegraph(graph);
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }

    #[tokio::test]
    async fn finish_passes_bug_plan_with_test_in_graph_and_captures() {
        // Full happy path: regression test recorded + present in the graph +
        // memory wired → finish proceeds, the PLAN:/BUG: digests are
        // captured, and the state transitions to Complete.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        bug_plan_to_reviewing(&wf, Some("crash_on_open_regression")).await;
        let report = write_pass_report(dir.path());

        // A graph containing the test symbol: write the test file into the
        // graph's root and index the whole tree.
        let graph = Arc::new(
            crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap(),
        );
        std::fs::write(
            dir.path().join("tests.rs"),
            "fn crash_on_open_regression() {}\n",
        )
        .unwrap();
        graph.index(None).unwrap();

        // A memory store wired into finish.
        let embedder: Arc<dyn crate::memory::Embedder> =
            Arc::new(crate::memory::embedder::HashEmbedder::new());
        let concrete = Arc::new(crate::memory::MemoryStore::open_in_memory(embedder).unwrap());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> = concrete.clone();

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews"))
            .with_codegraph(graph)
            .with_memory(store.clone());
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
        // The capture ran: a PLAN: digest + a BUG: digest exist (authored).
        let (authored, derived) = concrete.count_by_class().await.unwrap();
        assert_eq!((authored, derived), (2, 0), "PLAN: + BUG: digests captured");
        assert!(res.output.contains("captured PLAN:"), "{}", res.output);
        assert!(res.output.contains("captured BUG:"), "{}", res.output);
        // The nudge is present.
        assert!(res.output.contains("SPEC:/DECISION:"), "{}", res.output);
    }

    #[tokio::test]
    async fn finish_without_memory_skips_capture_with_note() {
        // No memory store wired → finish still proceeds, with a note.
        let dir = tempdir().unwrap();
        let wf = make_workflow(dir.path());
        let create = CreatePlanTool::new(wf.clone());
        create
            .execute(json!({"title": "T", "goal": "G", "context": GOOD_CTX, "steps": ["edit src/widget.rs"]}))
            .await;
        CompleteStepTool::new(wf.clone())
            .execute(json!({"step_index": 1}))
            .await;
        let report = write_pass_report(dir.path());

        let finish = FinishTool::new(wf.clone(), dir.path().join("reviews"));
        let res = finish
            .execute(json!({"review_report": report.to_string_lossy()}))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(
            res.output
                .contains("no memory store — finish capture skipped"),
            "{}",
            res.output
        );
        assert_eq!(
            wf.lock().await.state(),
            crate::workflow::WorkflowState::Complete
        );
    }
}
