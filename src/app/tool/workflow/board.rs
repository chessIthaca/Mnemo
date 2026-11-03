// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The plan-board tools of the Mnemo2 lead (`.coding/design/mnemo2.md`
//! Phase 6): `plan_create`, `plan_update`, `plan_show`, `plan_list`,
//! `plan_step_done`, `plan_abandon`. They replace the classic
//! `create_plan` / `update_plan` / `complete_step` / `finish` /
//! `abandon_plan` in TeamLead mode and are `AutoRun` — they only rewrite the
//! project's own `.coding/plans/` and `.coding/reviews/` bookkeeping.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::app::plan_board::{
    BoardPlan, BoardStatus, CloseOutcome, PlanBoard, PlanDraft, PlanEdit,
};
use crate::app::team_ledger::TeamLedger;
use crate::app::tool::workflow::plan::StepNumber;
use crate::app::workflow::PlanKind;
use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// How many closed plans `plan_list` shows after the open ones.
const CLOSED_SHOWN: usize = 5;

/// Parse a tool call's arguments or return the sanitized error result.
fn parse_args<T: serde::de::DeserializeOwned>(
    name: &str,
    args: serde_json::Value,
) -> Result<T, ToolResult> {
    serde_json::from_value(args).map_err(|e| {
        ToolResult::error(crate::tool::error_message::sanitize_arguments_error(
            name, &e,
        ))
    })
}

fn kind_from(token: Option<&str>) -> Result<PlanKind, String> {
    match token.map(str::trim).filter(|t| !t.is_empty()) {
        None | Some("implementation") => Ok(PlanKind::Implementation),
        Some("research") => Ok(PlanKind::Research),
        Some("bug_fixing") => Ok(PlanKind::BugFixing),
        Some(other) => Err(format!(
            "unknown kind '{other}' — use implementation, research, or bug_fixing"
        )),
    }
}

/// One line per step: mark, number, header or the start of the text.
fn step_lines(plan: &BoardPlan) -> String {
    plan.steps
        .iter()
        .map(|s| {
            let label = s.header.as_deref().unwrap_or(&s.text);
            let label: String = label.chars().take(100).collect();
            format!("[{}] {}. {label}", if s.done { 'x' } else { ' ' }, s.number)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plan_data(plan: &BoardPlan) -> serde_json::Value {
    serde_json::to_value(plan).unwrap_or(serde_json::Value::Null)
}

// ── advisory spec notes ───────────────────────────────────────────────────

/// The heading the advisory spec notes are appended under.
const SPEC_NOTES_HEADING: &str = "\n\nSpec notes (advisory; nothing was refused):";

/// Matches a `name(args): Type` shape — one of the signature signals
/// [`spec_gaps`] accepts beside `fn `, `->` and `=>`.
static SIGNATURE_SHAPED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\w+\([^)]*\)\s*:").expect("the signature-shape regex is valid")
});

/// Whether `text` carries a signature or a type shape.
fn has_signature(text: &str) -> bool {
    text.contains("fn ")
        || text.contains("->")
        || text.contains("=>")
        || SIGNATURE_SHAPED.is_match(text)
}

/// The spec gaps of one step's text, in a fixed order. Advisory only: the
/// caller notes them in the success output and never refuses the plan. A
/// `no-code` step is exempt — it has no signatures or tests to name.
fn spec_gaps(step_text: &str) -> Vec<&'static str> {
    let lower = step_text.to_lowercase();
    if lower.contains("no-code") {
        return Vec::new();
    }
    let mut gaps = Vec::new();
    if step_text.split_whitespace().count() < 120 {
        gaps.push("under 120 words");
    }
    if !has_signature(step_text) {
        gaps.push("no signatures");
    }
    if !lower.contains("test") {
        gaps.push("no tests named");
    }
    if !lower.contains("done") {
        gaps.push("no done checklist");
    }
    gaps
}

