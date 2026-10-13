// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel→app seam ([`AgentPolicy`]): the app's `WorkflowPolicy`
//! (src/app/workflow_policy.rs, 2a.2+) implements it by wrapping today's
//! `Workflow`; the kernel default [`NoPolicy`] is the free agent
//! (master-spec :200-202). Added by job 2a; the implementation moves under
//! `mnemo::app` in job 2b.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::agent::prompt::PromptSpec;
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

/// Per-call veto beyond the filter (master-spec :215-217). Consulted by the
/// dispatch funnel BEFORE the approval branch (3c): `Deny` refuses the call
/// outright (even under `OverridePermissions` — a veto is not an approval
/// skip), `RequireApproval` forces the approval prompt (the bypass branch
/// keeps its semantics under `OverridePermissions` — R3), `Allow` proceeds
/// through the normal safety/approval machinery. Default:
/// [`GateVerdict::Allow`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateVerdict {
    /// The call proceeds (subject to the surface filter).
    Allow,
    /// The call is refused with this message (dispatch surfaces it as the
    /// tool-call error result).
    Deny {
        /// The refusal the model sees.
        message: String,
    },
    /// The call must go through the approval prompt — dispatch forces the
    /// gated branch. Under `OverridePermissions` the existing bypass branch
    /// applies (`ApprovalBypassed` audit + proceed).
    RequireApproval,
}

/// The per-agent approval policy (2c.5, §5.4 :356-378): a PER-LOOP
/// setting, not a new `SafetyMode` variant — `config.toml` and the GUI
/// Settings stay unchanged, so the GUI cannot silently enter a
/// no-approval-for-push mode.
///
/// - [`PermissionPolicy::OverridePermissions`] (the kernel default, the
///   free agent): dispatch skips the whole approval branch — no
///   `AgentEvent::ApprovalRequest` is ever emitted, `never_auto_for`
///   (`force_prompt`) included. Each call that *would* have been gated
///   emits `AgentEvent::ApprovalBypassed` first (an audit trail), then
///   dispatches directly. `SafetyRules` is not consulted (it is an
///   auto-approve allow-list — nothing to do when nothing is gated).
/// - [`PermissionPolicy::ApproveViaApi`] (what the factory wires for
///   GUI/console loops): today's semantics exactly — the live
///   safety-mode read + `SafetyRules` auto-approve + `never_auto_for`.
///   The payload is the declared INITIAL mode (the serialization
///   surface); dispatch keeps reading the live handle (the runtime
///   toggle stays authoritative).
///
/// Subagents inherit the parent's policy (§5.4 :377): the `spawn_agent`
/// tool carries its parent loop's policy and passes it through the
/// spawner call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "mode")]
pub enum PermissionPolicy {
    /// Skip the approval branch entirely; gated calls emit
    /// `AgentEvent::ApprovalBypassed` and dispatch directly.
    #[default]
    OverridePermissions,
    /// Today's semantics: approvals surface as `ApprovalRequest` and are
    /// answered via the API (`SessionCommand::Approve`).
    ApproveViaApi {
        /// The safety mode the loop starts under (the GUI/console path
        /// passes the factory's shared mode at build).
        safety_mode: crate::config::SafetyMode,
    },
}

/// Prompt-intake result (master-spec :220-222): the unattended-marker read
/// and an optional volatile-tail rewrite (the Complete-state plan nudge).
/// Default no-op. Serde since 3c — carried by the external policy's
/// [`PolicyAnswer::Prompt`](crate::runtime::channels::PolicyAnswer) arm.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
/// auto-continue message). Serde since 3c — carried by the external
/// policy's [`PolicyAnswer::Continuation`]
/// (crate::runtime::channels::PolicyAnswer) arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
/// :207). Extended in 2b.3: the registrations the app factory owns (the
/// plan/skill tools) need the per-agent bits — the root spec, the resolved
/// sandbox, the step-verify handle (derived kernel-side from the root),
/// and the agent's own workflow handle (recovered app→app via
/// `as_any_arc`; every kernel consumer rides the trait's opaque
/// representations).
pub struct PolicyBuildCtx {
    pub agent_id: Option<crate::runtime::AgentId>,
    pub project_root: std::path::PathBuf,
    pub plans_dir: std::path::PathBuf,
    /// The agent's root spec (a worktree agent) — a kernel type, legal here.
    pub root: Option<crate::agent::factory::AgentRootSpec>,
    /// The agent's sandbox (the root-spec's or the factory's).
    pub sandbox: crate::tool::agent::sandbox::Sandbox,
    /// The step-verify handle derived from the root (the kernel keeps the
    /// derivation; only the derived handle crosses).
    pub step_verify: crate::agent::step_verify::StepVerifyHandle,
    /// The agent's own workflow handle.
    pub workflow: Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
}

