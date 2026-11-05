// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Mnemo2 plan board (`.coding/design/mnemo2.md` Phase 6).
//!
//! The lead holds any number of plans at once; each is a Mnemo1-format plan
//! file under `.coding/plans/` (steps, `## Progress`, `## Decisions`), and
//! `board.json` beside them records which plans are open, closed, or
//! abandoned. Plans are the record of intent: they outlive their close and
//! are digested into project memory.
//!
//! Step numbers in this API are 1-based where a model or a worker sees them
//! (`brief_for`, [`BoardStep::number`]) and 0-based where the host's task
//! records and the [`TeamLedger`] use them (`tick`, `untick`); each method
//! says which.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use crate::app::backlog::{BacklogStatus, BacklogStore};
use crate::app::openspec::archive;
use crate::app::openspec::schema::{ResolvedSchema, Schema};
use crate::app::team_ledger::{PlanReview, TeamLedger};
use crate::app::tool::workflow::plan::{
    bug_fixing_gate, collapse_whitespace, context_issue, escape_section_markers,
    resumability_gate_error, step_issue,
};
use crate::app::workflow::plan_file::render_spec_changes;
use crate::app::workflow::{PlanFile, PlanKind};
use crate::openspec::{apply, Capability, Delta, Spec};
use crate::codegraph::CodeGraph;
use crate::memory::knowledge::KnowledgeStore;
use crate::memory::{plan_scope, Memory, MemoryFilter, MemoryRecordType, MemoryStoreTrait};

/// The board file name inside the plans dir.
const BOARD_FILE: &str = "board.json";

/// The header of the step appended when steps run in parallel.
const INTEGRATION_HEADER: &str = "Integration check";
const INTEGRATION_STEP: &str = "**Integration check** — run the combined result (build, tests, or launching the entry point) and verify the parallel steps fit together; fix any seam breakage.";

/// Where a board plan stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoardStatus {
    /// Being worked; the lead's tail lists it.
    Open,
    /// Every step done and the close gates passed; a review report exists.
    Closed,
    /// Closed without review, with a recorded reason.
    Abandoned,
}

impl BoardStatus {
    /// The lowercase token the UI and the tools show.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// One step of a board plan, as the lead and the UI see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardStep {
    /// 1-based step number.
    pub number: usize,
    /// The bold punchline, when the step text starts with one.
    pub header: Option<String>,
    /// The full step text.
    pub text: String,
    /// Whether the step is checked off.
    pub done: bool,
}

/// A board plan: the plan file's steps joined with the board's status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoardPlan {
    /// The 8-hex plan id (the plan file's stem).
    pub id: String,
    pub title: String,
    pub goal: String,
    /// `implementation`, `research`, or `bug_fixing`.
    pub kind: String,
    pub status: BoardStatus,
    /// Epoch milliseconds the plan closed or was abandoned.
    pub closed_at_ms: Option<u64>,
    /// The closing review report (absolute path), once closed.
    pub report_path: Option<String>,
    /// Structured OpenSpec changes, also rendered into the plan markdown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec_deltas: Vec<Delta>,
    /// OpenSpec schema reference captured when the plan was created.
    pub schema: String,
    /// Additional schema artifacts keyed by artifact id.
    pub sections: BTreeMap<String, String>,
    pub steps: Vec<BoardStep>,
}

/// What `plan_create` takes.
#[derive(Debug, Clone, Default)]
pub struct PlanDraft {
    pub title: String,
    pub goal: String,
    /// Why the work exists, constraints, prior decisions.
    pub context: String,
    pub kind: PlanKind,
    /// The symptom of a `bug_fixing` plan (required for that kind).
    pub bug: Option<String>,
    pub steps: Vec<String>,
    /// Interface contracts (exports, signatures, data shapes, file
    /// ownership) recorded as dated entries in `## Interfaces`.
    pub interfaces: Vec<String>,
    /// Observable acceptance scenarios, one per entry, recorded as bullets
    /// in `## Acceptance scenarios` (high-risk dispatch requires one).
    pub acceptance: Vec<String>,
    /// OpenSpec changes to validate, store, and render with this plan.
    pub spec_deltas: Vec<Delta>,
}

/// What `plan_update` takes. Nothing already recorded is rewritten.
#[derive(Debug, Clone, Default)]
pub struct PlanEdit {
    /// Appended to the plan's context as a new paragraph.
    pub context_append: Option<String>,
    /// Appended as a dated entry to `## Decisions`.
    pub decision: Option<String>,
    /// Appended as a dated entry to `## Interfaces`: a contract between
    /// steps (exports, signatures, data shapes, file ownership).
    pub interface: Option<String>,
    /// Appended as a dated line to `## Progress`: execution state (commits,
    /// review status, what is next). Never part of a worker brief.
    pub progress_note: Option<String>,
    /// New steps appended after the existing ones.
    pub add_steps: Vec<String>,
    /// Acceptance scenarios appended to `## Acceptance scenarios`; one
    /// already recorded is not repeated.
    pub add_acceptance: Vec<String>,
    /// OpenSpec changes appended to the plan's existing deltas.
    pub spec_deltas: Vec<Delta>,
    /// The regression test of a `bug_fixing` plan.
    pub regression_test: Option<String>,
    /// Declare the fix feature-scale (the close then needs a "Landed design"
    /// context amendment).
    pub landed_design: bool,
}

/// The result of [`PlanBoard::try_close`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The plan closed; the review report is at `report_path`.
    /// `retired_notes` counts the plan-scoped memory notes the close
    /// retired into the report (`memory::plan_scope`).
    Closed {
        report_path: String,
        retired_notes: usize,
    },
    /// Steps are still undone.
    NotReady { reasons: Vec<String> },
    /// The team review or a kind gate refused; the plan stays open.
    Blocked { reasons: Vec<String> },
}

fn default_schema_reference() -> String {
    "spec-driven@1".to_string()
}

/// One `board.json` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BoardEntry {
    id: String,
    status: BoardStatus,
    created_at_ms: u64,
    #[serde(default)]
    closed_at_ms: Option<u64>,
    #[serde(default)]
    report_path: Option<String>,
    #[serde(default)]
    abandon_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spec_deltas: Vec<Delta>,
    /// The schema selected at creation; later config changes do not replace it.
    #[serde(default = "default_schema_reference")]
    schema: String,
    /// Additional schema artifacts keyed by artifact id.
    #[serde(default)]
    sections: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct BoardFile {
    plans: Vec<BoardEntry>,
}

/// The plans of one project. Every call takes the interior lock, so
/// concurrent tool calls and host commands serialize on the files.
pub struct PlanBoard {
    plans_dir: PathBuf,
    reviews_dir: PathBuf,
    memory: Option<Arc<dyn MemoryStoreTrait>>,
    knowledge: Option<Arc<KnowledgeStore>>,
    codegraph: Option<Arc<CodeGraph>>,
    /// The external-lead close gate is opt-in so the built-in lead keeps its
    /// prompt-driven memory workflow.
    external_lead_attached: AtomicBool,
    lock: Mutex<()>,
}

impl PlanBoard {
    /// A board over `plans_dir` (`.coding/plans/`) writing close reports to
    /// `reviews_dir` (`.coding/reviews/`).
    pub fn new(plans_dir: impl Into<PathBuf>, reviews_dir: impl Into<PathBuf>) -> Self {
        Self {
            plans_dir: plans_dir.into(),
            reviews_dir: reviews_dir.into(),
            memory: None,
            knowledge: None,
            codegraph: None,
            external_lead_attached: AtomicBool::new(false),
            lock: Mutex::new(()),
        }
    }

    /// Wire the memory store the PLAN:/BUG: capture on close writes to.
    pub fn with_memory(mut self, store: Arc<dyn MemoryStoreTrait>) -> Self {
        self.memory = Some(store);
        self
    }

