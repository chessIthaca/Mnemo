// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The enforced plan-first workflow.
//!
//! The agent operates under a hard workflow gate, not a soft prompt instruction.
//! `Workflow::allowed_tools()` returns a `ToolFilter` that the agent loop applies
//! before building the request schema — in the Planning state, write tools are
//! omitted entirely so the model cannot call them.

pub mod plan_file;

pub use plan_file::{PlanFile, PlanKind, Step};

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::tool::ToolFilter;

/// The workflow state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowState {
    /// No plan yet — only read tools + `create_plan` are available.
    #[default]
    Planning,
    /// A plan exists and is being executed — all tools available, including
    /// `create_plan` (replanning abandons the in-flight plan).
    Executing,
    /// All steps done; the review→fix→commit closing sequence is in flight.
    /// Entered when the root plan's last step completes. The closing tools
    /// (`spawn_agent`, `git`, `file_edit`, `file_append`, `shell`) are
    /// available here so the review + commit can run. [`Workflow::finish`] is
    /// the only exit to [`Complete`](WorkflowState::Complete), and it is gated
    /// on a non-empty review report under `.coding/reviews/` — so the review is
    /// unskippable by construction, not by convention.
    Reviewing,
    /// All steps complete and the closing sequence finished — read tools only.
    Complete,
    /// A skill is active — the skill's tool allow-list + prompt overlay apply
    /// instead of the base-state filter. Entered via `start_skill` (from
    /// Planning or Complete); exited via `end_skill` (→ target_state) or
    /// `abandon_skill` (→ the pre-skill state). The active skill's name +
    /// prompt are held on the [`Workflow`] itself.
    Skill,
    /// A parented sub-agent's role state — like [`Skill`](Self::Skill), this
    /// is NOT a lifecycle phase. It is stamped by the spawn path
    /// (`spawn_agent_shared`, src-tauri/src/ipc/spawn.rs) on every agent with
    /// a parent, replacing whatever state [`Workflow::load_latest`] derived
    /// from the shared main plan. The plan stack stays loaded as a READ-ONLY
    /// MIRROR of the main plan (so `current_plan` shows reviewers the plan
    /// under review and the UI staircase keeps its context), but the state
    /// itself never claims Executing/Reviewing — a sub-agent is a single-task
    /// worker, not a plan-lifecycle owner. Invariants: plan mutations are
    /// denied (`set_plan_mutations_allowed(false)`) and a tool allow-list is
    /// always set (`compute_subagent_allowlist` returns `Some` for every
    /// parented spawn), so [`Workflow::allowed_tools`] never consults the
    /// state-derived filter here — reaching it without an allow-list is an
    /// invariant violation and fails loudly. Never persisted: restart
    /// re-derivation belongs to main-like agents only.
    Subagent,
}

impl std::fmt::Display for WorkflowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Planning => f.write_str("Planning"),
            Self::Executing => f.write_str("Executing"),
            Self::Reviewing => f.write_str("Reviewing"),
            Self::Complete => f.write_str("Complete"),
            Self::Skill => f.write_str("Skill"),
            Self::Subagent => f.write_str("Subagent"),
        }
    }
}

/// A single plan on the stack: the plan document plus its on-disk id.
#[derive(Debug, Clone)]
pub struct PlanFrame {
    /// The plan document (title/goal/context/steps).
    pub plan: PlanFile,
    /// The on-disk id (file stem of `.coding/plans/<id>.md`).
    pub id: String,
}

/// The active skill overlay on the workflow.
///
/// While a skill is active, the workflow is in [`WorkflowState::Skill`] and the
/// skill's tool allow-list + prompt apply instead of the base-state filter.
/// `pre_skill_state` records where the workflow was when the skill started so
/// [`abandon_skill`](Workflow::abandon_skill) can roll back to it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActiveSkill {
    /// The skill name (selects the registry entry).
    pub name: String,
    /// The goal the agent drives toward while the skill is active.
    pub prompt: String,
    /// Where the workflow lands after [`end_skill`](Workflow::end_skill).
    pub target_state: WorkflowState,
    /// Where the workflow was when the skill started —
    /// [`abandon_skill`](Workflow::abandon_skill) returns here.
    pub pre_skill_state: WorkflowState,
    /// The tool allow-list active while the skill runs. Seeded from the
    /// registry at `start_skill` time so [`allowed_tools`](Workflow::allowed_tools)
    /// is self-contained (no registry lookup needed at filter time).
    pub tools: Vec<String>,
}

/// The on-disk sidecar shape written by [`Workflow::persist_stack`].
///
/// `stack` is the ordered list of plan ids (root → active). `skill` is the
/// active skill overlay, or `None` when no skill is in flight. `reviewed` is
/// true once [`Workflow::finish`] has closed out the active plan's review — it
/// lets [`Workflow::load_latest`] distinguish `Reviewing` (review pending) from
/// `Complete` (review done) on restart, since both have a fully-checked plan.
/// The legacy bare-array form (`[ids...]`) is still accepted on load for
/// backward compat.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StackSidecar {
    stack: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skill: Option<ActiveSkill>,
    /// Whether the active plan's review has been closed out via `finish`.
    /// Defaults to `false` for older sidecars (review pending).
    #[serde(default)]
    reviewed: bool,
}

/// The workflow, holding a stack of plans and the derived state.
///
/// The stack models nested work like a call stack: `create_plan` while a plan
/// is executing pushes a **sub-plan** on top; the parent plan stays underneath,
/// untouched. When the sub-plan completes (or is abandoned), it pops and the
/// parent resumes where it left off. The *active* plan is the top of the stack.
#[derive(Debug, Clone)]
pub struct Workflow {
    state: WorkflowState,
    /// The plan stack; the last frame is the active plan. Empty in Planning.
    stack: Vec<PlanFrame>,
    plans_dir: PathBuf,
    /// The active skill overlay. `Some` while in [`WorkflowState::Skill`].
    active_skill: Option<ActiveSkill>,
    /// Whether plan-mutation workflow tools are allowed for the owner of this
    /// workflow. Under the main-agent-only policy (Phase 4), sub-agents
    /// (spawned via `spawn_agent`) have this set to false; they may only read.
    plan_mutations_allowed: bool,
    /// An in-memory tool allow-list that overrides the state-derived
    /// [`ToolFilter`] when set. Used to constrain a spawned sub-agent to a
    /// read-only surface (e.g. a `role: "reviewer"` spawn is limited to read
    /// tools + `git_diff` + `write_review_report`). When `Some`,
    /// [`allowed_tools`](Self::allowed_tools) returns [`ToolFilter::Skill`]
    /// seeded with this list regardless of the workflow state — the
    /// constraint holds even though the sub-agent shares the main agent's
    /// plan, whose state it never inherits (a parented sub-agent is stamped
    /// [`WorkflowState::Subagent`], a role state, never a lifecycle phase) —
    /// unless
    /// [`tool_allowlist_reviewer`](Self::tool_allowlist_reviewer) is set, in
    /// which case [`ToolFilter::Reviewer`] is returned instead (a STRICT
    /// allow-list: no auto-granted memory/ask/backlog tools). NOT persisted
    /// in [`StackSidecar`]: it's a spawn-time property, like
    /// `plan_mutations_allowed`, and must not leak onto the shared on-disk
    /// plan state.
    tool_allowlist: Option<Vec<String>>,
    /// Whether the in-memory [`tool_allowlist`](Self::tool_allowlist) came
    /// from a `role: "reviewer"` spawn and must therefore be enforced as a
    /// strict [`ToolFilter::Reviewer`] allow-list (nothing auto-granted)
    /// rather than the permissive [`ToolFilter::Skill`] allow-list. Set by
    /// [`set_reviewer_allowlist`](Self::set_reviewer_allowlist); cleared
    /// whenever a plain [`set_tool_allowlist`](Self::set_tool_allowlist) is
    /// called.
    tool_allowlist_reviewer: bool,
    /// Whether the active plan's review has been closed out via [`finish`].
    /// Drives the `Reviewing` vs `Complete` distinction on restart: a
    /// fully-checked plan with `reviewed == false` re-derives `Reviewing`
    /// (review still pending); `reviewed == true` re-derives `Complete`.
    reviewed: bool,
}

impl Workflow {
    /// Create a new workflow with no plan (Planning state).
    pub fn new(plans_dir: impl Into<PathBuf>) -> Self {
        Self {
            state: WorkflowState::Planning,
            stack: Vec::new(),
            plans_dir: plans_dir.into(),
            active_skill: None,
            plan_mutations_allowed: true,
            tool_allowlist: None,
            tool_allowlist_reviewer: false,
            reviewed: false,
        }
    }

    /// The current workflow state.
    pub fn state(&self) -> WorkflowState {
        self.state
    }

    /// The project root implied by the plans dir layout (`<root>/.coding/plans`).
    ///
    /// Used by tools that need to reach the repo from the workflow handle —
    /// e.g. `create_plan`'s optional `branch` fork runs git at the repo root.
    /// Returns `None` when the plans dir does not imply a root: either too
    /// shallow (fewer than two parent components) or shallow AND relative
    /// (`Path::parent()` on `"plans"` yields `Some("")`, which would be an
    /// empty `current_dir` for git) — callers must skip the git step rather
    /// than guess.
    pub fn project_root(&self) -> Option<std::path::PathBuf> {
        let root = self.plans_dir.parent()?.parent()?.to_path_buf();
        if root.as_os_str().is_empty() {
            None
        } else {
            Some(root)
        }
    }

    /// The active skill overlay, if the workflow is in the Skill state.
    pub fn active_skill(&self) -> Option<&ActiveSkill> {
        self.active_skill.as_ref()
    }

    /// Whether this workflow allows its owner to call plan-mutation tools.
    /// Sub-agents under the main-agent-only policy have this false (read-only
    /// workflows).
    pub fn plan_mutations_allowed(&self) -> bool {
        self.plan_mutations_allowed
    }

    /// Set whether plan mutations are allowed. Called by the runtime when
    /// constructing a sub-agent workflow.
    pub fn set_plan_mutations_allowed(&mut self, allowed: bool) {
        self.plan_mutations_allowed = allowed;
    }

    /// The in-memory tool allow-list that overrides the state-derived filter,
    /// if one has been set (e.g. for a read-only reviewer sub-agent). `None`
    /// means the normal state-derived filter applies.
    pub fn tool_allowlist(&self) -> Option<&[String]> {
        self.tool_allowlist.as_deref()
    }

    /// Set an in-memory tool allow-list that overrides the state-derived
    /// [`ToolFilter`] for this workflow. Called by the spawner when a
    /// `spawn_agent` call passes `role: "reviewer"` — the spawned agent is
    /// then constrained to the named tools regardless of its (shared) plan
    /// state. Pass `None` to clear the override (restore state-derived
    /// filtering).
    ///
    /// This is the permissive variant: the list is enforced as a
    /// [`ToolFilter::Skill`] allow-list, which still auto-grants memory
    /// tools + `ask_user` + `current_plan` + the backlog tools
    /// (add/status/list — backlog 66c65db9). For the strict reviewer
    /// surface use [`set_reviewer_allowlist`](Self::set_reviewer_allowlist).
    pub fn set_tool_allowlist(&mut self, list: Option<Vec<String>>) {
        self.tool_allowlist = list;
        self.tool_allowlist_reviewer = false;
    }

    /// Set an in-memory tool allow-list enforced as a STRICT
    /// [`ToolFilter::Reviewer`] allow-list: only the named tools +
    /// `current_plan` are visible — no auto-granted memory/ask/backlog
    /// tools, no plan tools. Called by the spawner when a `spawn_agent` call
    /// passes `role: "reviewer"`, so the reviewer sub-agent cannot mutate
    /// memory, the backlog, the plan, or the review. Pass `None` to clear
    /// the override (restore state-derived filtering).
    pub fn set_reviewer_allowlist(&mut self, list: Option<Vec<String>>) {
        self.tool_allowlist = list;
        self.tool_allowlist_reviewer = true;
    }

    /// Stamp this workflow as a parented sub-agent's
    /// ([`WorkflowState::Subagent`]).
    ///
    /// Called by the spawn path (`spawn_agent_shared`) for every agent with a
    /// parent, after [`Workflow::load_latest`] has populated the plan stack
    /// from the shared plans dir. The stack is KEPT — it is the read-only
    /// mirror that lets `current_plan` show reviewers the plan under review
    /// and the UI show the plan staircase — but the derived lifecycle state
    /// (Executing/Reviewing/…) is replaced: a sub-agent is a single-task
    /// worker, not a plan-lifecycle owner, and no behavioral gate may key on
    /// a state it never owned. Idempotent.
    pub fn enter_subagent_state(&mut self) {
        self.state = WorkflowState::Subagent;
    }

