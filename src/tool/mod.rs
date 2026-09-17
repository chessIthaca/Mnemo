// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The tool system — three categories share one `Tool` trait.
//!
//! - **Agent tools** (`file_read`, `file_edit`, `file_write`, `shell`, `search`, `git`)
//!   do the coding work. Gated by workflow state.
//! - **Workflow tools** (`create_plan`, `update_plan`, `complete_step`, `abandon_plan`) drive the plan lifecycle.
//! - **Memory tools** (`memory_write`, `memory_recall`, `memory_consolidate`) are
//!   always available regardless of workflow state.
//! - **Browser tools** (`browser_navigate`, `browser_screenshot`, `browser_console`, ...)
//!   drive the headless debug browser; reads are visible everywhere, mutations are
//!   approval-gated.

pub mod agent;
#[cfg(feature = "browser")]
pub mod browser;
pub mod memory;
pub mod steering;
pub mod workflow;

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{ApprovalPreview, Capabilities, ToolSchema};
use crate::workflow::WorkflowState;

/// A tool's category — determines how it's gated by the workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Coding tools (`file_read`, `shell`, etc.).
    Agent,
    /// Plan-lifecycle tools (`create_plan`, `complete_step`).
    Workflow,
    /// Four-tier memory tools — always available.
    Memory,
    /// Headless-browser debugging tools (`browser_navigate`, `browser_screenshot`, ...).
    Browser,
}

/// Whether a tool runs automatically or needs human approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafetyLevel {
    /// Runs without prompting (read-only tools).
    AutoRun,
    /// Requires explicit approval before executing (mutations).
    NeedsApproval,
}

/// The result of executing a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// Whether the tool call succeeded.
    pub success: bool,
    /// The output (or error message) as a string.
    pub output: String,
    /// Optional structured data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ToolResult {
    /// A successful result with text output.
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            success: true,
            output: output.into(),
            data: None,
        }
    }

    /// A failed result with an error message.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            output: message.into(),
            data: None,
        }
    }

    /// Attach structured `data` to this result, returning `self` for chaining.
    /// Used by tools that want to surface machine-readable fields alongside
    /// their text output (e.g. a written file path).
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// A parsed tool call from the LLM.
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// The tool-call id assigned by the model.
    pub id: String,
    /// The tool name.
    pub name: String,
    /// The arguments as a JSON value (already parsed).
    pub arguments: Value,
}

/// The trait every tool implements.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The tool name (matches the schema name).
    fn name(&self) -> &str;

    /// The tool category.
    fn category(&self) -> ToolCategory;

    /// The JSON schema for this tool.
    fn schema(&self) -> ToolSchema;

    /// Whether this tool needs approval.
    fn safety(&self) -> SafetyLevel;

    /// Whether this tool must ALWAYS show the interactive approval prompt,
    /// even under `SafetyMode::Autonomous` or a matching safety rule.
    ///
    /// Argument-aware: a tool may need to force the interactive approval
    /// prompt only for *some* of its invocations. The canonical case is the
    /// `git` tool, which is one tool with many subcommands — `git status` is a
    /// harmless read, but `git merge` / `git push` land commits on `main` or
    /// push to a remote and must therefore always go through the approval
    /// gate, regardless of the active safety mode or any matching safety rule.
    ///
    /// The dispatch layer consults this before applying either bypass; the
    /// prompt is forced when this returns `true`. Default `false` — most
    /// tools are uniform and rely on the active safety mode alone.
    fn never_auto_for(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// Build a pure (no side-effect) approval preview for this call, if any.
    ///
    /// Called by the agent dispatch layer **before** emitting
    /// `ApprovalRequest`, so the UI can show a Rust-side unified diff / new-file
    /// payload instead of reconstructing from raw args. Default `None` — only
    /// file mutation tools override this. Failures are swallowed by the caller
    /// (preview is best-effort; approval still proceeds with args alone).
    fn approval_preview(&self, _args: &Value) -> Option<ApprovalPreview> {
        None
    }

    /// Whether this tool can actually do its job right now.
    ///
    /// Consulted per turn by [`ToolRegistry::schemas`]: an unavailable tool is
    /// left out of the `tools` array entirely, so the model never spends
    /// context on — or reaches for — a tool whose only possible outcome is a
    /// "not configured" error. This is a RUNTIME check, not a registration
    /// one: the tool stays in the registry and becomes visible again on the
    /// next turn once its dependency is configured, so a Settings toggle
    /// still takes effect live without rebuilding any registry.
    ///
    /// Default `true` — only tools with an optional external dependency
    /// (a vision model, an opt-in debug endpoint) override it.
    fn available(&self) -> bool {
        true
    }

    /// The progressive-disclosure group this tool belongs to, if any.
    ///
    /// A tool in a group is NOT advertised until the agent asks for the group
    /// by name via `load_tools`. Until then the group costs one line in the
    /// stable head's index instead of a full schema per tool — the browser
    /// family alone is ten schemas (~900 tokens) that a typical coding turn
    /// never calls.
    ///
    /// The trade-off is one extra round-trip the first time a group is
    /// actually needed, and one prompt-cache invalidation when the array
    /// grows. Both are paid once per session per group, which is why only
    /// *occasional* families are grouped: anything the agent reaches for on a
    /// normal turn (files, search, git, memory, plans) stays always-on.
    ///
    /// Default `None` — always advertised.
    fn deferred_group(&self) -> Option<&str> {
        None
    }

    /// Execute the tool with parsed arguments.
    async fn execute(&self, args: Value) -> ToolResult;
}

/// Which tools the LLM is allowed to see this turn, derived from workflow state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolFilter {
    /// Planning: read agent tools + `create_plan` + all memory tools.
    Planning,
    /// Executing: all agent tools + `complete_step`/`create_plan`/`update_plan`/
    /// `abandon_plan` + all memory tools. `skill_create` (authoring a skill
    /// file) is Executing-only; `skill_reload` is available in every state.
    Executing,
    /// Reviewing: all agent tools (so the closing sequence — spawn_agent,
    /// git, file_edit, file_append, shell — can run) + `finish`/`ask_user`/
    /// `current_plan`/`abandon_plan` + all memory tools. The plan is done, so
    /// `complete_step`/`create_plan`/`update_plan` are NOT available.
    /// `abandon_plan` is the escape hatch: an un-completable review pops the
    /// finished plan and returns to Planning, so the agent is never stuck.
    /// `write_review_report` is NEVER visible here: the main agent can never
    /// author a review — reports are authored only by spawned reviewers.
    Reviewing,
    /// Executing a `kind: "research"` plan — [`Executing`](Self::Executing)
    /// minus the source-mutating file tools.
    ///
    /// DISPATCH-ONLY since 2027-01-11: [`Workflow::schema_filter`] no longer
    /// advertises this surface for research plans — a per-kind advertised
    /// array rewrote the head on every plan-KIND change and collapsed the
    /// provider's prefix cache to 6,016 tokens with 15.7–17.9s TTFT
    /// (`.coding/analysis/cache-hit-6-report.md`, cause 2). The restriction
    /// itself is unchanged and still enforced here, at the gate.
    ///
    /// [`Workflow::schema_filter`]: crate::workflow::Workflow::schema_filter
    ///
    /// A research plan is *defined* as work that produces no source-code
    /// changes (it is the kind that skips review on completion). Denying
    /// `file_edit`/`file_write`/`file_append`/`convert_line_endings` turns
    /// that from a rule the prompt merely asserts into one the tool gate
    /// enforces.
    ///
    /// The ban is PATH-SCOPED, and this filter carries only the name half of
    /// it: a research plan may still write its own ARTIFACTS under
    /// `.coding/**` (analysis notes, extractor scripts, run logs — the
    /// deliverables of an investigation), which the dispatch layer passes
    /// through via `research_write_verdict`, because only dispatch sees
    /// the call's `path` argument. Source, docs, config, the protected
    /// side-car entries and `..` escapes stay denied there.
    ///
    /// NOT a security boundary: `shell` stays visible because investigation
    /// genuinely needs it, and a determined `shell` call can still write a
    /// file. The point is to stop the *accidental* drift where a plan marked
    /// research quietly starts editing code.
    ExecutingResearch,
    /// Plan-frozen executing surface — the ADVERTISEMENT filter for an active
    /// plan: [`Executing`](Self::Executing) plus `finish`.
    ///
    /// Why it exists: the provider prefix cache reuses the request body only
    /// up to the first changed byte, and the tools array rides at the head of
    /// that body — so the array changing at the Executing→Reviewing
    /// transition (`finish` joins, `complete_step`/`create_plan` leave)
    /// resets the cached prefix and re-bills the entire conversation
    /// (~40-75s of server-side prefill at late-plan context sizes; perf
    /// review L4, 2026-09-09). While a plan is active the schema array is
    /// therefore built from THIS filter — one byte-stable surface for the
    /// plan's whole lifetime (see `Workflow::schema_filter`).
    ///
    /// ADVERTISEMENT ONLY, NOT ENFORCEMENT: dispatch re-checks the per-state
    /// filter (`allowed_tools()`), so `complete_step`/`create_plan` being
    /// advertised during Reviewing and `finish` being advertised during
    /// Executing are rejected at dispatch with the state named in the error.
    /// The state discipline (no step/new-plan work mid-review, no finishing
    /// mid-execution) is unchanged — only the schema bytes are frozen.
    /// `skill_create` rides along for the same reason: it is Executing-only in
    /// practice, but the surface DURING Executing is this filter.
    ///
    /// Since 2027-01-11 this is the advertised surface for EVERY active plan,
    /// research plans included: a per-kind array changed the request head on
    /// every plan-kind change and collapsed the provider's prefix cache to
    /// 6,016 tokens with 15.7–17.9s TTFT (`.coding/analysis/cache-hit-6-report.md`,
    /// cause 2). A research plan's write restriction is enforced at dispatch by
    /// [`ToolFilter::ExecutingResearch`], never by this advertisement.
    PlanFrozen,
    /// Complete: read agent tools + `create_plan` + all memory tools.
    Complete,
    /// A skill is active — only the named tools in the allow-list are visible
    /// (plus all memory tools, which are always available). Seeded from the
    /// active skill's `tools` field at `start_skill` time. The always-available
    /// set (`skill_reload`, ask_user, current_plan and the backlog tools) is
    /// visible regardless of the list; `skill_create` is denied here by name —
    /// a skill file must not widen the Executing-only authoring rule.
    Skill(Vec<String>),
    /// A read-only reviewer sub-agent — a STRICT allow-list: only the named
    /// tools (+ `current_plan`, a read-only orientation query on the shared
    /// plan) are visible. Unlike [`ToolFilter::Skill`], NOTHING is
    /// auto-granted: memory tools (even reads) appear only when listed, and
    /// `ask_user` / backlog tools / plan-mutation tools / `finish` can never
    /// appear because the reviewer base list never names them. Seeded from
    /// the `role: "reviewer"` spawn path in `spawn_agent_shared`.
    ///
    /// This variant is the ONLY filter under which `write_review_report` is
    /// visible at all — the tool is constructor-only, granted exclusively on
    /// the `role: "reviewer"` spawn path. Every base state and every Skill
    /// allow-list denies it, so a review report can only ever be authored by
    /// a spawned reviewer, never by the main agent.
    Reviewer(Vec<String>),
}