    /// Wire the knowledge files the BUG: capture lands in.
    pub fn with_knowledge(mut self, knowledge: Arc<KnowledgeStore>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Wire the code graph the `bug_fixing` regression-test gate resolves in.
    pub fn with_codegraph(mut self, graph: Arc<CodeGraph>) -> Self {
        self.codegraph = Some(graph);
        self
    }

    /// Enable or disable the durable-knowledge close gate with the external
    /// lead attachment. Every close path observes the same atomic setting.
    pub fn set_external_lead_attached(&self, attached: bool) {
        self.external_lead_attached.store(attached, Ordering::Release);
    }

    /// The memory store, for the `plan_create` recall rider.
    pub fn memory(&self) -> Option<&Arc<dyn MemoryStoreTrait>> {
        self.memory.as_ref()
    }

    /// Every board plan: open ones first (oldest first), then closed and
    /// abandoned ones, most recently closed first. A plan whose file cannot
    /// be read is skipped.
    pub fn list(&self) -> Result<Vec<BoardPlan>, String> {
        let _guard = self.guard();
        let mut open = Vec::new();
        let mut done = Vec::new();
        for entry in self.load_board()?.plans {
            let Ok(plan) = self.load_plan(&entry.id) else {
                continue;
            };
            let view = board_plan(&entry, &plan);
            if entry.status == BoardStatus::Open {
                open.push(view);
            } else {
                done.push(view);
            }
        }
        done.sort_by_key(|b| std::cmp::Reverse(b.closed_at_ms));
        open.extend(done);
        Ok(open)
    }

    /// One board plan.
    pub fn get(&self, id: &str) -> Result<BoardPlan, String> {
        let _guard = self.guard();
        let (entry, plan) = self.load(id)?;
        Ok(board_plan(&entry, &plan))
    }

    /// The plan file's markdown, for display.
    pub fn read_markdown(&self, id: &str) -> Result<String, String> {
        let _guard = self.guard();
        self.load(id)?;
        std::fs::read_to_string(self.plans_dir.join(format!("{id}.md")))
            .map_err(|e| format!("could not read plan {id}: {e}"))
    }

    /// Advisory OpenSpec reminders for capabilities touched by the plan's
    /// steps but absent from its deltas. Metadata problems suppress only the
    /// reminder; they never refuse plan creation or update.
    pub fn spec_reminders(&self, plan: &BoardPlan) -> Vec<String> {
        let specs_dir = self.specs_dir();
        let capabilities = Capability::load_all(&specs_dir).unwrap_or_default();
        let paths: Vec<String> = plan
            .steps
            .iter()
            .flat_map(|step| step_paths(&step.text))
            .collect();
        capabilities
            .into_iter()
            .filter(|capability| {
                specs_dir
                    .join(&capability.name)
                    .join("spec.md")
                    .is_file()
            })
            .filter(|capability| {
                paths.iter().any(|path| capability.matches(path))
                    && !plan
                        .spec_deltas
                        .iter()
                        .any(|delta| delta.capability == capability.name)
            })
            .map(|capability| {
                format!(
                    "Spec reminder: this plan touches {} (.coding/openspec/specs/{}/spec.md) but changes none of its requirements.",
                    capability.name, capability.name
                )
            })
            .collect()
    }

    /// Create an open plan and write its plan file. An `## Acceptance
    /// scenarios` section written into the context joins the plan's
    /// acceptance scenarios instead of being escaped into the context.
    pub fn create(&self, draft: PlanDraft) -> Result<BoardPlan, String> {
        self.create_with_sections(draft, BTreeMap::new())
    }

    /// Create a plan with additional artifacts for its resolved OpenSpec schema.
    /// Existing callers use [`create`](Self::create), which supplies no sections.
    pub fn create_with_sections(
        &self,
        draft: PlanDraft,
        sections: BTreeMap<String, String>,
    ) -> Result<BoardPlan, String> {
        let title = collapse_whitespace(&draft.title);
        let goal = draft.goal.trim();
        let steps = clean_steps(&draft.steps);
        let (context, lifted) = lift_acceptance(&draft.context);
        if title.is_empty() || goal.is_empty() || context.is_empty() {
            return Err("a plan needs a title, a goal, and a context".to_string());
        }
        if steps.is_empty() {
            return Err("a plan needs at least one step".to_string());
        }
        let bug = draft
            .bug
            .as_deref()
            .map(collapse_whitespace)
            .filter(|b| !b.is_empty());
        if draft.kind == PlanKind::BugFixing && bug.is_none() {
            return Err("a bug_fixing plan needs the bug symptom".to_string());
        }
        // The same resumability bar create_plan applies. A board plan's steps
        // are real work (no locked bug skeleton), so every kind gets the
        // step check.
        let issues: Vec<String> = context_issue(&context, draft.kind, None)
            .into_iter()
            .chain(step_issue(&steps, false))
            .collect();
        if !issues.is_empty() {
            return Err(resumability_gate_error(
                "plan too thin to resume from",
                &issues,
            ));
        }
        self.validate_spec_deltas(&draft.spec_deltas)?;
        let resolved = Schema::resolve(&self.openspec_dir(), None)?;
        let _guard = self.guard();
        let mut board = self.load_board()?;
        let id = self.fresh_id(&board);
        let mut plan = PlanFile::new(
            title,
            escape_section_markers(goal),
            escape_section_markers(&context),
            steps,
        );
        plan.kind = draft.kind;
        for interface in &draft.interfaces {
            let interface = collapse_whitespace(interface);
            if !interface.is_empty() {
                push_line(&mut plan.interfaces, format!("- {}: {interface}", today()));
            }
        }
        add_scenarios(&mut plan, draft.acceptance.iter().chain(&lifted));
        plan.bug_symptom = bug.filter(|_| draft.kind == PlanKind::BugFixing);
        let entry = BoardEntry {
            id: id.clone(),
            status: BoardStatus::Open,
            created_at_ms: now_ms(),
            closed_at_ms: None,
            report_path: None,
            abandon_reason: None,
            spec_deltas: draft.spec_deltas,
            schema: resolved.schema.reference(),
            sections,
        };
        self.validate_artifacts(&resolved, &plan, &entry.spec_deltas, &entry.sections)?;
        self.validate_artifacts(&resolved, &plan, &entry.spec_deltas, &entry.sections)?;
        std::fs::create_dir_all(&self.plans_dir).map_err(|e| e.to_string())?;
        self.save_plan(&id, &plan, &entry)?;
        let view = board_plan(&entry, &plan);
        board.plans.push(entry);
        self.save_board(&board)?;
        Ok(view)
    }

    /// The project's backlog file (`.coding/backlog.jsonl`), derived from the
    /// plans dir like every other `.coding` path. The board owns no backlog
    /// state of its own — the kernel store over this file is the one source
    /// of truth, re-read before every mutation.
    fn backlog_file(&self) -> PathBuf {
        self.plans_dir
            .parent()
            .unwrap_or(&self.plans_dir)
            .join("backlog.jsonl")
    }

    /// Finish the "Plan this" handoff after a plan was created: the backlog
    /// item the lead was handed becomes `Done` — its idea now lives as the
    /// plan — and its `plan_id`/`plan_title` record which one. Both writes go
    /// through the guarded table ([`BacklogStore::transition`], then
    /// [`BacklogStore::set_plan_id`], each re-reading the file first, so a
    /// concurrent backlog editor never loses its write): a live `Pending` or
    /// `InFlight` item (the plan_this handoff marks it `InFlight`) links and
    /// finishes; anything else — an unknown id, an already finished item —
    /// changes nothing and returns `false`, so the caller warns instead of
    /// failing the plan. Returns `true` when the item was linked and marked
    /// done.
    pub fn link_backlog_item(&self, backlog_id: &str, plan: &BoardPlan) -> bool {
        let mut store = BacklogStore::open(self.backlog_file());
        if !store.transition(
            backlog_id,
            BacklogStatus::Done,
            Some(format!("planned as {}", plan.title)),
        ) {
            return false;
        }
        store.set_plan_id(backlog_id, Some(&plan.id), Some(&plan.title))
    }

    /// Edit an open plan: append context, record a decision, add steps,
    /// acceptance scenarios, or OpenSpec deltas, set the regression test or the landed-design
    /// flag. As in [`create`](Self::create), an `## Acceptance scenarios`
    /// section in the appended context joins the acceptance scenarios.
    pub fn update(&self, id: &str, edit: PlanEdit) -> Result<BoardPlan, String> {
        self.update_with_sections(id, edit, None)
    }

    /// Edit a plan and merge additional schema artifacts. An empty section
    /// removes an optional artifact; required artifacts still fail validation.
    pub fn update_with_sections(
        &self,
        id: &str,
        mut edit: PlanEdit,
        sections: Option<BTreeMap<String, String>>,
    ) -> Result<BoardPlan, String> {
        let _guard = self.guard();
        let (mut entry, mut plan) = self.load(id)?;
        require_open(&entry)?;
        let resolved = Schema::resolve(&self.openspec_dir(), Some(&entry.schema))?;
        let sections_changed = sections.is_some();
        if let Some(updates) = sections {
            for (artifact, text) in updates {
                if text.trim().is_empty() {
                    entry.sections.remove(&artifact);
                } else {
                    entry.sections.insert(artifact, text);
                }
            }
        }
        if !edit.spec_deltas.is_empty() {
            let mut combined = entry.spec_deltas.clone();
            combined.extend(edit.spec_deltas.iter().cloned());
            self.validate_spec_deltas(&combined)?;
        }
        let (extra, lifted) = lift_acceptance(edit.context_append.as_deref().unwrap_or(""));
        if !extra.is_empty() {
            plan.context = format!("{}\n\n{}", plan.context, escape_section_markers(&extra));
        }
        add_scenarios(&mut plan, edit.add_acceptance.iter().chain(&lifted));
        if let Some(decision) = edit.decision.as_deref().map(collapse_whitespace) {
            if !decision.is_empty() {
                push_line(&mut plan.decisions, format!("- {}: {decision}", today()));
            }
        }
        if let Some(interface) = edit.interface.as_deref().map(collapse_whitespace) {
            if !interface.is_empty() {
                push_line(&mut plan.interfaces, format!("- {}: {interface}", today()));
            }
        }
        if let Some(note) = edit.progress_note.as_deref().map(collapse_whitespace) {
            if !note.is_empty() {
                push_line(&mut plan.progress, format!("- {}: {note}", today()));
            }
        }
        for text in clean_steps(&edit.add_steps) {
            let index = plan.steps.len();
            plan.steps.push(crate::app::workflow::Step {
                index,
                header: crate::memory::plan_format::extract_bold_header(&text),
                text,
                done: false,
            });
        }
        if let Some(test) = edit.regression_test.as_deref().map(collapse_whitespace) {
            if !test.is_empty() {
                plan.regression_test = Some(test);
            }
        }
        if edit.landed_design {
            plan.landed_design = true;
        }
        let deltas_changed = !edit.spec_deltas.is_empty();
        entry.spec_deltas.append(&mut edit.spec_deltas);
        self.validate_artifacts(&resolved, &plan, &entry.spec_deltas, &entry.sections)?;
        self.save_plan(id, &plan, &entry)?;
        if deltas_changed || sections_changed {
            let mut board = self.load_board()?;
            let stored = board
                .plans
                .iter_mut()
                .find(|candidate| candidate.id == id)
                .ok_or_else(|| unknown(id))?;
            stored.spec_deltas = entry.spec_deltas.clone();
            stored.sections = entry.sections.clone();
            self.save_board(&board)?;
        }
        Ok(board_plan(&entry, &plan))
    }

    /// The worker brief header for `step` (1-based): the plan's title, goal,
    /// kind, bug, context, decisions, interfaces, acceptance scenarios, spec
    /// changes, a step list, and the assigned step's full text. Refuses an unknown,
    /// closed, or abandoned plan, a missing step, and a step that is already
    /// done.
    pub fn brief_for(&self, id: &str, step: usize) -> Result<String, String> {
        let _guard = self.guard();
        let (entry, plan) = self.load(id)?;
        require_open(&entry)?;
        let Some(assigned) = step.checked_sub(1).and_then(|i| plan.steps.get(i)) else {
            return Err(format!(
                "plan {id} has no step {step} (it has {} steps, numbered from 1)",
                plan.steps.len()
            ));
        };
        if assigned.done {
            return Err(format!("step {step} of plan {id} is already done"));
        }
        let mut out = format!(
            "# Plan: {} ({id})\nGoal: {}\nKind: {}\n",
            plan.title, plan.goal, plan.kind
        );
        if let Some(bug) = &plan.bug_symptom {
            out.push_str(&format!("Bug symptom: {bug}\n"));
        }
        out.push_str(&format!("\n## Context\n{}\n", plan.context));
        if let Some(decisions) = &plan.decisions {
            out.push_str(&format!("\n## Decisions\n{decisions}\n"));
        }
        if let Some(interfaces) = &plan.interfaces {
            out.push_str(&format!("\n## Interfaces\n{interfaces}\n"));
            out.push_str(
                "\nFollow these interface contracts exactly. If one is wrong or insufficient, \
                 message the lead instead of changing it.\n",
            );
        }
        if let Some(acceptance) = &plan.acceptance {
            out.push_str(&format!("\n## Acceptance scenarios\n{acceptance}\n"));
            out.push_str(
                "\nThe lead verifies these observable outcomes on the integrated result.\n",
            );
        }
        out.push_str(&self.worker_context(&entry)?);
        out.push_str(&render_spec_changes(&entry.spec_deltas));
        for (artifact, text) in &entry.sections {
            out.push_str(&format!("\n## {artifact}\n\n{}\n", text.trim()));
        }
        out.push_str("\n## Steps\n");
        for s in &plan.steps {
            let mark = if s.done { 'x' } else { ' ' };
            let label = s.header.as_deref().unwrap_or(&s.text);
            let here = if s.index + 1 == step {
                "  <- your step"
            } else {
                ""
            };
            out.push_str(&format!(
                "- [{mark}] {}. {}{here}\n",
                s.index + 1,
                clip(label, 100)
            ));
        }
        out.push_str(&format!("\n## Your step ({step})\n{}\n", assigned.text));
        out.push_str(&format!(
            "\nYou are working on step {step} of plan {} ({id}); other steps are handled separately.\n",
            plan.title
        ));
        Ok(out)
    }

    /// Return the schema artifacts that still block implementation dispatch.
    /// An empty vector means the recorded schema's `apply.requires` is met.
    pub fn dispatch_blockers(&self, id: &str) -> Result<Vec<String>, String> {
        let _guard = self.guard();
        let (entry, plan) = self.load(id)?;
        require_open(&entry)?;
        let resolved = Schema::resolve(&self.openspec_dir(), Some(&entry.schema))?;
        Ok(self.artifact_blockers(
            &resolved,
            &plan,
            &entry.spec_deltas,
            &entry.sections,
            &resolved.schema.apply.requires,
        ))
    }

    /// The contract gate for dispatching `step0` (0-based): every OTHER step
    /// of the plan in `in_flight_steps` (0-based steps with an open task)
    /// that shares a file path with `step0` needs that path in the plan's
    /// `## Interfaces`. The step itself is ignored, so re-dispatching it
    /// never trips the gate against itself.
    pub fn contract_gate(
        &self,
        id: &str,
        step0: usize,
        in_flight_steps: &[usize],
    ) -> Result<(), String> {
        let _guard = self.guard();
        let (_, plan) = self.load(id)?;
        let Some(target) = plan.steps.get(step0) else {
            return Ok(());
        };
        let mine = step_paths(&target.text);
        if mine.is_empty() {
            return Ok(());
        }
        let contracts = plan.interfaces.as_deref().unwrap_or("").replace('\\', "/");
        let mut others: Vec<usize> = in_flight_steps
            .iter()
            .copied()
            .filter(|&o| o != step0)
            .collect();
        others.sort_unstable();
        others.dedup();
        for other in others {
            let Some(other_step) = plan.steps.get(other) else {
                continue;
            };
            let theirs = step_paths(&other_step.text);
            let uncovered: Vec<&String> = mine
                .iter()
                .filter(|p| theirs.contains(p) && !contracts.contains(p.as_str()))
                .collect();
            if !uncovered.is_empty() {
                let paths = uncovered
                    .iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "step {} shares {paths} with step {}, which is already in flight, and the \
                     plan's Interfaces record no contract for it. Call plan_update with \
                     `interface` (exports, signatures, data shapes, file ownership) first, then \
                     dispatch again.",
                    step0 + 1,
                    other + 1
                ));
            }
        }
        Ok(())
    }

    /// Append the final "Integration check" step unless the plan already has
    /// one (done or not), recording in `## Decisions` that it was added
    /// because `parallel_steps` (0-based) ran in parallel. `Ok(true)` when
    /// the step was added. Every step must be done before the plan closes,
    /// so this blocks the close until the combined result is verified.
    pub fn ensure_integration_step(
        &self,
        id: &str,
        parallel_steps: &[usize],
    ) -> Result<bool, String> {
        let _guard = self.guard();
        let (entry, mut plan) = self.load(id)?;
        require_open(&entry)?;
        if plan
            .steps
            .iter()
            .any(|s| s.header.as_deref() == Some(INTEGRATION_HEADER))
        {
            return Ok(false);
        }
        let index = plan.steps.len();
        let text = INTEGRATION_STEP.to_string();
        plan.steps.push(crate::app::workflow::Step {
            index,
            header: crate::memory::plan_format::extract_bold_header(&text),
            text,
            done: false,
        });
        let mut numbers: Vec<String> = parallel_steps.iter().map(|s| (s + 1).to_string()).collect();
        numbers.dedup();
        push_line(
            &mut plan.decisions,
            format!(
                "- {}: added step {} (Integration check) because steps {} ran in parallel",
                today(),
                index + 1,
                numbers.join(" and ")
            ),
        );
        self.save_plan(id, &plan, &entry)?;
        Ok(true)
    }

    /// Mark `step` (0-based) done and append `outcome_line` to `## Progress`.
    /// `Ok(false)` when the step was already done.
    pub fn tick(&self, id: &str, step: usize, outcome_line: &str) -> Result<bool, String> {
        let _guard = self.guard();
        let (entry, mut plan) = self.load(id)?;
        require_open(&entry)?;
        let number = step + 1;
        let Some(target) = plan.steps.get_mut(step) else {
            return Err(format!("plan {id} has no step {number}"));
        };
        if target.done {
            return Ok(false);
        }
        target.done = true;
        let outcome = collapse_whitespace(outcome_line);
        push_line(
            &mut plan.progress,
            format!("- {} step {number} done: {outcome}", today()),
        );
        self.save_plan(id, &plan, &entry)?;
        Ok(true)
    }

    /// Reopen `step` (0-based), noting why in `## Progress`. A closed plan
    /// reopens with it. `Ok(false)` when the step was not done.
    pub fn untick(&self, id: &str, step: usize, note: &str) -> Result<bool, String> {
        let _guard = self.guard();
        let mut board = self.load_board()?;
        let mut plan = self.load_plan(id)?;
        let Some(entry) = board.plans.iter_mut().find(|e| e.id == id) else {
            return Err(unknown(id));
        };
        if entry.status == BoardStatus::Abandoned {
            return Err(format!("plan {id} was abandoned"));
        }
        let number = step + 1;
        let Some(target) = plan.steps.get_mut(step) else {
            return Err(format!("plan {id} has no step {number}"));
        };
        if !target.done {
            return Ok(false);
        }
        target.done = false;
        let note = collapse_whitespace(note);
        push_line(
            &mut plan.progress,
            format!("- {} step {number} reopened: {note}", today()),
        );
        self.save_plan(id, &plan, entry)?;
        if entry.status == BoardStatus::Closed {
            entry.status = BoardStatus::Open;
            entry.closed_at_ms = None;
            entry.report_path = None;
            self.save_board(&board)?;
        }
        Ok(true)
    }

    /// Close the plan when every step is done, the ledger's team review
    /// passes, and the kind gates hold: write the review report, mark the
    /// plan closed, and capture the PLAN:/BUG: digests. Closing an already
    /// closed plan reports its existing report.
    pub async fn try_close(&self, id: &str, ledger: &dyn TeamLedger) -> CloseOutcome {
        let (entry, plan) = {
            let _guard = self.guard();
            match self.load(id) {
                Ok(loaded) => loaded,
                Err(reason) => {
                    return CloseOutcome::Blocked {
                        reasons: vec![reason],
                    }
                }
            }
        };
        match entry.status {
            BoardStatus::Closed => {
                return CloseOutcome::Closed {
                    report_path: entry.report_path.unwrap_or_default(),
                    retired_notes: 0,
                };
            }
            BoardStatus::Abandoned => {
                return CloseOutcome::Blocked {
                    reasons: vec![format!("plan {id} was abandoned")],
                };
            }
            BoardStatus::Open => {}
        }
        let undone: Vec<String> = plan
            .steps
            .iter()
            .filter(|s| !s.done)
            .map(|s| {
                let label = s.header.as_deref().unwrap_or(&s.text);
                format!("step {} is not done: {}", s.index + 1, clip(label, 80))
            })
            .collect();
        if !undone.is_empty() {
            return CloseOutcome::NotReady { reasons: undone };
        }
        // No lock is held across the awaits below.
        let report = match ledger.plan_review(id, &plan.title).await {
            PlanReview::Blocked { reasons } => return CloseOutcome::Blocked { reasons },
            PlanReview::Ready { report } => report,
        };
        if !report.trim_start().starts_with("## Verdict: PASS") {
            return CloseOutcome::Blocked {
                reasons: vec!["the team review report does not open with a PASS verdict".into()],
            };
        }
        let mut notes = Vec::new();
        if let Err(reason) = bug_fixing_gate(&plan, self.codegraph.as_ref(), &mut notes).await {
            let reason = reason
                .replace("update_plan", "plan_update")
                .replace("before finish", "before the plan can close");
            return CloseOutcome::Blocked {
                reasons: vec![reason],
            };
        }
        if self.external_lead_attached.load(Ordering::Acquire)
            && matches!(plan.kind, PlanKind::Implementation | PlanKind::Research)
            && !matches!(self.has_durable_knowledge_since(entry.created_at_ms).await, Ok(true))
        {
            return CloseOutcome::Blocked {
                reasons: vec![
                    "Record what this plan established with memory_write (a durable SPEC:/DECISION:/HOW: title; no plan IDs, branch names or commit hashes), then call plan_step_done on the last step to close."
                        .to_string(),
                ],
            };
        }
        if self.reviews_dir.as_os_str().is_empty() {
            return CloseOutcome::Blocked {
                reasons: vec!["no reviews directory is configured".to_string()],
            };
        }
        let report_path = {
            let _guard = self.guard();
            match self.write_closed(id, &report) {
                Ok(Ok(path)) => path,
                Ok(Err(existing)) => {
                    return CloseOutcome::Closed {
                        report_path: existing,
                        retired_notes: 0,
                    }
                }
                Err(reason) => {
                    return CloseOutcome::Blocked {
                        reasons: vec![reason],
                    }
                }
            }
        };
        let retired_notes = self.settle_notes(id, &report_path).await;
        self.capture(id, &plan).await;
        CloseOutcome::Closed {
            report_path,
            retired_notes,
        }
    }

    /// Retire the plan's plan-scoped memory notes and append them to the
    /// closing report, so the lead can re-store their durable parts. Returns
    /// how many were retired; failures are logged, never block the close.
    async fn settle_notes(&self, id: &str, report_path: &str) -> usize {
        let Some(store) = &self.memory else {
            return 0;
        };
        let notes = match crate::memory::plan_scope::settle_plan_memories(store.as_ref(), id).await
        {
            Ok(notes) => notes,
            Err(e) => {
                eprintln!("plan board: settling plan notes for {id} failed: {e}");
                return 0;
            }
        };
        if notes.is_empty() {
            return 0;
        }
        let section = crate::memory::plan_scope::render_settled_notes(&notes);
        let appended = std::fs::read_to_string(report_path).and_then(|report| {
            std::fs::write(
                report_path,
                format!(
                    "{}

{section}",
                    report.trim_end()
                ),
            )
        });
        if let Err(e) = appended {
            eprintln!("plan board: could not append plan notes to {report_path}: {e}");
        }
        notes.len()
    }

    /// Close an open plan without review, recording `reason`.
    pub fn abandon(&self, id: &str, reason: &str) -> Result<BoardPlan, String> {
        let _guard = self.guard();
        let mut board = self.load_board()?;
        let mut plan = self.load_plan(id)?;
        let Some(entry) = board.plans.iter_mut().find(|e| e.id == id) else {
            return Err(unknown(id));
        };
        require_open(entry)?;
        entry.status = BoardStatus::Abandoned;
        entry.closed_at_ms = Some(now_ms());
        entry.abandon_reason = Some(collapse_whitespace(reason));
        push_line(
            &mut plan.progress,
            format!("- {} abandoned: {}", today(), collapse_whitespace(reason)),
        );
        self.save_plan(id, &plan, entry)?;
        let view = board_plan(entry, &plan);
        self.save_board(&board)?;
        Ok(view)
    }

    /// The lead's volatile-tail section: one short line per open plan.
    pub fn tail_summary(&self) -> String {
        const SHOWN: usize = 8;
        let open: Vec<BoardPlan> = self
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.status == BoardStatus::Open)
            .collect();
        let mut out = String::from("# PLAN BOARD\n");
        if open.is_empty() {
            out.push_str("No open plans.\n");
            return out;
        }
        for plan in open.iter().take(SHOWN) {
            let todo: Vec<String> = plan
                .steps
                .iter()
                .filter(|s| !s.done)
                .map(|s| s.number.to_string())
                .collect();
            let done = plan.steps.len() - todo.len();
            out.push_str(&format!(
                "- {} \"{}\" {done}/{} done; todo {}\n",
                &plan.id,
                clip(&plan.title, 60),
                plan.steps.len(),
                if todo.is_empty() {
                    "none (closing)".to_string()
                } else {
                    todo.join(",")
                }
            ));
        }
        if open.len() > SHOWN {
            out.push_str(&format!(
                "- ... and {} more (plan_list)\n",
                open.len() - SHOWN
            ));
        }
        out
    }

    // ── internals ──────────────────────────────────────────────────────

    /// Whether a live, non-plan-scoped learning was written or changed at or
    /// after `since_ms`. Knowledge-backed rows use their file's modification
    /// time, which also covers amendments of records created before the plan.
    async fn has_durable_knowledge_since(&self, since_ms: u64) -> Result<bool, String> {
        let Some(store) = self.memory.as_ref() else {
            // Without a memory store there is nothing the lead can write to;
            // the opt-in gate is not applicable.
            return Ok(true);
        };
        let records = store
            .list_filtered(&MemoryFilter::new())
            .await
            .map_err(|err| err.to_string())?;
        Ok(records
            .iter()
            .any(|memory| self.is_durable_knowledge_since(memory, since_ms)))
    }

    fn is_durable_knowledge_since(&self, memory: &Memory, since_ms: u64) -> bool {
        if !matches!(
            MemoryRecordType::from_title(&memory.title),
            MemoryRecordType::Spec
                | MemoryRecordType::Decision
                | MemoryRecordType::How
                | MemoryRecordType::Bug
        ) || memory
            .data
            .get("plan_scoped")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            || plan_scope::scoped_tier(memory.tier, &memory.title, &memory.content).is_scoped()
            || mentions_commit_hash(&memory.title)
            || mentions_commit_hash(&memory.content)
        {
            return false;
        }
        if let Some(modified_ms) = self.knowledge_modified_ms(memory) {
            return modified_ms >= since_ms;
        }
        // Authored DB rows have second precision. Comparing with the plan's
        // containing second lets a write immediately after plan_create count.
        memory.created_at >= (since_ms / 1000) as i64
    }

    fn knowledge_modified_ms(&self, memory: &Memory) -> Option<u64> {
        let relative = memory.data.get("rel_path")?.as_str()?;
        let modified = std::fs::metadata(self.knowledge.as_ref()?.dir().join(relative))
            .ok()?
            .modified()
            .ok()?;
        modified
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_millis()
            .try_into()
            .ok()
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load_board(&self) -> Result<BoardFile, String> {
        match std::fs::read_to_string(self.plans_dir.join(BOARD_FILE)) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| format!("{BOARD_FILE} is not valid JSON: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BoardFile::default()),
            Err(e) => Err(format!("could not read {BOARD_FILE}: {e}")),
        }
    }

    fn save_board(&self, board: &BoardFile) -> Result<(), String> {
        let text = serde_json::to_string_pretty(board).map_err(|e| e.to_string())?;
        let tmp = self.plans_dir.join(format!("{BOARD_FILE}.tmp"));
        std::fs::write(&tmp, text)
            .and_then(|_| std::fs::rename(&tmp, self.plans_dir.join(BOARD_FILE)))
            .map_err(|e| format!("could not write {BOARD_FILE}: {e}"))
    }

    fn load_plan(&self, id: &str) -> Result<PlanFile, String> {
        PlanFile::read_from_file(&self.plans_dir.join(format!("{id}.md")))
            .map_err(|e| format!("could not read plan {id}: {e}"))
    }

    fn save_plan(&self, id: &str, plan: &PlanFile, entry: &BoardEntry) -> Result<(), String> {
        plan.write_to_dir_with_sections(
            &self.plans_dir,
            id,
            &entry.spec_deltas,
            &entry.sections,
        )
        .map(|_| ())
        .map_err(|e| format!("could not write plan {id}: {e}"))
    }

    fn openspec_dir(&self) -> PathBuf {
        self.plans_dir
            .parent()
            .unwrap_or(&self.plans_dir)
            .join("openspec")
    }

    fn specs_dir(&self) -> PathBuf {
        self.openspec_dir().join("specs")
    }

    fn worker_context(&self, entry: &BoardEntry) -> Result<String, String> {
        let resolved = Schema::resolve(&self.openspec_dir(), Some(&entry.schema))?;
        let mut out = String::new();
        let context = resolved.config.context.trim();
        if !context.is_empty() {
            out.push_str(&format!("\n{context}\n"));
        }
        for artifact in &resolved.schema.artifacts {
            let instruction = artifact.instruction.trim();
            if !instruction.is_empty() {
                out.push_str(&format!(
                    "\n### Artifact: {}\n{instruction}\n",
                    artifact.id
                ));
            }
        }
        if out.is_empty() {
            Ok(out)
        } else {
            Ok(format!("\n## Project context\n{out}"))
        }
    }

    fn validate_artifacts(
        &self,
        resolved: &ResolvedSchema,
        plan: &PlanFile,
        deltas: &[Delta],
        sections: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        for (id, text) in sections {
            let Some(artifact) = resolved.schema.artifact(id) else {
                return Err(format!(
                    "OpenSpec schema '{}' has no artifact '{id}'; valid artifact ids: {}",
                    resolved.schema.name,
                    resolved
                        .schema
                        .artifacts
                        .iter()
                        .map(|artifact| artifact.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            };
            if let Some(source) = mapped_artifact_source(id) {
                return Err(format!(
                    "artifact '{id}' is supplied by {source}, not the sections object"
                ));
            }
            if text.trim() == artifact.template_text.trim() {
                return Err(self.artifact_guidance(
                    resolved,
                    id,
                    "is still identical to its template; replace the template with the plan's findings",
                ));
            }
        }
        // The built-in schema is a description of the existing board fields,
        // not a new gate: spec deltas and a design remain encouraged, optional.
        if resolved.schema.name == "spec-driven" {
            return Ok(());
        }
        let roots = resolved
            .schema
            .artifacts
            .iter()
            .filter(|artifact| self.artifact_present(&resolved.schema, artifact.id.as_str(), plan, deltas, sections))
            .map(|artifact| artifact.id.clone())
            .collect::<Vec<_>>();
        let blockers = self.artifact_blockers(resolved, plan, deltas, sections, &roots);
        if blockers.is_empty() {
            Ok(())
        } else {
            Err(blockers.join("\n\n"))
        }
    }

    fn artifact_blockers(
        &self,
        resolved: &ResolvedSchema,
        plan: &PlanFile,
        deltas: &[Delta],
        sections: &BTreeMap<String, String>,
        roots: &[String],
    ) -> Vec<String> {
        let mut blockers = Vec::new();
        for id in roots {
            self.collect_artifact_blockers(
                resolved,
                plan,
                deltas,
                sections,
                id,
                &mut blockers,
            );
        }
        blockers.sort();
        blockers.dedup();
        blockers
    }

    fn collect_artifact_blockers(
        &self,
        resolved: &ResolvedSchema,
        plan: &PlanFile,
        deltas: &[Delta],
        sections: &BTreeMap<String, String>,
        id: &str,
        blockers: &mut Vec<String>,
    ) {
        if !self.artifact_present(&resolved.schema, id, plan, deltas, sections) {
            blockers.push(self.artifact_guidance(
                resolved,
                id,
                "is required but missing (or still template text)",
            ));
            return;
        }
        // Schema loading has already rejected cycles. The built-in schema's
        // optional design/specs chain intentionally does not gate dispatch.
        if resolved.schema.name == "spec-driven" {
            return;
        }
        if let Some(artifact) = resolved.schema.artifact(id) {
            for required in &artifact.requires {
                self.collect_artifact_blockers(
                    resolved, plan, deltas, sections, required, blockers,
                );
            }
        }
    }

    fn artifact_present(
        &self,
        schema: &Schema,
        id: &str,
        plan: &PlanFile,
        deltas: &[Delta],
        sections: &BTreeMap<String, String>,
    ) -> bool {
        match id {
            "proposal" => !plan.goal.trim().is_empty(),
            "design" => {
                plan.decisions.as_deref().is_some_and(|text| !text.trim().is_empty())
                    || plan
                        .interfaces
                        .as_deref()
                        .is_some_and(|text| !text.trim().is_empty())
            }
            "tasks" => !plan.steps.is_empty(),
            "specs" => !deltas.is_empty(),
            _ => sections.get(id).is_some_and(|text| {
                !text.trim().is_empty()
                    && schema.artifact(id).is_some_and(|artifact| {
                        text.trim() != artifact.template_text.trim()
                    })
            }),
        }
    }

    fn artifact_guidance(&self, resolved: &ResolvedSchema, id: &str, problem: &str) -> String {
        let Some(artifact) = resolved.schema.artifact(id) else {
            return format!(
                "OpenSpec schema '{}' requires unknown artifact '{id}'",
                resolved.schema.name
            );
        };
        let source = mapped_artifact_source(id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("plan_create/plan_update sections.{id}"));
        let mut out = format!(
            "OpenSpec schema '{}': artifact '{id}' {problem}. Supply {source}.\nTemplate (templates/{}):\n{}\nInstruction:\n{}",
            resolved.schema.name,
            artifact.template,
            artifact.template_text.trim(),
            artifact.instruction.trim()
        );
        if let Some(rules) = resolved.config.rules.get(id) {
            out.push_str("\nConfig rules:");
            for rule in rules {
                out.push_str(&format!("\n- {rule}"));
            }
        }
        out
    }

    fn validate_spec_deltas(&self, deltas: &[Delta]) -> Result<(), String> {
        let specs_dir = self.specs_dir();
        let mut current: BTreeMap<String, Option<Spec>> = BTreeMap::new();
        for delta in deltas {
            if !current.contains_key(&delta.capability) {
                current.insert(
                    delta.capability.clone(),
                    load_current_spec(&specs_dir, &delta.capability)?,
                );
            }
            let base = current
                .get(&delta.capability)
                .expect("the capability spec was just loaded");
            let next = apply(base.as_ref(), delta).map_err(|error| error.to_string())?;
            current.insert(delta.capability.clone(), Some(next));
        }
        Ok(())
    }

    fn load(&self, id: &str) -> Result<(BoardEntry, PlanFile), String> {
        let entry = self
            .load_board()?
            .plans
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| unknown(id))?;
        let plan = self.load_plan(id)?;
        Ok((entry, plan))
    }

    /// An 8-hex id no plan file or board row uses.
    fn fresh_id(&self, board: &BoardFile) -> String {
        loop {
            let id: String = uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(8)
                .collect();
            if !board.plans.iter().any(|e| e.id == id)
                && !self.plans_dir.join(format!("{id}.md")).exists()
            {
                return id;
            }
        }
    }

    /// Under the lock: write the report and mark the plan closed. The inner
    /// `Err` carries the report of a plan another call closed first.
    fn write_closed(&self, id: &str, report: &str) -> Result<Result<String, String>, String> {
        let mut board = self.load_board()?;
        let Some(entry) = board.plans.iter_mut().find(|e| e.id == id) else {
            return Err(unknown(id));
        };
        match entry.status {
            BoardStatus::Open => {}
            BoardStatus::Closed => {
                return Ok(Err(entry.report_path.clone().unwrap_or_default()));
            }
            BoardStatus::Abandoned => return Err(format!("plan {id} was abandoned")),
        }
        // Idempotent OpenSpec close: the archive marker is written last, so its
        // presence means an earlier attempt already applied every delta and
        // wrote the archive — only the report and board save remain to retry.
        if !entry.spec_deltas.is_empty() && !archive::already_archived(&self.openspec_dir(), id) {
            let plan = self.load_plan(id)?;
            let resolved = Schema::resolve(&self.openspec_dir(), Some(&entry.schema))?;
            let prepared = archive::prepare_close(
                &self.openspec_dir(),
                id,
                &plan,
                &resolved,
                &entry.spec_deltas,
                &entry.sections,
                &today(),
            )?;
            prepared.write()?;
        }
        let now = now_ms();
        let name = format!("team-{id}-{}.md", report_stamp((now / 1000) as i64));
        let path = self.reviews_dir.join(name);
        std::fs::create_dir_all(&self.reviews_dir)
            .and_then(|_| std::fs::write(&path, report))
            .map_err(|e| {
                format!(
                    "could not write the review report '{}': {e}",
                    path.display()
                )
            })?;
        let report_path = path.to_string_lossy().into_owned();
        entry.status = BoardStatus::Closed;
        entry.closed_at_ms = Some(now);
        entry.report_path = Some(report_path.clone());
        self.save_board(&board)?;
        Ok(Ok(report_path))
    }

    /// The PLAN:/BUG: digests. Best effort: the plan is already closed.
    async fn capture(&self, id: &str, plan: &PlanFile) {
        let Some(store) = &self.memory else {
            return;
        };
        let repo_root = self
            .plans_dir
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .filter(|root| !root.as_os_str().is_empty());
        let result = crate::memory::finish_capture::capture_finish(
            store.as_ref(),
            plan,
            id,
            &self.plans_dir,
            repo_root,
            (now_ms() / 1000) as i64,
            self.knowledge.clone(),
        )
        .await;
        if let Err(e) = result {
            eprintln!("plan board: capture for {id} failed: {e}");
        }
    }
}

/// Load the current spec for a capability, if one has been written.
fn load_current_spec(specs_dir: &Path, capability: &str) -> Result<Option<Spec>, String> {
    let path = specs_dir.join(capability).join("spec.md");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not read spec for capability '{capability}' at '{}': {error}",
                path.display()
            ));
        }
    };
    Spec::parse(&text)
        .map(Some)
        .map_err(|error| format!("invalid spec for capability '{capability}': {error}"))
}