    /// The active plan (top of the stack), if any.
    pub fn plan(&self) -> Option<&PlanFile> {
        self.stack.last().map(|f| &f.plan)
    }

    /// The active plan id, if any.
    pub fn plan_id(&self) -> Option<&str> {
        self.stack.last().map(|f| f.id.as_str())
    }

    /// The kind of the active (top-of-stack) plan, if any.
    ///
    /// `None` when no plan is active (Planning with an empty stack). The top
    /// of the stack governs: a bug-fixing sub-plan pushed under an
    /// implementation root makes the session resolve the
    /// `[models.bug_fixing]` slot while it executes, and pop back to the
    /// executing slot when it completes. Consumed by the agent loop's
    /// per-iteration model resolution (see [`crate::model_resolver`]).
    pub fn active_plan_kind(&self) -> Option<PlanKind> {
        self.stack.last().map(|f| f.plan.kind)
    }

    /// The id of the ROOT plan (bottom of the stack), if any.
    ///
    /// Unlike [`plan_id`](Self::plan_id) — which tracks the active (top) plan
    /// — this is constant across sub-plan push/pop and skill transitions; it
    /// changes only when a fresh top-level plan starts (`create_plan` from
    /// Planning or Complete). Carried by the `WorkflowStateChanged` event so
    /// the frontend can reset per-top-plan UI (e.g. the Diff tab's changed-file
    /// list) exactly when a new top-level plan begins.
    pub fn top_plan_id(&self) -> Option<&str> {
        self.stack.first().map(|f| f.id.as_str())
    }

    /// The full plan stack, root first (index 0) to active (last).
    pub fn plan_stack(&self) -> &[PlanFrame] {
        &self.stack
    }

    /// How many plans deep the stack is (0 = Planning, 1 = a single root plan).
    pub fn plan_depth(&self) -> usize {
        self.stack.len()
    }

    /// The directory plans are written to (`.coding/plans/`). Exposed so the
    /// IPC layer can read a plan by id (the clickable staircase fetches an
    /// ancestor plan to view it read-only).
    pub fn plans_dir(&self) -> &std::path::Path {
        &self.plans_dir
    }

    /// The titles of the ancestor plans below the active one, root first.
    pub fn parent_titles(&self) -> Vec<String> {
        self.stack[..self.stack.len().saturating_sub(1)]
            .iter()
            .map(|f| f.plan.title.clone())
            .collect()
    }

    /// The ancestor plans below the active one, root first — each as
    /// `(id, title)`. Used by the workflow-state wire so the frontend staircase
    /// can click an ancestor to view it (the `id` is needed to fetch the plan).
    pub fn parent_infos(&self) -> Vec<(String, String)> {
        self.stack[..self.stack.len().saturating_sub(1)]
            .iter()
            .map(|f| (f.id.clone(), f.plan.title.clone()))
            .collect()
    }

    /// The tool filter for the current state.
    ///
    /// When a skill is active, this returns [`ToolFilter::Skill`] seeded with
    /// the skill's tool allow-list (stored on the active skill at
    /// `start_skill` time, so no registry lookup is needed here).
    ///
    /// When an in-memory [`tool_allowlist`](Self::tool_allowlist) is set (e.g.
    /// for a read-only reviewer sub-agent), it takes precedence over
    /// everything — returning [`ToolFilter::Skill`] seeded with that list
    /// (or [`ToolFilter::Reviewer`] when the list came from a `role:
    /// "reviewer"` spawn, see [`set_reviewer_allowlist`]) regardless of
    /// state. This is what constrains a spawned reviewer to read
    /// tools + `git_diff` + `write_review_report`: its workflow state is
    /// [`WorkflowState::Subagent`] (a role state stamped by the spawn path),
    /// and the allow-list — not any lifecycle state — defines its surface.
    pub fn allowed_tools(&self) -> ToolFilter {
        // Note: plan-mutation tools (create_plan/update_plan/complete_step/
        // abandon_plan) are restricted to the main agent, but that restriction
        // is enforced separately — at dispatch (dispatch.rs), in the schema
        // post-filter (turn.rs), and inside each tool wrapper (plan.rs). It is
        // deliberately NOT applied here: collapsing the whole surface to the
        // Planning filter for sub-agents would also strip every write/exec
        // agent tool (file_write, file_edit, shell, git, spawn_agent, …),
        // which would make sub-agents unable to do any real work. Sub-agents
        // carry a spawn-time allow-list (the intersection of the parent's
        // current permissions) and simply cannot mutate plans.
        //
        // The in-memory tool_allowlist (set for a read-only reviewer spawn)
        // overrides the state-derived filter entirely. The variant depends on
        // where the list came from: a `role: "reviewer"` spawn sets
        // tool_allowlist_reviewer → ToolFilter::Reviewer (strict allow-list,
        // nothing auto-granted); any other allow-list is ToolFilter::Skill
        // (permissive: memory/ask/backlog auto-granted, as before).
        if let Some(list) = &self.tool_allowlist {
            if self.tool_allowlist_reviewer {
                return ToolFilter::Reviewer(list.clone());
            }
            return ToolFilter::Skill(list.clone());
        }
        match (&self.state, &self.active_skill) {
            (WorkflowState::Skill, Some(skill)) => ToolFilter::Skill(skill.tools.clone()),
            // `from_state` returns `None` for `Skill` (the skill filter depends
            // on the active skill's tool list, handled above). Reaching here in
            // the Skill state means the invariant is violated (Skill with no
            // active skill) — fail loudly rather than silently degrading to a
            // wrong filter.
            // Subagent: the spawn path ALWAYS sets a tool allow-list
            // (`compute_subagent_allowlist` returns `Some` for every parented
            // spawn), so the allow-list branch above already returned.
            // Reaching here in the Subagent state means the invariant is
            // violated (Subagent with no allow-list) — fail loudly, mirroring
            // the Skill case.
            (WorkflowState::Subagent, _) => {
                panic!("Subagent-state workflow must carry a tool allow-list (set by the spawn path)")
            }
            // Executing a research plan narrows the surface: a plan that by
            // definition produces no source-code changes should not be
            // offered the source-mutating file tools. Keyed on the ACTIVE
            // plan (the one being executed), not the root — a research root
            // may push an implementation sub-plan, and that sub-plan does
            // need to write.
            (WorkflowState::Executing, _)
                if self.plan().map(|p| p.kind) == Some(PlanKind::Research) =>
            {
                ToolFilter::ExecutingResearch
            }
            _ => ToolFilter::from_state(self.state)
                .expect("from_state is Some for all lifecycle states"),
        }
    }

    /// The tool filter for the SCHEMA array this request — the plan-frozen
    /// advertisement.
    ///
    /// While a plan is active (Executing/Reviewing), the tools array must be
    /// byte-stable for the plan's whole lifetime: the array rides at the head
    /// of the request body, and the provider prefix cache reuses the body
    /// only up to the first changed byte — so the array changing at the
    /// Executing→Reviewing transition resets the cached prefix and re-bills
    /// the entire conversation (~40-75s of server-side prefill at late-plan
    /// context sizes; perf review L4, 2026-09-09). This method therefore
    /// returns the ONE plan-frozen surface ([`ToolFilter::PlanFrozen`] —
    /// Executing ∪ `finish`) for every plan kind, research included: the array
    /// must survive a research↔implementation transition as well, and a
    /// per-kind surface changed it on every plan change (measured 2027-01-11:
    /// the cached prefix collapsed to 6,016 tokens with 15.7–17.9s TTFT —
    /// `.coding/analysis/cache-hit-6-report.md`).
    ///
    /// The research restriction is NOT enforced by this choice: dispatch
    /// re-checks the research arm of [`allowed_tools`](Self::allowed_tools)
    /// ([`ToolFilter::ExecutingResearch`]), exactly as
    /// [`ToolFilter::PlanFrozen`] documents for `finish`.
    ///
    /// Outside an active plan (Planning/Complete — no plan, or the plan was
    /// popped at finish/abandon) and for allow-listed sub-agents
    /// (reviewer/skill), the per-state [`allowed_tools`](Self::allowed_tools)
    /// applies unchanged: Planning still hides every mutation tool (the
    /// no-changes-without-a-plan hard rule), and a reviewer's strict
    /// allow-list is never widened.
    ///
    /// ADVERTISEMENT ONLY: dispatch re-checks the per-state
    /// [`allowed_tools`](Self::allowed_tools), so tools advertised by the
    /// frozen surface but not allowed in the current state
    /// (complete_step/create_plan during Reviewing, finish during Executing)
    /// are rejected at dispatch with the state named in the error. The array
    /// is what the model sees; the state filter is what it may call.
    pub fn schema_filter(&self) -> ToolFilter {
        // Allow-listed sub-agents (reviewer/skill) keep their
        // constructor-granted surface — the freeze is a main-agent,
        // plan-scoped mechanism.
        if self.tool_allowlist.is_some() {
            return self.allowed_tools();
        }
        match self.state {
            WorkflowState::Executing | WorkflowState::Reviewing if self.plan().is_some() => {
                // ONE surface for every plan kind, research included: the array
                // must survive a research↔implementation transition too. A
                // per-kind surface rewrote the head on every plan change —
                // measured 2027-01-11: a kind change collapsed the provider
                // cache to 6,016 tokens (the common system prefix) and cost
                // 15.7–17.9s TTFT (`.coding/analysis/cache-hit-6-report.md`,
                // cause 2). The research restriction is enforced at dispatch
                // (`ExecutingResearch` in `ToolFilter::allows`), never here.
                ToolFilter::PlanFrozen
            }
            _ => self.allowed_tools(),
        }
    }

    /// The first unchecked step of the active plan (the resume point).
    pub fn current_step(&self) -> Option<&Step> {
        self.plan().and_then(|p| p.current_step())
    }

    /// Create a plan and transition to Executing.
    ///
    /// Stack behavior:
    /// - **Planning / Complete**: pushes a fresh root plan (any prior completed
    ///   plan is cleared first).
    /// - **Executing**: pushes a **sub-plan** on top of the current plan. The
    ///   parent stays underneath and resumes when the sub-plan completes or is
    ///   abandoned.
    ///
    /// Always available — the agent can plan nested work, or abandon a stale
    /// plan via [`abandon_plan`](Self::abandon_plan) then re-plan.
    ///
    /// This creates an **implementation** plan (the default) — completing the
    /// root plan's last step enters `Reviewing`. For an investigation/planning
    /// plan that changes no source code, use
    /// [`create_plan_with_kind`](Self::create_plan_with_kind) with
    /// [`PlanKind::Research`] to skip the review and go straight to `Complete`.
    pub fn create_plan(
        &mut self,
        title: &str,
        goal: &str,
        context: &str,
        steps: Vec<String>,
    ) -> Result<String> {
        self.create_plan_with_kind(title, goal, context, steps, PlanKind::Implementation, None)
    }

    /// Create a plan with an explicit [`PlanKind`] and transition to Executing.
    ///
    /// Like [`create_plan`](Self::create_plan) but lets the caller choose the
    /// kind, which decides what happens when the root plan's last step
    /// completes (see [`complete_step`](Self::complete_step)):
    /// - [`PlanKind::Implementation`] → `Reviewing` (the review→fix→commit
    ///   closing sequence; `finish` is gated on a reviewer report).
    /// - [`PlanKind::Research`] → `Complete` directly (no review — for
    ///   investigation/planning that produces no source-code changes).
    /// - [`PlanKind::BugFixing`] → `Reviewing` (bug plans get reviewed like
    ///   implementation plans), with the locked 4-step skeleton + the
    ///   required `bug_symptom` (the `bug:{symptom}` param) + the finish
    ///   auto-capture.
    ///
    /// The kind is persisted in the plan's `.md` (a `## Kind` section) so a
    /// restart re-derives the same completion behavior.
    ///
    /// The plan id is a SHORT, collision-checked handle (the first 8 hex
    /// chars of a UUIDv4, extended while a plan file with that name exists) —
    /// easy for a model to transcribe into `complete_step`'s `plan_id` guard
    /// and stable as the plan file's name.
    pub fn create_plan_with_kind(
        &mut self,
        title: &str,
        goal: &str,
        context: &str,
        steps: Vec<String>,
        kind: PlanKind,
        bug_symptom: Option<&str>,
    ) -> Result<String> {
        self.create_plan_with_kind_and_detail(title, goal, context, steps, kind, bug_symptom, None)
    }