/// The build context handed to [`AgentPolicyFactory::build_policy`] (2b.2;
/// the workflow field typed via the neutral handle trait in 2b.3 — the
/// kernel passes the same `Arc<dyn AgentWorkflowHandle>` it stores on the
/// loop, and the app side recovers its concrete lockable workflow through
/// `as_any_arc`, an app→app cast).
pub struct PolicyFactoryCtx {
    pub agent_id: Option<crate::runtime::AgentId>,
    pub project_root: std::path::PathBuf,
    pub plans_dir: std::path::PathBuf,
    /// The loop's workflow handle (kernel-typed via the neutral trait —
    /// 2b.3); the app side recovers its concrete, lockable Arc through
    /// `as_any_arc`.
    pub workflow: Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>,
    /// The caller's prompt spec (2c.1/G1, §5.9.1). `None` today — the
    /// Engine's SpawnOptions supplies it (3a); a wired factory building a
    /// spec-free free agent errors with the `prompt_required` code.
    pub prompt: Option<PromptSpec>,
}

/// The kernel errors a policy-factory build can fail with (2c.1). The
/// Engine's spawn surface (3a) maps these onto its own error with the
/// spec's stable codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// A `NoPolicy` build (§5.9.1) without a caller-supplied
    /// [`PromptSpec`](crate::agent::prompt::PromptSpec) — the free agent
    /// never silently defaults to a coding identity (G1).
    PromptRequired,
}

impl KernelError {
    /// The stable error code (`prompt_required`, §5.9.1 :630).
    pub fn code(&self) -> &'static str {
        match self {
            KernelError::PromptRequired => "prompt_required",
        }
    }
}

/// What [`AgentPolicyFactory::build_policy`] hands back: the policy the
/// kernel stores on the loop, plus the loop-attach hook the build's tail
/// invokes once the loop is Arc-wrapped (2a.6's back-ref). Packaging the
/// attach as a `FnOnce` keeps the concrete policy type entirely app-side
/// — the kernel never names it.
pub struct PolicyBuildResult {
    pub policy: Arc<dyn AgentPolicy>,
    /// The post-Arc back-ref attach (a no-op for [`NoPolicy`]-shaped
    /// policies that carry no loop facts).
    pub attach_loop: Box<dyn FnOnce(std::sync::Weak<crate::agent::AgentLoop>) + Send>,
}

/// The app-side policy factory (2b.2; master-spec :266-269: "The factory
/// takes `Arc<dyn PolicyFactory>`"). The kernel factory consumes this to
/// build each agent's policy and to let the app register its tools (D5:
/// kernel-called, app-owned). The app's concrete impl carries its wiring
/// setters (`set_backlog`/`set_backlog_notifier`) as inherent methods so
/// the kernel never names the app's wiring types at the trait. The kernel
/// default is [`NoPolicyFactory`] — production and the kernel test helpers
/// wire the app implementation at startup/factory-help time.
pub trait AgentPolicyFactory: Send + Sync {
    /// Create the agent's OWN workflow (2b.3): the kernel hands the plans
    /// dir + whether this agent is a subagent (the Subagent state stamp is
    /// an app-side fact), and receives the neutral handle. The kernel never
    /// constructs the app workflow type itself — the workflow lives app-side
    /// from construction on.
    fn create_workflow(
        &self,
        plans_dir: &std::path::Path,
        is_subagent: bool,
    ) -> Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle>;

    /// Build the policy wrapping the agent's own workflow (the opaque
    /// handle in `ctx`). Called once per built agent, before the final
    /// Arc-wrap; the returned `attach_loop` hook is invoked at the build's
    /// tail. MAY REFUSE (2c.1/G1): a [`NoPolicy`]-semantics factory without
    /// the caller's [`PromptSpec`] errors with
    /// [`KernelError::PromptRequired`] (§5.9.1 :629-630 — never a silent
    /// coding default). The CONSUMER owns the refusal policy (review F2,
    /// 3c.6): the infallible 2b build falls back to the explicit coding
    /// default (`NoPolicy::with_coding_default()`, the GATED form), and
    /// the engine's tier-1 registry (`Engine::register_policy`, headless)
    /// documents the same contract for registered factories.
    fn build_policy(
        &self,
        ctx: &PolicyFactoryCtx,
    ) -> Result<PolicyBuildResult, KernelError>;

