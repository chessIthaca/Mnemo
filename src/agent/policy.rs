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
    /// coding default; the Engine path (3a) propagates it).
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
}

impl NoPolicy {
    /// Build the free-agent policy over a caller-supplied spec (G1).
    pub fn with_spec(spec: Arc<PromptSpec>) -> Self {
        Self { spec }
    }

    /// The kernel's explicit fallback — the loop's bare default and the
    /// factory's unwired arm: the mnemo coding default (the console/GUI
    /// free mode's spec, §5.9.1 :631-632), never a silently-assembled
    /// coding head. The Engine spawn path (3a) errors without the
    /// caller's spec (`KernelError::PromptRequired`).
    pub fn with_coding_default() -> Self {
        Self {
            spec: Arc::new(crate::agent::prompt::PromptSpec::coding_default()),
        }
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