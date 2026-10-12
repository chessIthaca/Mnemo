// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel→app seam ([`AgentPolicy`]): the app's `WorkflowPolicy`
//! (src/app/workflow_policy.rs, 2a.2+) implements it by wrapping today's
//! `Workflow`; the kernel default [`NoPolicy`] is the free agent
//! (master-spec :200-202). Added by job 2a; the implementation moves under
//! `mnemo::app` in job 2b.

use async_trait::async_trait;
use serde_json::Value;

use crate::project::Constitution;
use crate::provider::Capabilities;
use crate::tool::{ToolFilter, ToolRegistry, ToolResult};

/// The per-call tool surface (master-spec :213-214): the advertised
/// (schema-array) filter and the dispatch-gate filter.
#[derive(Debug, Clone)]
pub struct ToolSurface {
    /// The tools the provider sees this turn (the schema array filter).
    pub advertise: Option<ToolFilter>,
    /// The dispatch gate filter (the per-call allow check).
    pub gate: Option<ToolFilter>,
}

impl ToolSurface {
    /// The constant, stateless surface (the default; NoPolicy's answer).
    pub fn constant() -> Self {
        Self {
            advertise: None,
            gate: None,
        }
    }
}

/// Per-call veto beyond the filter (master-spec :215-217): the plan-mutation
/// gate and the research write verdict move into `WorkflowPolicy` in 2b/2c.
/// Default: [`GateVerdict::Allow`].
#[derive(Debug, Clone)]
pub enum GateVerdict {
    /// The call proceeds (subject to the surface filter).
    Allow,
    /// The call is refused with this reason (dispatch surfaces it as an
    /// error).
    Block { reason: String },
}

/// Prompt-intake result (master-spec :220-222): the unattended-marker read
/// and an optional volatile-tail rewrite (the Complete-state plan nudge).
/// Default no-op.
#[derive(Debug, Clone, Default)]
pub struct PromptHook {
    /// `Some(v)` SETS the unattended flag outright (prefix read /
    /// plain-prompt clear). `None` leaves the runtime's own derivation
    /// untouched (2a.5 moves the reads across one arm at a time).
    pub unattended: Option<bool>,
    /// `Some(text)` replaces the synthesized Linux/newline-safe prompt tail
    /// (the plan nudge rides this hook from 2a.5).
    pub tail_rewrite: Option<String>,
}

/// Turn-end verdict (master-spec :224-225): `Stop` parks for the user;
/// `Continue` runs another turn, optionally with a synthetic note (today's
/// auto-continue message).
#[derive(Debug, Clone)]
pub enum Continuation {
    /// Park (the default for `NoPolicy`).
    Stop,
    /// Run another turn; `note` is the synthetic user message, `None` = none.
    Continue { note: Option<String> },
}

/// The turn outcome the gate needs (master-spec :225). Kernel-built from the
/// turn result before the hook runs.
#[derive(Debug, Clone)]
pub struct TurnSummary {
    /// `true` for `Finish::Stop` without error, per the emit gate.
    pub success: bool,
    /// The stop reason the runtime observed — `None` = the plain
    /// `Finish::Stop` end (the gate's None arm), `Some(v)` = the special
    /// stop the runtime folded (Steer/Interrupt/Cancel/Compact/
    /// CompactWithSteers/Clear; the enum has NO plain-Stop variant,
    /// verified at the 2a.5 gate).
    pub stop_reason: Option<crate::agent::StopReason>,
    /// The agent's unattended flag at turn end (the gate's own field,
    /// carried so the policy's predicate can run its
    /// `unattended :: Planning` arm — workflow_expects_progress's second
    /// condition, 2a.5).
    pub unattended: bool,
}

/// Opaque tags stamped on Spend/Savings rows (master-spec :226-227).
#[derive(Debug, Clone, Default)]
pub struct SpendTags {
    pub plan_id: Option<String>,
    pub step_index: Option<usize>,
}

/// Role allow-list specs for `spawn_agent` (master-spec :228-229).
#[derive(Debug, Clone)]
pub struct RoleSpec {
    pub role: String,
}

/// Routing key (master-spec :218-219): the ordered config slot names +
/// skill name the resolver walks, then default. `WorkflowPolicy` emits exactly today's
/// chain (the golden table test, 2a.0); `NoPolicy` emits `["free"]` →
/// `[models.free]` (routing contract :241-246).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteKey {
    /// Ordered slot names, e.g. `["bug_fixing", "executing"]`.
    pub slots: Vec<String>,
}

/// Build context handed to [`AgentPolicy::register_tools`] (master-spec
/// :207).
pub struct PolicyBuildCtx {
    pub agent_id: crate::runtime::channels::AgentId,
    pub project_root: std::path::PathBuf,
    pub plans_dir: std::path::PathBuf,
}

/// The kernel's six compiled prompt blocks (prompt.rs consts), handed to the
/// policy so `stable_head` can stay byte-identical (cache contract :237-240).
/// The app wrapper supplies `KernelPromptBlocks::workflow()`; job 2b relocates
/// the app halves and trims the kernel consts to KERNEL_PREAMBLE +
/// KERNEL_RULES.
pub struct KernelPromptBlocks {
    pub preamble: &'static str,
    pub lifecycle: &'static str,
    pub rules: &'static str,
    pub discipline: &'static str,
    pub strategy: &'static str,
    pub memory_records: &'static str,
}