    /// Register app-side tools onto the agent's registry (D5). 2b.2's move
    /// covers the param-free and wiring-only tools (ask_user, the backlog
    /// trio); the plan/skill registrations stay kernel-side until 2b.3
    /// carries their workflow/skill dependencies across the seam.
    fn register_tools(&self, reg: &mut ToolRegistry, ctx: &PolicyBuildCtx);
}

/// The kernel-default policy factory: builds [`NoPolicy`] policies and
/// registers nothing. Built agents' consults read the NoPolicy defaults
/// (the free agent). Used when no app factory is wired (tests that never
/// needed one).
pub struct NoPolicyFactory;

impl AgentPolicyFactory for NoPolicyFactory {
    fn create_workflow(
        &self,
        _plans_dir: &std::path::Path,
        _is_subagent: bool,
    ) -> Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle> {
        Arc::new(crate::agent::workflow_handle::UnwiredWorkflow)
    }

    /// Build the free agent's policy — enforcing G1 (§5.9.1 :629-630): the
    /// caller's PromptSpec is REQUIRED; without one the build errors with
    /// the `prompt_required` code, never a silent coding default.
    fn build_policy(
        &self,
        ctx: &PolicyFactoryCtx,
    ) -> Result<PolicyBuildResult, KernelError> {
        let spec = ctx
            .prompt
            .clone()
            .ok_or(KernelError::PromptRequired)
            .map(Arc::new)?;
        Ok(PolicyBuildResult {
            policy: Arc::new(NoPolicy::with_spec(spec)),
            attach_loop: Box::new(|_| {}),
        })
    }

    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {}
}

/// 3a.5 (D5): the ENGINE's policy factory — [`NoPolicy::ungated`]:
/// the free agent's real dispatch semantics (row 11's engine form: the
/// workflow state never gates the engine's sessions). Registered by the
/// headless Engine on ITS factory; [`NoPolicyFactory`] keeps the
/// workflow fallback everywhere else.
pub struct FreePolicyFactory;

impl AgentPolicyFactory for FreePolicyFactory {
    fn create_workflow(
        &self,
        _plans_dir: &std::path::Path,
        _is_subagent: bool,
    ) -> Arc<dyn crate::agent::workflow_handle::AgentWorkflowHandle> {
        Arc::new(crate::agent::workflow_handle::UnwiredWorkflow)
    }

    fn build_policy(
        &self,
        ctx: &PolicyFactoryCtx,
    ) -> Result<PolicyBuildResult, KernelError> {
        // The engine's config may carry no free spec (an empty config
        // dir): fall back to the coding-default head IN THE FACTORY —
        // the factory-level fallback (factory.rs :1194) attaches the
        // NON-ungated default, silently re-gating the engine's agents.
        let spec = ctx.prompt.clone().map(Arc::new).unwrap_or_else(|| {
            Arc::new(crate::agent::prompt::PromptSpec::coding_default())
        });
        Ok(PolicyBuildResult {
            policy: Arc::new(NoPolicy::with_spec(spec).ungated()),
            attach_loop: Box::new(|_| {}),
        })
    }

    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {}
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

/// The kernel default — the free agent (§5.2 :200-202): no workflow, no
/// plan tools, the `"free"` route, the constant surface (2a). 2c.1/G1:
/// its stable head composes from the CALLER's
/// [`PromptSpec`](crate::agent::prompt::PromptSpec) — a spec-free
/// `NoPolicy` cannot exist (the factory errors with `KernelError`
/// ::PromptRequired instead, never a silent coding default).
pub struct NoPolicy {
    /// The caller-supplied prompt spec (the head's recipe).
    spec: Arc<PromptSpec>,
    /// 3a.5 (D5): the ENGINE's sessions — the dispatch gate answers
    /// [`ToolFilter::Free`] (the workflow state never gates the agent).
    /// Default `false`: a plain NoPolicy build keeps the dispatch
    /// fallback to the workflow read (the kernel/tests' pre-3a.5
    /// semantics — workflow agents without an attached policy).
    ungated: bool,
}

impl NoPolicy {
    /// Build the free-agent policy over a caller-supplied spec (G1).
    pub fn with_spec(spec: Arc<PromptSpec>) -> Self {
        Self { spec, ungated: false }
    }

    /// The kernel's explicit fallback — the loop's bare default and the
    /// factory's unwired arm: the mnemo coding default (the console/GUI
    /// free mode's spec, §5.9.1 :631-632), never a silently-assembled
    /// coding head. The Engine spawn path (3a) errors without the
    /// caller's spec (`KernelError::PromptRequired`).
    pub fn with_coding_default() -> Self {
        Self {
            spec: Arc::new(crate::agent::prompt::PromptSpec::coding_default()),
            ungated: false,
        }
    }