    /// The full creation path — [`create_plan_with_kind`](Self::create_plan_with_kind)
    /// plus the caller's detailed steps for bug plans (backlog 77ff8f45):
    /// the locked 4-step skeleton stays the checklist, and the caller's
    /// step-level recipes persist as the `## Detailed steps` section so
    /// the plan file is the COMPLETE crash-resumption document — a
    /// restarted session resumes with the full recipe (paths, anchors,
    /// test designs) instead of re-deriving it from the bug + context
    /// alone. `None` for other kinds and for bug plans created without
    /// steps.
    pub fn create_plan_with_kind_and_detail(
        &mut self,
        title: &str,
        goal: &str,
        context: &str,
        steps: Vec<String>,
        kind: PlanKind,
        bug_symptom: Option<&str>,
        detailed_steps: Option<&str>,
    ) -> Result<String> {
        // A plan with no steps is not a plan — it cannot enter Executing (the
        // workflow gate would expose write tools with nothing to execute).
        if steps.is_empty() {
            return Err(crate::error::Error::InvalidInput(
                "plan must have at least one step".into(),
            ));
        }
        // Starting fresh from a non-executing state clears any finished stack.
        if self.state != WorkflowState::Executing {
            self.stack.clear();
        }
        let uuid_hex = uuid::Uuid::new_v4().simple().to_string();
        let id = unique_plan_id(&self.plans_dir, &uuid_hex);
        let mut plan = PlanFile::new(title, goal, context, steps);
        plan.kind = kind;
        plan.bug_symptom = bug_symptom.map(str::to_string);
        plan.detailed_steps = detailed_steps.map(str::to_string);
        std::fs::create_dir_all(&self.plans_dir)?;
        plan.write_to_dir(&self.plans_dir, &id)?;
        self.stack.push(PlanFrame {
            plan,
            id: id.clone(),
        });
        self.state = WorkflowState::Executing;
        self.persist_stack()?;
        Ok(id)
    }

    /// Update the active plan in place — the non-destructive alternative to
    /// [`abandon_plan`](Self::abandon_plan) + re-planning.
    ///
    /// Only the **active** (top-of-stack) plan can be updated, and only while
    /// the workflow is `Executing` (in Planning there is no plan yet — use
    /// [`create_plan`](Self::create_plan); in Complete the plan is finished —
    /// start a fresh one).
    ///
    /// - `title` / `goal` / `context`: when `Some` and non-empty, they replace
    ///   the corresponding fields. `None` (or an empty string) leaves the field
    ///   untouched. Exception: with `append: true` a non-empty `context` is
    ///   **extended** as `{old}\n\n{new}` instead of replaced (replacing when
    ///   the existing context is empty, so no leading blank line appears).
    /// - `steps`: when `Some`, it **replaces the remaining (not-yet-completed)
    ///   steps** with this list. Completed steps — the done prefix up to the
    ///   first unchecked step — are preserved verbatim and never reordered.
    ///   Provide more entries than remain to append work; fewer to trim. Must
    ///   be non-empty (omit the argument entirely to keep the current steps).
    ///   With `append: true` the entries are **added AFTER the remaining
    ///   steps** instead of replacing them (the remaining steps need not be
    ///   resent) — the chunked-write protocol for very long plans: create with
    ///   the first few steps, then extend per chunk.
    ///   **Out-of-order completion is rejected:** if a completed step sits
    ///   *after* an incomplete one (so the completed prefix is ambiguous),
    ///   this returns an error rather than silently dropping the later-done
    ///   step — complete steps in order before updating. (`append` never
    ///   discards anything — prefix, remaining, and new steps are all kept —
    ///   but the same guard applies.)
    /// - `append`: when `true`, `steps` entries are added after the remaining
    ///   steps (see above) and a non-empty `context` is extended instead of
    ///   replaced. Default `false`.
    ///
    /// At least one of `title` / `goal` / `context` / `steps` /
    /// `regression_test` / `landed_design` must be provided (a no-op call is
    /// an error). The workflow stays in its current state (`Executing` or
    /// `Reviewing`); the on-disk plan file is rewritten in place (same id).
    ///
    /// **BugFixing skeleton lock:** for `kind = BugFixing` plans the 4-step
    /// skeleton is locked — `steps` replacement AND `steps` append are refused
    /// (the reproduce → root-cause → fix → verify order is the bug protocol);
    /// title/goal/context/regression_test/landed_design updates are allowed.
    pub fn update_plan(
        &mut self,
        title: Option<&str>,
        goal: Option<&str>,
        context: Option<&str>,
        steps: Option<Vec<String>>,
        append: bool,
        regression_test: Option<&str>,
        landed_design: Option<bool>,
    ) -> Result<()> {
        // update_plan only adjusts an in-flight plan. In Planning there's no
        // plan; in Complete the plan is done (start a fresh one). In Reviewing
        // the plan is finished but the closing sequence is still running —
        // fixing review findings, re-running tests — and may legitimately need
        // to adjust title/goal/context or append steps for follow-on fix work
        // (backlog 24e1c98e: update_plan callable in Reviewing, main agent
        // only). Two invariants hold: complete_step stays hidden in Reviewing
        // (steps can't be checked off mid-review; finish still gates the exit
        // on the review report), and the reviewer subagent never sees the tool
        // at all (ToolFilter::Reviewer strict allow-list never names
        // plan-mutation tools; every subagent additionally runs with
        // plan_mutations_allowed(false), enforced at dispatch). The
        // regression_test field remains accepted (the finish↔update_plan
        // deadlock fix, 2026-12-04).
        match self.state {
            WorkflowState::Executing | WorkflowState::Reviewing => {}
            _ => {
                return Err(crate::error::Error::WorkflowWrongState {
                    current: self.state.to_string(),
                    expected: WorkflowState::Executing.to_string(),
                });
            }
        }
        let frame = self
            .stack
            .last_mut()
            .ok_or_else(|| crate::error::Error::WorkflowNoPlan)?;
        // The BugFixing skeleton lock: steps replacement AND steps append are
        // refused for bug plans — the locked skeleton is the bug protocol,
        // and growing it would let the reproduce/verify discipline slip.
        if frame.plan.kind == PlanKind::BugFixing && steps.is_some() {
            return Err(crate::error::Error::InvalidInput(
                "bug_fixing plans have a locked 4-step skeleton — steps cannot be \
                 replaced or appended; title/goal/context/regression_test/landed_design \
                 updates are allowed"
                    .into(),
            ));
        }
        let mut changed = false;
        if let Some(t) = title {
            if !t.trim().is_empty() {
                frame.plan.title = t.to_string();
                changed = true;
            }
        }
        if let Some(g) = goal {
            if !g.trim().is_empty() {
                frame.plan.goal = g.to_string();
                changed = true;
            }
        }
        if let Some(c) = context {
            if !c.trim().is_empty() {
                if append && !frame.plan.context.trim().is_empty() {
                    // Chunked-write protocol: extend the context instead of
                    // replacing it (blank-line separated, like plan sections).
                    frame.plan.context = format!("{}\n\n{}", frame.plan.context, c);
                } else {
                    frame.plan.context = c.to_string();
                }
                changed = true;
            }
        }
        if let Some(rt) = regression_test {
            if !rt.trim().is_empty() {
                // Collapse to a single line — the plan file's
                // `## Regression test` section is one line, and an embedded
                // newline would re-parse as a section header or truncate on
                // reload (section injection, review HIGH 2).
                frame.plan.regression_test =
                    Some(rt.split_whitespace().collect::<Vec<_>>().join(" "));
                changed = true;
            }
        }
        // The feature-scale marker (backlog e5a84ce9): set by the agent when
        // a bug_fixing fix turned out feature-scale — the finish gate then
        // requires a "Landed design" context amendment. Some(false) un-sets
        // it (a mistaken declaration).
        if let Some(ld) = landed_design {
            frame.plan.landed_design = ld;
            changed = true;
        }
        if let Some(new_steps) = steps {
            if new_steps.is_empty() {
                return Err(crate::error::Error::InvalidInput(
                    "steps must be non-empty (omit the field to keep the current steps)".into(),
                ));
            }
            // Preserve the completed prefix: every step up to (but not
            // including) the first not-done step. Under in-order completion
            // (the normal flow) this is exactly the done steps.
            let split = frame
                .plan
                .steps
                .iter()
                .position(|s| !s.done)
                .unwrap_or(frame.plan.steps.len());
            // Guard against out-of-order completion: if any step *after* the
            // first not-done one is already done, replacing `steps[split..]`
            // would silently drop that completed step's record. Refuse rather
            // than lose data — the agent should complete steps in order before
            // updating. (append=true never touches steps[..split+kept] either,
            // so the same guard applies.)
            if frame.plan.steps[split..].iter().any(|s| s.done) {
                return Err(crate::error::Error::Workflow(
                    "cannot update plan: steps were completed out of order — a \
                     completed step sits after an incomplete one, and replacing \
                     the remaining steps would lose it. Complete steps in order \
                     before updating."
                        .into(),
                ));
            }
            let mut combined: Vec<Step> = frame.plan.steps[..split].to_vec();
            if append {
                // Chunked-write protocol: keep the remaining steps and add the
                // new ones AFTER them (no resending of what already exists).
                combined.extend(frame.plan.steps[split..].iter().cloned());
            }
            for text in new_steps {
                let header = crate::workflow::plan_file::extract_bold_header_pub(&text);
                combined.push(Step {
                    index: combined.len(),
                    text,
                    header,
                    done: false,
                });
            }
            // Re-index so positions are contiguous after the edit.
            for (i, s) in combined.iter_mut().enumerate() {
                s.index = i;
            }
            frame.plan.steps = combined;
            changed = true;
        }
        if !changed {
            return Err(crate::error::Error::InvalidInput(
                "update_plan changed nothing — provide at least one of \
                 title/goal/context/steps/regression_test/landed_design, non-blank. \
                 A no-op update means the update content was never written: draft \
                 the update in your reply FIRST, then emit the call carrying it"
                    .into(),
            ));
        }
        // Rewrite the plan file in place (same id). The stack order/ids are
        // unchanged, so stack.json needs no rewrite.
        frame.plan.write_to_dir(&self.plans_dir, &frame.id)?;
        Ok(())
    }

    /// Complete a step of the active plan by index.
    ///
    /// When the active plan becomes fully complete:
    /// - **Sub-plan** (a parent remains): **popped** so the parent resumes
    ///   (Executing), regardless of kind.
    /// - **Root plan** (nothing beneath it): **retained** on the stack and the
    ///   workflow transitions based on the plan's [`PlanKind`]:
    ///   - [`PlanKind::Implementation`] → [`Reviewing`](WorkflowState::Reviewing)
    ///     — the review→fix→commit closing sequence. [`finish`](Self::finish) is
    ///     the only path onward to `Complete`, gated on a review report.
    ///   - [`PlanKind::Research`] → [`Complete`](WorkflowState::Complete)
    ///     directly — no review (the plan changed no source code). `reviewed` is
    ///     set to `true` so a restart re-derives `Complete`.
    ///
    /// Keeping the finished frame means a restart re-derives the right state
    /// (and the completed plan stays visible in the UI) instead of dropping to
    /// Planning and losing the record.
    pub fn complete_step(&mut self, step_index: usize) -> Result<()> {
        let frame = self
            .stack
            .last_mut()
            .ok_or_else(|| crate::error::Error::WorkflowNoPlan)?;
        frame.plan.complete_step(step_index)?;
        // Persist the updated plan to disk.
        frame.plan.write_to_dir(&self.plans_dir, &frame.id)?;
        if frame.plan.is_complete() {
            // Copy the kind out of the (mutably borrowed) frame before any
            // further `self.stack` access — reading it inside the branch below
            // would extend the mutable borrow past `self.stack.len()`/`pop()`
            // and into `persist_stack()`. `PlanKind` is `Copy`.
            let kind = frame.plan.kind;
            if self.stack.len() > 1 {
                // Sub-plan finished — pop it and resume the parent, regardless
                // of kind (sub-plans never trigger the review sequence).
                self.stack.pop();
                self.state = WorkflowState::Executing;
            } else {
                // Root plan finished — keep it. The kind decides whether the
                // review closing sequence runs (Implementation/BugFixing →
                // Reviewing) or is skipped (Research → Complete directly).
                match kind {
                    PlanKind::Implementation | PlanKind::BugFixing => {
                        self.state = WorkflowState::Reviewing;
                        self.reviewed = false;
                    }
                    PlanKind::Research => {
                        self.state = WorkflowState::Complete;
                        self.reviewed = true;
                    }
                }
            }
            self.persist_stack()?;
        }
        Ok(())
    }