fn mapped_artifact_source(id: &str) -> Option<&'static str> {
    match id {
        "proposal" => Some("the plan's goal and context"),
        "design" => Some("the plan's decisions and interfaces"),
        "tasks" => Some("the plan's steps"),
        "specs" => Some("the plan's spec_deltas"),
        _ => None,
    }
}

fn board_plan(entry: &BoardEntry, plan: &PlanFile) -> BoardPlan {
    BoardPlan {
        id: entry.id.clone(),
        title: plan.title.clone(),
        goal: plan.goal.clone(),
        kind: plan.kind.to_string(),
        status: entry.status,
        closed_at_ms: entry.closed_at_ms,
        report_path: entry.report_path.clone(),
        spec_deltas: entry.spec_deltas.clone(),
        schema: entry.schema.clone(),
        sections: entry.sections.clone(),
        steps: plan
            .steps
            .iter()
            .map(|s| BoardStep {
                number: s.index + 1,
                header: s.header.clone(),
                text: s.text.clone(),
                done: s.done,
            })
            .collect(),
    }
}

fn unknown(id: &str) -> String {
    format!("unknown plan '{id}' — plan_list shows the board")
}

fn require_open(entry: &BoardEntry) -> Result<(), String> {
    match entry.status {
        BoardStatus::Open => Ok(()),
        BoardStatus::Closed => Err(format!("plan {} is closed", entry.id)),
        BoardStatus::Abandoned => Err(format!("plan {} was abandoned", entry.id)),
    }
}