impl ToolFilter {
    /// Derive the filter from the workflow state.
    ///
    /// Returns `None` for [`WorkflowState::Skill`] — the skill filter is NOT
    /// derived from the state alone (it depends on the active skill's stored
    /// tool list) and is constructed directly by
    /// [`Workflow::allowed_tools`](crate::workflow::Workflow::allowed_tools).
    /// Returning `None` here makes a caller that forgets to handle the Skill
    /// case fail loudly at the call site rather than silently degrading to a
    /// wrong-but-plausible `Planning` filter.
    ///
    /// Returns `None` for [`WorkflowState::Subagent`] for the same reason: a
    /// sub-agent's tool surface comes from its spawn-time allow-list
    /// (`compute_subagent_allowlist` → `set_tool_allowlist` /
    /// `set_reviewer_allowlist`), never from a lifecycle state it doesn't
    /// own.
    pub fn from_state(state: WorkflowState) -> Option<Self> {
        match state {
            WorkflowState::Planning => Some(ToolFilter::Planning),
            WorkflowState::Executing => Some(ToolFilter::Executing),
            WorkflowState::Reviewing => Some(ToolFilter::Reviewing),
            WorkflowState::Complete => Some(ToolFilter::Complete),
            WorkflowState::Skill | WorkflowState::Subagent => None,
        }
    }

    /// Whether this filter is an explicit allow-list that NAMES `name`.
    ///
    /// Only the constructor-granted variants ([`Skill`](Self::Skill),
    /// [`Reviewer`](Self::Reviewer)) can name a tool; the workflow states
    /// admit tools by category, never by an explicit roster. Used to let an
    /// explicit grant bypass progressive disclosure.
    pub fn explicitly_names(&self, name: &str) -> bool {
        match self {
            ToolFilter::Skill(list) | ToolFilter::Reviewer(list) => list.iter().any(|t| t == name),
            _ => false,
        }
    }

    /// Whether a tool of this category + safety + name is visible under this filter.
    pub fn allows(&self, category: ToolCategory, safety: SafetyLevel, name: &str) -> bool {
        // HARD RULE: a review report can only be authored by a spawned
        // reviewer — `write_review_report` is visible ONLY under the
        // constructor-granted [`ToolFilter::Reviewer`] allow-list (and only
        // when the list actually names it, enforced by the Reviewer arm
        // below). Every base state (Planning/Executing/Reviewing/Complete)
        // and every Skill allow-list denies it, so the main agent can never
        // author a code review — through the schema, at dispatch, or via a
        // skill that tries to list it. This is the tool-layer enforcement of
        // the reviewer-only authorship rule.
        if name == "write_review_report" && !matches!(self, ToolFilter::Reviewer(_)) {
            return false;
        }
        // `skill_create` is EXECUTING-ONLY (the authoring gate). The base-state
        // arms below allow it in Executing / ExecutingResearch / PlanFrozen;
        // the constructor-granted allow-lists (Skill, Reviewer) deny it by name
        // so a skill file listing it — or a subagent allow-list carrying it —
        // can never become a back door around that. Same shape as the
        // reviewer-only rule above, mirrored.
        if name == "skill_create"
            && matches!(self, ToolFilter::Skill(_) | ToolFilter::Reviewer(_))
        {
            return false;
        }
        match self {
            ToolFilter::Planning => match category {
                // HARD RULE: no changes without a plan. Only read (AutoRun)
                // tools are visible — every mutation (file_edit/write/append,
                // shell, git) is hidden until a plan exists. Hiding (not just
                // approval-gating) makes the rule unskippable: the agent cannot
                // mutate the project in the Planning state. EXCEPTION:
                // spawn_agent is visible (it's NeedsApproval, so each spawn
                // still prompts) — the agent may fan out read-only/review
                // subagents from any state, including before a plan exists.
                // A TRUSTED MCP tool is AutoRun for APPROVAL only — its name
                // stays excluded here so per-server trust never widens the
                // plan-first gates (review: trust ≠ state visibility).
                ToolCategory::Agent => {
                    (safety == SafetyLevel::AutoRun && !name.starts_with("mcp__"))
                        || name == "spawn_agent"
                }
                // create_plan is the planning tool (AutoRun — it writes only to
                // the sandboxed `.coding/plans/` dir); complete_step is NOT
                // available (no plan exists yet to complete steps against).
                // ask_user + current_plan are always available (asking isn't a
                // mutation; current_plan is a read).
                ToolCategory::Workflow => {
                    name == "create_plan"
                        || name == "skill_start"
                        // skill_reload re-reads the project's own skills dir
                        // into the live library — a read plus an in-memory
                        // swap, never a plan/workflow mutation — so it is
                        // available in EVERY state (and inside skills: see the
                        // Skill arm's always-available set).
                        || name == "skill_reload"
                        || name == "ask_user"
                        || name == "current_plan"
                        // backlog_add queues a benign, AutoRun, user-visible
                        // planning note — capturing a follow-up is exactly a
                        // Planning-state activity. backlog_status is always
                        // available like ask_user/current_plan: the backlog is
                        // user-owned state the agent may legitimately update
                        // from any state. backlog_list is a read-only query,
                        // always available too.
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                }
                ToolCategory::Memory => true, // always available
                // Browser tools are available in every state: they drive the
                // user-visible Browser tab (child WebView2 via CDP) or
                // headless pages, never project files, so the "no changes
                // without a plan" rule does not apply. Mutating ones
                // (navigate/close/eval/click/type) are NeedsApproval and
                // prompt the user on every call — the approval gate is the
                // guard.
                ToolCategory::Browser => true,
            },
            ToolFilter::Executing => match category {
                // A plan exists — all tools are available so the agent can
                // execute it. Write tools still go through the approval gate
                // (the user's safety mode), which is the correct guard here.
                ToolCategory::Agent => true,
                // All workflow tools are available. complete_step checks off the
                // current plan; create_plan pushes a sub-plan (nested work) or a
                // fresh plan; update_plan edits the active plan in place
                // (preserving completed steps); abandon_plan (auto-run,
                // last resort) pops a stale/wrong plan and resumes the parent;
                // skill_start/skill_end/abandon_skill drive the skill lifecycle
                // (available in Complete + Planning, NOT Executing — see below).
                // ask_user + current_plan are always available.
                ToolCategory::Workflow => {
                    name == "complete_step"
                        || name == "create_plan"
                        || name == "update_plan"
                        || name == "abandon_plan"
                        // skill_reload: every state (see the Planning arm).
                        // skill_create: EXECUTING ONLY — authoring a skill file
                        // is executing work. It is in PlanFrozen too, because
                        // that is the advertised surface while a plan is
                        // active; a call during Reviewing is rejected at
                        // dispatch with the state named.
                        || name == "skill_reload"
                        || name == "skill_create"
                        || name == "ask_user"
                        || name == "current_plan"
                        // backlog_add stays available mid-execution: the user
                        // often asks to queue a follow-up while work is in
                        // flight. backlog_status is always available (see the
                        // Planning arm's note); backlog_list is a read-only
                        // query, always available.
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                }
                ToolCategory::Memory => true,
                // A plan exists — all browser tools (reads + mutations) are
                // available; mutations still go through the approval gate.
                ToolCategory::Browser => true,
            },
            ToolFilter::ExecutingResearch => match category {
                // Same surface as Executing, minus the tools whose only
                // purpose is changing source files. `shell`, `git`, `search`
                // and the read tools stay — research needs to run and inspect
                // things, it just must not edit them. MCP tools are excluded
                // wholesale: a research plan is defined by producing no
                // source-code changes, and foreign MCP tools can mutate
                // anything (they stay NeedsApproval-gated in the states that
                // do allow them).
                //
                // DENIED BY NAME ONLY: this arm cannot see the call's target
                // path, so the `.coding/**` artifact carve-out for a research
                // plan is applied one layer down, in dispatch
                // (`research_write_verdict` + `Sandbox::is_artifact_write_target`).
                ToolCategory::Agent => {
                    !matches!(
                        name,
                        "file_edit" | "file_write" | "file_append" | "convert_line_endings"
                    ) && !name.starts_with("mcp__")
                }
                // Same surface as Executing, skill_create included: a skill
                // file is a `.coding/**` artifact, not source — a research
                // plan's restriction is about source-code changes.
                ToolCategory::Workflow => {
                    name == "complete_step"
                        || name == "create_plan"
                        || name == "update_plan"
                        || name == "abandon_plan"
                        || name == "skill_reload"
                        || name == "skill_create"
                        || name == "ask_user"
                        || name == "current_plan"
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                }
                ToolCategory::Memory => true,
                // Investigating a running web app is squarely research work.
                ToolCategory::Browser => true,
            },
            ToolFilter::PlanFrozen => match category {
                // The plan-frozen advertisement: identical to Executing on
                // every category except Workflow, which adds `finish` so the
                // Executing→Reviewing transition changes no schema bytes.
                ToolCategory::Agent => true,
                // Executing's workflow surface plus `finish` (the Reviewing
                // exit). During Executing a finish call is rejected at
                // dispatch (the per-state filter has no finish); during
                // Reviewing complete_step/create_plan calls are rejected the
                // same way. The array is an advertisement; the per-state
                // filter re-checked at dispatch is the gate.
                ToolCategory::Workflow => {
                    name == "complete_step"
                        || name == "create_plan"
                        || name == "update_plan"
                        || name == "abandon_plan"
                        // Carried here because PlanFrozen IS the advertised
                        // surface during Executing; the per-state arms above
                        // decide whether a call actually runs (skill_create is
                        // Executing-only, skill_reload is in every state).
                        || name == "skill_reload"
                        || name == "skill_create"
                        || name == "finish"
                        || name == "ask_user"
                        || name == "current_plan"
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                }
                ToolCategory::Memory => true,
                ToolCategory::Browser => true,
            },
            ToolFilter::Reviewing => match category {
                // The plan is done — the review→fix→commit closing sequence is
                // in flight. ALL agent tools are available so the agent can
                // spawn a reviewer subagent, read its report, fix findings
                // (file_edit/shell), and commit (git). Write tools still go
                // through the approval gate (the user's safety mode).
                ToolCategory::Agent => true,
                // Browser tools are available during the closing sequence too
                // (~800 tokens/turn, mitigated by progressive disclosure):
                // verifying a frontend fix live (navigate → screenshot the
                // visible tab) is review-adjacent work, the tools never
                // mutate project files, and mutating ones still prompt via
                // their NeedsApproval gate. A reviewer subagent gets the tools
                // through its own allow-list.
                ToolCategory::Browser => true,
                // `finish` is the ONLY way out of Reviewing → Complete (and it
                // is gated on a review report). `abandon_plan` is the escape
                // hatch: an un-completable review (reviewer keeps failing, the
                // user declines to retry) pops the finished plan and returns
                // to Planning — the main agent is never stuck in Reviewing.
                // ask_user + current_plan are always available.
                // complete_step/create_plan are NOT: the plan is finished, so
                // no step or new-plan work is allowed mid-review. update_plan
                // IS available — fully callable for the main agent (title/
                // goal/context/steps append included; the workflow method
                // accepts them in Reviewing like Executing, minus
                // complete_step). Backlog 24e1c98e: fixing review findings or
                // appending follow-on fix work mid-closing-sequence is
                // legitimate; the regression_test field stays accepted (the
                // finish↔update_plan deadlock fix). The reviewer subagent
                // never sees this tool: ToolFilter::Reviewer's strict
                // allow-list never names plan-mutation tools, and every
                // subagent also runs with plan_mutations_allowed(false).
                ToolCategory::Workflow => {
                    name == "finish"
                        || name == "abandon_plan"
                        // skill_reload: every state, the closing sequence
                        // included — swapping the registry touches no plan
                        // state. (skill_create is denied here.)
                        || name == "skill_reload"
                        || name == "ask_user"
                        || name == "current_plan"
                        || name == "update_plan"
                        // The backlog tools are user-facing queue operations,
                        // not plan mutations — a direct user request to queue
                        // an item or update a status must never be impossible
                        // mid-closing-sequence (2026-12-30 reversal of the
                        // 2026-09-04 decision, after two blocked user requests
                        // and one silently-lost add). backlog_list is a
                        // read-only query. The reviewer subagent still never
                        // sees these (ToolFilter::Reviewer's allow-list
                        // excludes them).
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                }
                ToolCategory::Memory => true,
            },
            ToolFilter::Complete => match category {
                // HARD RULE: no changes without a plan, even after the previous
                // plan completed. Only read tools + create_plan are visible —
                // to make a follow-up change, the agent must plan first.
                // EXCEPTION: spawn_agent is visible (it's NeedsApproval, so
                // each spawn still prompts) — the agent may fan out
                // read-only/review subagents from Complete too.
                // A TRUSTED MCP tool is AutoRun for APPROVAL only — its name
                // stays excluded here so per-server trust never widens the
                // plan-first gates (review: trust ≠ state visibility).
                ToolCategory::Agent => {
                    (safety == SafetyLevel::AutoRun && !name.starts_with("mcp__"))
                        || name == "spawn_agent"
                }
                // Browser tools (reads AND mutations) stay visible in Complete:
                // between tasks is exactly when the user asks the agent to
                // open/navigate a page for them to follow. They drive the
                // user-visible Browser tab or headless pages — never project
                // files — and mutating ones prompt the user on every call
                // (NeedsApproval), so the approval gate is the guard.
                ToolCategory::Browser => true,
                // Allow create_plan so the agent can start a new plan (a
                // follow-up task or bug fix). complete_step is NOT available
                // (the previous plan is already done). skill_start IS available
                // — a finished plan is exactly when the user may want to run a
                // skill (e.g. merge_to_main). skill_end/abandon_skill are only
                // meaningful while a skill is active (the Skill filter exposes
                // them via the allow-list), so they're hidden here.
                // ask_user + current_plan are always available.
                ToolCategory::Workflow => {
                    name == "create_plan"
                        || name == "skill_start"
                        // skill_reload: every state (see the Planning arm) —
                        // between tasks is exactly when a skill file may have
                        // been hand-edited.
                        || name == "skill_reload"
                        || name == "ask_user"
                        || name == "current_plan"
                        // backlog_add stays available in Complete — capturing
                        // a user follow-up as a note needs no new plan.
                        // backlog_status is always available; backlog_list is
                        // a read-only query.
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                }
                ToolCategory::Memory => true,
            },
            ToolFilter::Skill(allowed) => match category {
                // Only the tools named in the skill's allow-list are visible.
                // Memory tools + ask_user + current_plan + the backlog tools
                // are always available regardless of the list (asking a
                // question, reading the active plan, or touching the user's
                // queue are never mutations of the plan/workflow, and a skill
                // may need any of them — the user asked for backlog_status
                // in all states). backlog_add rides the set too (backlog
                // 66c65db9): the 2026-12-30 reversal's principle — a direct
                // user request to queue an item must never be impossible —
                // extends mid-skill, e.g. a run-all item invoking
                // merge_to_main when the user steers to queue an item.
                ToolCategory::Memory => true,
                _ => {
                    name == "ask_user"
                        || name == "current_plan"
                        // skill_reload rides the always-available set: a reload
                        // is a read + in-memory swap, and mid-skill is exactly
                        // when a hand-edited skill file may need picking up
                        // (e.g. a run-all item in merge_to_main). skill_create
                        // deliberately does NOT — the guard at the top of
                        // `allows` denies it under every allow-list, so a skill
                        // file naming it cannot widen the Executing-only rule.
                        || name == "skill_reload"
                        || name == "backlog_add"
                        || name == "backlog_status"
                        || name == "backlog_list"
                        || allowed.iter().any(|n| n == name)
                }
            },
            ToolFilter::Reviewer(allowed) => {
                // A read-only reviewer sub-agent: a STRICT allow-list — only
                // the named tools + current_plan (read-only orientation on
                // the shared plan) are visible. Unlike Skill, NOTHING is
                // auto-granted: memory tools (even reads) appear only when
                // listed, and ask_user / backlog tools / plan-mutation tools
                // / finish can never appear because the reviewer base list
                // never names them. This is the enforcement point of the
                // reviewer contract: query, never mutate.
                name == "current_plan" || allowed.iter().any(|n| n == name)
            }
        }
    }
}