/// The advisory note block for `steps` (1-based number + text), or the empty
/// string when every step is a full spec. The plan is already created or
/// updated when this runs, so a gap is noted, never enforced.
fn spec_notes(steps: &[(usize, &str)]) -> String {
    let mut lines = Vec::new();
    for &(number, text) in steps {
        let gaps = spec_gaps(text);
        if !gaps.is_empty() {
            lines.push(format!("- step {number}: {}", gaps.join(", ")));
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{SPEC_NOTES_HEADING}\n{}", lines.join("\n"))
    }
}

// ── plan_create ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CreateArgs {
    title: String,
    goal: String,
    context: String,
    steps: Vec<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    bug: Option<String>,
    #[serde(default)]
    interfaces: Vec<String>,
    #[serde(default)]
    acceptance_scenarios: Vec<String>,
}

/// `plan_create` — open a new plan on the board.
pub struct PlanCreateTool {
    board: Arc<PlanBoard>,
}

impl PlanCreateTool {
    pub fn new(board: Arc<PlanBoard>) -> Self {
        Self { board }
    }
}

#[async_trait]
impl Tool for PlanCreateTool {
    fn name(&self) -> &str {
        "plan_create"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "plan_create",
            "Open a new plan on the board. Plan ALL real work first; any number of plans may \
             run at once. The plan is the record of intent that every worker receives, so put \
             the substance in `context`: why the work exists, constraints, intent, decisions already \
             taken, file anchors, how to verify; code carries none of it. Context is stable \
             intent that every worker reads, so never put execution state in it (commits, \
             review status, what is next belongs in plan_update `progress`). When two steps \
             touch the same file or module boundary, record the contract in `interfaces` \
             (or later with plan_update `interface`) before dispatching them. Every step must name the file path(s) it touches; a bug_fixing plan's context must name its regression test (which test, which file, how it fails without the fix). One step per independently deliverable unit; \
             name the files each step touches. List the observable outcomes in \
             `acceptance_scenarios` (or add them later with plan_update \
             `add_acceptance_scenarios`): workers receive them, and a review_risk='high' \
             dispatch on this plan requires at least one. Delegate each step with \
             team_dispatch(plan_id, step, ...). The plan stays on disk after it closes.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "Short plan title."},
                    "goal": {"type": "string", "description": "The outcome, in one or two sentences."},
                    "context": {"type": "string", "description": "The record of intent, min 40 chars; workers read it. State WHY the work exists, the CONSTRAINTS, the key DECISIONS already taken, exact file paths/symbols, and the verification commands. Stable intent only; no execution state (commits, review status, next actions). Code does not carry this."},
                    "steps": {"type": "array", "items": {"type": "string"}, "description": "Ordered steps, each a bold **header** followed by an implementation spec a cheaper model can follow without design decisions: the files and the functions/types to add or change with signatures (mark new code as new), the data shapes, the behaviour rules and edge cases, the order of edits, every test by name with its setup and assertions, a done checklist, and the files not to touch. Longer is better than vague."},
                    "kind": {"type": "string", "enum": ["implementation", "research", "bug_fixing"], "description": "Default implementation. bug_fixing needs `bug` and, to close, a recorded regression test."},
                    "bug": {"type": "string", "description": "The symptom (bug_fixing only)."},
                    "interfaces": {"type": "array", "items": {"type": "string"}, "description": "Contracts between steps, one per entry: exports, signatures, data shapes, file ownership, e.g. \"`src/game.js` — exports `Game` as a named export; UI owns rendering and must not modify it\". Record one whenever two steps touch the same file or module boundary, before dispatching them."},
                    "acceptance_scenarios": {"type": "array", "items": {"type": "string"}, "description": "Observable outcomes the finished work must show, one per entry: a command and its result, a persisted state, a concurrency or security property, a UI behavior. Kept in the plan's '## Acceptance scenarios' section and given to every worker; a review_risk='high' dispatch requires at least one."}
                },
                "required": ["title", "goal", "context", "steps"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: CreateArgs = match parse_args(self.name(), args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let kind = match kind_from(args.kind.as_deref()) {
            Ok(k) => k,
            Err(e) => return ToolResult::error(e),
        };
        let query = match &args.bug {
            Some(bug) => format!("{} {} {bug}", args.title, args.goal),
            None => format!("{} {}", args.title, args.goal),
        };
        let draft = PlanDraft {
            title: args.title.clone(),
            goal: args.goal,
            context: args.context,
            kind,
            bug: args.bug.clone(),
            steps: args.steps,
            interfaces: args.interfaces,
            acceptance: args.acceptance_scenarios,
        };
        let plan = match self.board.create(draft) {
            Ok(plan) => plan,
            Err(e) => return ToolResult::error(format!("plan_create failed: {e}")),
        };
        let mut output = format!(
            "Created plan {} \"{}\" ({}) with {} steps:\n{}\nDelegate with team_dispatch(plan_id: \"{}\", step: N, ...).",
            plan.id,
            plan.title,
            plan.kind,
            plan.steps.len(),
            step_lines(&plan),
            plan.id
        );
        // Advisory spec notes: a thin step is noted, never refused — the plan
        // already exists, and the lead decides whether the step is enough.
        let step_specs: Vec<(usize, &str)> = plan
            .steps
            .iter()
            .map(|s| (s.number, s.text.as_str()))
            .collect();
        output.push_str(&spec_notes(&step_specs));
        // Passive recall rides the result, like create_plan: prior knowledge
        // arrives with the plan. Best-effort; never blocks creation.
        if let Some(rider) = crate::tool::steering::recalled_context_block(
            self.board.memory(),
            Some(&args.title),
            &query,
            args.bug.as_deref(),
        )
        .await
        {
            output.push_str(&rider);
        }
        ToolResult {
            success: true,
            output,
            data: Some(plan_data(&plan)),
        }
    }
}

// ── plan_update ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct UpdateArgs {
    plan_id: String,
    #[serde(default)]
    context_append: Option<String>,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    interface: Option<String>,
    #[serde(default)]
    progress: Option<String>,
    #[serde(default)]
    add_steps: Vec<String>,
    #[serde(default)]
    add_acceptance_scenarios: Vec<String>,
    #[serde(default)]
    regression_test: Option<String>,
    #[serde(default)]
    landed_design: bool,
}