/// Whether text carries something shaped like a Git commit hash. Durable
/// learnings must outlive the commit and plan that motivated them.
fn mentions_commit_hash(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| (7..=40).contains(&word.len()))
        .any(|word| {
            word.bytes().all(|b| b.is_ascii_hexdigit())
                && word.bytes().any(|b| b.is_ascii_digit())
                && word.bytes().any(|b| matches!(b, b'a'..=b'f' | b'A'..=b'F'))
        })
}

/// Single-line, non-empty step texts.
fn clean_steps(steps: &[String]) -> Vec<String> {
    steps
        .iter()
        .map(|s| collapse_whitespace(s))
        .filter(|s| !s.is_empty())
        .collect()
}

/// The file paths a step's text names (`/`-normalized): tokens with a path
/// separator or a code-file extension, minus URLs, a trailing `:line`, and
/// surrounding punctuation or markdown.
fn step_paths(text: &str) -> Vec<String> {
    const EXTS: [&str; 18] = [
        "rs", "ts", "tsx", "toml", "md", "json", "js", "jsx", "css", "html", "py", "go", "yml",
        "yaml", "sql", "sh", "ps1", "svelte",
    ];
    let mut out: Vec<String> = Vec::new();
    for raw in text.split_whitespace() {
        let tok = raw.trim_matches(|c: char| {
            matches!(
                c,
                '`' | '\'' | '"' | '(' | ')' | '[' | ']' | ',' | ';' | '*' | '.' | ':'
            )
        });
        if tok.is_empty() || tok.contains("://") || !tok.chars().any(|c| c.is_alphabetic()) {
            continue;
        }
        let mut tok = tok.replace('\\', "/");
        // Drop a trailing `:42` or `:42:7` location.
        while let Some((head, tail)) = tok.rsplit_once(':') {
            if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
                tok = head.to_string();
            } else {
                break;
            }
        }
        let ext_ok = tok
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && EXTS.contains(&ext));
        let has_sep = tok.contains('/') && tok.rsplit('/').next().is_some_and(|f| f.contains('.'));
        if (ext_ok || has_sep) && !out.contains(&tok) {
            out.push(tok);
        }
    }
    out
}