    /// 3a.5 (D5, row 11's engine form): the UNGATED variant — the
    /// dispatch gate answers [`ToolFilter::Free`], so the workflow state
    /// never gates this agent's tools. The engine's FreePolicyFactory
    /// builds this; every other NoPolicy build keeps the fallback.
    pub fn ungated(mut self) -> Self {
        self.ungated = true;
        self
    }
}

#[async_trait]
impl AgentPolicy for NoPolicy {
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {}
    fn stable_head(&self, _k: &KernelPromptBlocks, c: &Constitution) -> String {
        // 2c.1/G1 (§5.9.1 :626-627): the free head composes from the
        // caller's spec — identity, the selected blocks, extra_stable,
        // constitution (the spec's mode filters the loaded sources) — NOT
        // the workflow block set. The 2a stub mirrored the workflow head;
        // the 2a.0 byte-golden pins the WORKFLOW path only.
        crate::agent::prompt::assemble_spec_head(&self.spec, c)
    }
    async fn volatile_tail(&self, _caps: &Capabilities) -> String {
        String::new()
    }
    async fn tool_surface(&self) -> ToolSurface {
        if self.ungated {
            // 3a.5: the free agent's gate — a `gate: None` answer falls
            // back to the workflow read at dispatch (dispatch.rs
            // :255-258), which refuses mutation tools in Complete
            // state. The workflow state must NEVER gate the engine's
            // free sessions (row 11's engine form).
            ToolSurface {
                advertise: None,
                gate: Some(ToolFilter::Free),
            }
        } else {
            ToolSurface::constant()
        }
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
    // original body did). The section pushes are shared with the free
    // agent's spec assembler (2c.1) — one body, byte-pinned by the
    // 2a.0 golden.
    push_constitution_sections(&mut out, c);
    out
}

/// The constitution section pushes, shared by the two head assemblers
/// (`assemble_stable_head`, the 2a.0 golden's body, and prompt.rs's
/// `assemble_spec_head`, the free agent's). Byte-pinned: the `# HARD
/// RULES` headers + the raw field content + the blank-line separators
/// must never drift.
pub fn push_constitution_sections(out: &mut String, c: &Constitution) {
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
}
#[cfg(test)]
mod tests {
    use super::*;

    /// 2c.1/G1 (row 25's kernel form): a NoPolicy factory build WITHOUT a
    /// caller-supplied PromptSpec errors with the spec's stable code —
    /// never a silent coding default.
    #[test]
    fn no_policy_requires_identity() {
        let ctx = PolicyFactoryCtx {
            agent_id: None,
            project_root: std::path::PathBuf::from("/tmp/proj"),
            plans_dir: std::path::PathBuf::from("/tmp/proj/.coding/plans"),
            prompt: None,
            workflow: Arc::new(crate::agent::workflow_handle::UnwiredWorkflow),
        };
        let Err(error) = NoPolicyFactory.build_policy(&ctx) else {
            panic!("a spec-free NoPolicy build must refuse");
        };
        assert_eq!(error.code(), "prompt_required");
    }

    /// 2c.1/G1: WITH the spec the factory builds a policy whose head
    /// composes from the spec (identity first; no workflow text) and whose
    /// constitution mode is the spec's (the assembler's filter).
    #[test]
    fn no_policy_with_spec_builds_spec_head() {
        let ctx = PolicyFactoryCtx {
            agent_id: None,
            project_root: std::path::PathBuf::from("/tmp/proj"),
            plans_dir: std::path::PathBuf::from("/tmp/proj/.coding/plans"),
            prompt: Some(crate::agent::prompt::PromptSpec::coding_default()),
            workflow: Arc::new(crate::agent::workflow_handle::UnwiredWorkflow),
        };
        let built = NoPolicyFactory.build_policy(&ctx).unwrap();
        let head = built
            .policy
            .stable_head(&KernelPromptBlocks::workflow(), &Constitution::default());
        assert!(
            head.starts_with(crate::agent::prompt::KERNEL_CODING_DEFAULT_IDENTITY),
            "the free head starts with the spec's identity"
        );
        assert!(
            !head.contains("WORKFLOW LIFECYCLE") && !head.contains("APP RULES"),
            "the free head carries no workflow text"
        );
    }