/// `plan_update` — record decisions and extend an open plan.
pub struct PlanUpdateTool {
    board: Arc<PlanBoard>,
}

impl PlanUpdateTool {
    pub fn new(board: Arc<PlanBoard>) -> Self {
        Self { board }
    }
}

#[async_trait]
impl Tool for PlanUpdateTool {
    fn name(&self) -> &str {
        "plan_update"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "plan_update",
            "Extend an open plan without rewriting what is recorded: record a `decision` (a \
             dated entry in the plan's Decisions — do this whenever you settle a question that \
             workers or a future session must know), record an `interface` contract (exports, \
             signatures, data shapes, file ownership — REQUIRED before dispatching two steps \
             that touch the same file or module boundary), record execution state with \
             `progress` (commits, review status, what is next), append to the context (stable \
             why, constraints, intent only — never status, because every worker brief copies \
             it), add steps, add observable acceptance scenarios (required before a \
             review_risk='high' dispatch), record a bug_fixing plan's regression test, or flag a feature-scale fix with \
             landed_design (closing then needs a \"Landed design\" context paragraph).",
            json!({
                "type": "object",
                "properties": {
                    "plan_id": {"type": "string"},
                    "decision": {"type": "string", "description": "A decision taken and why."},
                    "interface": {"type": "string", "description": "A contract between steps, dated into Interfaces: exports, signatures, data shapes, file ownership. Workers must follow it exactly."},
                    "progress": {"type": "string", "description": "Execution state, dated into Progress: commits, review status, what is next. Not shown to workers."},
                    "context_append": {"type": "string", "description": "A paragraph of stable intent appended to the context (why, constraints). Workers read the context, so never put execution state here; use `progress`."},
                    "add_steps": {"type": "array", "items": {"type": "string"}, "description": "Ordered steps, each a bold **header** followed by an implementation spec a cheaper model can follow without design decisions: the files and the functions/types to add or change with signatures (mark new code as new), the data shapes, the behaviour rules and edge cases, the order of edits, every test by name with its setup and assertions, a done checklist, and the files not to touch. Longer is better than vague."},
                    "add_acceptance_scenarios": {"type": "array", "items": {"type": "string"}, "description": "Observable outcomes appended to the plan's '## Acceptance scenarios', one per entry; workers receive them, and a review_risk='high' dispatch requires at least one. A scenario already recorded is not repeated."},
                    "regression_test": {"type": "string", "description": "The regression test name (bug_fixing)."},
                    "landed_design": {"type": "boolean"}
                },
                "required": ["plan_id"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: UpdateArgs = match parse_args(self.name(), args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        // Steps this call adds are the plan's tail; their numbers are the
        // plan's own 1-based numbers, so the advisory note can name them.
        let added = args
            .add_steps
            .iter()
            .filter(|s| !s.trim().is_empty())
            .count();
        let edit = PlanEdit {
            context_append: args.context_append,
            decision: args.decision,
            interface: args.interface,
            progress_note: args.progress,
            add_steps: args.add_steps,
            add_acceptance: args.add_acceptance_scenarios,
            regression_test: args.regression_test,
            landed_design: args.landed_design,
        };
        match self.board.update(&args.plan_id, edit) {
            Ok(plan) => {
                let mut output = format!(
                    "Updated plan {} \"{}\" ({} steps)",
                    plan.id,
                    plan.title,
                    plan.steps.len()
                );
                if added > 0 {
                    let start = plan.steps.len().saturating_sub(added);
                    let step_specs: Vec<(usize, &str)> = plan.steps[start..]
                        .iter()
                        .map(|s| (s.number, s.text.as_str()))
                        .collect();
                    output.push_str(&spec_notes(&step_specs));
                }
                ToolResult {
                    success: true,
                    output,
                    data: Some(plan_data(&plan)),
                }
            }
            Err(e) => ToolResult::error(format!("plan_update failed: {e}")),
        }
    }
}

// ── plan_show / plan_list ─────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ShowArgs {
    plan_id: String,
}

/// `plan_show` — one plan in full.
pub struct PlanShowTool {
    board: Arc<PlanBoard>,
}

impl PlanShowTool {
    pub fn new(board: Arc<PlanBoard>) -> Self {
        Self { board }
    }
}

#[async_trait]
impl Tool for PlanShowTool {
    fn name(&self) -> &str {
        "plan_show"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "plan_show",
            "Show one plan in full: status, goal, context, acceptance scenarios, steps, decisions, interfaces, progress.",
            json!({
                "type": "object",
                "properties": {"plan_id": {"type": "string"}},
                "required": ["plan_id"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: ShowArgs = match parse_args(self.name(), args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let plan = match self.board.get(&args.plan_id) {
            Ok(plan) => plan,
            Err(e) => return ToolResult::error(e),
        };
        let markdown = match self.board.read_markdown(&args.plan_id) {
            Ok(text) => text,
            Err(e) => return ToolResult::error(e),
        };
        ToolResult {
            success: true,
            output: format!(
                "Plan {} — status: {}\n\n{markdown}",
                plan.id,
                plan.status.as_str()
            ),
            data: Some(plan_data(&plan)),
        }
    }
}

/// `plan_list` — the board at a glance.
pub struct PlanListTool {
    board: Arc<PlanBoard>,
}

impl PlanListTool {
    pub fn new(board: Arc<PlanBoard>) -> Self {
        Self { board }
    }
}

#[async_trait]
impl Tool for PlanListTool {
    fn name(&self) -> &str {
        "plan_list"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "plan_list",
            "List the board: every open plan with its progress, then recently closed plans.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let plans = match self.board.list() {
            Ok(plans) => plans,
            Err(e) => return ToolResult::error(e),
        };
        let mut lines = Vec::new();
        let mut shown = Vec::new();
        let mut closed_shown = 0;
        for plan in &plans {
            if plan.status != BoardStatus::Open {
                if closed_shown >= CLOSED_SHOWN {
                    continue;
                }
                closed_shown += 1;
            }
            let done = plan.steps.iter().filter(|s| s.done).count();
            lines.push(format!(
                "{} [{}] \"{}\" {} {done}/{} steps",
                plan.id,
                plan.status.as_str(),
                plan.title,
                plan.kind,
                plan.steps.len()
            ));
            shown.push(plan);
        }
        ToolResult {
            success: true,
            output: if lines.is_empty() {
                "The board is empty.".to_string()
            } else {
                lines.join("\n")
            },
            data: Some(serde_json::to_value(&shown).unwrap_or(serde_json::Value::Null)),
        }
    }
}

// ── plan_step_done ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct StepDoneArgs {
    plan_id: String,
    step: StepNumber,
    #[serde(default)]
    note: String,
}

/// `plan_step_done` — tick a lead-only step; the plan closes when complete.
pub struct PlanStepDoneTool {
    board: Arc<PlanBoard>,
    ledger: Arc<dyn TeamLedger>,
}

impl PlanStepDoneTool {
    pub fn new(board: Arc<PlanBoard>, ledger: Arc<dyn TeamLedger>) -> Self {
        Self { board, ledger }
    }
}

#[async_trait]
impl Tool for PlanStepDoneTool {
    fn name(&self) -> &str {
        "plan_step_done"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "plan_step_done",
            "Tick a step YOU did yourself (lead-only work). Steps handled by workers tick \
             automatically when their tasks merge; this is refused while the step has an open \
             task. When it was the last step, the plan closes with a review report if the \
             team review passes.",
            json!({
                "type": "object",
                "properties": {
                    "plan_id": {"type": "string"},
                    "step": {"type": "integer", "description": "1-based step number."},
                    "note": {"type": "string", "description": "One line: what was done."}
                },
                "required": ["plan_id", "step"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: StepDoneArgs = match parse_args(self.name(), args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let number = match args.step.parse() {
            Ok(n) if n >= 1 => n as usize,
            Ok(_) => return ToolResult::error("step is 1-based"),
            Err(e) => return ToolResult::error(e),
        };
        let open = self
            .ledger
            .open_tasks(&args.plan_id, Some(number - 1))
            .await;
        if !open.is_empty() {
            let tasks = open
                .iter()
                .map(|t| format!("{} ({})", t.worker_id, t.status))
                .collect::<Vec<_>>()
                .join(", ");
            return ToolResult::error(format!(
                "plan_step_done refused: step {number} still has open team tasks: {tasks}. \
                 Merge them (team_review, team_merge) or cancel them; the step then ticks on \
                 its own"
            ));
        }
        let note = if args.note.trim().is_empty() {
            "done by the lead"
        } else {
            args.note.trim()
        };
        let ticked = match self.board.tick(&args.plan_id, number - 1, note) {
            Ok(ticked) => ticked,
            Err(e) => return ToolResult::error(format!("plan_step_done failed: {e}")),
        };
        let mut output = if ticked {
            format!("Step {number} of plan {} done.", args.plan_id)
        } else {
            format!("Step {number} of plan {} was already done.", args.plan_id)
        };
        match self
            .board
            .try_close(&args.plan_id, self.ledger.as_ref())
            .await
        {
            CloseOutcome::Closed {
                report_path,
                retired_notes,
            } => {
                output.push_str(&format!(" Plan closed; review report: {report_path}"));
                if retired_notes > 0 {
                    output.push_str(&format!(
                        " {retired_notes} plan-scoped memory note(s) were retired into that report; re-store any durable fact from them without plan or branch references."
                    ));
                }
            }
            CloseOutcome::NotReady { reasons } => {
                output.push_str(&format!(" Still open:\n- {}", reasons.join("\n- ")));
            }
            CloseOutcome::Blocked { reasons } => {
                output.push_str(&format!(
                    " The plan cannot close yet:\n- {}",
                    reasons.join("\n- ")
                ));
            }
        }
        ToolResult {
            success: true,
            output,
            data: None,
        }
    }
}

// ── plan_abandon ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct AbandonArgs {
    plan_id: String,
    reason: String,
}

/// `plan_abandon` — close a plan without review.
pub struct PlanAbandonTool {
    board: Arc<PlanBoard>,
}

impl PlanAbandonTool {
    pub fn new(board: Arc<PlanBoard>) -> Self {
        Self { board }
    }
}

#[async_trait]
impl Tool for PlanAbandonTool {
    fn name(&self) -> &str {
        "plan_abandon"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "plan_abandon",
            "Close a plan WITHOUT review, recording why. Last resort: the plan file stays as \
             the record. Stop or cancel its workers separately.",
            json!({
                "type": "object",
                "properties": {
                    "plan_id": {"type": "string"},
                    "reason": {"type": "string"}
                },
                "required": ["plan_id", "reason"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: AbandonArgs = match parse_args(self.name(), args) {
            Ok(a) => a,
            Err(e) => return e,
        };
        if args.reason.trim().is_empty() {
            return ToolResult::error("plan_abandon needs a reason");
        }
        match self.board.abandon(&args.plan_id, &args.reason) {
            Ok(plan) => ToolResult {
                success: true,
                output: format!("Abandoned plan {} \"{}\".", plan.id, plan.title),
                data: Some(plan_data(&plan)),
            },
            Err(e) => ToolResult::error(format!("plan_abandon failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::team_ledger::fake::FakeLedger;
    use crate::app::team_ledger::OpenTask;

    fn board(dir: &std::path::Path) -> Arc<PlanBoard> {
        Arc::new(PlanBoard::new(dir.join("plans"), dir.join("reviews")))
    }

    async fn create(board: &Arc<PlanBoard>) -> String {
        let res = PlanCreateTool::new(board.clone())
            .execute(json!({
                "title": "Importer",
                "goal": "Fix the importer",
                "context": "Rows vanish on empty cells; keep the CLI flags.",
                "steps": ["Fix src/import.rs", "Test tests/import.rs"]
            }))
            .await;
        assert!(res.success, "{}", res.output);
        res.data.unwrap()["id"].as_str().unwrap().to_string()
    }

    /// A step that satisfies every spec check: 120+ words, a signature, a
    /// named test, and a done checklist.
    fn full_spec_step() -> String {
        let rule = "The loader keeps every existing default and rejects silently corrupt input. ";
        format!(
            "**Load the config** — edit src/config.rs: new `fn load(path: &Path) -> Result<Config>` \
             parses the file. Rule: a blank file is an error, not an empty config. The test \
             `load_rejects_empty` asserts the error. Done: `cargo test --lib config` is green. {}",
            rule.repeat(16)
        )
    }

    #[tokio::test]
    async fn the_board_tools_create_update_show_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let id = create(&board).await;

        let res = PlanUpdateTool::new(board.clone())
            .execute(json!({"plan_id": id, "decision": "use serde", "add_steps": ["Docs"]}))
            .await;
        assert!(res.success, "{}", res.output);
        let shown = PlanShowTool::new(board.clone())
            .execute(json!({"plan_id": id}))
            .await;
        assert!(shown.output.contains("status: open"), "{}", shown.output);
        assert!(shown.output.contains("use serde"));
        assert!(shown.output.contains("3. Docs"));
        let listed = PlanListTool::new(board.clone()).execute(json!({})).await;
        assert!(listed.output.contains(&format!("{id} [open] \"Importer\"")));
        assert!(listed.output.contains("0/3 steps"));

        let bad = PlanCreateTool::new(board.clone())
            .execute(json!({"title": "T", "goal": "G", "context": "C", "steps": [], "kind": "x"}))
            .await;
        assert!(!bad.success);
        assert!(bad.output.contains("unknown kind"), "{}", bad.output);
        assert!(
            !PlanShowTool::new(board)
                .execute(json!({"plan_id": "nope"}))
                .await
                .success
        );
    }

    #[tokio::test]
    async fn create_notes_thin_steps_without_refusing() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let res = PlanCreateTool::new(board)
            .execute(json!({
                "title": "Importer",
                "goal": "Fix the importer",
                "context": "Rows vanish on empty cells; keep the CLI flags.",
                "steps": ["**One** — edit src/lib.rs"]
            }))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(
            res.output
                .contains("Spec notes (advisory; nothing was refused)"),
            "{}",
            res.output
        );
        assert!(
            res.output.contains(
                "step 1: under 120 words, no signatures, no tests named, no done checklist"
            ),
            "{}",
            res.output
        );
    }

    #[tokio::test]
    async fn create_with_a_full_spec_step_has_no_notes() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let step = full_spec_step();
        assert!(step.split_whitespace().count() >= 120, "{step}");
        let res = PlanCreateTool::new(board)
            .execute(json!({
                "title": "Config loader",
                "goal": "Load the config from disk",
                "context": "The loader keeps every default and rejects corrupt input; cargo test --lib config verifies it.",
                "steps": [step]
            }))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(!res.output.contains("Spec notes"), "{}", res.output);
    }

    #[tokio::test]
    async fn no_code_steps_are_not_noted() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let res = PlanCreateTool::new(board)
            .execute(json!({
                "title": "Retry policy",
                "goal": "Pick the retry policy",
                "context": "Compare the retry policies in use; no source changes result from this.",
                "steps": ["no-code research — compare retry policies"]
            }))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(!res.output.contains("Spec notes"), "{}", res.output);
    }

    #[tokio::test]
    async fn add_steps_notes_thin_steps() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let res = PlanCreateTool::new(board.clone())
            .execute(json!({
                "title": "Config loader",
                "goal": "Load the config from disk",
                "context": "The loader keeps every default and rejects corrupt input; cargo test --lib config verifies it.",
                "steps": [full_spec_step()]
            }))
            .await;
        assert!(res.success, "{}", res.output);
        let id = res.data.unwrap()["id"].as_str().unwrap().to_string();
        let res = PlanUpdateTool::new(board.clone())
            .execute(json!({"plan_id": id, "add_steps": ["**Two** — edit src/a.rs"]}))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(
            res.output
                .contains("Spec notes (advisory; nothing was refused)"),
            "{}",
            res.output
        );
        assert!(res.output.contains("step 2:"), "{}", res.output);
        // An update that adds no steps notes nothing.
        let res = PlanUpdateTool::new(board)
            .execute(json!({"plan_id": id, "decision": "use serde"}))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(!res.output.contains("Spec notes"), "{}", res.output);
    }

    #[tokio::test]
    async fn acceptance_scenarios_flow_through_plan_create_update_and_show() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let res = PlanCreateTool::new(board.clone())
            .execute(json!({
                "title": "Importer",
                "goal": "Fix the importer",
                "context": "Rows vanish on empty cells; keep the CLI flags.",
                "steps": ["Fix src/import.rs"],
                "acceptance_scenarios": ["Importing a file with empty cells keeps every row."]
            }))
            .await;
        assert!(res.success, "{}", res.output);
        let id = res.data.unwrap()["id"].as_str().unwrap().to_string();
        let res = PlanUpdateTool::new(board.clone())
            .execute(json!({
                "plan_id": id,
                "add_acceptance_scenarios": ["The CLI flags parse unchanged."],
                "progress": "parser fixed"
            }))
            .await;
        assert!(res.success, "{}", res.output);
        let shown = PlanShowTool::new(board)
            .execute(json!({"plan_id": id}))
            .await;
        assert!(
            shown.output.contains(
                "## Acceptance scenarios\n- Importing a file with empty cells keeps every row.\n- The CLI flags parse unchanged.\n"
            ),
            "{}",
            shown.output
        );
    }

    #[tokio::test]
    async fn plan_step_done_is_refused_while_the_step_has_open_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let id = create(&board).await;
        let ledger = Arc::new(FakeLedger::with_open(vec![OpenTask {
            worker_id: "w1abcdef".into(),
            name: "one".into(),
            status: "working".into(),
            step_index: Some(1),
        }]));
        let tool = PlanStepDoneTool::new(board.clone(), ledger.clone());
        let res = tool
            .execute(json!({"plan_id": id, "step": 2, "note": "did it"}))
            .await;
        assert!(!res.success);
        assert!(res.output.contains("w1abcdef (working)"), "{}", res.output);
        // The 1-based tool step maps to the ledger's 0-based step.
        assert_eq!(ledger.asked.lock().unwrap()[0], (id.clone(), Some(1)));
        assert!(!board.get(&id).unwrap().steps[1].done);

        // A numeric string works, and nothing is ticked on a refusal.
        let res = tool.execute(json!({"plan_id": id, "step": "1"})).await;
        assert!(!res.success);
        assert_eq!(ledger.asked.lock().unwrap()[1].1, Some(0));
        assert!(!board.get(&id).unwrap().steps[0].done);
    }

    #[tokio::test]
    async fn plan_step_done_ticks_and_closes_the_finished_plan() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let id = create(&board).await;
        let tool = PlanStepDoneTool::new(board.clone(), Arc::new(FakeLedger::passing()));
        let res = tool
            .execute(json!({"plan_id": id, "step": 1, "note": "edited by the lead"}))
            .await;
        assert!(res.success, "{}", res.output);
        assert!(res.output.contains("Still open"), "{}", res.output);
        assert!(res.output.contains("step 2 is not done"), "{}", res.output);

        let res = tool.execute(json!({"plan_id": id, "step": 2})).await;
        assert!(res.success, "{}", res.output);
        assert!(
            res.output.contains("Plan closed; review report:"),
            "{}",
            res.output
        );
        assert_eq!(board.get(&id).unwrap().status, BoardStatus::Closed);
        assert!(
            !tool
                .execute(json!({"plan_id": id, "step": 0}))
                .await
                .success
        );
    }

    #[tokio::test]
    async fn plan_step_done_reports_a_blocked_close_and_abandon_records_why() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let id = create(&board).await;
        let tool = PlanStepDoneTool::new(
            board.clone(),
            Arc::new(FakeLedger::blocked(&["finding f1 is open"])),
        );
        tool.execute(json!({"plan_id": id, "step": 1})).await;
        let res = tool.execute(json!({"plan_id": id, "step": 2})).await;
        assert!(res.success);
        assert!(res.output.contains("cannot close yet"), "{}", res.output);
        assert!(res.output.contains("finding f1 is open"), "{}", res.output);
        assert_eq!(board.get(&id).unwrap().status, BoardStatus::Open);

        let abandon = PlanAbandonTool::new(board.clone());
        assert!(
            !abandon
                .execute(json!({"plan_id": id, "reason": " "}))
                .await
                .success
        );
        let res = abandon
            .execute(json!({"plan_id": id, "reason": "not needed"}))
            .await;
        assert!(res.success, "{}", res.output);
        assert_eq!(board.get(&id).unwrap().status, BoardStatus::Abandoned);
    }
}