/// Split `## Acceptance scenarios` sections out of free text (a lead's
/// context): `(the rest, trimmed; the sections' scenarios)`. The context is
/// escaped on write, so a heading left in it would read as prose rather
/// than as the plan's acceptance scenarios. A section runs to the next line
/// the plan parser treats as structure (`# ` or `## `); a line that is not
/// a list item continues the scenario before it.
fn lift_acceptance(text: &str) -> (String, Vec<String>) {
    let mut rest = Vec::new();
    let mut scenarios: Vec<String> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            inside = heading.trim().eq_ignore_ascii_case("acceptance scenarios");
            if inside {
                continue;
            }
        } else if trimmed.starts_with("# ") {
            inside = false;
        }
        if !inside {
            rest.push(line);
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            scenarios.push(trimmed.to_string());
        } else if !trimmed.is_empty() {
            match scenarios.last_mut() {
                Some(last) => {
                    last.push(' ');
                    last.push_str(trimmed);
                }
                None => scenarios.push(trimmed.to_string()),
            }
        }
    }
    (rest.join("\n").trim().to_string(), scenarios)
}

/// Record acceptance scenarios as `- ` bullets in `## Acceptance
/// scenarios`: each collapsed to one line (so it cannot inject a section),
/// a leading list marker dropped, blanks and already recorded ones skipped.
fn add_scenarios<'a>(plan: &mut PlanFile, scenarios: impl IntoIterator<Item = &'a String>) {
    for scenario in scenarios {
        let text = collapse_whitespace(scenario);
        let text = text
            .strip_prefix("- ")
            .or_else(|| text.strip_prefix("* "))
            .unwrap_or(&text)
            .trim();
        if text.is_empty() {
            continue;
        }
        let line = format!("- {text}");
        let recorded = plan
            .acceptance
            .as_deref()
            .is_some_and(|body| body.lines().any(|l| l.trim() == line));
        if !recorded {
            push_line(&mut plan.acceptance, line);
        }
    }
}

/// Append one line to an optional section body.
fn push_line(body: &mut Option<String>, line: String) {
    match body {
        Some(existing) => {
            existing.push('\n');
            existing.push_str(&line);
        }
        None => *body = Some(line),
    }
}

/// `text` cut to `max` chars, with an ellipsis when cut.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(max).collect::<String>())
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn today() -> String {
    crate::tool::memory::format_epoch_date((now_ms() / 1000) as i64)
}