    /// 2c.5 (§5.4 :358-359): the serde shape — internally tagged "mode",
    /// the kernel default `OverridePermissions`.
    #[test]
    fn permission_policy_serde_shape_and_default() {
        assert_eq!(
            serde_json::to_value(PermissionPolicy::default()).unwrap(),
            serde_json::json!({"mode": "OverridePermissions"})
        );
        let approve: PermissionPolicy = serde_json::from_value(serde_json::json!({
            "mode": "ApproveViaApi",
            "safety_mode": "autonomous"
        }))
        .unwrap();
        assert_eq!(
            approve,
            PermissionPolicy::ApproveViaApi {
                safety_mode: crate::config::SafetyMode::Autonomous
            }
        );
        assert_eq!(
            serde_json::to_value(&approve).unwrap(),
            serde_json::json!({"mode": "ApproveViaApi", "safety_mode": "autonomous"})
        );
    }
}
// ---------------------------------------------------------------------------
// 3c: G6 tier 3 — the external policy (master-spec §5.9.6 :697-716)
// ---------------------------------------------------------------------------

/// What an unanswered (or mismatched-form) hook resolves to. The
/// per-kind mapping is documented on [`ExternalPolicy`]'s hook arms:
/// the gate-ish hooks honor Allow (proceed) / Deny (veto); the turn-end
/// hook ALWAYS parks on a timeout — an auto-continue is an explicit act,
/// never a timeout default (a silent infinite loop is a footgun no
/// caller should inherit from a broken host). `Stop` is the turn-end
/// verb and a no-op at the gates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutAction {
    /// Proceed (or the kind's neutral default).
    #[default]
    Allow,
    /// Veto — meaningful at the gate; inert where the kind has no deny
    /// form (post-hoc observations, prompt rewrites, teardown).
    Deny,
    /// Park the turn end — the turn-end verb; a no-op at the gates.
    Stop,
}

/// The 3c default for [`ExternalPolicySpec::timeout_ms`]: 120 s (§5.9.6
/// "with the timeout default applied" — matching the host-tool default).
pub fn external_policy_default_timeout_ms() -> u64 {
    120_000
}

/// The tier-3 external-policy spec (G6, §5.9.6) — pure wire data: which
/// hooks fire, how long the kernel waits per hook, and what an
/// unanswered hook resolves to. The HOST implements the policy; the
/// kernel only emits `AgentEvent::PolicyHook` and awaits the host's
/// `PolicyAnswer` — the same request/response shape as approvals,
/// questions, and host tools, so an orchestrator in any language can
/// gate calls (R1: gate-only — the spec can never change the tool
/// array mid-session).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalPolicySpec {
    /// The hooks that fire; each selected kind emits at its site, an
    /// unselected kind keeps the base policy's behavior.
    pub hooks: Vec<crate::runtime::channels::HookKind>,
    /// The per-hook await timeout in milliseconds (default 120 s).
    #[serde(default = "external_policy_default_timeout_ms")]
    pub timeout_ms: u64,
    /// What an unanswered hook resolves to (default Allow).
    #[serde(default)]
    pub on_timeout: TimeoutAction,
}

impl Default for ExternalPolicySpec {
    fn default() -> Self {
        Self {
            hooks: Vec::new(),
            timeout_ms: external_policy_default_timeout_ms(),
            on_timeout: TimeoutAction::Allow,
        }
    }
}

/// The tier-3 policy layer (G6, §5.9.6): a LAYER over the session's base
/// policy, never a replacement — every UNSELECTED hook (and the
/// head/tail/surface/route/spend/roles) delegates to `inner` (the
/// engine's free `NoPolicy` base today; tier 2's `DeclarativePolicy`
/// once guardrails exist). Each SELECTED hook emits a `PolicyHook` event
/// with a fresh monotonic `hook_{n}` id + awaits the host's `PolicyAnswer`
/// under the spec's timeout. An unanswered hook — or an answer with the
/// WRONG form for the kind — is a protocol error resolved exactly like a
/// timeout, per the on_timeout mapping documented on the hook arms.
pub struct ExternalPolicy {
    /// The base policy every unselected hook + surface consult delegates
    /// to.
    inner: Arc<dyn AgentPolicy>,
    /// The wire spec (hooks / timeout / on_timeout).
    spec: ExternalPolicySpec,
    /// The loop back-ref (2a.6's pattern) — set post-build by the engine;
    /// the hooks need the loop's agent id + fan-in sender to emit.
    loop_ref: std::sync::Mutex<std::sync::Weak<crate::agent::AgentLoop>>,
    /// Monotonic per-session hook ids (`hook_1`, `hook_2`, …).
    hook_counter: std::sync::atomic::AtomicU64,
}