/// The registry of all available tools.
/// The progressive-disclosure groups a registry can reveal, and the one-line
/// index the model sees for the ones still hidden.
///
/// Kept here (not in the prompt module) so the index can never drift from the
/// tools that actually exist: it is generated from the live registry.
pub const DEFERRED_GROUPS: &[(&str, &str)] = &[
    (
        "browser",
        "drive a headless browser (and the app's own tab): navigate, click, type, eval JS, screenshot, DOM snapshot, console logs, multi-page control",
    ),
    (
        "image",
        "read an image with the vision model: diagnose an error screenshot, extract text, turn a UI mockup into code, explain a diagram or chart, diff two UI screenshots",
    ),
];

/// The deferred-group table for THIS install: the static base (browser,
/// image) plus one `mcp.<server>` group per ENABLED configured MCP server
/// (see [`crate::config::mcp`]). Built by the agent factory at
/// registry-construction time from the parsed `mcp.toml` — an absent or
/// empty config yields exactly the static base, so prompts stay
/// byte-identical on installs that configure no MCP servers.
pub fn deferred_groups_with(mcp: &[crate::config::mcp::McpServerDef]) -> Vec<(String, String)> {
    let mut groups: Vec<(String, String)> = DEFERRED_GROUPS
        .iter()
        .map(|(g, s)| ((*g).to_string(), (*s).to_string()))
        .collect();
    for server in mcp.iter().filter(|s| s.enabled) {
        groups.push((
            format!("mcp.{}", server.name),
            format!(
                "tools from the '{}' MCP server — load the group, then call them by name \
                 (each call goes through your approval gate)",
                server.name
            ),
        ));
    }
    groups
}

/// Shared, interior-mutable store for tools that JOIN the registry after
/// it was built — today only MCP tools, materialized when a `load_tools`
/// reveal connects to a server and lists what it offers.
///
/// The classic registry pattern ([`LoadedGroups`]) breaks the
/// load_tools-lives-inside-the-registry cycle with a shared set; this is
/// the same move for the tools themselves. The registry keeps a clone and
/// consults the slot in `get`/`iter`/`schemas`/`dispatch`, so a revealed
/// tool is callable and advertised from the next request on — without
/// wrapping the whole registry in an `Arc` or changing `register`'s
/// receiver for the ~100 existing call sites.
#[derive(Clone, Default)]
pub struct ToolSlot(std::sync::Arc<std::sync::RwLock<Vec<std::sync::Arc<dyn Tool>>>>);

impl ToolSlot {
    /// A fresh, empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a dynamically-revealed tool (insertion order preserved).
    /// Duplicate names are the caller's problem — the MCP reveal path
    /// checks [`Self::contains_name`] first.
    pub fn push(&self, tool: std::sync::Arc<dyn Tool>) {
        self.0.write().expect("tool slot lock poisoned").push(tool);
    }

    /// Whether a tool with this name is already in the slot (reveal-side
    /// dedup guard).
    pub fn contains_name(&self, name: &str) -> bool {
        self.0
            .read()
            .expect("tool slot lock poisoned")
            .iter()
            .any(|t| t.name() == name)
    }

    /// A snapshot of the slot's tools (Arc clones — cheap).
    fn snapshot(&self) -> Vec<std::sync::Arc<dyn Tool>> {
        self.0.read().expect("tool slot lock poisoned").clone()
    }
}

/// The set of progressive-disclosure groups revealed so far, shared between a
/// [`ToolRegistry`] and the `load_tools` tool that reveals them.
///
/// Interior mutability is required, not incidental: an agent holds its
/// registry behind an `Arc`, and `load_tools` is itself a tool *inside* that
/// registry. A shared handle breaks the cycle — both sides hold a clone of
/// the same set, so a reveal is visible to the next `schemas()` call without
/// anyone needing `&mut ToolRegistry`.
#[derive(Clone, Default)]
pub struct LoadedGroups(std::sync::Arc<std::sync::RwLock<std::collections::HashSet<String>>>);

impl LoadedGroups {
    /// A fresh, empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `group` has been revealed.
    pub fn contains(&self, group: &str) -> bool {
        self.0
            .read()
            .expect("loaded groups lock poisoned")
            .contains(group)
    }

    /// Reveal `group`; returns `false` if it was already revealed.
    pub fn insert(&self, group: &str) -> bool {
        self.0
            .write()
            .expect("loaded groups lock poisoned")
            .insert(group.to_string())
    }
}

impl std::fmt::Debug for LoadedGroups {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = self.0.read().expect("loaded groups lock poisoned");
        f.debug_set().entries(set.iter()).finish()
    }
}