/// `YYYYMMDD-HHMMSS` (UTC) for a report file name. The crate has no chrono
/// dependency; the date half reuses the memory tools' civil date formatter.
fn report_stamp(secs: i64) -> String {
    let date = crate::tool::memory::format_epoch_date(secs).replace('-', "");
    let day_secs = secs.rem_euclid(86_400);
    format!(
        "{date}-{:02}{:02}{:02}",
        day_secs / 3600,
        day_secs % 3600 / 60,
        day_secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::team_ledger::fake::FakeLedger;
    use crate::openspec::{Requirement, Scenario};

    const CONTEXT: &str =
        "Why: the importer drops rows (src/import.rs).\n## Not a section\nConstraint: keep the CLI flags. Verify: cargo test.";

    fn board(dir: &Path) -> PlanBoard {
        PlanBoard::new(dir.join("plans"), dir.join("reviews"))
    }

    fn draft(title: &str) -> PlanDraft {
        PlanDraft {
            title: title.to_string(),
            goal: "Ship the importer fix".to_string(),
            context: CONTEXT.to_string(),
            steps: vec![
                "**Fix parser** — edit src/import.rs".to_string(),
                "Add tests in tests/import.rs".to_string(),
            ],
            ..Default::default()
        }
    }

    fn create(board: &PlanBoard, title: &str) -> BoardPlan {
        board.create(draft(title)).expect("create")
    }

    fn research_schema(dir: &Path, proposal_requires_research: bool) {
        let openspec = dir.join("openspec");
        let templates = openspec.join("schemas/custom/templates");
        std::fs::create_dir_all(&templates).unwrap();
        for (name, text) in [
            ("proposal.md", "# Proposal\n"),
            ("research.md", "# Research\n\nReplace with findings.\n"),
            ("tasks.md", "# Tasks\n"),
        ] {
            std::fs::write(templates.join(name), text).unwrap();
        }
        let proposal_requires = if proposal_requires_research {
            "[research]"
        } else {
            "[]"
        };
        std::fs::write(
            openspec.join("schemas/custom/schema.yaml"),
            format!(
                "name: custom\nversion: 7\ndescription: Custom research workflow\nartifacts:\n  - id: proposal\n    generates: proposal.md\n    template: proposal.md\n    instruction: Explain the proposal.\n    requires: {proposal_requires}\n  - id: research\n    generates: research.md\n    template: research.md\n    instruction: Investigate and record evidence.\n  - id: tasks\n    generates: tasks.md\n    template: tasks.md\n    instruction: Name verification in every step.\n    requires: [proposal]\napply:\n  requires: [research]\n  tracks: tasks.md\n"
            ),
        )
        .unwrap();
        std::fs::write(
            openspec.join("config.yaml"),
            "schema: custom\ncontext: Project schema context.\nrules:\n  research:\n    - Cite the inspected files.\n    - Name the evidence.\n",
        )
        .unwrap();
    }

    fn research_section(text: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("research".to_string(), text.to_string())])
    }

    fn requirement(name: &str) -> Requirement {
        Requirement::new(
            name,
            "The system SHALL keep the observable contract.",
            vec![Scenario::new(
                "Contract exercised",
                vec!["the contract is exercised".to_string()],
                vec!["the behavior is observable".to_string()],
                vec![],
            )],
        )
    }

    fn added_delta(capability: &str, requirement_name: &str) -> Delta {
        let mut delta = Delta {
            capability: capability.to_string(),
            ..Delta::default()
        };
        delta.added.push(requirement(requirement_name));
        delta
    }

    fn write_spec(dir: &Path, capability: &str, requirements: &[&str]) {
        let folder = dir.join("openspec").join("specs").join(capability);
        std::fs::create_dir_all(&folder).unwrap();
        let mut spec = Spec::new(capability);
        for name in requirements {
            spec.add_requirement(requirement(name));
        }
        std::fs::write(folder.join("spec.md"), spec.render()).unwrap();
    }

    fn complete_steps(board: &PlanBoard, id: &str) {
        let step_count = board.get(id).unwrap().steps.len();
        for index in 0..step_count {
            board.tick(id, index, "done").unwrap();
        }
    }

    fn archive_dir(dir: &Path, id: &str, title_slug: &str) -> PathBuf {
        dir.join("openspec")
            .join("changes")
            .join("archive")
            .join(format!("{}-{id}-{title_slug}", today()))
    }

    #[test]
    fn plans_round_trip_through_the_board_and_the_mnemo1_parser() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let a = create(&board, "Importer");
        let b = create(&board, "Exporter");
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.len(), 8);
        assert_eq!(a.status, BoardStatus::Open);
        assert_eq!(a.steps[0].header.as_deref(), Some("Fix parser"));
        assert_eq!(a.steps[1].number, 2);

        let edited = board
            .update(
                &a.id,
                PlanEdit {
                    decision: Some("Keep the\nCLI flags".to_string()),
                    context_append: Some("Also: windows paths.".to_string()),
                    add_steps: vec!["Update the docs".to_string()],
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(edited.steps.len(), 3);
        board.tick(&a.id, 0, "merged abc123").unwrap();

        // Mnemo1's parser reads the same file: steps, kind, and context
        // survive, and the new sections round-trip.
        let path = dir.path().join("plans").join(format!("{}.md", a.id));
        let parsed = PlanFile::read_from_file(&path).unwrap();
        assert_eq!(parsed.title, "Importer");
        assert_eq!(parsed.kind, PlanKind::Implementation);
        assert_eq!(parsed.steps.len(), 3);
        assert!(parsed.steps[0].done && !parsed.steps[1].done);
        assert!(parsed.context.contains("\\## Not a section"));
        assert!(parsed.context.contains("Also: windows paths."));
        let decisions = parsed.decisions.clone().expect("decisions");
        assert!(decisions.contains(": Keep the CLI flags"), "{decisions}");
        assert!(parsed
            .progress
            .clone()
            .expect("progress")
            .contains("step 1 done: merged abc123"));
        assert_eq!(
            parsed.serialize(),
            PlanFile::parse(&parsed.serialize()).unwrap().serialize()
        );

        // list: open plans in creation order.
        let listed = board.list().unwrap();
        assert_eq!(
            listed.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            vec![a.id.clone(), b.id.clone()]
        );
        assert!(board.get(&a.id).unwrap().steps[0].done);
        assert!(board.get("deadbeef").unwrap_err().contains("unknown plan"));
        // board.json is the status record.
        let json = std::fs::read_to_string(dir.path().join("plans/board.json")).unwrap();
        assert!(json.contains("\"open\""), "{json}");
    }

    fn shared_file_plan(board: &PlanBoard) -> BoardPlan {
        board
            .create(PlanDraft {
                steps: vec![
                    "**Game logic** — implement src/game.js and tests/game.test.js".to_string(),
                    "**UI** — render in src/ui.js, importing src/game.js".to_string(),
                    "**Docs** — update README.md".to_string(),
                ],
                ..draft("Game")
            })
            .expect("create")
    }

    #[test]
    fn interfaces_are_recorded_and_carried_into_briefs() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                interfaces: vec!["`src/game.js` — exports `Game` as a named export".to_string()],
                ..draft("Game")
            })
            .unwrap();
        board
            .update(
                &plan.id,
                PlanEdit {
                    interface: Some("`src/ui.js` — default export `render(game)`".to_string()),
                    progress_note: Some("engine committed at abc123, review pending".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        let parsed =
            PlanFile::read_from_file(&dir.path().join("plans").join(format!("{}.md", plan.id)))
                .unwrap();
        let interfaces = parsed.interfaces.clone().expect("interfaces");
        assert_eq!(interfaces.lines().count(), 2, "{interfaces}");
        assert!(interfaces.contains("exports `Game` as a named export"));
        let progress = parsed.progress.clone().expect("progress");
        assert!(
            progress.contains(": engine committed at abc123, review pending"),
            "{progress}"
        );

        let brief = board.brief_for(&plan.id, 1).unwrap();
        assert!(
            brief.contains(
                "## Interfaces
- "
            ),
            "{brief}"
        );
        assert!(brief.contains("named export"), "{brief}");
        assert!(brief.contains("message the lead"), "{brief}");
        assert!(brief.contains("## Context"), "{brief}");
        assert!(!brief.contains("## Progress"), "{brief}");
        assert!(!brief.contains("engine committed"), "{brief}");
        assert!(brief.find("## Interfaces").unwrap() < brief.find("## Steps").unwrap());

        // A plan without contracts has neither the section nor the sentence.
        let bare = create(&board, "Bare");
        let brief = board.brief_for(&bare.id, 1).unwrap();
        assert!(!brief.contains("Interfaces") && !brief.contains("message the lead"));
    }

    #[test]
    fn acceptance_scenarios_are_recorded_kept_and_carried_into_briefs() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                acceptance: vec!["A restart reloads\nevery saved row.".to_string(), " ".to_string()],
                context: format!(
                    "{CONTEXT}\n## Acceptance scenarios\n- Empty cells keep their row\n  in the export.\n## Notes\nSee the issue."
                ),
                ..draft("Importer")
            })
            .unwrap();
        // The heading written into the context becomes the plan's real
        // section; other headings there are still escaped context.
        let text = board.read_markdown(&plan.id).unwrap();
        assert!(
            text.contains(
                "\n## Acceptance scenarios\n- A restart reloads every saved row.\n- Empty cells keep their row in the export.\n"
            ),
            "{text}"
        );
        assert!(!text.contains("\\## Acceptance scenarios"), "{text}");
        assert!(text.contains("\\## Notes\nSee the issue."), "{text}");

        // Later updates and step ticks keep the section; plan_update adds to
        // it (also from appended context) without repeating a scenario.
        board
            .update(
                &plan.id,
                PlanEdit {
                    add_acceptance: vec![
                        "- A restart reloads every saved row.".to_string(),
                        "The CLI flags are unchanged.".to_string(),
                    ],
                    context_append: Some(
                        "Also: windows paths.\n## Acceptance scenarios\n- Windows paths round-trip through the importer."
                            .to_string(),
                    ),
                    decision: Some("use serde".to_string()),
                    add_steps: vec!["Update docs/import.md".to_string()],
                    ..Default::default()
                },
            )
            .unwrap();
        board.tick(&plan.id, 0, "merged abc123").unwrap();
        board.untick(&plan.id, 0, "reverted").unwrap();
        board.tick(&plan.id, 1, "merged def456").unwrap();
        let parsed =
            PlanFile::read_from_file(&dir.path().join("plans").join(format!("{}.md", plan.id)))
                .unwrap();
        assert_eq!(
            parsed.acceptance.as_deref(),
            Some(
                "- A restart reloads every saved row.\n- Empty cells keep their row in the export.\n- The CLI flags are unchanged.\n- Windows paths round-trip through the importer."
            )
        );
        assert!(parsed.context.ends_with("Also: windows paths."), "{}", parsed.context);
        assert!(!parsed.context.contains("Empty cells"), "{}", parsed.context);

        let brief = board.brief_for(&plan.id, 3).unwrap();
        assert!(
            brief.contains("\n## Acceptance scenarios\n- A restart reloads every saved row.\n"),
            "{brief}"
        );
        assert!(brief.find("## Acceptance scenarios").unwrap() < brief.find("## Steps").unwrap());

        // A plan without scenarios has no section, in its file or its brief.
        let plain = create(&board, "Exporter");
        assert!(!board.read_markdown(&plain.id).unwrap().contains("Acceptance scenarios"));
        assert!(!board.brief_for(&plain.id, 1).unwrap().contains("Acceptance scenarios"));
    }

    #[test]
    fn contract_gate_requires_a_contract_for_shared_paths_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = shared_file_plan(&board);
        let id = &plan.id;

        // Nothing else in flight, re-dispatch of itself, unrelated step: pass.
        board.contract_gate(id, 1, &[]).unwrap();
        board.contract_gate(id, 1, &[1]).unwrap();
        board.contract_gate(id, 2, &[0, 1]).unwrap();

        // Shared src/game.js with step 1 in flight and no contract: refused.
        let err = board.contract_gate(id, 1, &[0]).unwrap_err();
        assert!(err.contains("`src/game.js`"), "{err}");
        assert!(
            err.contains("step 2 shares") && err.contains("with step 1"),
            "{err}"
        );
        assert!(
            err.contains("plan_update") && err.contains("`interface`"),
            "{err}"
        );
        assert!(!err.contains("tests/game.test.js"), "{err}");

        // A step that is not in flight is ignored.
        board.contract_gate(id, 1, &[2]).unwrap();

        // With the contract recorded, it passes.
        board
            .update(
                id,
                PlanEdit {
                    interface: Some("`src/game.js` — exports `Game` (named)".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        board.contract_gate(id, 1, &[0]).unwrap();
    }

    #[test]
    fn integration_step_is_appended_once() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = shared_file_plan(&board);
        assert!(board.ensure_integration_step(&plan.id, &[1, 0]).unwrap());
        assert!(!board.ensure_integration_step(&plan.id, &[1, 0]).unwrap());
        let got = board.get(&plan.id).unwrap();
        assert_eq!(got.steps.len(), 4);
        assert_eq!(got.steps[3].header.as_deref(), Some("Integration check"));
        assert!(!got.steps[3].done);
        let md = board.read_markdown(&plan.id).unwrap();
        assert_eq!(md.matches("Integration check").count(), 2, "{md}"); // step + decision
        assert!(md.contains("because steps 2 and 1 ran in parallel"), "{md}");

        // A done integration step still counts: no second one.
        board.tick(&plan.id, 3, "verified").unwrap();
        assert!(!board.ensure_integration_step(&plan.id, &[0, 1]).unwrap());
        assert_eq!(board.get(&plan.id).unwrap().steps.len(), 4);
    }

    #[test]
    fn create_validates_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let mut no_steps = draft("T");
        no_steps.steps = vec!["  ".to_string()];
        assert!(board
            .create(no_steps)
            .unwrap_err()
            .contains("at least one step"));
        let mut no_context = draft("T");
        no_context.context = " ".to_string();
        assert!(board.create(no_context).is_err());
        let mut bug = draft("T");
        bug.kind = PlanKind::BugFixing;
        assert!(board.create(bug).unwrap_err().contains("bug symptom"));
        assert!(board.list().unwrap().is_empty());
    }

    #[test]
    fn create_applies_the_create_plan_resumability_checks() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let mut thin = draft("T");
        thin.context = "fix crash".to_string();
        assert!(board
            .create(thin)
            .unwrap_err()
            .contains("too thin to resume"));
        let mut pathless = draft("T");
        pathless.steps = vec!["Do the thing".to_string()];
        assert!(board
            .create(pathless)
            .unwrap_err()
            .contains("names no file path"));
        let mut bug = draft("T");
        bug.kind = PlanKind::BugFixing;
        bug.bug = Some("rows vanish".to_string());
        bug.context =
            "Why: the importer drops rows in src/import.rs when a cell is empty.".to_string();
        assert!(board
            .create(bug)
            .unwrap_err()
            .contains("regression-test design"));
        assert!(board.list().unwrap().is_empty());
    }

    #[test]
    fn brief_for_composes_the_worker_header_and_refuses_bad_targets() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                kind: PlanKind::BugFixing,
                bug: Some("rows vanish".to_string()),
                ..draft("Importer")
            })
            .unwrap();
        board
            .update(
                &plan.id,
                PlanEdit {
                    decision: Some("use serde".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        let brief = board.brief_for(&plan.id, 2).unwrap();
        assert!(brief.contains("# Plan: Importer"));
        assert!(brief.contains("Goal: Ship the importer fix"));
        assert!(brief.contains("Kind: bug_fixing"));
        assert!(brief.contains("Bug symptom: rows vanish"));
        assert!(brief.contains("Constraint: keep the CLI flags."));
        assert!(brief.contains("use serde"));
        assert!(brief.contains("- [ ] 1. Fix parser"));
        assert!(brief.contains("- [ ] 2. Add tests in tests/import.rs  <- your step"));
        assert!(brief.contains("## Your step (2)\nAdd tests in tests/import.rs"));
        assert!(brief.contains(&format!(
            "You are working on step 2 of plan Importer ({}); other steps are handled separately.",
            plan.id
        )));

        assert!(board
            .brief_for("nope0000", 1)
            .unwrap_err()
            .contains("unknown plan"));
        assert!(board
            .brief_for(&plan.id, 0)
            .unwrap_err()
            .contains("no step 0"));
        assert!(board
            .brief_for(&plan.id, 3)
            .unwrap_err()
            .contains("no step 3"));
        board.tick(&plan.id, 0, "done").unwrap();
        assert!(board
            .brief_for(&plan.id, 1)
            .unwrap_err()
            .contains("already done"));
        assert!(board
            .brief_for(&plan.id, 2)
            .unwrap()
            .contains("- [x] 1. Fix parser"));

        board.abandon(&plan.id, "obsolete").unwrap();
        assert!(board
            .brief_for(&plan.id, 2)
            .unwrap_err()
            .contains("abandoned"));
    }

    #[test]
    fn tick_untick_and_progress() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = create(&board, "Importer");
        assert!(board.tick(&plan.id, 0, "merged  abc\nreviewed").unwrap());
        assert!(!board.tick(&plan.id, 0, "again").unwrap(), "already done");
        assert!(board
            .tick(&plan.id, 5, "x")
            .unwrap_err()
            .contains("no step 6"));
        assert!(board.untick(&plan.id, 0, "reverted merge").unwrap());
        assert!(!board.untick(&plan.id, 0, "again").unwrap(), "already open");
        assert!(!board.get(&plan.id).unwrap().steps[0].done);
        let text = board.read_markdown(&plan.id).unwrap();
        assert!(text.contains("## Progress"), "{text}");
        assert!(text.contains("step 1 done: merged abc reviewed"), "{text}");
        assert!(text.contains("step 1 reopened: reverted merge"), "{text}");
    }

    #[tokio::test]
    async fn try_close_not_ready_blocked_and_closed() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = create(&board, "Importer");

        let passing = FakeLedger::passing();
        board.tick(&plan.id, 0, "ok").unwrap();
        let CloseOutcome::NotReady { reasons } = board.try_close(&plan.id, &passing).await else {
            panic!("an undone step must not close");
        };
        assert_eq!(reasons.len(), 1);
        assert!(reasons[0].contains("step 2"), "{reasons:?}");

        board.tick(&plan.id, 1, "ok").unwrap();
        let blocked = FakeLedger::blocked(&["w1abcdef is working"]);
        let CloseOutcome::Blocked { reasons } = board.try_close(&plan.id, &blocked).await else {
            panic!("a blocked review must not close");
        };
        assert_eq!(reasons, vec!["w1abcdef is working".to_string()]);
        assert_eq!(board.get(&plan.id).unwrap().status, BoardStatus::Open);
        assert!(!dir.path().join("reviews").exists());

        let CloseOutcome::Closed { report_path, .. } = board.try_close(&plan.id, &passing).await
        else {
            panic!("a ready plan closes");
        };
        let report = std::fs::read_to_string(&report_path).unwrap();
        assert!(report.starts_with("## Verdict: PASS"));
        let name = Path::new(&report_path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with(&format!("team-{}-", plan.id)) && name.ends_with(".md"),
            "{name}"
        );
        let closed = board.get(&plan.id).unwrap();
        assert_eq!(closed.status, BoardStatus::Closed);
        assert_eq!(closed.report_path.as_deref(), Some(report_path.as_str()));
        assert!(closed.closed_at_ms.is_some());
        // Closing again reports the same report; the plan stays on disk.
        assert_eq!(
            board.try_close(&plan.id, &passing).await,
            CloseOutcome::Closed {
                report_path: report_path.clone(),
                retired_notes: 0,
            }
        );
        assert!(dir
            .path()
            .join("plans")
            .join(format!("{}.md", plan.id))
            .exists());
        assert!(board
            .update(&plan.id, PlanEdit::default())
            .unwrap_err()
            .contains("closed"));

        // Reverting a merged step reopens the plan.
        assert!(board.untick(&plan.id, 1, "revert").unwrap());
        let reopened = board.get(&plan.id).unwrap();
        assert_eq!(reopened.status, BoardStatus::Open);
        assert_eq!(reopened.report_path, None);
    }

    #[tokio::test]
    async fn closing_retires_plan_scoped_notes_into_the_report() {
        use crate::memory::{HashEmbedder, Memory, MemoryStore, MemoryTier};
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
        let board = board(dir.path()).with_memory(store.clone());
        let plan = create(&board, "Importer");
        let mut note = Memory::new(
            MemoryTier::Episodic,
            "SPEC: game contract",
            "src/game.js exports Game as a named export",
            100,
        );
        note.data =
            serde_json::json!({"plan_scoped": true, "plan_ids": [plan.id], "branch": false});
        store.write(note).await.unwrap();
        board.tick(&plan.id, 0, "ok").unwrap();
        board.tick(&plan.id, 1, "ok").unwrap();
        let CloseOutcome::Closed {
            report_path,
            retired_notes,
        } = board.try_close(&plan.id, &FakeLedger::passing()).await
        else {
            panic!("expected a close");
        };
        assert_eq!(retired_notes, 1);
        let report = std::fs::read_to_string(&report_path).unwrap();
        assert!(report.starts_with("## Verdict: PASS"), "{report}");
        assert!(
            report.contains("## Plan notes (retired at close)"),
            "{report}"
        );
        assert!(
            report.contains("exports Game as a named export"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn bug_fixing_plans_need_a_regression_test_to_close() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                kind: PlanKind::BugFixing,
                bug: Some("rows vanish".to_string()),
                context: format!(
                    "{CONTEXT} Regression test: importer_keeps_rows in tests/import.rs fails without the fix."
                ),
                steps: vec!["Fix src/import.rs".to_string()],
                ..draft("Bug")
            })
            .unwrap();
        board.tick(&plan.id, 0, "merged").unwrap();
        let ledger = FakeLedger::passing();
        let CloseOutcome::Blocked { reasons } = board.try_close(&plan.id, &ledger).await else {
            panic!("no regression test recorded");
        };
        assert!(reasons[0].contains("regression test"), "{reasons:?}");
        assert!(reasons[0].contains("plan_update"), "{reasons:?}");

        board
            .update(
                &plan.id,
                PlanEdit {
                    regression_test: Some("importer_keeps_rows".to_string()),
                    landed_design: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let CloseOutcome::Blocked { reasons } = board.try_close(&plan.id, &ledger).await else {
            panic!("landed design needs its context amendment");
        };
        assert!(reasons[0].contains("Landed design"), "{reasons:?}");

        board
            .update(
                &plan.id,
                PlanEdit {
                    context_append: Some("Landed design — parser split in two.".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &ledger).await,
            CloseOutcome::Closed { .. }
        ));
    }

    #[test]
    fn abandon_closes_without_review_and_records_why() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = create(&board, "Importer");
        let done = board
            .abandon(&plan.id, "superseded by\nthe rewrite")
            .unwrap();
        assert_eq!(done.status, BoardStatus::Abandoned);
        assert!(done.closed_at_ms.is_some());
        assert!(board
            .read_markdown(&plan.id)
            .unwrap()
            .contains("abandoned: superseded by the rewrite"));
        assert!(board
            .abandon(&plan.id, "again")
            .unwrap_err()
            .contains("abandoned"));
        assert!(board.tick(&plan.id, 0, "x").is_err());
    }

    #[test]
    fn list_orders_open_plans_before_recently_closed_ones() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let first = create(&board, "First");
        let second = create(&board, "Second");
        let third = create(&board, "Third");
        board.abandon(&first.id, "no").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        board.abandon(&third.id, "no").unwrap();
        let ids: Vec<String> = board.list().unwrap().into_iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![second.id, third.id, first.id]);
    }

    #[test]
    fn tail_summary_is_compact_and_lists_undone_steps() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        assert_eq!(board.tail_summary(), "# PLAN BOARD\nNo open plans.\n");
        let plan = create(&board, "Importer");
        board.tick(&plan.id, 0, "ok").unwrap();
        let gone = create(&board, "Gone");
        board.abandon(&gone.id, "no").unwrap();
        assert_eq!(
            board.tail_summary(),
            format!(
                "# PLAN BOARD\n- {} \"Importer\" 1/2 done; todo 2\n",
                plan.id
            )
        );
        for n in 0..9 {
            create(&board, &format!("Extra {n}"));
        }
        let tail = board.tail_summary();
        assert!(tail.contains("... and 2 more (plan_list)"), "{tail}");
    }

    #[tokio::test]
    async fn external_close_requires_a_new_durable_learning() {
        use crate::memory::{HashEmbedder, Memory, MemoryStore, MemoryTier};

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
        let board = board(dir.path()).with_memory(store.clone());
        board.set_external_lead_attached(true);

        let mut old = Memory::new(
            MemoryTier::Semantic,
            "SPEC: Existing importer contract",
            "Rows retain their source order.",
            100,
        );
        old.created_at = 1;
        store.write(old).await.unwrap();
        let plan = create(&board, "Importer");
        board.tick(&plan.id, 0, "ok").unwrap();
        board.tick(&plan.id, 1, "ok").unwrap();
        let ledger = FakeLedger::passing();

        let CloseOutcome::Blocked { reasons } = board.try_close(&plan.id, &ledger).await else {
            panic!("an old learning must not satisfy the external-lead gate");
        };
        assert!(reasons[0].contains("memory_write"), "{reasons:?}");
        assert!(reasons[0].contains("plan_step_done"), "{reasons:?}");

        let mut note = Memory::new(
            MemoryTier::Episodic,
            "SPEC: Importer decision",
            "Use the parser selected for this plan.",
            (now_ms() / 1000) as i64,
        );
        note.data = serde_json::json!({"plan_scoped": true, "plan_ids": [plan.id]});
        store.write(note).await.unwrap();
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "PLAN: Importer",
                "Automatic plan digest, not a durable learning.",
                (now_ms() / 1000) as i64,
            ))
            .await
            .unwrap();
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "REVIEW: Importer",
                "Automatic review digest, not a durable learning.",
                (now_ms() / 1000) as i64,
            ))
            .await
            .unwrap();
        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "SPEC: Commit-bound importer note",
                "Recorded in commit 435c399e, so this is not durable.",
                (now_ms() / 1000) as i64,
            ))
            .await
            .unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &ledger).await,
            CloseOutcome::Blocked { .. }
        ));

        store
            .write(Memory::new(
                MemoryTier::Semantic,
                "DECISION: Keep importer rows ordered",
                "Preserve source order because downstream imports depend on it.",
                (now_ms() / 1000) as i64,
            ))
            .await
            .unwrap();
        let outcome = board.try_close(&plan.id, &ledger).await;
        assert!(matches!(outcome, CloseOutcome::Closed { .. }), "{outcome:?}");
    }

    #[tokio::test]
    async fn external_research_close_also_requires_a_learning() {
        use crate::memory::{HashEmbedder, MemoryStore};

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
        let board = board(dir.path()).with_memory(store);
        board.set_external_lead_attached(true);
        let plan = board
            .create(PlanDraft {
                kind: PlanKind::Research,
                steps: vec!["Investigate the importer (no-code research)".to_string()],
                ..draft("Research importer")
            })
            .unwrap();
        board.tick(&plan.id, 0, "ok").unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn amended_knowledge_satisfies_the_external_close_gate() {
        use crate::memory::{HashEmbedder, Memory, MemoryStore, MemoryTier};

        let dir = tempfile::tempdir().unwrap();
        let knowledge = Arc::new(KnowledgeStore::new(dir.path().join("knowledge")));
        std::fs::create_dir_all(knowledge.dir().join("spec")).unwrap();
        let path = knowledge.dir().join("spec/importer.md");
        std::fs::write(&path, "# SPEC: Importer\n\nOriginal contract.\n").unwrap();
        let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
        let mut record = Memory::new(
            MemoryTier::Semantic,
            "SPEC: Importer",
            "Original contract.",
            100,
        );
        record.created_at = 1;
        record.data = serde_json::json!({"rel_path": "spec/importer.md"});
        store.write(record).await.unwrap();
        let board = board(dir.path())
            .with_memory(store)
            .with_knowledge(knowledge);
        board.set_external_lead_attached(true);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let plan = create(&board, "Importer");
        board.tick(&plan.id, 0, "ok").unwrap();
        board.tick(&plan.id, 1, "ok").unwrap();
        let ledger = FakeLedger::passing();
        assert!(matches!(
            board.try_close(&plan.id, &ledger).await,
            CloseOutcome::Blocked { .. }
        ));

        std::fs::write(&path, "# SPEC: Importer\n\nAmended durable contract.\n").unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &ledger).await,
            CloseOutcome::Closed { .. }
        ));
    }

    #[test]
    fn spec_delta_refuses_modified_requirement_missing_from_the_current_spec() {
        let dir = tempfile::tempdir().unwrap();
        write_spec(dir.path(), "importer", &["Existing"]);
        let board = board(dir.path());
        let mut delta = Delta {
            capability: "importer".to_string(),
            ..Delta::default()
        };
        delta.modified.push(requirement("Missing requirement"));
        let error = board
            .create(PlanDraft {
                spec_deltas: vec![delta],
                ..draft("Invalid delta")
            })
            .unwrap_err();
        assert!(
            error.contains("MODIFIED requirement 'Missing requirement' does not exist"),
            "{error}"
        );
        assert!(board.list().unwrap().is_empty());
    }

    #[test]
    fn spec_delta_refuses_a_duplicate_added_requirement() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let mut delta = added_delta("importer", "Duplicate");
        delta.added.push(requirement("Duplicate"));
        let error = board
            .create(PlanDraft {
                spec_deltas: vec![delta],
                ..draft("Duplicate delta")
            })
            .unwrap_err();
        assert!(
            error.contains("ADDED requirement 'Duplicate' already exists"),
            "{error}"
        );
    }

    #[test]
    fn spec_delta_refuses_non_normative_text_and_incomplete_scenarios() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let mut delta = added_delta("importer", "Invalid");
        delta.added[0].text = "The system keeps the contract.".to_string();
        let error = board
            .create(PlanDraft {
                spec_deltas: vec![delta.clone()],
                ..draft("Non normative")
            })
            .unwrap_err();
        assert!(error.contains("must contain SHALL or MUST"), "{error}");

        let mut delta = added_delta("importer", "Invalid");
        delta.added[0].scenarios = vec![Scenario::new(
            "Missing condition",
            vec![],
            vec!["a result".to_string()],
            vec![],
        )];
        let error = board
            .create(PlanDraft {
                spec_deltas: vec![delta.clone()],
                ..draft("Missing when")
            })
            .unwrap_err();
        assert!(error.contains("is missing WHEN"), "{error}");

        delta.added[0].scenarios[0].when = vec!["a condition".to_string()];
        delta.added[0].scenarios[0].then.clear();
        let error = board
            .create(PlanDraft {
                spec_deltas: vec![delta],
                ..draft("Missing then")
            })
            .unwrap_err();
        assert!(error.contains("is missing THEN"), "{error}");
    }

    #[test]
    fn plan_update_can_modify_a_requirement_added_by_an_earlier_delta() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                acceptance: vec!["The lead's observable outcome remains unchanged.".to_string()],
                spec_deltas: vec![added_delta("importer", "Rows survive")],
                ..draft("Sequential deltas")
            })
            .unwrap();
        let before_update = board.read_markdown(&plan.id).unwrap();
        let mut modified = Delta {
            capability: "importer".to_string(),
            ..Delta::default()
        };
        modified.modified.push(requirement("Rows survive"));
        modified.modified[0].text = "The system MUST retain every imported row.".to_string();
        let updated = board
            .update(
                &plan.id,
                PlanEdit {
                    spec_deltas: vec![modified],
                    ..PlanEdit::default()
                },
            )
            .unwrap();
        assert_eq!(updated.spec_deltas.len(), 2);
        let markdown = board.read_markdown(&plan.id).unwrap();
        let acceptance = |doc: &str| {
            doc.split("## Acceptance scenarios\n")
                .nth(1)
                .and_then(|rest| rest.split("\n## ").next())
                .expect("lead-authored acceptance section")
                .to_string()
        };
        assert_eq!(acceptance(&markdown), acceptance(&before_update));
        assert!(!acceptance(&markdown).contains("OpenSpec"), "{markdown}");
        assert!(markdown.contains("#### ADDED Requirements"), "{markdown}");
        assert!(markdown.contains("#### MODIFIED Requirements"), "{markdown}");
        assert!(markdown.contains("MUST retain every imported row"), "{markdown}");
    }

    #[test]
    fn delta_scenarios_alone_satisfy_the_high_risk_acceptance_contract() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                acceptance: vec![],
                spec_deltas: vec![added_delta("importer", "Rows survive")],
                ..draft("Delta acceptance")
            })
            .unwrap();
        let markdown = board.read_markdown(&plan.id).unwrap();
        assert!(!markdown.contains("## Acceptance scenarios"), "{markdown}");
        let spec_changes = markdown
            .split("## Spec changes\n")
            .nth(1)
            .and_then(|rest| rest.split("\n## ").next())
            .expect("spec changes section");
        assert!(spec_changes.contains("Scenario: Contract exercised"), "{markdown}");
        assert!(spec_changes.contains("- **WHEN**"), "{markdown}");
        assert!(spec_changes.contains("- **THEN**"), "{markdown}");
        let brief = board.brief_for(&plan.id, 1).unwrap();
        assert!(!brief.contains("## Acceptance scenarios"), "{brief}");
        assert!(brief.contains("Contract exercised"), "{brief}");
        assert!(brief.contains("## Spec changes"), "{brief}");
    }

    #[test]
    fn board_json_round_trips_deltas_and_plan_rewrites_keep_spec_changes() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                spec_deltas: vec![added_delta("importer", "Rows survive")],
                ..draft("Persistent deltas")
            })
            .unwrap();
        board
            .update(
                &plan.id,
                PlanEdit {
                    progress_note: Some("implemented".to_string()),
                    ..PlanEdit::default()
                },
            )
            .unwrap();
        let stored: BoardFile = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("plans").join(BOARD_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(stored.plans[0].spec_deltas, plan.spec_deltas);
        let markdown = board.read_markdown(&plan.id).unwrap();
        assert_eq!(markdown.matches("## Spec changes").count(), 1, "{markdown}");
        let reopened = PlanBoard::new(dir.path().join("plans"), dir.path().join("reviews"))
            .get(&plan.id)
            .unwrap();
        assert_eq!(reopened.spec_deltas, plan.spec_deltas);
        assert_eq!(
            reopened.spec_deltas[0].added[0].scenarios[0].when,
            vec!["the contract is exercised".to_string()]
        );
    }

    #[test]
    fn built_in_schema_preserves_existing_plan_and_dispatch_behaviour() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = create(&board, "Existing style");
        assert_eq!(plan.schema, "spec-driven@1");
        assert!(plan.sections.is_empty());
        assert!(board.dispatch_blockers(&plan.id).unwrap().is_empty());
        let markdown = board.read_markdown(&plan.id).unwrap();
        assert!(!markdown.contains("## Project context"));
        assert!(markdown.contains("## Steps"));
    }

    #[tokio::test]
    async fn close_with_added_delta_merges_the_spec_and_writes_the_archive() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                title: "Added capability".to_string(),
                goal: "Keep imports lossless".to_string(),
                context: "Why: imported rows must retain their source data.".to_string(),
                spec_deltas: vec![added_delta("importer", "Preserve source rows")],
                ..draft("Added capability")
            })
            .unwrap();
        board
            .update(
                &plan.id,
                PlanEdit {
                    decision: Some("Use the existing parser".to_string()),
                    interface: Some("src/import.rs keeps its public API".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        complete_steps(&board, &plan.id);

        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));
        let spec = std::fs::read_to_string(
            dir.path().join("openspec/specs/importer/spec.md"),
        )
        .unwrap();
        assert!(spec.contains("### Requirement: Preserve source rows"), "{spec}");

        let archive = archive_dir(dir.path(), &plan.id, "added-capability");
        for relative in [
            "proposal.md",
            "design.md",
            "tasks.md",
            "specs/importer/spec.md",
            ".openspec.yaml",
        ] {
            assert!(archive.join(relative).is_file(), "missing {relative}");
        }
        let proposal = std::fs::read_to_string(archive.join("proposal.md")).unwrap();
        assert!(proposal.contains("Keep imports lossless"), "{proposal}");
        assert!(proposal.contains("Why: imported rows"), "{proposal}");
        let design = std::fs::read_to_string(archive.join("design.md")).unwrap();
        assert!(design.contains("Use the existing parser"), "{design}");
        assert!(design.contains("src/import.rs keeps its public API"), "{design}");
        let tasks = std::fs::read_to_string(archive.join("tasks.md")).unwrap();
        assert_eq!(tasks.matches("- [x]").count(), 2, "{tasks}");
        assert!(!tasks.contains("- [ ]"), "{tasks}");
        let delta = std::fs::read_to_string(archive.join("specs/importer/spec.md")).unwrap();
        assert!(delta.starts_with("## ADDED Requirements\n"), "{delta}");
        assert!(delta.contains("### Requirement: Preserve source rows"), "{delta}");
        let metadata: BTreeMap<String, String> = serde_norway::from_str(
            &std::fs::read_to_string(archive.join(".openspec.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata.get("schema").map(String::as_str), Some("spec-driven"));
        assert_eq!(metadata.get("mnemo_plan"), Some(&plan.id));
    }

    #[tokio::test]
    async fn close_applies_modified_removed_and_renamed_requirements() {
        let dir = tempfile::tempdir().unwrap();
        write_spec(dir.path(), "importer", &["Modify me", "Remove me", "Rename me"]);
        let board = board(dir.path());
        let mut modified = requirement("Modify me");
        modified.text = "The system MUST retain every imported row.".to_string();
        let delta = Delta {
            capability: "importer".to_string(),
            modified: vec![modified],
            removed: vec![crate::openspec::RemovedRequirement {
                requirement: "Remove me".to_string(),
                reason: "The legacy behavior is obsolete.".to_string(),
            }],
            renamed: vec![crate::openspec::RenamedRequirement {
                from: "Rename me".to_string(),
                to: "Renamed requirement".to_string(),
            }],
            ..Default::default()
        };
        let plan = board
            .create(PlanDraft {
                spec_deltas: vec![delta],
                ..draft("Update capability")
            })
            .unwrap();
        complete_steps(&board, &plan.id);

        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));
        let spec = std::fs::read_to_string(
            dir.path().join("openspec/specs/importer/spec.md"),
        )
        .unwrap();
        assert!(spec.contains("The system MUST retain every imported row."), "{spec}");
        assert!(!spec.contains("### Requirement: Remove me"), "{spec}");
        assert!(!spec.contains("### Requirement: Rename me\n"), "{spec}");
        assert!(spec.contains("### Requirement: Renamed requirement"), "{spec}");
        let delta = std::fs::read_to_string(
            archive_dir(dir.path(), &plan.id, "update-capability")
                .join("specs/importer/spec.md"),
        )
        .unwrap();
        assert!(delta.contains("## MODIFIED Requirements"), "{delta}");
        assert!(delta.contains("## REMOVED Requirements"), "{delta}");
        assert!(delta.contains("The legacy behavior is obsolete."), "{delta}");
        assert!(delta.contains("## RENAMED Requirements"), "{delta}");
        assert!(delta.contains("FROM: Rename me\nTO: Renamed requirement"), "{delta}");
    }

    #[tokio::test]
    async fn conflicting_delta_blocks_close_and_writes_no_openspec_files() {
        let dir = tempfile::tempdir().unwrap();
        write_spec(dir.path(), "importer", &["Original requirement"]);
        let board = board(dir.path());
        let delta = Delta {
            capability: "importer".to_string(),
            modified: vec![requirement("Original requirement")],
            ..Default::default()
        };
        let plan = board
            .create(PlanDraft {
                spec_deltas: vec![delta],
                ..draft("Conflicting delta")
            })
            .unwrap();
        complete_steps(&board, &plan.id);
        write_spec(dir.path(), "importer", &["Replacement requirement"]);
        let spec_path = dir.path().join("openspec/specs/importer/spec.md");
        let before = std::fs::read(&spec_path).unwrap();

        let CloseOutcome::Blocked { reasons } =
            board.try_close(&plan.id, &FakeLedger::passing()).await
        else {
            panic!("a stale delta must block the close");
        };
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        for expected in ["importer", "MODIFIED", "Original requirement", "plan_update"] {
            assert!(reasons[0].contains(expected), "missing {expected}: {reasons:?}");
        }
        assert_eq!(std::fs::read(&spec_path).unwrap(), before);
        assert!(!dir.path().join("openspec/changes").exists());
        assert_eq!(board.get(&plan.id).unwrap().status, BoardStatus::Open);
    }

    #[tokio::test]
    async fn close_without_deltas_writes_no_openspec_files() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = create(&board, "Ordinary plan");
        complete_steps(&board, &plan.id);

        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));
        assert!(!dir.path().join("openspec").exists());
    }

    #[tokio::test]
    async fn close_with_custom_schema_archives_its_plan_sections() {
        let dir = tempfile::tempdir().unwrap();
        research_schema(dir.path(), true);
        let board = board(dir.path());
        let plan = board
            .create_with_sections(
                PlanDraft {
                    spec_deltas: vec![added_delta("importer", "Preserve source rows")],
                    ..draft("Custom archive")
                },
                research_section("Inspected src/import.rs: rows retain their source order."),
            )
            .unwrap();
        complete_steps(&board, &plan.id);

        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));
        let archive = archive_dir(dir.path(), &plan.id, "custom-archive");
        let research = std::fs::read_to_string(archive.join("research.md")).unwrap();
        assert_eq!(
            research,
            "Inspected src/import.rs: rows retain their source order.\n"
        );
        assert!(archive.join("proposal.md").is_file());
        assert!(archive.join("tasks.md").is_file());
        let metadata: BTreeMap<String, String> = serde_norway::from_str(
            &std::fs::read_to_string(archive.join(".openspec.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata.get("schema").map(String::as_str), Some("custom"));
        assert_eq!(metadata.get("mnemo_plan"), Some(&plan.id));
        assert!(!archive.join("specs").exists());
        assert!(dir.path().join("openspec/specs/importer/spec.md").is_file());
    }

    #[tokio::test]
    async fn a_close_retries_after_the_archive_succeeded_but_the_report_failed() {
        let dir = tempfile::tempdir().unwrap();
        let board = board(dir.path());
        let plan = board
            .create(PlanDraft {
                spec_deltas: vec![added_delta("importer", "Preserve source rows")],
                ..draft("Archive retry")
            })
            .unwrap();
        complete_steps(&board, &plan.id);

        // A plain file where the reviews directory should be makes the report
        // write fail while the OpenSpec writes before it already succeeded.
        std::fs::write(dir.path().join("reviews"), "not a directory").unwrap();
        let CloseOutcome::Blocked { reasons } =
            board.try_close(&plan.id, &FakeLedger::passing()).await
        else {
            panic!("a failed report write must block the close");
        };
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("could not write the review report")),
            "{reasons:?}"
        );
        assert!(dir
            .path()
            .join("openspec/specs/importer/spec.md")
            .is_file());
        assert_eq!(board.get(&plan.id).unwrap().status, BoardStatus::Open);

        // Repair the reviews directory and retry: the close must not re-apply
        // the already-merged and already-archived spec changes.
        std::fs::remove_file(dir.path().join("reviews")).unwrap();
        std::fs::create_dir(dir.path().join("reviews")).unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));
        let spec =
            std::fs::read_to_string(dir.path().join("openspec/specs/importer/spec.md")).unwrap();
        assert_eq!(
            spec.matches("### Requirement: Preserve source rows")
                .count(),
            1,
            "{spec}"
        );
        let archive = dir.path().join("openspec/changes/archive");
        assert_eq!(std::fs::read_dir(&archive).unwrap().count(), 1);
        assert_eq!(board.get(&plan.id).unwrap().status, BoardStatus::Closed);
    }

    #[test]
    fn custom_required_section_shows_guidance_and_recorded_schema_survives_config_changes() {
        let dir = tempfile::tempdir().unwrap();
        research_schema(dir.path(), true);
        let board = board(dir.path());

        let missing = board.create(draft("Missing research")).unwrap_err();
        for expected in [
            "artifact 'research'",
            "# Research",
            "Replace with findings.",
            "Investigate and record evidence.",
            "Cite the inspected files.",
            "sections.research",
        ] {
            assert!(missing.contains(expected), "missing {expected:?}: {missing}");
        }
        let unchanged = board
            .create_with_sections(
                draft("Template research"),
                research_section("# Research\n\nReplace with findings.\n"),
            )
            .unwrap_err();
        assert!(unchanged.contains("still identical to its template"), "{unchanged}");
        assert!(unchanged.contains("Cite the inspected files."), "{unchanged}");

        let plan = board
            .create_with_sections(
                draft("Researched"),
                research_section("Inspected src/import.rs; the parser drops rows."),
            )
            .unwrap();
        assert_eq!(plan.schema, "custom@7");
        assert!(board.dispatch_blockers(&plan.id).unwrap().is_empty());
        let markdown = board.read_markdown(&plan.id).unwrap();
        assert!(markdown.contains("## research\n\nInspected src/import.rs"), "{markdown}");
        let brief = board.brief_for(&plan.id, 1).unwrap();
        assert!(brief.contains("## Project context\n\nProject schema context."), "{brief}");
        assert!(brief.contains("### Artifact: research\nInvestigate and record evidence."));
        assert!(brief.contains("## research\n\nInspected src/import.rs"));

        std::fs::write(
            dir.path().join("openspec/config.yaml"),
            "schema: spec-driven\ncontext: Changed project context.\nrules:\n  research: New rule\n",
        )
        .unwrap();
        let updated = board
            .update(
                &plan.id,
                PlanEdit {
                    progress_note: Some("schema remains pinned".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.schema, "custom@7");
        assert_eq!(updated.sections, plan.sections);
        assert!(board.brief_for(&plan.id, 1).unwrap().contains("Changed project context."));
    }

    #[test]
    fn apply_requires_blocks_dispatch_until_the_section_is_supplied() {
        let dir = tempfile::tempdir().unwrap();
        research_schema(dir.path(), false);
        let board = board(dir.path());
        let plan = board.create(draft("Research before apply")).unwrap();

        let blockers = board.dispatch_blockers(&plan.id).unwrap();
        assert_eq!(blockers.len(), 1, "{blockers:?}");
        assert!(blockers[0].contains("artifact 'research'"));
        assert!(blockers[0].contains("# Research"));
        assert!(blockers[0].contains("Cite the inspected files."));

        let updated = board
            .update_with_sections(
                &plan.id,
                PlanEdit::default(),
                Some(research_section("Evidence: src/import.rs retains every row.")),
            )
            .unwrap();
        assert_eq!(updated.sections["research"], "Evidence: src/import.rs retains every row.");
        assert!(board.dispatch_blockers(&plan.id).unwrap().is_empty());
        assert!(board
            .read_markdown(&plan.id)
            .unwrap()
            .contains("## research\n\nEvidence: src/import.rs retains every row."));
    }

    #[tokio::test]
    async fn external_bug_fixing_close_is_exempt_from_the_learning_gate() {
        use crate::memory::{HashEmbedder, MemoryStore};

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
        let board = board(dir.path()).with_memory(store);
        board.set_external_lead_attached(true);
        let plan = board
            .create(PlanDraft {
                kind: PlanKind::BugFixing,
                bug: Some("rows vanish".to_string()),
                context: format!(
                    "{CONTEXT} Regression test: importer_keeps_rows in tests/import.rs fails without the fix."
                ),
                steps: vec!["Fix src/import.rs".to_string()],
                ..draft("Bug")
            })
            .unwrap();
        board
            .update(
                &plan.id,
                PlanEdit {
                    regression_test: Some("importer_keeps_rows".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        board.tick(&plan.id, 0, "merged").unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));
    }

    #[tokio::test]
    async fn built_in_and_store_less_external_close_skip_the_learning_gate() {
        use crate::memory::{HashEmbedder, MemoryStore};

        let dir = tempfile::tempdir().unwrap();
        let external = board(dir.path());
        external.set_external_lead_attached(true);
        let store = Arc::new(MemoryStore::open_in_memory(Arc::new(HashEmbedder::new())).unwrap());
        let board = board(dir.path()).with_memory(store);
        let plan = create(&board, "Importer");
        board.tick(&plan.id, 0, "ok").unwrap();
        board.tick(&plan.id, 1, "ok").unwrap();
        assert!(matches!(
            board.try_close(&plan.id, &FakeLedger::passing()).await,
            CloseOutcome::Closed { .. }
        ));

        let plan = create(&external, "Store-less external");
        external.tick(&plan.id, 0, "ok").unwrap();
        external.tick(&plan.id, 1, "ok").unwrap();
        assert!(matches!(
            external
                .try_close(&plan.id, &FakeLedger::passing())
                .await,
            CloseOutcome::Closed { .. }
        ));
    }
}