impl ExternalPolicy {
    /// Wrap the base policy with the external layer (the spec selects the
    /// hooks; the engine calls [`Self::set_loop`] right after building).
    pub fn new(inner: Arc<dyn AgentPolicy>, spec: ExternalPolicySpec) -> Self {
        Self {
            inner,
            spec,
            loop_ref: std::sync::Mutex::new(std::sync::Weak::new()),
            hook_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The post-Arc loop back-ref (2a.6's pattern) — the engine sets it
    /// at spawn, before the first turn.
    pub fn set_loop(&self, weak: std::sync::Weak<crate::agent::AgentLoop>) {
        *self.loop_ref.lock().expect("external policy loop_ref poisoned") = weak;
    }

    /// Whether the kind is selected in the spec.
    fn selected(&self, kind: crate::runtime::channels::HookKind) -> bool {
        self.spec.hooks.contains(&kind)
    }

    /// The gate hooks' on_timeout verdict (Allow proceeds; Deny vetoes;
    /// Stop is a no-op at a gate — the turn-end verb).
    fn gate_timeout_verdict(&self) -> GateVerdict {
        match self.spec.on_timeout {
            TimeoutAction::Allow | TimeoutAction::Stop => GateVerdict::Allow,
            TimeoutAction::Deny => GateVerdict::Deny {
                message: format!(
                    "the external policy's tool gate timed out after {} ms without an answer",
                    self.spec.timeout_ms
                ),
            },
        }
    }

    /// Emit one hook + await the answer under the spec timeout. `None` =
    /// no loop / no id / no fan-in (pre-task) / the fan-in closed / the
    /// timeout elapsed / the responder dropped — every path maps to the
    /// caller's on_timeout resolution.
    async fn ask(
        &self,
        kind: crate::runtime::channels::HookKind,
        payload: serde_json::Value,
    ) -> Option<crate::runtime::channels::PolicyAnswer> {
        let hook_id = format!(
            "hook_{}",
            self.hook_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1
        );
        let loop_arc = self
            .loop_ref
            .lock()
            .expect("external policy loop_ref poisoned")
            .upgrade()?;
        let agent_id = loop_arc.agent_id()?;
        let fanin = loop_arc.event_fanin_sender()?;
        let (responder, answer_rx) = tokio::sync::oneshot::channel();
        fanin
            .send((
                agent_id,
                crate::runtime::channels::AgentEvent::PolicyHook {
                    hook_id,
                    hook_kind: kind,
                    payload,
                    responder,
                },
            ))
            .await
            .ok()?;
        match tokio::time::timeout(
            std::time::Duration::from_millis(self.spec.timeout_ms),
            answer_rx,
        )
        .await
        {
            Ok(Ok(answer)) => Some(answer),
            Ok(Err(_)) | Err(_) => None,
        }
    }
}

#[async_trait]
impl AgentPolicy for ExternalPolicy {
    fn register_tools(&self, reg: &mut ToolRegistry, ctx: &PolicyBuildCtx) {
        self.inner.register_tools(reg, ctx)
    }

    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        self.inner.stable_head(k, c)
    }

    async fn volatile_tail(&self, caps: &Capabilities) -> String {
        self.inner.volatile_tail(caps).await
    }

    async fn tool_surface(&self) -> ToolSurface {
        self.inner.tool_surface().await
    }

    async fn gate_call(&self, name: &str, args: &Value) -> GateVerdict {
        if !self.selected(crate::runtime::channels::HookKind::ToolGate) {
            return self.inner.gate_call(name, args).await;
        }
        let payload = serde_json::json!({"tool": name, "args": args});
        match self
            .ask(crate::runtime::channels::HookKind::ToolGate, payload)
            .await
        {
            Some(crate::runtime::channels::PolicyAnswer::Gate { verdict }) => verdict,
            // A mismatched form (a Prompt/Continuation answered to a
            // gate) is a protocol error — the on_timeout resolution
            // applies.
            _ => self.gate_timeout_verdict(),
        }
    }

    async fn route(&self) -> RouteKey {
        self.inner.route().await
    }

    async fn on_prompt(&self, text: &str) -> PromptHook {
        if !self.selected(crate::runtime::channels::HookKind::PromptIntake) {
            return self.inner.on_prompt(text).await;
        }
        let payload = serde_json::json!({"text": text});
        match self
            .ask(crate::runtime::channels::HookKind::PromptIntake, payload)
            .await
        {
            Some(crate::runtime::channels::PolicyAnswer::Prompt { hook }) => hook,
            // The intake hook can only REWRITE a tail, never block a
            // prompt — an unanswered (or mismatched) hook falls to the
            // default hook; on_timeout has no deny form here
            // (documented mapping).
            _ => PromptHook::default(),
        }
    }

    async fn after_tool(&self, name: &str, result: &ToolResult) {
        if self.selected(crate::runtime::channels::HookKind::AfterTool) {
            let payload = serde_json::json!({"tool": name, "result": result});
            // The answer acknowledges the observation; the call already
            // ran — a Deny is informational only.
            let _ = self
                .ask(crate::runtime::channels::HookKind::AfterTool, payload)
                .await;
        }
        self.inner.after_tool(name, result).await
    }

    async fn on_turn_end(&self, t: &TurnSummary) -> Continuation {
        if !self.selected(crate::runtime::channels::HookKind::TurnEnd) {
            return self.inner.on_turn_end(t).await;
        }
        let payload = serde_json::json!({
            "success": t.success,
            "unattended": t.unattended,
            "stop_reason": t.stop_reason.as_ref().map(|reason| format!("{reason:?}")),
        });
        match self
            .ask(crate::runtime::channels::HookKind::TurnEnd, payload)
            .await
        {
            Some(crate::runtime::channels::PolicyAnswer::Continuation { continuation }) => {
                continuation
            }
            // An unanswered turn end ALWAYS parks — even with
            // on_timeout: Allow (an auto-continue is an explicit act,
            // never a timeout default; a broken host must not spin the
            // session into an infinite loop). Deny/Stop map to Stop
            // identically (documented mapping).
            _ => Continuation::Stop,
        }
    }

    async fn spend_tags(&self) -> SpendTags {
        self.inner.spend_tags().await
    }

    fn spawn_roles(&self) -> &[RoleSpec] {
        self.inner.spawn_roles()
    }

    async fn on_session_end(&self) {
        if self.selected(crate::runtime::channels::HookKind::SessionEnd) {
            // The answer acknowledges; the session ends regardless — no
            // on_timeout form can block teardown (documented mapping).
            let _ = self
                .ask(crate::runtime::channels::HookKind::SessionEnd, serde_json::json!({}))
                .await;
        }
        self.inner.on_session_end().await
    }
}use crate::agent::factory::ToolSelection;

/// G6 tier 2 (D4, §5.9.6 :706-711): one declarative gate rule. A rule
/// matches a call when its tool name equals `tool` AND — when
/// `arg_pattern` is set — the call's JSON-serialized arguments match
/// the regex (`None` matches every call of the tool).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct GateRule {
    /// The tool name the rule matches (exact).
    pub tool: String,
    /// An optional regex over the call's JSON-serialized arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arg_pattern: Option<String>,
    /// The verdict a matching call gets.
    pub verdict: GateVerdict,
}

/// G6 tier 2 (D4, §5.9.6 :711): what a session does when the model
/// asks the user a question. `WaitForHost` is the only live behavior —
/// headless sessions register no `ask_user` today (the free factory's
/// `register_tools` is a no-op); the `AnswerConstant`/`Deny`
/// enforcement rides the app's ask_user wiring at job 5. The wire
/// lands binding-ready, pinned by the contract fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPolicy {
    /// Park the question on the session — the host answers via
    /// events (today's behavior).
    #[default]
    WaitForHost,
    /// Answer with the constant, never asking the host.
    AnswerConstant(String),
    /// Refuse the question outright.
    Deny,
}