pub struct ToolRegistry {
    tools: HashMap<String, std::sync::Arc<dyn Tool>>,
    /// Dynamically revealed tools (MCP) — join via the shared slot when
    /// `load_tools` connects to a server and lists its tools.
    dynamic: ToolSlot,
    /// The deferred-group table this registry advertises: the static base
    /// plus any configured `mcp.<server>` groups (see
    /// [`deferred_groups_with`]).
    groups: Vec<(String, String)>,
    /// Deferred groups the agent has asked for with `load_tools`. Tools in a
    /// group stay out of the `tools` array until their group is in here.
    loaded_groups: LoadedGroups,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::with_groups(LoadedGroups::new(), deferred_groups_with(&[]))
    }

    /// Build a registry sharing an existing reveal-set, so the `load_tools`
    /// tool registered inside it can reveal groups this registry advertises.
    /// Uses the static deferred-group table (no MCP servers).
    pub fn with_loaded_groups(groups: LoadedGroups) -> Self {
        Self::with_groups(groups, deferred_groups_with(&[]))
    }

    /// Build with an explicit deferred-group table (static base + MCP
    /// groups via [`deferred_groups_with`]) and a fresh dynamic-tool slot.
    pub fn with_groups(groups: LoadedGroups, table: Vec<(String, String)>) -> Self {
        Self {
            tools: HashMap::new(),
            dynamic: ToolSlot::new(),
            groups: table,
            loaded_groups: groups,
        }
    }

    /// The dynamic-tool slot, to hand to the MCP reveal path BEFORE the
    /// `load_tools` tool is registered (same shared-handle pattern as
    /// [`Self::loaded_groups`]).
    pub fn dynamic_slot(&self) -> ToolSlot {
        self.dynamic.clone()
    }

    /// The shared reveal-set (to hand to the `load_tools` tool).
    pub fn loaded_groups(&self) -> LoadedGroups {
        self.loaded_groups.clone()
    }

    /// Reveal a deferred group, so its tools join the `tools` array from the
    /// next request on. Returns the names revealed (empty if the group is
    /// unknown or was already loaded).
    pub fn load_group(&self, group: &str) -> Vec<String> {
        if !self.groups.iter().any(|(g, _)| g == group) {
            return Vec::new();
        }
        if !self.loaded_groups.insert(group) {
            return Vec::new();
        }
        let mut names: Vec<String> = self
            .iter()
            .filter(|t| t.deferred_group() == Some(group) && t.available())
            .map(|t| t.name().to_string())
            .collect();
        names.sort();
        names
    }

    /// Whether a deferred group has been revealed.
    pub fn group_loaded(&self, group: &str) -> bool {
        self.loaded_groups.contains(group)
    }

    /// The groups still hidden **and loadable under `filter`**, with their
    /// one-line descriptions — the index rendered into the stable head.
    ///
    /// Filter-aware on purpose. A group is only worth advertising if loading
    /// it would actually produce tools this agent can use: a category the
    /// filter disallows in the current state would cost index tokens (and a
    /// `load_tools` schema) to sell something that cannot be delivered.
    /// Groups whose tools are all unavailable — no vision model, CDP off —
    /// drop out for the same reason. (The browser family is loadable in
    /// Reviewing — the closing sequence allows it for live frontend
    /// verification — so it is advertised there like anywhere else.)
    pub fn hidden_groups_for(&self, filter: &ToolFilter) -> Vec<(String, String)> {
        let tools: Vec<_> = self.iter().collect();
        self.groups
            .iter()
            .filter(|(g, _)| !self.loaded_groups.contains(g))
            .filter(|(g, _)| {
                // MCP groups have NO registered tools until the reveal
                // connects and lists them — probe the filter with a
                // synthetic tool instead (category Agent, NeedsApproval,
                // mcp__ name so the research arm's prefix rule applies).
                if g.starts_with("mcp.") {
                    return filter.allows(
                        ToolCategory::Agent,
                        SafetyLevel::NeedsApproval,
                        "mcp__probe__probe",
                    );
                }
                tools.iter().any(|t| {
                    t.deferred_group() == Some(g.as_str())
                        && t.available()
                        && filter.allows(t.category(), t.safety(), t.name())
                })
            })
            .cloned()
            .collect()
    }

    /// Register a tool.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools
            .insert(tool.name().to_string(), std::sync::Arc::from(tool));
    }

    /// Look up a tool by name (registered or dynamically revealed).
    /// Returns an owned `Arc` clone so callers can hold the tool across
    /// awaits without borrowing the registry.
    pub fn get(&self, name: &str) -> Option<std::sync::Arc<dyn Tool>> {
        if let Some(t) = self.tools.get(name) {
            return Some(std::sync::Arc::clone(t));
        }
        self.dynamic
            .snapshot()
            .into_iter()
            .find(|t| t.name() == name)
    }

    /// Iterate over all tools — registered first, then dynamically revealed
    /// (slot order). Snapshot semantics: an owned `IntoIter`, safe to hold
    /// across awaits.
    pub fn iter(&self) -> std::vec::IntoIter<std::sync::Arc<dyn Tool>> {
        let mut all: Vec<std::sync::Arc<dyn Tool>> = self.tools.values().cloned().collect();
        all.extend(self.dynamic.snapshot());
        all.into_iter()
    }

    /// Build the `tools` array for the LLM, filtered by the workflow gate.
    ///
    /// Each filter arm decides whether a tool is included — including Memory
    /// tools: most arms admit all of them, but a strict allow-list (e.g. a
    /// read-only reviewer) only includes the named ones, so a reviewer is
    /// never even *advertised* memory-mutation tools. Strict schema
    /// enforcement is only set when `caps.supports_strict_schema`.
    pub fn schemas(&self, caps: &Capabilities, filter: &ToolFilter) -> Vec<ToolSchema> {
        let mut out = Vec::new();
        for tool in self.iter() {
            // A tool whose dependency is not configured (no vision model, CDP
            // inspection off) can only ever return "not configured" — leave it
            // out of the array rather than pay context for an error path.
            if !tool.available() {
                continue;
            }
            let category = tool.category();
            let safety = tool.safety();
            let name = tool.name();
            // Progressive disclosure: a grouped tool costs nothing until the
            // agent asks for its group (see `load_tools`).
            //
            // An EXPLICIT allow-list wins over deferral: when a skill or a
            // reviewer spawn names a tool outright, someone has already
            // decided that agent needs it, and making them spend a turn on
            // load_tools to get a tool they were handed would be absurd.
            if let Some(group) = tool.deferred_group() {
                if !self.loaded_groups.contains(group) && !filter.explicitly_names(name) {
                    continue;
                }
            }
            // The filter decides inclusion for every category (memory tools
            // are NOT unconditionally included — Reviewer must be able to
            // hide them from the schema).
            if !filter.allows(category, safety, name) {
                continue;
            }
            // `load_tools` only earns its ~700 chars when there is a group it
            // could usefully reveal — `hidden_groups_for` is filter-aware, so
            // when every hidden group's tools are unavailable under the
            // current filter (category-blocked, no vision model, CDP off),
            // the loader would be advertising a door that opens onto nothing.
            if name == "load_tools" && self.hidden_groups_for(filter).is_empty() {
                continue;
            }
            let mut schema = tool.schema();
            if caps.supports_strict_schema {
                schema.strict = Some(true);
            } else {
                schema.strict = None;
            }
            out.push(schema);
        }
        // Deterministic ordering for provider prompt-cache prefix stability:
        // HashMap iteration order is in-process-stable but not guaranteed
        // across restarts, which can reshuffle the tools array and reset the
        // cache for no semantic reason. The cache law needs DETERMINISM, not
        // alphabetical order specifically — so the semantic/knowledge tools
        // the agent underuses (memory + code-graph + search) sort FIRST for
        // primacy at the tool-selection point (order effects are real for the
        // smaller/local models this app supports), with plain alphabetical
        // order within each class. See `priority_class`.
        out.sort_by_key(|s| (priority_class(&s.name), s.name.clone()));
        out
    }

    /// Dispatch a tool call. Returns an error result if the tool is unknown.
    pub async fn dispatch(&self, call: &ToolCall) -> ToolResult {
        match self.get(&call.name) {
            // The tool is an owned Arc clone (the registry's get snapshots
            // its maps): `execute` takes `serde_json::Value` by value and
            // runs across awaits with no borrow of the registry.
            Some(tool) => tool.execute(call.arguments.clone()).await,
            None => ToolResult::error(format!(
                "unknown tool '{}'. Available: {}",
                call.name,
                self.available_names()
            )),
        }
    }

    /// A comma-separated list of available tool names (for error messages).
    fn available_names(&self) -> String {
        let mut names: Vec<String> = self.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        names.join(", ")
    }
}