    /// Tick a detailed sub-step (by 1-indexed number) on the ACTIVE plan —
    /// the checkable sub-items under `## Detailed steps` (backlog 9441d776:
    /// a mid-fix restart must see exactly which sub-steps shipped). Unlike
    /// [`complete_step`](Self::complete_step) this NEVER transitions the
    /// workflow: sub-steps are in-step progress inside the locked
    /// bug_fixing skeleton, so ticking the last one completes nothing —
    /// only the skeleton's own steps drive state. Returns the progress
    /// summary from [`PlanFile::complete_detailed_step`].
    pub fn complete_detailed_step(&mut self, detailed_step_index: usize) -> Result<String> {
        let frame = self
            .stack
            .last_mut()
            .ok_or_else(|| crate::error::Error::WorkflowNoPlan)?;
        let summary = frame.plan.complete_detailed_step(detailed_step_index)?;
        // Persist the updated plan to disk — the marks must survive
        // restarts (the whole point of the tick).
        frame.plan.write_to_dir(&self.plans_dir, &frame.id)?;
        Ok(summary)
    }

    /// Whether completing `step_index` on the ACTIVE plan would exit the
    /// Executing phase — i.e. this is the FINAL step of the ROOT plan (a
    /// sub-plan's last step merely pops to the parent, which stays
    /// Executing). Pure query: nothing is mutated. The dispatch layer uses
    /// this to gate only the review-exit complete_step call on running
    /// descendants (backlog 569b5922) while non-final checklist ticks
    /// proceed freely during parallel work.
    pub fn completing_step_exits_executing(&self, step_index: usize) -> bool {
        if self.stack.len() != 1 {
            // A sub-plan's completion pops to the parent (still Executing);
            // only the ROOT plan's completion is a phase exit.
            return false;
        }
        let Some(frame) = self.stack.last() else {
            return false;
        };
        !frame.plan.is_complete() && frame.plan.step_completes_plan(step_index)
    }

    /// Close out the review and transition [`Reviewing`](WorkflowState::Reviewing)
    /// → [`Complete`](WorkflowState::Complete).
    ///
    /// This is the only path from `Reviewing` to `Complete` (`abandon_plan`
    /// is the escape hatch back to `Planning`), and the `finish` workflow
    /// tool is its sole caller — the tool gates on a non-empty review report
    /// under `.coding/reviews/` before calling this, so the review is
    /// unskippable by construction. Sets the persisted `reviewed` flag so a
    /// restart re-derives `Complete` (not `Reviewing`) for this plan.
    ///
    /// # Errors
    ///
    /// Returns a workflow error if the current state is not `Reviewing`.
    pub fn finish(&mut self) -> Result<()> {
        if self.state != WorkflowState::Reviewing {
            return Err(crate::error::Error::WorkflowWrongState {
                current: self.state.to_string(),
                expected: WorkflowState::Reviewing.to_string(),
            });
        }
        self.state = WorkflowState::Complete;
        self.reviewed = true;
        self.persist_stack()?;
        Ok(())
    }

    /// Deterministic task-entry transition:
    /// [`Complete`](WorkflowState::Complete) →
    /// [`Planning`](WorkflowState::Planning). Used by the Run-All backlog
    /// dispatcher so a dispatched item puts the resting agent into Planning
    /// BEFORE its turn starts — immediate UI feedback plus Planning-state
    /// prompt guidance, instead of relying on the model to call `create_plan`
    /// unprompted (the Complete-state PLAN_NUDGE stays advisory for chat).
    ///
    /// Deliberate narrowness:
    ///
    /// - The plan stack is left UNTOUCHED. In `Complete` it holds the
    ///   just-finished plan (legitimately non-empty); that plan stays visible
    ///   across the transition, and `create_plan` clears the stack when the
    ///   agent plans the new task (fresh root from a non-Executing state).
    /// - The transition is NOT persisted. The finished plan's stored state
    ///   stays `Complete`, so a crash between this transition and the
    ///   agent's `create_plan` reloads the workflow as `Complete` (via
    ///   `load_latest`) and Run-All simply re-dispatches — self-healing, and
    ///   a finished plan's record is never rewritten as `Planning`.
    /// - It never counts as plan-loop evidence: the dispatcher emits the
    ///   state change straight to the UI (bypassing the runtime event
    ///   channel), and even a channel-observed transition is wiped by
    ///   `TurnResolveLatch::on_started` when the dispatched turn begins — a
    ///   turn that never plans still cannot be marked `Done`.
    ///
    /// # Errors
    ///
    /// Returns a workflow error if the current state is not `Complete` —
    /// the transition is only meaningful for a RESTING agent; Executing/
    /// Reviewing carry an active plan and Skill an active overlay that must
    /// not be clobbered from the outside.
    pub fn enter_planning_for_task(&mut self) -> Result<()> {
        if self.state != WorkflowState::Complete {
            return Err(crate::error::Error::WorkflowWrongState {
                current: self.state.to_string(),
                expected: WorkflowState::Complete.to_string(),
            });
        }
        self.state = WorkflowState::Planning;
        Ok(())
    }

    /// Abandon the active plan without completing it: pop it off the stack and
    /// resume the parent (or return to Planning if the stack empties).
    ///
    /// Returns the title of the abandoned plan, or an error if no plan exists.
    pub fn abandon_plan(&mut self) -> Result<String> {
        let frame = self
            .stack
            .pop()
            .ok_or_else(|| crate::error::Error::WorkflowNoPlan)?;
        self.state = if self.stack.is_empty() {
            WorkflowState::Planning
        } else {
            WorkflowState::Executing
        };
        self.persist_stack()?;
        Ok(frame.plan.title)
    }

    /// Start a skill: record the pre-skill state, store the skill overlay, and
    /// transition to [`WorkflowState::Skill`]. The skill's tool allow-list +
    /// prompt take over from the base-state filter until
    /// [`end_skill`](Self::end_skill) or [`abandon_skill`](Self::abandon_skill).
    ///
    /// `tools` is the registry's allow-list for this skill (seeded onto the
    /// active skill so [`allowed_tools`](Self::allowed_tools) is self-contained).
    /// Returns an error if a skill is already active (no nesting).
    pub fn start_skill(
        &mut self,
        name: &str,
        prompt: &str,
        target_state: WorkflowState,
        tools: Vec<String>,
    ) -> Result<()> {
        if self.state == WorkflowState::Skill {
            return Err(crate::error::Error::Workflow(
                "a skill is already active — end or abandon it before starting another".into(),
            ));
        }
        self.active_skill = Some(ActiveSkill {
            name: name.to_string(),
            prompt: prompt.to_string(),
            target_state,
            pre_skill_state: self.state,
            tools,
        });
        self.state = WorkflowState::Skill;
        self.persist_stack()?;
        Ok(())
    }

    /// End the active skill: transition to the skill's `target_state` and clear
    /// the overlay. Returns an error if no skill is active.
    pub fn end_skill(&mut self) -> Result<()> {
        let skill = self
            .active_skill
            .take()
            .ok_or_else(|| crate::error::Error::Workflow("no skill is active".into()))?;
        self.state = skill.target_state;
        self.persist_stack()?;
        Ok(())
    }

    /// Abandon the active skill: roll back to the pre-skill state (where the
    /// workflow was when the skill started) and clear the overlay. Returns an
    /// error if no skill is active.
    pub fn abandon_skill(&mut self) -> Result<()> {
        let skill = self
            .active_skill
            .take()
            .ok_or_else(|| crate::error::Error::Workflow("no skill is active".into()))?;
        self.state = skill.pre_skill_state;
        self.persist_stack()?;
        Ok(())
    }

    /// Persist the plan stack + active skill + reviewed flag to a sidecar file
    /// so a restart resumes the full nesting AND an in-flight skill AND the
    /// review-pending vs review-done distinction. No-op-safe: writes
    /// `stack.json` next to the plans.
    ///
    /// The sidecar shape is `{"stack":[ids...],"skill":{...}|null,"reviewed":bool}`.
    /// The legacy bare-array form (`[ids...]`) is still read by
    /// [`load_latest`](Self::load_latest).
    fn persist_stack(&self) -> Result<()> {
        std::fs::create_dir_all(&self.plans_dir)?;
        let json = serde_json::to_string(&StackSidecar {
            stack: self.stack.iter().map(|f| f.id.clone()).collect::<Vec<_>>(),
            skill: self.active_skill.clone(),
            reviewed: self.reviewed,
        })
        .map_err(|e| crate::error::Error::Workflow(format!("serialize stack: {e}")))?;
        std::fs::write(self.plans_dir.join("stack.json"), json)?;
        Ok(())
    }

    /// Load the plan stack + active skill from disk (for resume on restart).
    ///
    /// Reads the `stack.json` sidecar written by [`persist_stack`](Self::persist_stack)
    /// to rebuild the full nesting (root → active). Accepts both the current
    /// object form (`{"stack":[...],"skill":{...}|null}`) and the legacy bare
    /// array form (`[ids...]`). If the sidecar is missing (older single-plan
    /// layout), falls back to the most-recently-modified `.md` plan as a
    /// one-deep stack. State is derived from the active plan's checkboxes: any
    /// unchecked → Executing, all checked → Reviewing (review pending) or
    /// Complete (review done, per the persisted `reviewed` flag), none →
    /// Planning — unless an active skill was persisted, in which case the state
    /// is Skill.
    pub fn load_latest(&mut self) -> Result<()> {
        self.stack.clear();
        self.active_skill = None;
        if !self.plans_dir.exists() {
            self.state = WorkflowState::Planning;
            return Ok(());
        }

        let sidecar = self.plans_dir.join("stack.json");
        let mut ids: Vec<String> = Vec::new();
        let mut sidecar_present = false;
        let mut persisted_skill: Option<ActiveSkill> = None;
        let mut persisted_reviewed: bool = false;
        if sidecar.exists() {
            if let Ok(text) = std::fs::read_to_string(&sidecar) {
                sidecar_present = true;
                // Try the current object form first; fall back to the legacy
                // bare array.
                if let Ok(sc) = serde_json::from_str::<StackSidecar>(&text) {
                    ids = sc.stack;
                    persisted_skill = sc.skill;
                    persisted_reviewed = sc.reviewed;
                } else {
                    // Legacy bare-array form (`[ids...]`). If this parse also
                    // fails, the sidecar is corrupt — log a warning rather than
                    // silently treating it as an empty stack (which would mask
                    // corruption as "no plan").
                    match serde_json::from_str::<Vec<String>>(&text) {
                        Ok(parsed) => ids = parsed,
                        Err(e) => {
                            eprintln!(
                                "warning: plan stack sidecar at {} failed to parse \
                                 (both object and bare-array forms): {e} — treating as empty",
                                sidecar.display()
                            );
                        }
                    }
                }
            }
        }
        // Fallback for the legacy single-plan layout: latest .md. Only when the
        // sidecar is ABSENT — an explicit empty stack (`[]` / `{"stack":[]}`)
        // means "no active plan", not "go hunt for the newest .md".
        if ids.is_empty() && !sidecar_present {
            if let Some(id) = self.latest_plan_id() {
                ids.push(id);
            }
        }

        // Load each frame, skipping any whose file vanished OR that parses to
        // zero steps. A plan with no steps is not a plan (e.g. a free-form .md
        // doc with no "## Steps" checklist) and must not drive the workflow
        // into Executing — otherwise it would expose write tools with nothing
        // to execute and stall the gate.
        for id in ids {
            let path = self.plans_dir.join(format!("{id}.md"));
            if path.exists() {
                let plan = PlanFile::read_from_file(&path)?;
                if plan.steps.is_empty() {
                    continue;
                }
                self.stack.push(PlanFrame { plan, id });
            }
        }

        // Restore the persisted `reviewed` flag BEFORE the skill early-return
        // below — otherwise a restart while a skill is active would leave
        // `reviewed` at its `new()` default (false), and the next persist_stack
        // (e.g. end_skill) would write that stale false back, corrupting the
        // sidecar (a closed-out review would re-derive Reviewing on a later
        // restart).
        self.reviewed = persisted_reviewed;

        // If a skill was persisted, resume in the Skill state with its overlay.
        if let Some(skill) = persisted_skill {
            self.active_skill = Some(skill);
            self.state = WorkflowState::Skill;
            return Ok(());
        }

        // Derive state from the active (top) plan. A fully-checked plan is
        // Reviewing when the review hasn't been closed out (reviewed == false)
        // and Complete when it has — the `reviewed` flag is what survives the
        // restart, since the plan checkboxes alone can't tell the two apart.
        self.state = match self.plan() {
            None => WorkflowState::Planning,
            Some(p) if p.is_complete() => {
                if persisted_reviewed {
                    WorkflowState::Complete
                } else {
                    WorkflowState::Reviewing
                }
            }
            Some(_) => WorkflowState::Executing,
        };
        Ok(())
    }