/// G6 tier 2 (D4, §5.9.6 :711): the turn-end verb. Only `Stop` (the
/// free agent's park) exists today — the enum exists so the wire shape
/// is stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TurnEndPolicy {
    /// Park at the turn's end (today's behavior).
    #[default]
    Stop,
}

/// G6 tier 2 (D4, §5.9.6 :706-716): the declarative guardrails a
/// spawn carries — the spec [`DeclarativePolicy`] enforces. The
/// default is exactly today's free session (the coding-default head,
/// every group, the `["free"]` route, no rules, `WaitForHost`,
/// `Stop`); `is_default` is the engine's exclusivity helper (a named
/// tier-1 policy plus a DEFAULT-shaped spec is not a conflict).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Guardrails {
    /// The prompt spec's head; `None` = the coding default
    /// (FreePolicyFactory's exact behavior).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptSpec>,
    /// The tool-surface selection — the factory's registration table
    /// narrows the registry to the selected groups at spawn (R1: the
    /// array changes only there); `exclude` carves per-name. The wire
    /// default is the free agent's constant surface (every group
    /// except `Host`).
    #[serde(default = "crate::agent::factory::ToolSelection::free_default")]
    pub tools: ToolSelection,
    /// The routing key's slots (default `["free"]`).
    #[serde(default)]
    pub route: Vec<String>,
    /// Per-session limits; overrides the spawn option's limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::runtime::channels::SessionLimits>,
    /// The declarative gate rules (first match wins).
    #[serde(default)]
    pub gate_rules: Vec<GateRule>,
    /// What a session does when the model asks the user a question.
    #[serde(default)]
    pub question_policy: QuestionPolicy,
    /// The turn-end verb.
    #[serde(default)]
    pub turn_end: TurnEndPolicy,
}