/// Priority class for the deterministic `tools` array order (see
/// [`ToolRegistry::schemas`]).
///
/// 3-tier ordering preserves provider prompt-cache prefix stability across
/// workflow state transitions (Planning -> Executing -> Reviewing):
/// - Class 0: Underused semantic/knowledge tools (memory + code graph + text search)
///   sort FIRST for primacy at the tool-selection point.
/// - Class 1: Universal base tools visible across all workflow states
///   (read_files, git_read, ask_user, backlog_*, etc.).
/// - Class 2: State-dependent workflow and mutation tools (file_edit, file_write,
///   shell, git, create_plan, complete_step, etc.).
///
/// Because state-dependent tools are segregated into Class 2, transitioning
/// from Planning to Executing or Reviewing only appends or mutates the tail of
/// the `tools` array, leaving the Class 0 + Class 1 prefix 100% byte-stable.
/// Alphabetical by name within each class (the sort key is `(class, name)`).
fn priority_class(name: &str) -> u8 {
    const SEMANTIC_FIRST: &[&str] = &[
        "graph_context",
        "graph_impact",
        "graph_path",
        "graph_search",
        "memory_consolidate",
        "memory_recall",
        "memory_write",
        "search",
        "search_read",
    ];
    const UNIVERSAL_BASE: &[&str] = &[
        "ask_user",
        "backlog_list",
        "convert_line_endings",
        "current_plan",
        "file_read",
        "git_read",
        "list_models",
        "read_files",
        "web_fetch",
    ];
    if SEMANTIC_FIRST.contains(&name) {
        0
    } else if UNIVERSAL_BASE.contains(&name) {
        1
    } else {
        2
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A pair of test tools to exercise the registry + filter.
    struct ReadTool;
    struct WriteTool;
    struct PlanTool;
    struct MemoryTool;

    /// A second memory-category tool (the memory-write counterpart to
    /// [`MemoryTool`]'s recall), so tests can prove the Reviewer strict
    /// allow-list excludes memory MUTATIONS while base-state filters admit
    /// all memory tools.
    struct MemoryWriteStub;

    #[async_trait]
    impl Tool for ReadTool {
        fn name(&self) -> &str {
            "file_read"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Agent
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("file_read", "read a file", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("contents")
        }
    }

    #[async_trait]
    impl Tool for WriteTool {
        fn name(&self) -> &str {
            "file_write"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Agent
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("file_write", "write a file", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::NeedsApproval
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("written")
        }
    }

    #[async_trait]
    impl Tool for PlanTool {
        fn name(&self) -> &str {
            "create_plan"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Workflow
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("create_plan", "create a plan", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::NeedsApproval
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("plan created")
        }
    }

    #[async_trait]
    impl Tool for MemoryTool {
        fn name(&self) -> &str {
            "memory_recall"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Memory
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("memory_recall", "recall memory", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("recalled")
        }
    }

    #[async_trait]
    impl Tool for MemoryWriteStub {
        fn name(&self) -> &str {
            "memory_write"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Memory
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("memory_write", "write memory", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("written")
        }
    }

    struct CompleteStepTool;

    #[async_trait]
    impl Tool for CompleteStepTool {
        fn name(&self) -> &str {
            "complete_step"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Workflow
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("complete_step", "complete a step", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::NeedsApproval
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("step completed")
        }
    }

    struct FinishTool;

    #[async_trait]
    impl Tool for FinishTool {
        fn name(&self) -> &str {
            "finish"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Workflow
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("finish", "finish the review", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("finished")
        }
    }

    struct UpdatePlanTool;

    #[async_trait]
    impl Tool for UpdatePlanTool {
        fn name(&self) -> &str {
            "update_plan"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Workflow
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("update_plan", "edit the active plan", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("plan updated")
        }
    }

    struct AbandonPlanTool;

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
                "abandon the active plan",
                serde_json::json!({}),
            )
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("plan abandoned")
        }
    }

    struct SkillStartTool;

    #[async_trait]
    impl Tool for SkillStartTool {
        fn name(&self) -> &str {
            "skill_start"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Workflow
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("skill_start", "start a skill", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("skill started")
        }
    }

    struct SkillEndTool;

    #[async_trait]
    impl Tool for SkillEndTool {
        fn name(&self) -> &str {
            "skill_end"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Workflow
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("skill_end", "end the active skill", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("skill ended")
        }
    }

    /// A stub `spawn_agent` tool for filter tests — Agent category +
    /// NeedsApproval, mirroring the real tool's safety posture so the filter
    /// gating (visible in all base states) is exercised against the registry.
    struct SpawnAgentStub;

    #[async_trait]
    impl Tool for SpawnAgentStub {
        fn name(&self) -> &str {
            "spawn_agent"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Agent
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new("spawn_agent", "spawn a subagent", serde_json::json!({}))
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::NeedsApproval
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("spawned")
        }
    }

    /// A stub `write_review_report` tool for filter tests — Agent category +
    /// AutoRun, mirroring the real tool. It must NEVER appear in any filter
    /// other than the reviewer allow-list (reviewer-only authorship).
    struct WriteReviewReportStub;

    #[async_trait]
    impl Tool for WriteReviewReportStub {
        fn name(&self) -> &str {
            "write_review_report"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Agent
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "write_review_report",
                "write a review report",
                serde_json::json!({}),
            )
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::success("report written")
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Box::new(ReadTool));
        r.register(Box::new(WriteTool));
        r.register(Box::new(PlanTool));
        r.register(Box::new(CompleteStepTool));
        r.register(Box::new(FinishTool));
        r.register(Box::new(UpdatePlanTool));
        r.register(Box::new(AbandonPlanTool));
        r.register(Box::new(SkillStartTool));
        r.register(Box::new(SkillEndTool));
        r.register(Box::new(SpawnAgentStub));
        r.register(Box::new(WriteReviewReportStub));
        r.register(Box::new(MemoryTool));
        r.register(Box::new(MemoryWriteStub));
        // The real BacklogAddTool — the filter-visibility regression must
        // exercise the shipped tool's name/category, not a stub. The store
        // points at a never-written path; schema listing never executes, so
        // the store stays an empty in-memory instance.
        r.register(Box::new(
            crate::tool::workflow::backlog::BacklogAddTool::new(
                std::sync::Arc::new(tokio::sync::Mutex::new(crate::backlog::BacklogStore::open(
                    std::path::PathBuf::from("test-backlog-never-written.json"),
                ))),
                None,
            ),
        ));
        // The real BacklogStatusTool — same rationale as above: the
        // all-state visibility regression must exercise the shipped tool.
        r.register(Box::new(
            crate::tool::workflow::backlog::BacklogStatusTool::new(
                std::sync::Arc::new(tokio::sync::Mutex::new(crate::backlog::BacklogStore::open(
                    std::path::PathBuf::from("test-backlog-never-written.json"),
                ))),
                None,
            ),
        ));
        // The real BacklogListTool — same rationale: the read-only-query
        // visibility regression must exercise the shipped tool.
        r.register(Box::new(
            crate::tool::workflow::backlog::BacklogListTool::new(std::sync::Arc::new(
                tokio::sync::Mutex::new(crate::backlog::BacklogStore::open(
                    std::path::PathBuf::from("test-backlog-never-written.json"),
                )),
            )),
        ));
        r
    }

    #[test]
    fn planning_filter_hides_write_tools_until_a_plan_exists() {
        // HARD RULE: no changes without a plan. In Planning, only read
        // (AutoRun) tools + create_plan are visible; every mutation is hidden
        // so the agent cannot change the project before planning.
        let r = registry();
        let caps = Capabilities::openai();
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Planning)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"file_read".to_string())); // read tool
        assert!(names.contains(&"create_plan".to_string())); // the way forward
        assert!(!names.contains(&"file_write".to_string())); // hidden: mutation
        assert!(!names.contains(&"file_edit".to_string())); // hidden: mutation
        assert!(!names.contains(&"shell".to_string())); // hidden: mutation
        assert!(!names.contains(&"git".to_string())); // hidden: can commit
                                                      // spawn_agent IS visible in Planning — the agent may fan out
                                                      // read-only/review subagents from any state (it stays NeedsApproval,
                                                      // so each spawn still prompts). The subagent's tool surface is
                                                      // intersected with the parent's current filter at spawn time, so a
                                                      // subagent spawned from Planning can't reach file_edit/shell/git.
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(!names.contains(&"complete_step".to_string())); // not in Planning
        assert!(!names.contains(&"update_plan".to_string())); // not in Planning
                                                              // skill_start IS available in Planning — skills may be started from
                                                              // Planning or Complete (validated against the registry's available_in).
        assert!(names.contains(&"skill_start".to_string()));
        assert!(names.contains(&"memory_recall".to_string())); // always
    }

    #[test]
    fn executing_filter_includes_everything() {
        let r = registry();
        let caps = Capabilities::openai();
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"file_read".to_string()));
        assert!(names.contains(&"file_write".to_string()));
        // create_plan IS available in Executing — the agent can abandon a
        // stale/wrong plan and replan. (It overwrites the in-flight plan.)
        assert!(names.contains(&"create_plan".to_string()));
        // complete_step + update_plan ARE available in Executing.
        assert!(names.contains(&"complete_step".to_string()));
        assert!(names.contains(&"update_plan".to_string()));
        // finish is NOT available in Executing — it's the Reviewing→Complete
        // exit gate, only meaningful once all steps are done.
        assert!(!names.contains(&"finish".to_string()));
        // skill_start is NOT available in Executing — skills are only
        // startable from Complete or Planning.
        assert!(!names.contains(&"skill_start".to_string()));
        assert!(names.contains(&"memory_recall".to_string()));
    }

    #[test]
    fn trusted_mcp_tools_never_widen_read_only_state_visibility() {
        // Per-server trust is APPROVAL-only: a trusted MCP tool (AutoRun
        // safety, mcp__ name) must stay hidden in Planning/Complete/research
        // — the plan-first gates never widen — while Executing still admits
        // it (its calls skip the per-call prompt per the trust flag).
        for name in ["mcp__fs__echo", "mcp__fs__get_prompt"] {
            assert!(
                !ToolFilter::Planning.allows(ToolCategory::Agent, SafetyLevel::AutoRun, name),
                "trusted mcp tool {name} hidden in Planning"
            );
            assert!(
                !ToolFilter::Complete.allows(ToolCategory::Agent, SafetyLevel::AutoRun, name),
                "trusted mcp tool {name} hidden in Complete"
            );
            assert!(
                !ToolFilter::ExecutingResearch.allows(
                    ToolCategory::Agent,
                    SafetyLevel::AutoRun,
                    name
                ),
                "trusted mcp tool {name} hidden in research"
            );
            assert!(
                ToolFilter::Executing.allows(ToolCategory::Agent, SafetyLevel::AutoRun, name),
                "trusted mcp tool {name} visible in Executing"
            );
        }
        // Untrusted MCP tools (NeedsApproval) were already excluded by the
        // safety gate — trust does not change their state story.
        assert!(!ToolFilter::Complete.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "mcp__fs__echo"
        ));
        // A NON-mcp AutoRun tool keeps its read-only-state visibility — the
        // exclusion is mcp__-specific, so built-ins are unaffected.
        assert!(ToolFilter::Planning.allows(ToolCategory::Agent, SafetyLevel::AutoRun, "search"));
        assert!(ToolFilter::Complete.allows(ToolCategory::Agent, SafetyLevel::AutoRun, "git_read"));
    }

    #[test]
    fn complete_filter_hides_writes_until_a_new_plan() {
        // HARD RULE: no changes without a plan, even after the previous plan
        // completed. Only read tools + create_plan are visible — to make a
        // follow-up change, the agent must plan first.
        let r = registry();
        let caps = Capabilities::openai();
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Complete)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"file_read".to_string())); // read tool
        assert!(names.contains(&"create_plan".to_string())); // can start a new plan
        assert!(!names.contains(&"file_write".to_string())); // hidden: mutation
        assert!(!names.contains(&"file_edit".to_string())); // hidden: mutation
        assert!(!names.contains(&"shell".to_string())); // hidden: mutation
                                                        // spawn_agent IS visible in Complete — the agent may fan out
                                                        // read-only/review subagents from Complete too (stays NeedsApproval).
        assert!(names.contains(&"spawn_agent".to_string()));
        assert!(!names.contains(&"complete_step".to_string())); // previous plan is done
        assert!(!names.contains(&"update_plan".to_string())); // no active plan to edit
                                                              // finish is NOT available in Complete — the review is already closed
                                                              // out (finish is the Reviewing→Complete gate, already used).
        assert!(!names.contains(&"finish".to_string()));
        // skill_start IS available in Complete — a finished plan is when the
        // user may want to run a skill (e.g. merge_to_main).
        assert!(names.contains(&"skill_start".to_string()));
        assert!(names.contains(&"memory_recall".to_string())); // always
    }

    /// A tool that reports itself unavailable — stands in for an image tool
    /// with no vision model, or an on-screen browser tool with CDP off.
    struct UnavailableTool;

    #[async_trait]
    impl Tool for UnavailableTool {
        fn name(&self) -> &str {
            "unavailable_tool"
        }
        fn category(&self) -> ToolCategory {
            ToolCategory::Agent
        }
        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                "unavailable_tool",
                "never advertised",
                serde_json::json!({}),
            )
        }
        fn safety(&self) -> SafetyLevel {
            SafetyLevel::AutoRun
        }
        fn available(&self) -> bool {
            false
        }
        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::error("not configured")
        }
    }

    #[test]
    fn explicit_allow_list_bypasses_deferral() {
        // A skill or reviewer spawn that NAMES a deferred tool has already
        // decided the agent needs it — making it spend a turn on load_tools
        // to reach a tool it was handed would be absurd. Workflow states,
        // which admit by category rather than by roster, never bypass.
        let skill = ToolFilter::Skill(vec!["offscreen_browser_eval".to_string()]);
        assert!(skill.explicitly_names("offscreen_browser_eval"));
        assert!(!skill.explicitly_names("offscreen_browser_click"));

        let reviewer = ToolFilter::Reviewer(vec!["offscreen_browser_snapshot".to_string()]);
        assert!(reviewer.explicitly_names("offscreen_browser_snapshot"));

        for f in [
            ToolFilter::Planning,
            ToolFilter::Executing,
            ToolFilter::ExecutingResearch,
            ToolFilter::Reviewing,
            ToolFilter::Complete,
        ] {
            assert!(
                !f.explicitly_names("offscreen_browser_eval"),
                "{f:?} admits by category, never by explicit roster"
            );
        }
    }

    #[test]
    fn research_filter_hides_source_mutating_file_tools() {
        // A research plan produces no source-code changes by definition, so
        // the gate enforces what the prompt only asserts — and stops paying
        // ~780 tokens per turn for schemas the plan must not use.
        for name in [
            "file_edit",
            "file_write",
            "file_append",
            "convert_line_endings",
        ] {
            assert!(
                ToolFilter::Executing.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, name),
                "{name} is available while executing an implementation plan"
            );
            assert!(
                !ToolFilter::ExecutingResearch.allows(
                    ToolCategory::Agent,
                    SafetyLevel::NeedsApproval,
                    name
                ),
                "{name} is hidden while executing a research plan"
            );
        }
        // Investigation still needs to run and read things — shell, git and
        // the read tools stay. (This is a nudge + a token saving, not a
        // sandbox: shell can still write a file if the model insists.)
        for (name, safety) in [
            ("shell", SafetyLevel::NeedsApproval),
            ("git", SafetyLevel::NeedsApproval),
            ("file_read", SafetyLevel::AutoRun),
            ("read_files", SafetyLevel::AutoRun),
            ("search", SafetyLevel::AutoRun),
        ] {
            assert!(
                ToolFilter::ExecutingResearch.allows(ToolCategory::Agent, safety, name),
                "{name} stays available on a research plan"
            );
        }
    }

    #[test]
    fn browser_tools_available_in_every_base_state() {
        // Browser tools drive the user-visible Browser tab (child WebView2 via
        // CDP) or headless pages — they never mutate project files, so the
        // "no changes without a plan" rule does NOT apply to them. Mutating
        // ones (navigate/click/type/eval) are NeedsApproval and prompt the
        // user on every call — the approval gate is the guard, in every state.
        // Regression: these used to be hidden in Planning/Complete
        // (AutoRun-only) and Reviewing (all browser tools), so the agent could
        // never navigate the visible tab when the user asked between tasks.
        for name in [
            "browser_navigate",
            "browser_click",
            "browser_type",
            "browser_eval",
        ] {
            assert!(
                ToolFilter::Planning.allows(
                    ToolCategory::Browser,
                    SafetyLevel::NeedsApproval,
                    name
                ),
                "{name} is available in Planning"
            );
            assert!(
                ToolFilter::Executing.allows(
                    ToolCategory::Browser,
                    SafetyLevel::NeedsApproval,
                    name
                ),
                "{name} is available while executing"
            );
            assert!(
                ToolFilter::Reviewing.allows(
                    ToolCategory::Browser,
                    SafetyLevel::NeedsApproval,
                    name
                ),
                "{name} is available during the closing sequence"
            );
            assert!(
                ToolFilter::Complete.allows(
                    ToolCategory::Browser,
                    SafetyLevel::NeedsApproval,
                    name
                ),
                "{name} is available after the plan completes"
            );
        }
        // The genuinely read-only browser tools stay available everywhere too
        // (offscreen_browser_navigate below is a mutation — NeedsApproval).
        for name in ["browser_snapshot", "offscreen_browser_list_pages"] {
            for filter in [
                ToolFilter::Planning,
                ToolFilter::Executing,
                ToolFilter::Reviewing,
                ToolFilter::Complete,
            ] {
                assert!(
                    filter.allows(ToolCategory::Browser, SafetyLevel::AutoRun, name),
                    "{name} is available in {filter:?}"
                );
            }
        }
        // The offscreen mutating tools are pinned at their real safety level
        // (NeedsApproval, per safety_levels_are_set in tool/browser/mod.rs)
        // across the four base states — they prompt on every call.
        for name in ["offscreen_browser_navigate", "offscreen_browser_eval"] {
            for filter in [
                ToolFilter::Planning,
                ToolFilter::Executing,
                ToolFilter::Reviewing,
                ToolFilter::Complete,
            ] {
                assert!(
                    filter.allows(ToolCategory::Browser, SafetyLevel::NeedsApproval, name),
                    "{name} is available in {filter:?}"
                );
            }
        }
        // The tools Reviewing actually needs are untouched.
        assert!(ToolFilter::Reviewing.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "file_edit"
        ));
        assert!(ToolFilter::Reviewing.allows(
            ToolCategory::Workflow,
            SafetyLevel::AutoRun,
            "finish"
        ));
    }

    #[test]
    fn unavailable_tools_are_registered_but_never_advertised() {
        // `available()` is a RUNTIME gate applied in schemas(), not a
        // registration gate: the tool stays reachable in the registry (so a
        // config toggle needs no rebuild) but costs zero context until its
        // dependency exists.
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(UnavailableTool));
        assert!(
            registry.get("unavailable_tool").is_some(),
            "still registered"
        );
        let caps = Capabilities::openai();
        for filter in [
            ToolFilter::Planning,
            ToolFilter::Executing,
            ToolFilter::Reviewing,
            ToolFilter::Complete,
        ] {
            let names: Vec<String> = registry
                .schemas(&caps, &filter)
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert!(
                names.is_empty(),
                "{filter:?} must not advertise an unavailable tool, got: {names:?}"
            );
        }
    }

    #[test]
    fn spawn_agent_visible_in_all_base_states() {
        // The refined subagent permission model: ALL workflow states can spawn
        // subagents (so the agent can fan out read-only/review work from any
        // state). spawn_agent is NeedsApproval, so each spawn still prompts —
        // visibility here is not auto-execution. The subagent's tool surface is
        // intersected with the parent's current filter at spawn time (handled
        // in the IPC spawner), so a subagent spawned from Planning can't reach
        // file_edit/shell/git even though spawn_agent itself is visible.
        assert!(ToolFilter::Planning.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "spawn_agent"
        ));
        assert!(ToolFilter::Executing.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "spawn_agent"
        ));
        assert!(ToolFilter::Reviewing.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "spawn_agent"
        ));
        assert!(ToolFilter::Complete.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "spawn_agent"
        ));
        // A read tool is still AutoRun-visible everywhere (sanity).
        assert!(ToolFilter::Planning.allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "file_read"
        ));
        // And a NeedsApproval mutation (file_edit) is still hidden in the
        // read-only states — spawn_agent is the only NeedsApproval Agent tool
        // visible in Planning/Complete.
        assert!(!ToolFilter::Planning.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "file_edit"
        ));
        assert!(!ToolFilter::Complete.allows(
            ToolCategory::Agent,
            SafetyLevel::NeedsApproval,
            "file_edit"
        ));
    }

    #[test]
    fn reviewing_filter_exposes_closing_tools() {
        // The plan is done; the review→fix→commit closing sequence is in
        // flight. ALL agent tools are visible (the test registry has
        // file_read + file_write; file_write is NeedsApproval, so its
        // visibility proves the Agent => true arm — the closing tools
        // spawn_agent/git/file_edit/shell would be visible too in the real
        // registry). finish is the ONLY workflow tool beyond ask_user/
        // current_plan — the plan is done, so complete_step/create_plan/
        // update_plan are hidden.
        let r = registry();
        let caps = Capabilities::openai();
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Reviewing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        // Agent tools are visible — including NeedsApproval ones (file_write),
        // which proves the closing-sequence write/exec tools would be visible.
        assert!(names.contains(&"file_read".to_string()));
        assert!(names.contains(&"file_write".to_string()));
        // finish IS visible — it's the Reviewing→Complete exit gate.
        assert!(names.contains(&"finish".to_string()));
        // abandon_plan IS visible — the escape hatch: an un-completable
        // review can be abandoned (pops the finished plan → Planning), so the
        // agent is never stuck in Reviewing.
        assert!(names.contains(&"abandon_plan".to_string()));
        // Plan-lifecycle tools are NOT — the plan is finished. EXCEPTION:
        // update_plan IS visible (regression_test-only — the workflow method
        // rejects title/goal/context/steps in Reviewing): the test name is
        // often only known after running tests, and finish needs it.
        assert!(!names.contains(&"complete_step".to_string()));
        assert!(!names.contains(&"create_plan".to_string()));
        assert!(names.contains(&"update_plan".to_string()));
        assert!(!names.contains(&"skill_start".to_string()));
        assert!(names.contains(&"memory_recall".to_string())); // always
        // Backlog tools ARE available during the closing sequence
        // (2026-12-30 reversal of the 2026-09-04 decision): user
        // queue requests are never blocked mid-review.
        assert!(names.contains(&"backlog_add".to_string()));
        assert!(names.contains(&"backlog_status".to_string()));
        assert!(names.contains(&"backlog_list".to_string()));
    }

    #[test]
    fn plan_frozen_allows_finish_and_the_executing_surface() {
        // The plan-frozen ADVERTISEMENT: the Executing surface plus `finish`,
        // so the Executing→Reviewing transition changes no schema bytes (the
        // tools array rides at the head of the request body — any byte change
        // resets the provider prefix cache). Dispatch still enforces the
        // per-state rules; this filter only freezes what the model can SEE.
        let r = registry();
        let caps = Capabilities::openai();
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::PlanFrozen)
            .into_iter()
            .map(|s| s.name)
            .collect();
        // finish IS advertised — the Reviewing exit must already be in the
        // frozen array so the transition to Reviewing adds nothing.
        assert!(names.contains(&"finish".to_string()));
        // The full Executing workflow surface stays advertised (during
        // Reviewing these are rejected at dispatch — advertisement ≠
        // enforcement).
        assert!(names.contains(&"complete_step".to_string()));
        assert!(names.contains(&"create_plan".to_string()));
        assert!(names.contains(&"update_plan".to_string()));
        assert!(names.contains(&"abandon_plan".to_string()));
        assert!(names.contains(&"backlog_add".to_string()));
        assert!(names.contains(&"backlog_status".to_string()));
        assert!(names.contains(&"backlog_list".to_string()));
        // Agent tools visible (file_write is NeedsApproval — proves
        // Agent => true, i.e. the closing-sequence write/exec tools would be
        // visible in the real registry too).
        assert!(names.contains(&"file_write".to_string()));
        // skill_start is NOT in the frozen surface (Executing doesn't have
        // it; adding it would widen what Executing-phase requests advertise).
        assert!(!names.contains(&"skill_start".to_string()));
        assert!(names.contains(&"memory_recall".to_string())); // always
    }

    #[test]
    fn plan_frozen_differs_from_executing_only_by_finish() {
        // The frozen surface must be EXACTLY Executing ∪ {finish}: anything
        // more would advertise tools Executing never showed; anything less
        // would re-introduce an array change at the Reviewing transition.
        let r = registry();
        let caps = Capabilities::openai();
        let mut executing: Vec<String> = r
            .schemas(&caps, &ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        let mut frozen: Vec<String> = r
            .schemas(&caps, &ToolFilter::PlanFrozen)
            .into_iter()
            .map(|s| s.name)
            .collect();
        executing.sort();
        frozen.sort();
        executing.push("finish".to_string());
        executing.sort();
        assert_eq!(executing, frozen);
    }

    #[test]
    fn plan_frozen_never_admits_write_review_report() {
        // HARD RULE: a review report can only be authored by a spawned
        // reviewer. The plan-frozen advertisement must not widen that — the
        // check sits ahead of the category match in allows().
        assert!(!ToolFilter::PlanFrozen.allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "write_review_report"
        ));
    }

    /// skill_start must be visible in Complete and Planning (the states a skill
    /// may be started from, per the registry's available_in), and hidden in
    /// Executing. The Planning-state availability is also enforced by the
    /// SkillStartTool itself (it checks the registry's available_in list).
    #[test]
    fn skill_start_gating() {
        let r = registry();
        let caps = Capabilities::openai();
        let visible = |f: ToolFilter| {
            r.schemas(&caps, &f)
                .into_iter()
                .any(|s| s.name == "skill_start")
        };
        assert!(visible(ToolFilter::Planning), "visible in Planning");
        assert!(!visible(ToolFilter::Executing), "hidden in Executing");
        assert!(visible(ToolFilter::Complete), "visible in Complete");
    }

    /// `skill_reload` must be ALLOWED in every workflow state AND inside
    /// skills: a skill file can be hand-edited at any moment, and the registry
    /// is otherwise loaded only once, at app startup. The arms are explicit
    /// name allow-lists, so forgetting one silently hides the tool from the
    /// model — this is the regression for that failure mode.
    ///
    /// Asserted against [`ToolFilter::allows`] (the gate itself) rather than a
    /// schema listing from the test `registry()`: that helper holds hand-written
    /// stubs for the skill lifecycle tools, so a schema assertion would test the
    /// stub instead of the arm. Registration is pinned separately, by the
    /// factory's expected-tool-name set.
    #[test]
    fn skill_reload_allowed_in_every_state_and_inside_skills() {
        let v = |f: ToolFilter| {
            f.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "skill_reload")
        };
        assert!(v(ToolFilter::Planning), "allowed in Planning");
        assert!(v(ToolFilter::Executing), "allowed in Executing");
        assert!(
            v(ToolFilter::ExecutingResearch),
            "allowed in ExecutingResearch"
        );
        assert!(
            v(ToolFilter::PlanFrozen),
            "allowed on the frozen surface — this IS the surface during Executing"
        );
        assert!(v(ToolFilter::Reviewing), "allowed in Reviewing");
        assert!(v(ToolFilter::Complete), "allowed in Complete");
        assert!(
            v(ToolFilter::Skill(vec![])),
            "allowed inside a skill (always-available set, empty allow-list)"
        );
        assert!(
            !v(ToolFilter::Reviewer(vec!["file_read".into()])),
            "a reviewer must not get skill_reload implicitly — its arm is a literal allow-list"
        );
    }

    /// `skill_create` — authoring a skill file — is EXECUTING ONLY (the user's
    /// requirement). PlanFrozen carries it because that IS the advertised
    /// surface while a plan is active; the per-state arms are what decide (a
    /// call during Reviewing is rejected at dispatch with the state named).
    /// The constructor-granted allow-lists deny it by name, so a skill file (or
    /// a subagent list) naming it cannot widen the rule.
    #[test]
    fn skill_create_allowed_only_in_the_executing_surfaces() {
        let v = |f: ToolFilter| {
            f.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "skill_create")
        };
        assert!(v(ToolFilter::Executing), "allowed in Executing");
        assert!(
            v(ToolFilter::ExecutingResearch),
            "allowed in ExecutingResearch — a skill file is a .coding artifact, not source"
        );
        assert!(v(ToolFilter::PlanFrozen), "advertised on the frozen surface");
        assert!(!v(ToolFilter::Planning), "hidden in Planning");
        assert!(!v(ToolFilter::Reviewing), "hidden in Reviewing");
        assert!(!v(ToolFilter::Complete), "hidden in Complete");
        assert!(
            !v(ToolFilter::Skill(vec![])),
            "hidden inside a skill — an overlay is not the Executing state"
        );
        assert!(
            !v(ToolFilter::Skill(vec!["skill_create".into()])),
            "naming it in a skill allow-list must NOT grant it (the Executing-only guard)"
        );
        assert!(
            !v(ToolFilter::Reviewer(vec!["skill_create".into()])),
            "the reviewer never authors skills"
        );
    }

    /// backlog_add must be VISIBLE in every workflow state (it is a benign,
    /// AutoRun, user-visible note — the ToolFilter arms are explicit name
    /// allow-lists, so forgetting an arm silently hides the tool from the
    /// model; this is the regression for review finding 1). Reviewing
    /// included since 2026-12-30 (reversal of the 2026-09-04 decision): a
    /// direct user request to queue an item must never be impossible
    /// mid-closing-sequence. Inside skills too since backlog 66c65db9: it
    /// rides the always-available set next to backlog_status/backlog_list,
    /// so a user queue request is never impossible mid-skill either (e.g.
    /// a run-all item invoking merge_to_main when the user steers to queue
    /// an item).
    #[test]
    fn backlog_add_visible_in_all_states() {
        let r = registry();
        let caps = Capabilities::openai();
        let visible = |f: ToolFilter| {
            r.schemas(&caps, &f)
                .into_iter()
                .any(|s| s.name == "backlog_add")
        };
        assert!(visible(ToolFilter::Planning), "visible in Planning");
        assert!(visible(ToolFilter::Executing), "visible in Executing");
        assert!(visible(ToolFilter::ExecutingResearch), "visible in ExecutingResearch");
        assert!(
            visible(ToolFilter::Reviewing),
            "visible in Reviewing (2026-12-30 reversal — user queue requests are never blocked mid-review)"
        );
        assert!(visible(ToolFilter::Complete), "visible in Complete");
        assert!(
            visible(ToolFilter::Skill(vec![])),
            "visible inside a skill (always-available set — backlog 66c65db9: a \
             user queue request must never be impossible mid-skill, e.g. a \
             run-all item invoking merge_to_main)"
        );
    }

    /// backlog_status must be VISIBLE in every workflow state AND inside
    /// skills (the user requirement: the tool is available in all states).
    /// Reviewing included since 2026-12-30 (reversal of the 2026-09-04
    /// decision): a direct user request to update an item's status must
    /// never be impossible mid-closing-sequence. backlog_status rides the
    /// always-available set next to ask_user / current_plan (and, since
    /// backlog 66c65db9, backlog_add too), so it stays visible even under
    /// an empty skill allow-list.
    #[test]
    fn backlog_status_visible_in_all_states_including_skills() {
        let r = registry();
        let caps = Capabilities::openai();
        let visible = |f: ToolFilter| {
            r.schemas(&caps, &f)
                .into_iter()
                .any(|s| s.name == "backlog_status")
        };
        assert!(visible(ToolFilter::Planning), "visible in Planning");
        assert!(visible(ToolFilter::Executing), "visible in Executing");
        assert!(visible(ToolFilter::ExecutingResearch), "visible in ExecutingResearch");
        assert!(
            visible(ToolFilter::Reviewing),
            "visible in Reviewing (2026-12-30 reversal — user status updates are never blocked mid-review)"
        );
        assert!(visible(ToolFilter::Complete), "visible in Complete");
        assert!(
            visible(ToolFilter::Skill(vec![])),
            "visible inside a skill (always-available set)"
        );
    }

    /// backlog_list is a READ-ONLY query, visible in every workflow state
    /// (like the two backlog mutation tools since the 2026-12-30 reversal
    /// of the 2026-09-04 decision) AND inside skills via the
    /// always-available set. A read-only reviewer gets it via its strict
    /// allow-list (REVIEWER_BASE_TOOLS names it).
    #[test]
    fn backlog_list_visible_in_all_states_and_skills() {
        let r = registry();
        let caps = Capabilities::openai();
        let visible = |f: ToolFilter| {
            r.schemas(&caps, &f)
                .into_iter()
                .any(|s| s.name == "backlog_list")
        };
        assert!(visible(ToolFilter::Planning), "visible in Planning");
        assert!(visible(ToolFilter::Executing), "visible in Executing");
        assert!(visible(ToolFilter::ExecutingResearch), "visible in ExecutingResearch");
        assert!(visible(ToolFilter::Reviewing), "visible in Reviewing");
        assert!(visible(ToolFilter::Complete), "visible in Complete");
        assert!(
            visible(ToolFilter::Skill(vec![])),
            "visible inside a skill (always-available set)"
        );
        // A reviewer sees it only when its strict allow-list names it.
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Reviewer(vec!["backlog_list".into()]))
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(names.contains(&"backlog_list".to_string()));
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Reviewer(vec![]))
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(!names.contains(&"backlog_list".to_string()));
    }

    /// The Skill filter is an allow-list: only the named tools (+ all memory
    /// tools, `ask_user`, `current_plan`, and the backlog tools) are
    /// visible, regardless of category or safety.
    #[test]
    fn skill_filter_is_an_allow_list() {
        let r = registry();
        let caps = Capabilities::openai();
        let filter = ToolFilter::Skill(vec!["file_read".into(), "skill_end".into()]);
        let names: Vec<String> = r
            .schemas(&caps, &filter)
            .into_iter()
            .map(|s| s.name)
            .collect();
        // Only the allow-listed tools + memory tools.
        assert!(names.contains(&"file_read".to_string()));
        assert!(names.contains(&"skill_end".to_string()));
        assert!(names.contains(&"memory_recall".to_string())); // always
                                                               // Everything else is hidden.
        assert!(!names.contains(&"file_write".to_string()));
        assert!(!names.contains(&"create_plan".to_string()));
        assert!(!names.contains(&"skill_start".to_string()));
    }

    /// HARD RULE: a review report can only be authored by a spawned reviewer.
    /// `write_review_report` is visible ONLY under the constructor-granted
    /// [`ToolFilter::Reviewer`] allow-list (and only when the list names it) —
    /// every base state (Planning/Executing/Reviewing/Complete) and every
    /// Skill allow-list denies it, so the main agent can never author a code
    /// review through the schema, and the dispatch-time re-check (which calls
    /// the same `allows`) denies a hallucinated call too. This is the
    /// regression guard for the reviewer-only authorship rule.
    #[test]
    fn write_review_report_visible_only_under_reviewer_filter() {
        let r = registry();
        let caps = Capabilities::openai();
        // Schema-level: never advertised outside the reviewer allow-list.
        for filter in [
            ToolFilter::Planning,
            ToolFilter::Executing,
            ToolFilter::Reviewing,
            ToolFilter::Complete,
            ToolFilter::Skill(vec!["write_review_report".into()]),
        ] {
            let names: Vec<String> = r
                .schemas(&caps, &filter)
                .into_iter()
                .map(|s| s.name)
                .collect();
            assert!(
                !names.contains(&"write_review_report".to_string()),
                "write_review_report must be hidden under {filter:?} — even a skill that lists it"
            );
        }
        // Dispatch-level: allows() denies it under every non-reviewer filter,
        // regardless of category/safety (a hallucinated call cannot run).
        assert!(!ToolFilter::Planning.allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "write_review_report"
        ));
        assert!(!ToolFilter::Executing.allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "write_review_report"
        ));
        assert!(!ToolFilter::Reviewing.allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "write_review_report"
        ));
        assert!(!ToolFilter::Complete.allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "write_review_report"
        ));
        assert!(
            !ToolFilter::Skill(vec!["write_review_report".into()]).allows(
                ToolCategory::Agent,
                SafetyLevel::AutoRun,
                "write_review_report"
            )
        );
        // A reviewer allow-list naming it grants it (the constructor path).
        assert!(
            ToolFilter::Reviewer(vec!["write_review_report".into()]).allows(
                ToolCategory::Agent,
                SafetyLevel::AutoRun,
                "write_review_report"
            )
        );
        // …and only when the reviewer list actually names it.
        assert!(!ToolFilter::Reviewer(vec!["file_read".into()]).allows(
            ToolCategory::Agent,
            SafetyLevel::AutoRun,
            "write_review_report"
        ));
    }

    /// The Reviewer filter is a STRICT allow-list: only the named tools +
    /// current_plan are visible — no auto-granted memory tools, no ask_user,
    /// no backlog tools, no plan tools, no finish. This is the enforcement
    /// point of the reviewer contract (query, never mutate).
    #[test]
    fn reviewer_filter_is_a_strict_allow_list() {
        let r = registry();
        let caps = Capabilities::openai();
        let filter = ToolFilter::Reviewer(vec!["file_read".into(), "memory_recall".into()]);
        let names: Vec<String> = r
            .schemas(&caps, &filter)
            .into_iter()
            .map(|s| s.name)
            .collect();
        // Allow-listed tools are visible (current_plan's admission is covered
        // by reviewer_filter_grants_nothing_implicitly via allows(); the
        // registry stub here has no current_plan tool registered).
        assert!(names.contains(&"file_read".to_string()));
        assert!(names.contains(&"memory_recall".to_string()));
        // Mutations of ANY kind are never visible, even though Skill auto-grants
        // some of them and the plan-reviewing state normally allows others.
        assert!(!names.contains(&"memory_write".to_string()));
        assert!(!names.contains(&"memory_delete".to_string()));
        assert!(!names.contains(&"ask_user".to_string()));
        assert!(!names.contains(&"backlog_status".to_string()));
        assert!(!names.contains(&"backlog_add".to_string()));
        assert!(!names.contains(&"create_plan".to_string()));
        assert!(!names.contains(&"update_plan".to_string()));
        assert!(!names.contains(&"complete_step".to_string()));
        assert!(!names.contains(&"abandon_plan".to_string()));
        assert!(!names.contains(&"finish".to_string()));
        assert!(!names.contains(&"spawn_agent".to_string()));
        assert!(!names.contains(&"file_write".to_string()));
        assert!(!names.contains(&"shell".to_string()));
        assert!(!names.contains(&"git".to_string()));
        assert!(!names.contains(&"skill_start".to_string()));
    }

    /// The Reviewer filter's strictness is independent of category: a memory
    /// tool is visible ONLY when explicitly allow-listed (unlike Skill, which
    /// auto-grants every memory tool), and ask_user/backlog_status — which
    /// Skill auto-grants (with current_plan and the other backlog tools)
    /// too — stay hidden even under an empty reviewer list.
    #[test]
    fn reviewer_filter_grants_nothing_implicitly() {
        let filter = ToolFilter::Reviewer(vec![]);
        // Memory tools are NOT auto-granted under Reviewer.
        assert!(!filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_recall"));
        assert!(!filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_write"));
        // The Skill auto-grant set stays hidden for a reviewer.
        assert!(!filter.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "ask_user"));
        assert!(!filter.allows(
            ToolCategory::Workflow,
            SafetyLevel::AutoRun,
            "backlog_status"
        ));
        // Only current_plan rides along (read-only orientation).
        assert!(filter.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, "current_plan"));
        // An explicitly listed tool is visible regardless of category.
        let filter = ToolFilter::Reviewer(vec!["memory_recall".into()]);
        assert!(filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_recall"));
        assert!(!filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, "memory_list"));
    }

    /// schemas() must stop unconditionally including Memory tools: under
    /// Reviewer the schema set equals the allow-list, while every other arm
    /// (Skill included) still admits all memory tools — the regression guard
    /// for the memory-leak fix.
    #[test]
    fn schemas_reviewer_excludes_memory_mutations() {
        let r = registry();
        let caps = Capabilities::openai();
        let reviewer: Vec<String> = r
            .schemas(&caps, &ToolFilter::Reviewer(vec!["file_read".into()]))
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(reviewer.contains(&"file_read".to_string()));
        assert!(!reviewer.contains(&"memory_write".to_string()));
        assert!(!reviewer.contains(&"memory_recall".to_string()));
        // Regression: the base-state arms still include memory tools.
        let executing: Vec<String> = r
            .schemas(&caps, &ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(executing.contains(&"memory_write".to_string()));
        let skill: Vec<String> = r
            .schemas(&caps, &ToolFilter::Skill(vec![]))
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(skill.contains(&"memory_recall".to_string()));
    }

    #[test]
    fn strict_only_when_caps_allow() {
        let r = registry();
        let caps_openai = Capabilities::openai();
        let caps_local = Capabilities::local();
        let openai_schemas = r.schemas(&caps_openai, &ToolFilter::Executing);
        let local_schemas = r.schemas(&caps_local, &ToolFilter::Executing);
        assert!(openai_schemas.iter().all(|s| s.strict == Some(true)));
        assert!(local_schemas.iter().all(|s| s.strict.is_none()));
    }

    #[test]
    fn schemas_semantic_tools_first_then_stable() {
        // The tools array must be byte-stable across restarts for provider
        // prompt-cache prefix stability — but the cache law only needs
        // DETERMINISM, so the order is (priority class, name): the underused
        // semantic/knowledge tools (memory + code graph + search) sort first
        // for primacy at the tool-selection point, alphabetical within each
        // class.
        let r = registry();
        let caps = Capabilities::openai();
        let names: Vec<String> = r
            .schemas(&caps, &ToolFilter::Executing)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(
            names.len() > 1,
            "test is meaningful only with multiple tools"
        );
        // Deterministic: the Vec equals its own (class, name)-sorted clone.
        let mut expected = names.clone();
        expected.sort_by_key(|n| (priority_class(n), n.clone()));
        assert_eq!(names, expected, "schemas must be sorted by (class, name)");
        // The registry's memory tool (class 0) precedes every class-1 tool
        // even though it is not alphabetically first.
        let first_memory = names
            .iter()
            .position(|n| n.starts_with("memory_"))
            .expect("registry has a memory tool");
        let first_class1 = names
            .iter()
            .position(|n| priority_class(n) == 1)
            .expect("registry has class-1 tools");
        assert!(
            first_memory < first_class1,
            "semantic tools must sort before class-1 tools: {names:?}"
        );
        let first_class2 = names
            .iter()
            .position(|n| priority_class(n) == 2)
            .expect("registry has class-2 tools");
        assert!(
            first_class1 < first_class2,
            "class-1 base tools must sort before class-2 state-dependent tools: {names:?}"
        );
    }

    #[test]
    fn priority_class_covers_exactly_the_semantic_tools() {
        // Guard against a typo'd entry silently demoting a production tool to
        // class 1: pin the full class-0 set directly (the stub registry only
        // carries memory_recall, so the ordering test above can't catch a
        // misspelled graph_*/search* entry). Names verified against the real
        // registrations in codegraph.rs / memory/mod.rs / search.rs /
        // search_read.rs.
        const CLASS0: &[&str] = &[
            "graph_context",
            "graph_impact",
            "graph_path",
            "graph_search",
            "memory_consolidate",
            "memory_recall",
            "memory_write",
            "search",
            "search_read",
        ];
        for name in CLASS0 {
            assert_eq!(priority_class(name), 0, "{name} must be class 0");
        }
        assert_eq!(priority_class("file_read"), 1, "universal read tools are class 1");
        assert_eq!(priority_class("read_files"), 1);
        assert_eq!(priority_class("ask_user"), 1);
        assert_eq!(priority_class("backlog_list"), 1);
        assert_eq!(priority_class("backlog_add"), 2, "state-dependent mutation tools are class 2");
        assert_eq!(priority_class("backlog_status"), 2);
        assert_eq!(priority_class("load_tools"), 2);
        assert_eq!(priority_class("shell"), 2);
        assert_eq!(priority_class("file_write"), 2);
        assert_eq!(priority_class("complete_step"), 2);
    }

    #[test]
    fn state_transition_preserves_class0_and_class1_prefix() {
        // The core motivation of 3-tier ordering: transitioning from Planning to
        // Executing to Reviewing only appends Class 2 mutation tools, leaving the
        // Class 0 + Class 1 prefix 100% byte-stable in the LLM tools array.
        let r = registry();
        let caps = Capabilities::openai();
        let planning_tools: Vec<String> = r
            .schemas(&caps, &ToolFilter::Planning)
            .into_iter()
            .filter(|s| priority_class(&s.name) < 2)
            .map(|s| s.name)
            .collect();
        let executing_tools: Vec<String> = r
            .schemas(&caps, &ToolFilter::Executing)
            .into_iter()
            .filter(|s| priority_class(&s.name) < 2)
            .map(|s| s.name)
            .collect();
        let reviewing_tools: Vec<String> = r
            .schemas(&caps, &ToolFilter::Reviewing)
            .into_iter()
            .filter(|s| priority_class(&s.name) < 2)
            .map(|s| s.name)
            .collect();
        assert_eq!(
            planning_tools, executing_tools,
            "Class 0 + Class 1 tools must be identical across Planning and Executing"
        );
        assert_eq!(
            executing_tools, reviewing_tools,
            "Class 0 + Class 1 tools must be identical across Executing and Reviewing"
        );
    }

    #[tokio::test]
    async fn dispatch_known_tool() {
        let r = registry();
        let call = ToolCall {
            id: "1".into(),
            name: "file_read".into(),
            arguments: serde_json::json!({}),
        };
        let result = r.dispatch(&call).await;
        assert!(result.success);
        assert_eq!(result.output, "contents");
    }

    #[tokio::test]
    async fn dispatch_unknown_tool() {
        let r = registry();
        let call = ToolCall {
            id: "1".into(),
            name: "nonexistent".into(),
            arguments: serde_json::json!({}),
        };
        let result = r.dispatch(&call).await;
        assert!(!result.success);
        assert!(result.output.contains("unknown tool"));
        assert!(result.output.contains("file_read")); // lists available
    }
}