    /// The id of the most-recently-modified plan file, if any (legacy fallback).
    fn latest_plan_id(&self) -> Option<String> {
        // Use `flatten` so one unreadable dir entry is skipped rather than
        // aborting the whole scan (the old `entry.ok()?` returned `None` on the
        // first bad entry, masking all later plans).
        let mut entries: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
        for entry in std::fs::read_dir(&self.plans_dir).ok()?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        entries.push((path, modified));
                    }
                }
            }
        }
        entries.sort_by_key(|(_, t)| *t);
        let (path, _) = entries.pop()?;
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    }
}

/// Derive a short, collision-checked plan id from a fresh UUID's hex: the
/// first 8 chars, extended one char at a time while a plan file with that
/// name already exists in `plans_dir` (in practice never past 8 — an 8-hex
/// collision is ~1 in 4 billion — but the check makes the short-id invariant
/// unconditional). The short id is the plan file's stem and the handle the
/// model transcribes into `complete_step`'s `plan_id` guard; the full hex is
/// the exhaust fallback (a collision there is cryptographically impossible).
fn unique_plan_id(plans_dir: &std::path::Path, hex: &str) -> String {
    let mut len = 8;
    loop {
        let end = len.min(hex.len());
        let candidate = &hex[..end];
        if !plans_dir.join(format!("{candidate}.md")).exists() || end == hex.len() {
            return candidate.to_string();
        }
        len += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{SafetyLevel, ToolCategory};
    use tempfile::tempdir;

    #[test]
    fn new_workflow_starts_in_planning() {
        let dir = tempdir().unwrap();
        let wf = Workflow::new(dir.path());
        assert_eq!(wf.state(), WorkflowState::Planning);
        assert!(wf.plan().is_none());
        assert!(wf.current_step().is_none());
    }

    #[test]
    fn create_plan_rejects_empty_steps() {
        // A plan with no steps is not a plan — it must not enter Executing.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        let result = wf.create_plan("Empty", "G", "C", vec![]);
        assert!(result.is_err());
        assert_eq!(wf.state(), WorkflowState::Planning);
        assert_eq!(wf.plan_depth(), 0);
    }

    #[test]
    fn enter_planning_for_task_from_complete() {
        // Run-All dispatch entry: a finished (research) plan rests in
        // Complete; the dispatcher transitions it to Planning before the
        // item's turn runs.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Investigate",
            "G",
            "C",
            vec!["Look around".to_string()],
            PlanKind::Research,
            None,
        )
        .unwrap();
        assert_eq!(wf.state(), WorkflowState::Executing);
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);

        wf.enter_planning_for_task().unwrap();
        assert_eq!(wf.state(), WorkflowState::Planning);
        // The finished plan stays visible — the stack is untouched.
        assert!(wf.plan().is_some());
        // And the agent can plan the new task from Planning (create_plan
        // clears the finished stack, fresh root).
        let id = wf
            .create_plan("Next task", "G", "C", vec!["Do it".to_string()])
            .unwrap();
        assert_eq!(wf.state(), WorkflowState::Executing);
        assert_eq!(wf.plan_id(), Some(id.as_str()));
        assert_eq!(wf.plan_depth(), 1);
    }

    #[test]
    fn enter_planning_for_task_rejects_every_other_state() {
        let dir = tempdir().unwrap();
        // Planning (fresh workflow, nothing to plan-entry from).
        let mut wf = Workflow::new(dir.path().join("p"));
        assert!(wf.enter_planning_for_task().is_err());
        assert_eq!(wf.state(), WorkflowState::Planning);
        // Executing — an active plan must not be clobbered.
        let mut wf = Workflow::new(dir.path().join("e"));
        wf.create_plan("T", "G", "C", vec!["S".to_string()])
            .unwrap();
        assert!(wf.enter_planning_for_task().is_err());
        assert_eq!(wf.state(), WorkflowState::Executing);
        // Reviewing — the closing sequence is in flight.
        let mut wf = Workflow::new(dir.path().join("r"));
        wf.create_plan("T", "G", "C", vec!["S".to_string()])
            .unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        assert!(wf.enter_planning_for_task().is_err());
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // Skill — an active overlay must not be clobbered.
        let mut wf = Workflow::new(dir.path().join("s"));
        wf.start_skill("merge_to_main", "prompt", WorkflowState::Complete, vec![])
            .unwrap();
        assert!(wf.enter_planning_for_task().is_err());
        assert_eq!(wf.state(), WorkflowState::Skill);
    }

    #[test]
    fn load_latest_skips_stepless_plan_files() {
        // A free-form .md with no "## Steps" checklist parses to zero steps.
        // It must NOT drive the workflow into Executing on resume.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        // A step-less plan doc (free-form, no checklist).
        std::fs::write(
            plans_dir.join("freeform.md"),
            "# Plan: Doc\n\n## Goal\nJust prose, no steps.\n",
        )
        .unwrap();
        // Point the (empty) stack sidecar at it so it's the resume candidate.
        std::fs::write(plans_dir.join("stack.json"), "[\"freeform\"]").unwrap();

        let mut wf = Workflow::new(plans_dir);
        wf.load_latest().unwrap();
        assert_eq!(wf.state(), WorkflowState::Planning);
        assert_eq!(wf.plan_depth(), 0);
    }

    #[test]
    fn load_latest_empty_sidecar_is_planning_not_latest_plan() {
        // An explicit empty stack.json means "no active plan" — it must NOT
        // fall back to resuming the newest .md on disk (which could be a stale
        // or complete plan). The mtime fallback only applies when the sidecar
        // is absent (legacy layout).
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        // A complete plan on disk (would otherwise resume as Complete).
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("Old", "G", "C", vec!["done".into()])
            .unwrap();
        wf1.complete_step(0).unwrap();
        // Overwrite the sidecar with an explicit empty stack.
        std::fs::write(plans_dir.join("stack.json"), "[]").unwrap();

        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Planning);
        assert_eq!(wf2.plan_depth(), 0);
    }

    #[test]
    fn load_latest_mixed_stepless_and_real_plans() {
        // With a step-less doc AND a real plan in the stack, only the real
        // plan loads (the step-less one is skipped, not fatal).
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        let real_id = wf1
            .create_plan("Real", "G", "C", vec!["do it".into()])
            .unwrap();
        // Simulate a step-less doc also present on disk + referenced.
        std::fs::write(
            plans_dir.join("stepless.md"),
            "# Plan: Doc\n\n## Goal\nNo steps here.\n",
        )
        .unwrap();
        std::fs::write(
            plans_dir.join("stack.json"),
            format!("[\"stepless\", \"{real_id}\"]"),
        )
        .unwrap();

        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.plan_depth(), 1);
        assert_eq!(wf2.plan().unwrap().title, "Real");
        assert_eq!(wf2.state(), WorkflowState::Executing);
    }

    #[test]
    fn create_plan_transitions_to_executing() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        let id = wf
            .create_plan("T", "G", "C", vec!["a".into(), "b".into()])
            .unwrap();
        assert!(!id.is_empty());
        assert_eq!(wf.state(), WorkflowState::Executing);
        assert!(wf.plan().is_some());
        assert_eq!(wf.current_step().unwrap().text, "a");
    }

    #[test]
    fn create_plan_mints_a_short_collision_checked_id() {
        // Regression: plan ids are SHORT 8-hex handles (not 36-char UUIDs) so
        // the model can reliably transcribe them into complete_step's
        // plan_id guard; the plan file is named with the short id.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf = Workflow::new(plans_dir.clone());
        let id = wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        assert_eq!(id.len(), 8, "plan ids are short 8-hex handles, got {id}");
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "the id is hex, got {id}"
        );
        assert!(
            plans_dir.join(format!("{id}.md")).exists(),
            "the plan file is named with the short id"
        );
    }

    #[test]
    fn unique_plan_id_extends_past_a_filename_collision() {
        // The 8-char prefix is extended only when a plan file with that
        // exact name already exists — the collision check makes the
        // short-id invariant unconditional.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        std::fs::create_dir_all(&plans_dir).unwrap();
        // No collision → the plain 8-char prefix.
        let id = unique_plan_id(&plans_dir, "cafebabe000000000000000000000000");
        assert_eq!(id, "cafebabe");
        // A plan file with the 8-char prefix exists → extend by one char.
        std::fs::write(plans_dir.join("cafebabe.md"), "x").unwrap();
        let id = unique_plan_id(&plans_dir, "cafebabe000000000000000000000000");
        assert_eq!(
            id, "cafebabe0",
            "the id extends past the collision, got {id}"
        );
        assert!(!plans_dir.join(format!("{id}.md")).exists());
    }

    #[test]
    fn complete_step_persists_and_transitions_to_reviewing() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("T", "G", "C", vec!["a".into(), "b".into()])
            .unwrap();
        assert_eq!(wf.state(), WorkflowState::Executing);
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Executing); // not all done
        wf.complete_step(1).unwrap();
        // Final step → Reviewing (the review→fix→commit closing sequence),
        // NOT Complete. `finish` is the only path onward.
        assert_eq!(wf.state(), WorkflowState::Reviewing);
    }

    #[test]
    fn research_plan_skips_reviewing_on_completion() {
        // A research plan's last step goes straight to Complete — no review.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Investigate",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::Research,
            None,
        )
        .unwrap();
        assert_eq!(wf.state(), WorkflowState::Executing);
        wf.complete_step(0).unwrap();
        // Research root plan completion → Complete directly (not Reviewing),
        // and reviewed=true so a restart re-derives Complete.
        assert_eq!(wf.state(), WorkflowState::Complete);
        assert!(wf.reviewed);
    }

    #[test]
    fn research_plan_completion_sets_reviewed_true() {
        // The reviewed flag must be true after a research plan completes, so
        // load_latest re-derives Complete (not Reviewing) on restart.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf = Workflow::new(plans_dir.clone());
        wf.create_plan_with_kind(
            "Investigate",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::Research,
            None,
        )
        .unwrap();
        wf.complete_step(0).unwrap();
        assert!(wf.reviewed);
        assert_eq!(wf.state(), WorkflowState::Complete);
    }

    #[test]
    fn restart_derives_complete_for_research_plan() {
        // A completed research plan re-derives Complete on restart (the
        // reviewed flag was set true at completion).
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan_with_kind(
            "Investigate",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::Research,
            None,
        )
        .unwrap();
        wf1.complete_step(0).unwrap(); // → Complete, reviewed=true

        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Complete);
    }

    #[test]
    fn bug_fixing_plan_completes_to_reviewing() {
        // A bug_fixing root plan's last step → Reviewing (bug plans get
        // reviewed like implementation plans — the fix must pass review).
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Fix crash",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::BugFixing,
            Some("the app crashes on open"),
        )
        .unwrap();
        assert_eq!(wf.state(), WorkflowState::Executing);
        // The symptom is persisted on the plan.
        assert_eq!(
            wf.plan().unwrap().bug_symptom.as_deref(),
            Some("the app crashes on open")
        );
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        assert!(!wf.reviewed);
    }

    #[test]
    fn update_plan_sets_and_unsets_landed_design() {
        // The feature-scale marker (backlog e5a84ce9): update_plan's
        // landed_design field sets the frame flag (the finish gate then
        // requires a "Landed design" context amendment) and Some(false)
        // un-sets it (a mistaken declaration). A landed_design-only call
        // is not a no-op.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Fix slowness",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::BugFixing,
            Some("slow"),
        )
        .unwrap();
        wf.update_plan(None, None, None, None, false, None, Some(true))
            .unwrap();
        assert!(wf.plan().unwrap().landed_design);
        // Un-set.
        wf.update_plan(None, None, None, None, false, None, Some(false))
            .unwrap();
        assert!(!wf.plan().unwrap().landed_design);
    }

    #[test]
    fn update_plan_sets_regression_test() {
        // The verify step records the regression test name via update_plan's
        // regression_test field — finish is blocked without it.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Fix crash",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::BugFixing,
            Some("crash"),
        )
        .unwrap();
        wf.update_plan(
            None,
            None,
            None,
            None,
            false,
            Some("crash_on_open_regression"),
            None,
        )
        .unwrap();
        assert_eq!(
            wf.plan().unwrap().regression_test.as_deref(),
            Some("crash_on_open_regression")
        );
        // The field round-trips through the on-disk plan file.
        let id = wf.plan_id().unwrap().to_string();
        let text = std::fs::read_to_string(wf.plans_dir().join(format!("{id}.md"))).unwrap();
        assert!(
            text.contains("## Regression test\ncrash_on_open_regression"),
            "{text}"
        );
    }

    #[test]
    fn update_plan_refuses_steps_replacement_on_bug_fixing_plan() {
        // The skeleton lock: a bug_fixing plan's 4-step skeleton cannot be
        // replaced (the bug protocol is not negotiable) — title/goal/context/
        // regression_test updates remain allowed.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Fix crash",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::BugFixing,
            Some("crash"),
        )
        .unwrap();
        let err = wf
            .update_plan(
                None,
                None,
                None,
                Some(vec!["replacement".into()]),
                false,
                None,
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("locked 4-step skeleton"), "{err}");
        // The steps are untouched.
        assert_eq!(wf.plan().unwrap().steps.len(), 1);
        // A non-bug plan still allows steps replacement.
        let mut wf2 = Workflow::new(dir.path().join("plans2"));
        wf2.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf2.update_plan(None, None, None, Some(vec!["b".into()]), false, None, None)
            .unwrap();
        assert_eq!(wf2.plan().unwrap().steps.len(), 1);
        assert_eq!(wf2.plan().unwrap().steps[0].text, "b");
    }

    #[test]
    fn research_sub_plan_pops_to_parent_without_skipping() {
        // A research sub-plan pops to the parent on completion (it does NOT
        // skip to Complete — only the root plan's completion is gated on kind).
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into(), "m2".into()])
            .unwrap();
        wf.complete_step(0).unwrap(); // 1 of 2 main steps done
        wf.create_plan_with_kind("Sub", "G", "C", vec!["s1".into()], PlanKind::Research, None)
            .unwrap();
        assert_eq!(wf.plan_depth(), 2);
        // Finish the research sub-plan: it pops, resuming Main (Executing).
        wf.complete_step(0).unwrap();
        assert_eq!(wf.plan_depth(), 1);
        assert_eq!(wf.state(), WorkflowState::Executing);
        assert_eq!(wf.plan().unwrap().title, "Main");
    }

    #[test]
    fn finish_transitions_to_complete() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // finish is the only Reviewing → Complete transition.
        wf.finish().unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);
    }

    #[test]
    fn finish_errors_outside_reviewing() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        // Planning — no plan, finish must error.
        assert!(wf.finish().is_err());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        // Executing — plan not done, finish must error.
        assert!(wf.finish().is_err());
        wf.complete_step(0).unwrap();
        wf.finish().unwrap(); // → Complete
                              // Complete — already finished, finish must error.
        assert!(wf.finish().is_err());
    }

    #[test]
    fn restart_derives_reviewing_when_review_pending() {
        // A complete-but-unreviewed plan re-derives Reviewing on restart
        // (reviewed flag is false). After finish, it re-derives Complete.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf1.complete_step(0).unwrap(); // → Reviewing, reviewed=false

        let mut wf2 = Workflow::new(plans_dir.clone());
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Reviewing); // review pending

        // Close out the review, then restart again.
        wf2.finish().unwrap();
        let mut wf3 = Workflow::new(plans_dir);
        wf3.load_latest().unwrap();
        assert_eq!(wf3.state(), WorkflowState::Complete); // review done
    }

    #[test]
    fn reviewed_flag_survives_restart_while_skill_active() {
        // Regression: a restart while a skill is active after a reviewed plan
        // must NOT lose the `reviewed` flag. Previously load_latest took the
        // skill early-return before restoring `reviewed`, leaving it false;
        // the next persist_stack (e.g. end_skill) then wrote that stale false
        // back, corrupting the sidecar so a later restart re-derived
        // Reviewing for an already-closed-out review.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf1.complete_step(0).unwrap(); // → Reviewing
        wf1.finish().unwrap(); // → Complete, reviewed=true
        wf1.start_skill("s", "p", WorkflowState::Planning, vec![])
            .unwrap();

        // Restart with the skill persisted.
        let mut wf2 = Workflow::new(plans_dir.clone());
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Skill);
        // The reviewed flag must have been restored despite the skill path.
        assert!(wf2.reviewed);

        // Ending the skill persists again — the flag must stay true.
        wf2.end_skill().unwrap();

        // A final restart must derive Complete (not Reviewing).
        let mut wf3 = Workflow::new(plans_dir);
        wf3.load_latest().unwrap();
        assert_eq!(wf3.state(), WorkflowState::Complete);
    }

    #[test]
    fn load_latest_resumes_executing() {
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        // Create a plan with one step done.
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("T", "G", "C", vec!["a".into(), "b".into()])
            .unwrap();
        wf1.complete_step(0).unwrap();

        // Simulate restart: new workflow loads from disk.
        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Executing);
        assert_eq!(wf2.current_step().unwrap().text, "b"); // resume at first unchecked
    }

    #[test]
    fn load_latest_detects_reviewing_when_unreviewed() {
        // A complete-but-unreviewed plan re-derives Reviewing (the `reviewed`
        // flag defaults to false). Previously this was Complete; the
        // Reviewing state now sits between "all steps done" and "review closed".
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf1.complete_step(0).unwrap();

        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Reviewing);
    }

    #[test]
    fn load_latest_no_plan_is_planning() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.load_latest().unwrap();
        assert_eq!(wf.state(), WorkflowState::Planning);
    }

    #[test]
    fn research_plan_selects_the_research_filter() {
        // The variant is keyed on the ACTIVE plan, so a research root that
        // pushes an implementation sub-plan gets write tools back for the
        // duration of that sub-plan.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan_with_kind("R", "G", "C", vec!["a".into()], PlanKind::Research, None)
            .unwrap();
        assert_eq!(wf.allowed_tools(), ToolFilter::ExecutingResearch);

        wf.create_plan_with_kind(
            "I",
            "G",
            "C",
            vec!["b".into()],
            PlanKind::Implementation,
            None,
        )
        .unwrap();
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Executing,
            "an implementation sub-plan restores the write tools"
        );
    }

    #[test]
    fn allowed_tools_matches_state() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        assert_eq!(wf.allowed_tools(), ToolFilter::Planning);
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        assert_eq!(wf.allowed_tools(), ToolFilter::Executing);
        wf.complete_step(0).unwrap();
        assert_eq!(wf.allowed_tools(), ToolFilter::Reviewing);
        wf.finish().unwrap();
        assert_eq!(wf.allowed_tools(), ToolFilter::Complete);
    }

    #[test]
    fn schema_filter_is_frozen_across_executing_and_reviewing() {
        // The whole point: while a plan is active, the SCHEMA filter must not
        // change at the Executing→Reviewing transition — the tools array
        // rides at the head of the request body and any byte change resets
        // the provider prefix cache. allowed_tools() still follows the state
        // (the dispatch gate); schema_filter() is the frozen advertisement.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        assert_eq!(wf.schema_filter(), ToolFilter::Planning);
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        let frozen = wf.schema_filter();
        assert_eq!(frozen, ToolFilter::PlanFrozen);
        wf.complete_step(0).unwrap();
        assert_eq!(wf.allowed_tools(), ToolFilter::Reviewing);
        assert_eq!(
            wf.schema_filter(),
            frozen,
            "the schema filter must be stable across the Executing→Reviewing transition"
        );
        wf.finish().unwrap();
        assert_eq!(
            wf.schema_filter(),
            ToolFilter::Complete,
            "once the plan is popped, the per-state filter applies again"
        );
    }

    #[test]
    fn schema_filter_is_stable_across_plan_kinds() {
        // The advertised array must be byte-identical across a research↔
        // implementation transition: it rides at the head of the request body,
        // so any change resets the provider's prefix cache. A per-kind surface
        // (research used to get ExecutingResearch) rewrote the head on every
        // plan change — measured 2027-01-11: cached collapsed to 6,016 tokens
        // and TTFT hit 15.7–17.9s (cache-hit round 6, cause 2).
        let dir = tempdir().unwrap();
        let mut research = Workflow::new(dir.path());
        research
            .create_plan_with_kind(
                "R",
                "G",
                "C",
                vec!["a".into(), "b".into()],
                PlanKind::Research,
                None,
            )
            .unwrap();
        let impl_dir = tempdir().unwrap();
        let mut implementation = Workflow::new(impl_dir.path());
        implementation
            .create_plan("I", "G", "C", vec!["a".into(), "b".into()])
            .unwrap();

        assert_eq!(research.schema_filter(), ToolFilter::PlanFrozen);
        assert_eq!(
            research.schema_filter(),
            implementation.schema_filter(),
            "the advertised surface must not depend on the plan kind"
        );
        // ... and not on the state transition either, for the plan's lifetime.
        research.complete_step(0).unwrap();
        assert_eq!(research.schema_filter(), ToolFilter::PlanFrozen);
        research.complete_step(1).unwrap();
        // Research plans skip Reviewing on completion: the plan is popped and
        // the per-state filter applies again.
        assert_eq!(research.schema_filter(), ToolFilter::Complete);
    }

    #[test]
    fn research_plan_still_cannot_write_despite_the_advertised_surface() {
        // Advertising the full frozen surface must NOT grant write access: the
        // research restriction lives in the dispatch filter.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan_with_kind(
            "R",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::Research,
            None,
        )
        .unwrap();
        assert_eq!(wf.schema_filter(), ToolFilter::PlanFrozen);
        let enforced = wf.allowed_tools();
        assert_eq!(enforced, ToolFilter::ExecutingResearch);
        for name in ["file_write", "file_edit", "file_append", "convert_line_endings"] {
            assert!(
                !enforced.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, name),
                "a research plan must still be unable to call {name}"
            );
        }
    }

    #[test]
    fn schema_filter_without_plan_follows_state() {
        // No active plan → the per-state filter: Planning hides every
        // mutation tool (the no-changes-without-a-plan hard rule) and
        // Complete is the read+create_plan surface. The freeze never widens
        // a plan-less state.
        let dir = tempdir().unwrap();
        let wf = Workflow::new(dir.path());
        assert_eq!(wf.schema_filter(), ToolFilter::Planning);
        assert_eq!(wf.schema_filter(), wf.allowed_tools());
    }

    #[test]
    fn schema_filter_allowlist_wins() {
        // A reviewer sub-agent's strict allow-list must never be widened by
        // the freeze — even with a plan active on the shared stack.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.set_reviewer_allowlist(Some(vec![
            "read_files".into(),
            "write_review_report".into(),
        ]));
        let filter = wf.schema_filter();
        assert!(
            matches!(filter, ToolFilter::Reviewer(_)),
            "allow-listed sub-agents keep their constructor-granted surface"
        );
        assert_eq!(filter, wf.allowed_tools());
    }

    #[test]
    fn allowed_tools_follows_state_even_when_plan_mutations_disallowed() {
        // Sub-agents have plan_mutations_allowed = false. The filter must still
        // follow the real workflow state (so write/exec tools are visible in
        // Executing); the plan-mutation restriction is enforced elsewhere
        // (dispatch, schema retain, tool wrappers). Regression guard: a
        // previous version collapsed this to Planning, which denied sub-agents
        // every write/exec tool.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.set_plan_mutations_allowed(false);
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Planning,
            "Planning state stays Planning even for subs"
        );
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Executing,
            "sub-agent in Executing must keep the Executing filter (write tools visible)"
        );
    }

    #[test]
    fn tool_allowlist_overrides_state_derived_filter() {
        // A read-only reviewer sub-agent has tool_allowlist set. The filter
        // must be the Skill allow-list regardless of the workflow state —
        // so the reviewer cannot reach file_edit/shell/git. (Pre-2026-01-03
        // the reviewer inherited the main agent's Reviewing state via the
        // shared plan; since then its state is Subagent — either way the
        // allow-list, not the state, defines the surface.)
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // Before the allow-list is set, Reviewing exposes all agent tools.
        assert_eq!(wf.allowed_tools(), ToolFilter::Reviewing);

        wf.set_tool_allowlist(Some(vec![
            "file_read".into(),
            "git_diff".into(),
            "write_review_report".into(),
        ]));
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Skill(vec![
                "file_read".into(),
                "git_diff".into(),
                "write_review_report".into(),
            ]),
            "tool_allowlist overrides the state-derived filter"
        );
        // The allow-list is honored by ToolFilter::allows: read tool visible,
        // write/exec tools denied.
        let filter = wf.allowed_tools();
        assert!(filter.allows(ToolCategory::Agent, SafetyLevel::AutoRun, "file_read"));
        assert!(!filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, "file_edit"));
        assert!(!filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, "shell"));
        assert!(!filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, "git"));
        // Memory tools remain always-available even under the allow-list.
        assert!(filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_recall"));

        // Clearing the allow-list restores the state-derived filter.
        wf.set_tool_allowlist(None);
        assert_eq!(wf.allowed_tools(), ToolFilter::Reviewing);
    }

    #[test]
    fn subagent_state_with_allowlist_uses_allowlist_filter() {
        // The Subagent state's tool surface comes ONLY from the spawn-time
        // allow-list — never a lifecycle filter (from_state returns None for
        // Subagent). With an allow-list set, allowed_tools returns the
        // allow-list-seeded filter regardless of the mirrored plan.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.enter_subagent_state();
        wf.set_tool_allowlist(Some(vec!["file_read".into()]));
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Skill(vec!["file_read".into()]),
            "the allow-list overrides — Subagent never consults a lifecycle filter"
        );
    }

    #[test]
    #[should_panic(expected = "Subagent-state workflow must carry a tool allow-list")]
    fn subagent_state_without_allowlist_fails_loudly() {
        // Invariant: a Subagent-state workflow ALWAYS carries a spawn-time
        // allow-list (compute_subagent_allowlist returns Some for every
        // parented spawn). Reaching the state-derived filter without one is
        // an invariant violation — fail loudly, mirroring Skill-with-no-skill.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.enter_subagent_state();
        wf.allowed_tools();
    }

    #[test]
    fn reviewer_allowlist_is_a_strict_filter() {
        // A `role: "reviewer"` spawn calls set_reviewer_allowlist → the
        // filter must be ToolFilter::Reviewer (strict: nothing auto-granted),
        // NOT ToolFilter::Skill (which auto-grants memory/ask/backlog). A
        // reviewer must never reach memory_write, ask_user, backlog_status,
        // or plan tools — even though the shared plan is in Reviewing.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        wf.create_plan("T", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);

        wf.set_reviewer_allowlist(Some(vec![
            "file_read".into(),
            "git_diff".into(),
            "write_review_report".into(),
            "memory_recall".into(),
        ]));
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Reviewer(vec![
                "file_read".into(),
                "git_diff".into(),
                "write_review_report".into(),
                "memory_recall".into(),
            ]),
            "reviewer allow-list must produce the strict Reviewer filter"
        );
        // Allow-listed tools visible; current_plan rides along (orientation).
        let filter = wf.allowed_tools();
        assert!(filter.allows(ToolCategory::Agent, SafetyLevel::AutoRun, "file_read"));
        assert!(filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_recall"));
        assert!(filter.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "current_plan"));
        // NOTHING is auto-granted: memory writes, ask_user, backlog_status,
        // and every mutation the Reviewing state would otherwise allow.
        assert!(!filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_write"));
        assert!(!filter.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "ask_user"));
        assert!(!filter.allows(
            ToolCategory::Workflow,
            SafetyLevel::AutoRun,
            "backlog_status"
        ));
        assert!(!filter.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "finish"));
        assert!(!filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, "file_edit"));

        // A plain set_tool_allowlist clears the reviewer flag (Skill again).
        wf.set_tool_allowlist(Some(vec!["file_read".into()]));
        assert!(matches!(wf.allowed_tools(), ToolFilter::Skill(_)));
        assert!(wf.allowed_tools().allows(
            ToolCategory::Memory,
            SafetyLevel::AutoRun,
            "memory_recall"
        ));

        // Clearing the reviewer allow-list restores the state-derived filter.
        wf.set_reviewer_allowlist(None);
        assert_eq!(wf.allowed_tools(), ToolFilter::Reviewing);
    }

    #[test]
    fn complete_step_without_plan_errors() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        let result = wf.complete_step(0);
        assert!(result.is_err());
    }

    // ---- plan stack (sub-plans) ----

    #[test]
    fn create_plan_while_executing_pushes_sub_plan() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into(), "m2".into()])
            .unwrap();
        let main_id = wf.plan_id().unwrap().to_string();
        assert_eq!(wf.plan_depth(), 1);

        // A sub-plan pushes on top; the parent stays underneath, untouched.
        wf.create_plan("Sub", "G", "C", vec!["s1".into()]).unwrap();
        assert_eq!(wf.plan_depth(), 2);
        assert_eq!(wf.plan().unwrap().title, "Sub");
        assert_ne!(wf.plan_id().unwrap(), main_id);
        assert_eq!(wf.parent_titles(), vec!["Main".to_string()]);
        // The parent plan is still the bottom of the stack.
        assert_eq!(wf.plan_stack()[0].plan.title, "Main");
        assert_eq!(wf.plan_stack()[0].id, main_id);
    }

    #[test]
    fn top_plan_id_is_root_plan_constant_across_sub_plans() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        // No plan yet — no root plan id.
        assert!(wf.top_plan_id().is_none());

        // A root plan: top_plan_id is the root's id.
        let root_id = wf.create_plan("Root", "G", "C", vec!["r1".into()]).unwrap();
        assert_eq!(wf.top_plan_id(), Some(root_id.as_str()));
        assert_eq!(wf.plan_id(), Some(root_id.as_str()));

        // A sub-plan on top: the root id is unchanged (only the active id moves).
        wf.create_plan("Sub", "G", "C", vec!["s1".into()]).unwrap();
        assert_eq!(wf.plan_depth(), 2);
        assert_ne!(wf.plan_id().unwrap(), root_id);
        assert_eq!(wf.top_plan_id(), Some(root_id.as_str()));

        // Popping the sub-plan restores the root as active; top_plan_id stays.
        wf.complete_step(0).unwrap();
        assert_eq!(wf.plan_depth(), 1);
        assert_eq!(wf.top_plan_id(), Some(root_id.as_str()));
    }

    #[test]
    fn completing_sub_plan_pops_and_resumes_parent() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into(), "m2".into()])
            .unwrap();
        wf.complete_step(0).unwrap(); // 1 of 2 main steps done
        wf.create_plan("Sub", "G", "C", vec!["s1".into()]).unwrap();
        assert_eq!(wf.plan_depth(), 2);

        // Finish the sub-plan: it pops, and we resume Main at its step 2.
        wf.complete_step(0).unwrap();
        assert_eq!(wf.plan_depth(), 1);
        assert_eq!(wf.state(), WorkflowState::Executing); // parent still in flight
        assert_eq!(wf.plan().unwrap().title, "Main");
        assert_eq!(wf.current_step().unwrap().text, "m2"); // resume where it left off
    }

    #[test]
    fn completing_last_plan_transitions_to_reviewing() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into()]).unwrap();
        wf.complete_step(0).unwrap(); // completes Main (root) — retained as Reviewing
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // The finished root frame is kept so Reviewing persists across restart
        // and the completed plan stays visible. `finish` moves it to Complete.
        assert_eq!(wf.plan_depth(), 1);
        assert!(wf.plan().unwrap().is_complete());
        wf.finish().unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);
    }

    #[test]
    fn abandon_sub_plan_pops_to_parent() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into()]).unwrap();
        wf.create_plan("Sub", "G", "C", vec!["s1".into()]).unwrap();
        let abandoned = wf.abandon_plan().unwrap();
        assert_eq!(abandoned, "Sub");
        assert_eq!(wf.plan_depth(), 1);
        assert_eq!(wf.state(), WorkflowState::Executing);
        assert_eq!(wf.plan().unwrap().title, "Main");
    }

    #[test]
    fn abandon_last_plan_returns_to_planning() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into()]).unwrap();
        wf.abandon_plan().unwrap();
        assert_eq!(wf.state(), WorkflowState::Planning);
        assert_eq!(wf.plan_depth(), 0);
    }

    #[test]
    fn abandon_without_plan_errors() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path());
        assert!(wf.abandon_plan().is_err());
    }

    #[test]
    fn create_plan_from_reviewing_clears_finished_stack() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Main", "G", "C", vec!["m1".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // A brand-new plan from Reviewing (or Complete) starts a fresh
        // single-frame stack — create_plan clears any non-Executing stack.
        wf.create_plan("Follow-up", "G", "C", vec!["f1".into()])
            .unwrap();
        assert_eq!(wf.plan_depth(), 1);
        assert_eq!(wf.plan().unwrap().title, "Follow-up");
    }

    #[test]
    fn stack_persists_across_restart() {
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("Main", "G", "C", vec!["m1".into(), "m2".into()])
            .unwrap();
        wf1.complete_step(0).unwrap();
        wf1.create_plan("Sub", "G", "C", vec!["s1".into()]).unwrap();

        // Simulate restart: the whole stack (Main + Sub) is restored.
        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.plan_depth(), 2);
        assert_eq!(wf2.plan().unwrap().title, "Sub"); // active = top
        assert_eq!(wf2.parent_titles(), vec!["Main".to_string()]);
        assert_eq!(wf2.state(), WorkflowState::Executing);

        // Completing the resumed sub-plan pops back to Main's step 2.
        wf2.complete_step(0).unwrap();
        assert_eq!(wf2.plan().unwrap().title, "Main");
        assert_eq!(wf2.current_step().unwrap().text, "m2");
    }

    #[test]
    fn update_plan_appends_steps_preserving_completed_prefix() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into(), "b".into(), "c".into()])
            .unwrap();
        // Complete the first two steps (absolute indices 0 and 1).
        wf.complete_step(0).unwrap();
        wf.complete_step(1).unwrap(); // now step "c" is current
        let id = wf.plan_id().unwrap().to_string();
        // Replace the remaining ("c") with ["c","d","e"]: the two completed
        // steps must stay; remaining is replaced by the new list.
        wf.update_plan(
            None,
            None,
            None,
            Some(vec!["c".into(), "d".into(), "e".into()]),
            false,
            None,
            None,
        )
        .unwrap();
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps.len(), 5);
        assert_eq!(plan.steps[0].text, "a");
        assert!(plan.steps[0].done);
        assert_eq!(plan.steps[1].text, "b");
        assert!(plan.steps[1].done);
        assert_eq!(plan.steps[2].text, "c");
        assert!(!plan.steps[2].done);
        assert_eq!(plan.steps[4].text, "e");
        assert!(!plan.steps[4].done);
        // Id is unchanged (in-place rewrite, not a new plan).
        assert_eq!(wf.plan_id().unwrap(), id);
        assert_eq!(wf.state(), WorkflowState::Executing);
    }

    #[test]
    fn update_plan_can_edit_title_and_goal() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("Old", "Old goal", "C", vec!["a".into()])
            .unwrap();
        wf.update_plan(Some("New title"), Some("New goal"), None, None, false, None, None)
            .unwrap();
        let plan = wf.plan().unwrap();
        assert_eq!(plan.title, "New title");
        assert_eq!(plan.goal, "New goal");
        assert_eq!(plan.context, "C");
    }

    #[test]
    fn update_plan_append_adds_after_remaining_steps_and_extends_context() {
        // The chunked-write protocol (backlog 0085ccc0): append=true keeps the
        // remaining steps and adds the new ones AFTER them (no resending), and
        // extends the context instead of replacing it.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "base ctx", vec!["a".into(), "b".into()])
            .unwrap();
        let id = wf.plan_id().unwrap().to_string();
        wf.update_plan(
            None,
            None,
            Some("more ctx"),
            Some(vec!["c".into(), "d".into()]),
            true,
            None,
            None,
        )
        .unwrap();
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps.len(), 4);
        assert_eq!(plan.steps[0].text, "a");
        assert_eq!(plan.steps[1].text, "b");
        assert_eq!(plan.steps[2].text, "c");
        assert_eq!(plan.steps[3].text, "d");
        assert!(!plan.steps.iter().any(|s| s.done));
        assert_eq!(plan.context, "base ctx\n\nmore ctx");
        // In-place: same id, still Executing.
        assert_eq!(wf.plan_id().unwrap(), id);
        assert_eq!(wf.state(), WorkflowState::Executing);
    }

    #[test]
    fn update_plan_append_preserves_completed_prefix_and_remaining() {
        // Append with completed steps: the done prefix stays done, the
        // remaining steps stay (unlike replace, which would resend them), and
        // the new steps land at the end.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into(), "b".into(), "c".into()])
            .unwrap();
        wf.complete_step(0).unwrap(); // "a" done
        wf.update_plan(None, None, None, Some(vec!["d".into()]), true, None, None)
            .unwrap();
        let plan = wf.plan().unwrap();
        assert_eq!(plan.steps.len(), 4);
        assert!(plan.steps[0].done);
        assert_eq!(plan.steps[1].text, "b");
        assert!(!plan.steps[1].done);
        assert_eq!(plan.steps[2].text, "c");
        assert_eq!(plan.steps[3].text, "d");
        // Step indices are contiguous after the insert.
        for (i, s) in plan.steps.iter().enumerate() {
            assert_eq!(s.index, i);
        }
    }

    #[test]
    fn update_plan_append_refused_for_bug_fixing_skeleton() {
        // The skeleton lock is total: append must not grow the locked 4-step
        // bug protocol either.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan_with_kind(
            "Fix crash",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::BugFixing,
            Some("crash"),
        )
        .unwrap();
        let err = wf
            .update_plan(None, None, None, Some(vec!["extra".into()]), true, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("locked 4-step skeleton"), "{err}");
        assert_eq!(wf.plan().unwrap().steps.len(), 1);
        // Context-only append still works on a bug plan (title/goal/context
        // updates remain allowed).
        wf.update_plan(None, None, Some("found root cause: x"), None, true, None, None)
            .unwrap();
        assert_eq!(wf.plan().unwrap().context, "C\n\nfound root cause: x");
    }

    #[test]
    fn update_plan_errors_outside_executable_states() {
        // Renamed from update_plan_errors_outside_executing (2026-12-23,
        // backlog 24e1c98e): update_plan is allowed in Executing AND Reviewing
        // with the full field set (see
        // update_plan_full_edit_allowed_in_reviewing), so the error contract
        // now covers Planning (no plan) and Complete (plan finished — start a
        // fresh one) only.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        // Planning — no plan yet.
        let err = wf
            .update_plan(Some("T"), None, None, None, false, None, None)
            .unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::WorkflowWrongState { .. }
        ));
        // Complete — a research plan completing goes straight to Complete;
        // update_plan must not resurrect a finished plan.
        let mut wf = Workflow::new(dir.path().join("plans-research"));
        wf.create_plan_with_kind("R", "G", "C", vec!["a".into()], PlanKind::Research, None)
            .unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);
        let err = wf
            .update_plan(Some("T"), None, None, None, false, None, None)
            .unwrap_err();
        assert!(matches!(
            err,
            crate::error::Error::WorkflowWrongState { .. }
        ));
    }

    #[test]
    fn update_plan_regression_test_allowed_in_reviewing() {
        // The finish↔update_plan deadlock (2026-12-04): finish requires the
        // regression_test name recorded via update_plan, but update_plan was
        // blocked in Reviewing (the state finish runs in). The first
        // relaxation accepted regression_test ONLY; since 2026-12-23
        // (backlog 24e1c98e) Reviewing accepts the full field set like
        // Executing (minus complete_step, which stays hidden mid-review).
        // This test keeps guarding the original deadlock: regression_test
        // must be recordable in Reviewing. Before the first fix this errors
        // with WorkflowWrongState; after, it succeeds.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // regression_test-only update MUST succeed in Reviewing.
        wf.update_plan(None, None, None, None, false, Some("my_regression_test"), None)
            .expect("regression_test-only update_plan must be allowed in Reviewing");
        assert_eq!(
            wf.plan().unwrap().regression_test.as_deref(),
            Some("my_regression_test")
        );
        // landed_design-only updates are equally allowed in Reviewing (the
        // feature-scale marker may only become clear during review fixes).
        wf.update_plan(None, None, None, None, false, None, Some(true))
            .expect("landed_design-only update_plan must be allowed in Reviewing");
        assert!(wf.plan().unwrap().landed_design);
    }

    #[test]
    fn update_plan_full_edit_allowed_in_reviewing() {
        // Backlog 24e1c98e (2026-12-23): update_plan in Reviewing is fully
        // callable for the main agent — title/goal/context edits succeed like
        // in Executing (previously the workflow guard froze structural fields
        // there and errored with WorkflowWrongState). complete_step stays
        // hidden mid-review; the reviewer subagent never sees the tool at all
        // (ToolFilter::Reviewer strict allow-list + plan_mutations_allowed).
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        wf.update_plan(
            Some("P2"),
            Some("G2"),
            Some("review finding 1 fixed; re-run tests"),
            None,
            false,
            None,
            None,
        )
        .expect("full-field update_plan must be allowed in Reviewing");
        let plan = wf.plan().unwrap();
        assert_eq!(plan.title, "P2");
        assert_eq!(plan.goal, "G2");
        assert_eq!(plan.context, "review finding 1 fixed; re-run tests");
    }

    #[test]
    fn update_plan_steps_append_allowed_in_reviewing_implementation() {
        // Backlog 24e1c98e (2026-12-23): appended follow-on steps are allowed
        // in Reviewing for implementation plans (the closing sequence may
        // surface extra fix work). The steps stay unchecked — complete_step
        // is hidden mid-review; finish still gates the exit on the review
        // report. The BugFixing skeleton lock keeps refusing steps for bug
        // plans in Reviewing too (kind-based lock, orthogonal to state).
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        wf.update_plan(
            None,
            None,
            None,
            Some(vec!["fix finding 2".into()]),
            true,
            None,
            None,
        )
        .expect("steps append must be allowed in Reviewing for implementation plans");
        assert_eq!(wf.plan().unwrap().steps.len(), 2);
        // The skeleton lock still holds for bug plans in Reviewing.
        let mut bug = Workflow::new(dir.path().join("plans-bug"));
        bug.create_plan_with_kind(
            "Fix crash",
            "G",
            "C",
            vec!["a".into()],
            PlanKind::BugFixing,
            Some("crash"),
        )
        .unwrap();
        bug.complete_step(0).unwrap();
        assert_eq!(bug.state(), WorkflowState::Reviewing);
        let err = bug
            .update_plan(None, None, None, Some(vec!["extra".into()]), true, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("locked 4-step skeleton"), "{err}");
    }

    #[test]
    fn update_plan_rejects_empty_or_noop() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        // Empty step list.
        let err = wf
            .update_plan(None, None, None, Some(vec![]), false, None, None)
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::InvalidInput(_)));
        // Nothing provided at all.
        let err = wf
            .update_plan(None, None, None, None, false, None, None)
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::InvalidInput(_)));
        // Blank string is treated as "leave unchanged" (a no-op) — error.
        let err = wf
            .update_plan(Some("   "), None, None, None, false, None, None)
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::InvalidInput(_)));
    }

    #[test]
    fn update_plan_rejects_out_of_order_completion() {
        // If a completed step sits after an incomplete one, replacing the
        // remaining steps would silently drop the later-done step — refuse
        // rather than lose data.
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into(), "b".into(), "c".into()])
            .unwrap();
        // Complete step 2 ("c") while step 0 ("a") and 1 ("b") are open.
        wf.complete_step(2).unwrap();
        let err = wf
            .update_plan(None, None, None, Some(vec!["b2".into()]), false, None, None)
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::Workflow(_)));
    }

    // ---- skills ----

    #[test]
    fn start_skill_transitions_to_skill_and_stores_overlay() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        assert_eq!(wf.state(), WorkflowState::Reviewing);
        // Close out the review → Complete before starting a skill (skills
        // start from Complete or Planning, not mid-review).
        wf.finish().unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);

        wf.start_skill(
            "merge_to_main",
            "merge the branch",
            WorkflowState::Planning,
            vec!["git".into(), "skill_end".into()],
        )
        .unwrap();
        assert_eq!(wf.state(), WorkflowState::Skill);
        let skill = wf.active_skill().unwrap();
        assert_eq!(skill.name, "merge_to_main");
        assert_eq!(skill.prompt, "merge the branch");
        assert_eq!(skill.target_state, WorkflowState::Planning);
        assert_eq!(skill.pre_skill_state, WorkflowState::Complete);
        assert_eq!(
            skill.tools,
            vec!["git".to_string(), "skill_end".to_string()]
        );
    }

    #[test]
    fn end_skill_transitions_to_target_state() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        wf.start_skill("s", "p", WorkflowState::Planning, vec![])
            .unwrap();
        assert_eq!(wf.state(), WorkflowState::Skill);
        wf.end_skill().unwrap();
        assert_eq!(wf.state(), WorkflowState::Planning);
        assert!(wf.active_skill().is_none());
    }

    #[test]
    fn abandon_skill_rolls_back_to_pre_skill_state() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf.complete_step(0).unwrap();
        wf.finish().unwrap(); // → Complete (review closed out)
                              // Entered from Complete → abandon returns to Complete.
        wf.start_skill("s", "p", WorkflowState::Planning, vec![])
            .unwrap();
        assert_eq!(wf.state(), WorkflowState::Skill);
        wf.abandon_skill().unwrap();
        assert_eq!(wf.state(), WorkflowState::Complete);
        assert!(wf.active_skill().is_none());

        // Entered from Planning → abandon returns to Planning.
        let dir2 = tempdir().unwrap();
        let mut wf2 = Workflow::new(dir2.path().join("plans"));
        assert_eq!(wf2.state(), WorkflowState::Planning);
        wf2.start_skill("s", "p", WorkflowState::Complete, vec![])
            .unwrap();
        wf2.abandon_skill().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Planning);
    }

    #[test]
    fn start_skill_rejects_nesting() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.start_skill("s", "p", WorkflowState::Planning, vec![])
            .unwrap();
        let err = wf
            .start_skill("s2", "p2", WorkflowState::Planning, vec![])
            .unwrap_err();
        assert!(matches!(err, crate::error::Error::Workflow(_)));
    }

    #[test]
    fn end_skill_without_active_errors() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        assert!(wf.end_skill().is_err());
        assert!(wf.abandon_skill().is_err());
    }

    #[test]
    fn skill_persists_across_load_latest() {
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        wf1.create_plan("P", "G", "C", vec!["a".into()]).unwrap();
        wf1.complete_step(0).unwrap();
        wf1.finish().unwrap(); // → Complete (review closed out before skill)
        wf1.start_skill(
            "merge_to_main",
            "merge it",
            WorkflowState::Planning,
            vec!["git".into(), "skill_end".into()],
        )
        .unwrap();

        // Simulate restart: the active skill is restored.
        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Skill);
        let skill = wf2.active_skill().unwrap();
        assert_eq!(skill.name, "merge_to_main");
        assert_eq!(skill.prompt, "merge it");
        assert_eq!(skill.target_state, WorkflowState::Planning);
        assert_eq!(skill.pre_skill_state, WorkflowState::Complete);
        assert_eq!(
            skill.tools,
            vec!["git".to_string(), "skill_end".to_string()]
        );
    }

    #[test]
    fn allowed_tools_returns_skill_filter_when_active() {
        let dir = tempdir().unwrap();
        let mut wf = Workflow::new(dir.path().join("plans"));
        wf.start_skill(
            "s",
            "p",
            WorkflowState::Planning,
            vec!["git".into(), "skill_end".into()],
        )
        .unwrap();
        assert_eq!(
            wf.allowed_tools(),
            ToolFilter::Skill(vec!["git".into(), "skill_end".into()])
        );
    }

    #[test]
    fn load_latest_reads_legacy_bare_array_sidecar() {
        // The legacy sidecar was a bare JSON array of plan ids. It must still
        // load (no skill), deriving state from the plan.
        let dir = tempdir().unwrap();
        let plans_dir = dir.path().join("plans");
        let mut wf1 = Workflow::new(plans_dir.clone());
        let id = wf1
            .create_plan("Legacy", "G", "C", vec!["a".into()])
            .unwrap();
        // Overwrite with the legacy bare-array form.
        std::fs::write(plans_dir.join("stack.json"), format!("[\"{id}\"]")).unwrap();

        let mut wf2 = Workflow::new(plans_dir);
        wf2.load_latest().unwrap();
        assert_eq!(wf2.state(), WorkflowState::Executing);
        assert!(wf2.active_skill().is_none());
    }
}