impl Default for Guardrails {
    fn default() -> Self {
        Self {
            prompt: None,
            tools: ToolSelection::free_default(),
            route: vec!["free".to_string()],
            limits: None,
            gate_rules: Vec::new(),
            question_policy: QuestionPolicy::WaitForHost,
            turn_end: TurnEndPolicy::Stop,
        }
    }
}

impl Guardrails {
    /// Whether the spec is a no-op — today's free session, exactly
    /// (the engine's exclusivity check).
    pub fn is_default(&self) -> bool {
        self.prompt.is_none()
            && self.tools == ToolSelection::free_default()
            && self.route.len() == 1
            && self.route[0] == "free"
            && self.limits.is_none()
            && self.gate_rules.is_empty()
            && self.question_policy == QuestionPolicy::WaitForHost
            && self.turn_end == TurnEndPolicy::Stop
    }
}

/// G6 tier 2 (D4, §5.9.6 :706-716): the policy a [`Guardrails`] spec
/// builds — the engine's declarative alternative to a named tier-1
/// factory policy and the tier-3 external layer. The free-agent base
/// ([`NoPolicy`] over the resolved prompt spec — the spec's head else
/// the coding default — UNGATED, FreePolicyFactory's exact product)
/// carries every default; the spec overrides the route, the gate
/// rules, and the turn-end verb. The tool SELECTION is not read here:
/// the engine threads it through the per-spawn factory clone so the
/// registration table itself narrows the array (R1 at spawn — see
/// engine.rs's spawn).
pub struct DeclarativePolicy {
    /// The spec.
    guardrails: Arc<Guardrails>,
    /// The compiled arg patterns, parallel to `guardrails.gate_rules`.
    arg_patterns: Vec<Option<regex::Regex>>,
    /// The free-agent base the defaults mirror.
    inner: NoPolicy,
}

impl DeclarativePolicy {
    /// Build the policy over a spec. Arg patterns compile ONCE here —
    /// a bad pattern is a spawn-time error (never a silent
    /// match-nothing).
    pub fn new(guardrails: Arc<Guardrails>) -> Result<Self, String> {
        let arg_patterns = guardrails
            .gate_rules
            .iter()
            .map(|rule| {
                rule.arg_pattern
                    .as_deref()
                    .map(regex::Regex::new)
                    .transpose()
                    .map_err(|e| {
                        format!(
                            "gate rule for '{}': bad arg_pattern '{}': {e}",
                            rule.tool,
                            rule.arg_pattern.as_deref().unwrap_or_default()
                        )
                    })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let spec = guardrails
            .prompt
            .clone()
            .map(Arc::new)
            .unwrap_or_else(|| Arc::new(PromptSpec::coding_default()));
        Ok(Self {
            inner: NoPolicy::with_spec(spec).ungated(),
            guardrails,
            arg_patterns,
        })
    }
}

#[async_trait]
impl AgentPolicy for DeclarativePolicy {
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {}
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        self.inner.stable_head(k, c)
    }
    async fn volatile_tail(&self, caps: &Capabilities) -> String {
        self.inner.volatile_tail(caps).await
    }
    async fn tool_surface(&self) -> ToolSurface {
        // The ungated engine form: advertise None — the registry is
        // already narrowed at registration by the factory clone — and
        // gate Free (no workflow state ever gates a session).
        self.inner.tool_surface().await
    }
    async fn gate_call(&self, name: &str, args: &Value) -> GateVerdict {
        for (rule, pattern) in self.guardrails.gate_rules.iter().zip(&self.arg_patterns) {
            let args_match = pattern
                .as_ref()
                .map_or(true, |re| re.is_match(&args.to_string()));
            if rule.tool == name && args_match {
                return rule.verdict.clone();
            }
        }
        GateVerdict::Allow
    }
    async fn route(&self) -> RouteKey {
        RouteKey {
            slots: self.guardrails.route.clone(),
        }
    }
    async fn on_prompt(&self, text: &str) -> PromptHook {
        self.inner.on_prompt(text).await
    }
    async fn after_tool(&self, name: &str, result: &ToolResult) {
        self.inner.after_tool(name, result).await
    }
    async fn on_turn_end(&self, _t: &TurnSummary) -> Continuation {
        match self.guardrails.turn_end {
            TurnEndPolicy::Stop => Continuation::Stop,
        }
    }
    async fn spend_tags(&self) -> SpendTags {
        self.inner.spend_tags().await
    }
    fn spawn_roles(&self) -> &[RoleSpec] {
        self.inner.spawn_roles()
    }
    async fn on_session_end(&self) {
        self.inner.on_session_end().await
    }
}