impl KernelPromptBlocks {
    /// Today's full const set (byte-identical to the pre-2a head).
    ///
    /// Delegates to prompt.rs's `kernel_prompt_blocks` — the consts are
    /// file-private there (E0603), and 2b trims them to the kernel halves
    /// anyway.
    pub fn workflow() -> Self {
        crate::agent::prompt::kernel_prompt_blocks()
    }
}

/// The kernel→app policy seam (master-spec :204-231 verbatim).
#[async_trait]
pub trait AgentPolicy: Send + Sync {
    /// Called once at build: register app tools (plan/backlog/skill/review).
    fn register_tools(&self, reg: &mut ToolRegistry, ctx: &PolicyBuildCtx);

    /// The stable head. Default = kernel head (KERNEL_PREAMBLE +
    /// KERNEL_RULES + TOOL_CALL_DISCIPLINE + TOOL_STRATEGY +
    /// MEMORY_RECORDS + constitution).
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String;

    /// The policy's part of the volatile tail (kernel adds recall +
    /// CAPABILITIES). Default: no app part.
    async fn volatile_tail(&self, caps: &Capabilities) -> String;

    /// Advertised tool array filter + dispatch gate filter.
    async fn tool_surface(&self) -> ToolSurface;

    /// Per-call veto/redirect beyond the filter (plan-mutation gate,
    /// research write verdict). Default Allow.
    async fn gate_call(&self, name: &str, args: &Value) -> GateVerdict;

    /// Routing key for this request: ordered config slot names + skill
    /// name. Default: `["free"]`.
    async fn route(&self) -> RouteKey;

    /// Prompt intake: may rewrite the volatile tail (plan nudge), read
    /// markers (unattended prefix). Default no-op.
    async fn on_prompt(&self, text: &str) -> PromptHook;

    /// After a tool call completes (steering/stats hooks). Default no-op.
    async fn after_tool(&self, name: &str, result: &ToolResult);

    /// Turn end: Stop, or Continue { note } (auto-continue). Default Stop.
    async fn on_turn_end(&self, t: &TurnSummary) -> Continuation;

    /// Opaque tags stamped on Spend/Savings rows (plan_id, step_index).
    async fn spend_tags(&self) -> SpendTags;

    /// Role allow-lists for spawn_agent (reviewer etc.). Default: none.
    fn spawn_roles(&self) -> &[RoleSpec];

    /// The agent's task is terminating (engine/session teardown, 3b).
    /// Default no-op.
    async fn on_session_end(&self);
}

/// The kernel default — the free agent (master-spec :200-202). Everything
/// no-op/constant here; the free-agent behaviors land in job 2c.
pub struct NoPolicy;

#[async_trait]
impl AgentPolicy for NoPolicy {
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {}
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        assemble_stable_head(k, c)
    }
    async fn volatile_tail(&self, _caps: &Capabilities) -> String {
        String::new()
    }
    async fn tool_surface(&self) -> ToolSurface {
        ToolSurface::constant()
    }
    async fn gate_call(&self, _name: &str, _args: &Value) -> GateVerdict {
        GateVerdict::Allow
    }
    async fn route(&self) -> RouteKey {
        RouteKey {
            slots: vec!["free".to_string()],
        }
    }
    async fn on_prompt(&self, _text: &str) -> PromptHook {
        PromptHook::default()
    }
    async fn after_tool(&self, _name: &str, _result: &ToolResult) {}
    async fn on_turn_end(&self, _t: &TurnSummary) -> Continuation {
        Continuation::Stop
    }
    async fn spend_tags(&self) -> SpendTags {
        SpendTags::default()
    }
    fn spawn_roles(&self) -> &[RoleSpec] {
        &[]
    }
    async fn on_session_end(&self) {}
}

/// The byte-stable head assembly — ONE body, called by `build_stable_head`
/// (kernel default) and both policy impls. Byte-faithful to prompt.rs's
/// original body (:505-528 pre-2a.1): `push_section`'s empty-skip
/// semantics (skip if trim-empty, push RAW bytes + "\n\n") and the two
/// raw constitution pushes — NO trims on the pushed bytes. The 2a.0
/// golden (prompt::tests::workflow_stable_head_bytes_unchanged) gates it.
pub fn assemble_stable_head(k: &KernelPromptBlocks, c: &Constitution) -> String {
    let mut out = String::new();
    for block in [
        k.preamble,
        k.lifecycle,
        k.rules,
        k.discipline,
        k.strategy,
        k.memory_records,
    ] {
        if !block.trim().is_empty() {
            out.push_str(block);
            out.push_str("\n\n");
        }
    }
    // Constitution — global first, then project (trim-empty check only;
    // the pushed bytes are the raw field contents, exactly as the
    // original body did).
    if !c.global.trim().is_empty() {
        out.push_str("# HARD RULES — GLOBAL (agent.md)\n\n");
        out.push_str(&c.global);
        out.push_str("\n\n");
    }
    if !c.project.trim().is_empty() {
        out.push_str("# HARD RULES — PROJECT (agent.md)\n\n");
        out.push_str(&c.project);
        out.push_str("\n\n");
    }
    out
}
