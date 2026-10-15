# Job 2a — the AgentPolicy seam (step file)

Source of truth: `.coding/specs/headless-free-agent.md` §5.2 Phase 2 (:200-279 — the trait
verbatim :204-231, cache contract :237-240, routing contract :241-246, kernel-owned stays
:247-258, the 2a bullet :261-265), job-table row :760, acceptance rows :785-792, named tests
:913-914, method §14 (:1002-1077). The per-site anchors live in the audit appendix:
`.coding/specs/headless-free-agent/job-02a-seam-audit.md` (every production site
probe-verified against tip `ef65642` this session). Job 1 (host extraction) is merged; job 2a
routes kernel coupling through the policy — ZERO behaviour change — and 2b moves the app
implementation under `mnemo::app`.

Worker tier: deepseek, one step per ALF job, strictly sequential (§14.4). Steps may be stacked
into one session but never reordered and never skipped.

## The step list (each ≤ ~400 changed lines, one concern per step, green at every boundary)

- 2a.0 Golden characterization tests — added FIRST, must pass WITHOUT touching non-test code.
- 2a.1 The types compile (new src/agent/policy.rs: trait + supporting types + KernelPromptBlocks
  + NoPolicy stub) — nothing routed yet, nothing reads it.
- 2a.2 The policy attaches (AgentLoop field + accessor; the factory builds + attaches
  WorkflowPolicy wrapping today's Workflow) — still no reads moved.
- 2a.3 tool_surface + gate_call (dispatch.rs :253; turn.rs :553-561).
- 2a.4 stable_head + volatile_tail (KernelPromptBlocks; the tail's workflow-section param).
- 2a.5 on_prompt + on_turn_end (the nudge call, the auto-continue gate, the unattended
  re-derivations).
- 2a.6 route (ModelContext → { route: RouteKey, is_subagent }; the `#[models.free]` default).
- 2a.7 spawn_roles (the host/runtime.rs parent-snapshot read; installs stay put).
- 2a.8 spend_tags + after_tool (site verification first — the one uneumerated surface).

## Design decisions locked here (per §14.2, the file decides; deviations are drift)

D1 (supporting types): the spec names `ToolSurface`, `GateVerdict`, `PromptHook`,
`Continuation`, `TurnSummary`, `SpendTags`, `RoleSpec`, `RouteKey`, `PolicyBuildCtx`,
`KernelPromptBlocks` without defining them. Definitional bodies are in step 2a.1 below,
written out in full. They are `pub` in `mnemo` (a `pub` item is never dead_code under the
`deny(warnings)` rule, so the types compile clean while nothing else consumes them yet).

D2 (policy storage): `AgentLoop` gains `policy: Arc<dyn AgentPolicy>` (Send + Sync) with a
`pub fn policy(&self) -> Arc<dyn AgentPolicy>` accessor. Arc, not Box: gather call sites may
clone the handle cheaply, and `host::runtime` needs the parent's policy while building a child
(2a.7). The field default: `NoPolicy` — the factory attaches `WorkflowPolicy` explicitly.

D3 (who implements what in 2a): `WorkflowPolicy` (NEW `src/app/workflow_policy.rs` per job-table
:760 — wraps `Arc<tokio::sync::Mutex<Workflow>>` AND the existing factory wiring) implements
everything by delegating to today's logic. `NoPolicy` (kernel, `src/agent/policy.rs`) returns
constant/free answers. In 2a the factory attaches `WorkflowPolicy` — the shipped agent is the
workflow agent, byte-identical.

D4 (emitter boundary): the policy does NOT get an EventEmitter in 2a (:233-236 says "at
attach" — the attach event arrives with 2b); `emit_workflow_state` (turn.rs :1811-1956) keeps
its kernel-side emission; only the auto-continue DECISION moves to `on_turn_end`, whose
Return value the runtime consumes. The event variants (channels.rs) are untouched.

D5 (registration stays kernel in 2a): the app-tool registration fns (factory.rs :1294-1428,
:1431+) do NOT move behind `policy.register_tools` yet — they stay kernel-called (2b moves the
implementation under `mnemo::app` and only THEN the call routes through the policy). Reason:
moving them inside 2a would double-touch; the registration seam is exercised by the SAME
call sites in 2b. The step-2a.1 pass still installs the trait method (default no-op on
`NoPolicy`; `WorkflowPolicy` keeps calling today's kernel helpers internally).

D6 (the free slot): `[models.free]` is added to `ModelsConfig` + its DTO with
`#[serde(default)]` in step 2a.6 (routing contract :241-246).

D7 (golden style): FNV-1a 64-bit fold over the head bytes + full length — dependency-free,
deterministic, catches any byte change; NO sha2 dependency is added (verify at execution that
none exists: `grep sha2 Cargo.toml` — if present, prefer `sha2::Sha256` instead of FNV).

## Step 2a.0 — the golden characterization tests (ADD ONLY; nothing else changes)

File list: `src/agent/prompt.rs` (append one test to the existing `#[cfg(test)] mod tests`),
`src/model_resolver.rs` (append one test to its existing `#[cfg(test)] mod tests`), and
`Cargo.toml`+`src-tauri/Cargo.toml` only if D7's drift check found no test-visible need (it
should not — the tests use only existing deps + std). **No other file may change.**

Definition: the tests must PASS against the tree as it stood at step-2a.0 start (the boundary
commit under them is the plan's base). If either fails before any non-test change, the worker
STOPPED (the stop rule below) — that is the whole point: they pin today's bytes.

```rust
// src/agent/prompt.rs — the golden-stable-head test (spec :913)
#[test]
fn workflow_stable_head_bytes_unchanged() {
    use crate::project::Constitution;
    // Fixed fixture constitution — tiny, deterministic, never synced to disk.
    let constitution = Constitution {
        global: "# HARD RULES — GLOBAL (agent.md)\n\n- fixture global rule one.\n".to_string(),
        project: "# HARD RULES — PROJECT (agent.md)\n\n- fixture project rule one.\n".to_string(),
    };
    let head = build_stable_head(&constitution);
    // Byte-count pin (D7): the full golden digest + length below must not move
    // through 2a.2-2a.8 — the head is the cache prefix (master-spec :237-240).
    let len = head.len();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in head.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // Pins the EXACT byte stream of the six consts + constitution markers at
    // step-2a.0 start. The worker runs this once, records the FNV-1a hex +
    // length in the constants below, and NEVER edits them again in 2a.
    const GOLDEN_LEN: usize = 0usize /* filled by the worker on first run */;
    const GOLDEN_FNV: u64 = 0x0u64 /* filled by the worker on first run */;
    assert!(GOLDEN_LEN != 0, "worker must fill GOLDEN_LEN/GOLDEN_FNV on the \
        first run from the pre-change tree (these constants freeze it)");
    // Structural sanity so the digest pin is explainable if it ever moves:
    assert!(head.contains("CORE PRINCIPLES:"));
    assert!(head.contains("WORKFLOW LIFECYCLE"));
    assert!(head.contains("APP RULES — universal hard rules"));
    assert!(head.contains("# HARD RULES — GLOBAL (agent.md)"));
    assert_eq!(len, GOLDEN_LEN, "stable head byte length drifted");
    assert_eq!(h, GOLDEN_FNV, "stable head bytes drifted");
}
```

Worker note: the constants are filled by RUNNING this test against the untouched tree once,
transcribing the two values, then re-running. The test then gates every later step.

```rust
// src/model_resolver.rs — the golden routing-table test (spec :914)
#[test]
fn workflow_policy_routing_table_unchanged() {
    use crate::model_resolver::{ModelsConfig, Resolver};
    use crate::workflow::WorkflowState;
    // The full lattice the solver walks — every (state, skill, subagent, plan
    // kind) combination the production resolver can receive today. Fixture
    // config: one DISTINCT endpoint model per section so every cell resolves
    // to exactly one named model.
    let cfg: ModelsConfig = serde_json::from_str(r#"
      { "endpoints": {
          "planning":    { "model": "m-planning",    "provider": null },
          "executing":   { "model": "m-executing",   "provider": null },
          "reviewing":   { "model": "m-reviewing",   "provider": null },
          "complete":    { "model": "m-complete",    "provider": null },
          "bug_fixing":  { "model": "m-bug-fixing",  "provider": null },
          "research":    { "model": "m-research",    "provider": null },
          "subagent":    { { "model": "m-subagent",  "provider": null } },
          "deepseek":    { "model": "m-deepseek",    "provider": null }
        }
      } "#
    ).map_err(|e| e.to_string()).unwrap_or_else(|_| unreachable!());
    let resolver = Resolver::new(Arc::new(cfg));
    // Cell matrix to walk: state × (skill, subagent, plan kind).
    // (the matrix mirrors the existing lattice tests at :847-851 + :865-:957 —
    // same fixture shape, walked exhaustively)
    let cells: &[(WorkflowState, &str, bool, &str)] = &[ /* … fill from the audit's
        lattice table — every combo the existing tests cover … */ ];
}
```

DRIFT (resolved at execution, before filling): the exact `ModelsConfig`/`Resolver`
fixture shape MUST be transcribed from the EXISTING lattice tests (:680-1409 — pick one and
copy its construction verbatim; the above sketch is not authoritative — the fixtures differ
from the real production types). The test then walks the SAME cells with the SAME fixture
other tests use, plus the full-state ordering pin. And THAT version fills the golden.

The point of 2a.0: pin today's behavior BEFORE any policy wiring so 2a.2-2a.8 can be checked
against the frozen table (acceptance row :786-787).

---
## Step 2a.1 — the types compile (`src/agent/policy.rs` is NEW; nothing routes yet)

File list: `src/agent/policy.rs` (NEW), `src/agent/mod.rs` (insert `pub mod policy;` after
`pub mod prompt;`). **No other file may change.** Everything stays `pub` (D1: `pub` items are
never dead_code under `deny(warnings)`).

New file written out IN FULL below (the trait is master-spec :204-231 verbatim; the supporting
types are the D1 definitions; every member carries its spec line):

```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel→app seam ([`AgentPolicy`]): the app's `WorkflowPolicy`
//! (src/app/workflow_policy.rs, 2a.2+) implements it by wrapping today's
//! `Workflow`; the kernel default `NoPolicy` is the free agent
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
/// gate and the research write verdict move into `WorkflowPolicy` in 2a.3.
/// Default: [`GateVerdict::Allow`].
#[derive(Debug, Clone)]
pub enum GateVerdict {
    /// The call proceeds (subject to the surface filter).
    Allow,
    /// The call is refused with this reason (dispatch surfaces it as an error).
    Block { reason: String },
}

/// Prompt-intake result (master-spec :220-222): the unattended-marker read and
/// an optional volatile-tail rewrite (the Complete-state plan nudge). Default
/// no-op (`PromptHook::default()`).
#[derive(Debug, Clone, Default)]
pub struct PromptHook {
    /// `Some(v)` SETS the unattended flag outright (prefix read / plain-prompt
    /// clear). `None` leaves the runtime's own derivation untouched (2a.5
    /// moves the reads across one arm at a time).
    pub unattended: Option<bool>,
    /// `Some(text)` replaces the synthesized Linux/newline-safe prompt tail
    /// (today's plan nudge rides this hook from 2a.5).
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
    /// The stop reason the runtime observed (`crate::agent::StopReason`).
    pub stop_reason: crate::agent::StopReason,
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
/// skill name the resolver walks, then default. `WorkflowPolicy` emits
/// exactly today's chain (the golden table test, 2a.0); `NoPolicy` emits
/// `["free"]` → `[models.free]` (routing contract :241-246).
#[derive(Debug, Clone)]
pub struct RouteKey {
    /// Ordered slot names, e.g. `["bug_fixing", "executing"]`.
    pub slots: Vec<String>,
}

/// Build context handed to [`AgentPolicy::register_tools`] (master-spec :207).
pub struct PolicyBuildCtx {
    pub agent_id: crate::runtime::channels::AgentId,
    pub project_root: std::path::PathBuf,
    pub plans_dir: std::path::PathBuf,
}

/// The kernel's six compiled prompt blocks (prompt.rs consts), handed to the
/// policy so `stable_head` can stay byte-identical (cache contract
/// :237-240). The app wrapper supplies `KernelPromptBlocks::workflow()`;
/// job 2b relocates the app halves and trims the kernel consts to
/// KERNEL_PREAMBLE + KERNEL_RULES.
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
    pub fn workflow() -> Self {
        use crate::agent::prompt::{
            APP_RULES, CODING_SYSTEM_PREAMBLE, MEMORY_RECORDS, TOOL_CALL_DISCIPLINE,
            TOOL_STRATEGY, WORKFLOW_LIFECYCLE,
        };
        Self {
            preamble: CODING_SYSTEM_PREAMBLE,
            lifecycle: WORKFLOW_LIFECYCLE,
            rules: APP_RULES,
            discipline: TOOL_CALL_DISCIPLINE,
            strategy: TOOL_STRATEGY,
            memory_records: MEMORY_RECORDS,
        }
    }
}

/// The kernel→app policy seam (master-spec :204-231 verbatim).
#[async_trait]
pub trait AgentPolicy: Send + Sync {
    /// Called once at build: register app tools (plan/backlog/skill/review).
    fn register_tools(&self, reg: &mut ToolRegistry, ctx: &PolicyBuildCtx);

    /// The stable head. Default = kernel head (KERNEL_PREAMBLE + KERNEL_RULES
    /// + TOOL_CALL_DISCIPLINE + TOOL_STRATEGY + MEMORY_RECORDS + constitution).
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String;

    /// The policy's part of the volatile tail (kernel adds recall + CAPABILITIES).
    async fn volatile_tail(&self, caps: &Capabilities) -> String;

    /// Advertised tool array filter + dispatch gate filter.
    async fn tool_surface(&self) -> ToolSurface;

    /// Per-call veto/redirect beyond the filter (plan-mutation gate, research
    /// write verdict). Default Allow.
    async fn gate_call(&self, name: &str, args: &Value) -> GateVerdict;

    /// Routing key for this request: ordered config slot names + skill name.
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

    /// The agent's task is terminating (engine/session teardown, 3b). Default
    /// no-op.
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
/// (kernel default) and both policy impls. Added HERE (2a.1) with the six
/// blocks + the constitution markers; `build_stable_head` delegates to it in
/// this same step so there is exactly one assembly body (the 2a.0 golden
/// test pins its bytes).
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
    if !c.global.trim().is_empty() {
        out.push_str("# HARD RULES — GLOBAL (agent.md)\n\n");
        out.push_str(c.global.trim());
        out.push_str("\n\n");
    }
    if !c.project.trim().is_empty() {
        out.push_str("# HARD RULES — PROJECT (agent.md)\n\n");
        out.push_str(c.project.trim());
        out.push_str("\n\n");
    }
    out
}
```

Note: `assemble_stable_head` uses `.trim()` normalization. The original
`build_stable_head` (prompt.rs :505-528) pushes constitution text UNTRIMMED:
`prompt.push_str(&constitution.global);` — the trim is a CHANGE. This step therefore does
NOT move `build_stable_head` onto the helper. The helper ships unused-but-pub (fine:
`pub`), and step 2a.4 re-lands `build_stable_head`'s body so the assembly is byte-matched
(no trim) — the trim here is a drift marker; either drop the `.trim()`s in the helper and
push raw (byte-faithful: drop them NOW and match the original), or do the move in 2a.4.
RESOLVED: drop `.trim()`s — byte-faithful — the helper matches :505-528 exactly except the
loop form. (Keep `push_section`'s empty-skip semantics: the original skips EMPTY sections;
that is what the trim-empty check does per block, no trim of the pushed bytes.)

Replacement table (prompt.rs):

| old (exact) | new | count |
|---|---|---|
| `pub fn build_stable_head(constitution: &Constitution) -> String {` | `pub fn build_stable_head(constitution: &Constitution) -> String {` (unchanged signature) — body becomes: `assemble_stable_head(&KernelPromptBlocks::workflow(), constitution)` | 1 |

…i.e. the body :507-528 (the six `push_section` + two constitution `if`s + `prompt`) is
REPLACED by `assemble_stable_head(&KernelPromptBlocks::workflow(), constitution)` — the
moved bytes now live in `policy.rs::assemble_stable_head` (verbatim, minus the trim
decision above). The 2a.0 golden test re-runs green IMMEDIATELY (same bytes). `import`
additions to prompt.rs: `use crate::agent::policy::{assemble_stable_head, KernelPromptBlocks};`
(compiler-driven; exact additions allowed).

Verify: `cargo build --workspace` (the policy.rs types compile; nothing else reads them);
`cargo test --workspace` (the 2a.0 golden tests still green — the head is byte-identical).

## Step 2a.2 — the policy attaches (field + accessor + factory attach; no reads moved)

File list: `src/agent/loop_impl.rs` (field + accessor + builder),
`src/agent/factory.rs` (attach),
`src/app/mod.rs` (NEW: `pub mod workflow_policy;`) and
`src/app/workflow_policy.rs` (NEW: the `WorkflowPolicy` skeleton).
**No other file may change.**

(a) `src/app/mod.rs` (NEW file):

```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The app-side kernel-policy implementation (wrapper). Added in 2a; job 2b
//! moves the full app module graph under `mnemo::app`.

pub mod workflow_policy;
```
Wait — `src/app/mod.rs` is inside src/ (the LIB) — that's mnemo::app. Fine (2b moves the
REST under it; the wrapper starts here per job-table :760). `src/lib.rs` gains `pub mod app;`
(compiler-driven exact insert; count the mod lines against :el-1).

(b) `src/app/workflow_policy.rs` (NEW, skeleton — full delegates land in 2a.3-2a.8):

```rust
// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The workflow agent's policy: wraps today's `Workflow` so every kernel
//! coupling consults this wrapper (master-spec :200-202). Job 2b moves the
//! implementation under `mnemo::app`.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::agent::policy::{
    AgentPolicy, Continuation, GateVerdict, KernelPromptBlocks, PolicyBuildCtx, PromptHook,
    RoleSpec, RouteKey, SpendTags, ToolSurface,
};
use crate::project::Constitution;
use crate::provider::Capabilities;
use crate::tool::{ToolRegistry, ToolResult};
use crate::workflow::{Workflow, WorkflowState};

/// The workflow agent's policy — byte-identical behavior to the pre-2a
/// direct reads (acceptance :785-788).
pub struct WorkflowPolicy {
    pub(crate) workflow: Arc<Mutex<Workflow>>,
}

impl WorkflowPolicy {
    /// Wrap the loop's workflow (factory build path, 2a.2).
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self { workflow }
    }
    /// The workflow handle (kernel helpers occasionally need the state map
    /// during migration; shrinks as sites flip).
    pub fn workflow(&self) -> Arc<Mutex<Workflow>> {
        Arc::clone(&self.workflow)
    }
}

#[async_trait]
impl AgentPolicy for WorkflowPolicy {
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String {
        crate::agent::policy::assemble_stable_head(k, c)
    }
    fn register_tools(&self, _reg: &mut ToolRegistry, _ctx: &PolicyBuildCtx) {
        // D5: registration stays kernel-called in 2a (factory fns run as today).
    }
    async fn volatile_tail(&self, _caps: &Capabilities) -> String {
        // 2a.4 moves the workflow-section assembly here.
        String::new()
    }
    async fn tool_surface(&self) -> ToolSurface {
        // 2a.3 delegates to the workflow's allowed_tools (today's filter).
        ToolSurface::constant()
    }
    async fn gate_call(&self, _name: &str, _args: &Value) -> GateVerdict {
        GateVerdict::Allow
    }
    async fn route(&self) -> RouteKey {
        // 2a.6 delegates to today's slot-chain builder.
        RouteKey { slots: vec![] }
    }
    async fn on_prompt(&self, _text: &str) -> PromptHook {
        PromptHook::default()
    }
    async fn after_tool(&self, _name: &str, _result: &ToolResult) {}
    async fn on_turn_end(&self, _t: &crate::agent::policy::TurnSummary) -> Continuation {
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
```

(c) `loop_impl.rs` edits (compiler-anchored — the field list/drift markers noted):

| # | edit | count |
|---|---|---|
| 1 | In `pub struct AgentLoop {` — after the LAST field, insert: `policy: Arc<crate::agent::policy::AgentPolicy>,` | 1 |
| 2 | In `AgentLoop::new` (the ONE constructor — verify; if the struct is built in multiple places, ALL gain the field: the compiler points at each) — `policy: Arc::new(crate::agent::policy::NoPolicy),` | ≥1 |
| 3 | Near the other accessors (next to `workflow_handle` :2386-2388), add: `pub fn policy(&self) -> Arc<crate::agent::policy::AgentPolicy> { Arc::clone(&self.policy) }` | 1 |
| 4 | Add the setter next to it: `pub fn set_policy(&mut self, policy: Arc<crate::agent::policy::AgentPolicy>) { self.policy = policy; }` | 1 |

(d) `factory.rs` edits:

| # | edit | count |
|---|---|---|
| 5 | In build_inner, directly AFTER the `Workflow::new`/`load_latest` block (:983-987, the audit's anchor), nothing changes yet (the attach picks a default NoPolicy in (c)). The Attach move happens AT THE CONSTRUCTOR via the builder — i.e. the site that builds `AgentLoop::new(...)` for the workflow agent gains `.set_policy_or_with_policy(...)` — CONCRETE: after the loop is constructed in build_inner, insert `loop.set_policy(Arc::new(crate::app::workflow_policy::WorkflowPolicy::new(<the workflow arc — the loop's own workflow handle>)));` using `loop.workflow_handle()`. | 1 |

Drift markers for 2a.2: the exact insertion lines for (1)-(5) follow the compiler's
direction (deny(warnings) flags the missing field; the accessor placement mirrors
`workflow_handle`); every construction site the compiler names gets the same
two-line treatment. `use crate::app::workflow_policy::WorkflowPolicy;` in factory.rs.

Verify: `cargo build --workspace`; `cargo test --workspace` — the 2a.0 goldens still green;
no new tests needed (no behavior moved yet).

---
## Step 2a.3 — tool_surface + gate_call (the dispatch gate)

WorkflowPolicy members that get their REAL delegate in this step:

```rust
async fn tool_surface(&self) -> ToolSurface {
    let wf = self.workflow.lock().await;
    let filter = wf.allowed_tools();
    ToolSurface {
        advertise: Some(filter.clone()),
        gate: Some(filter),
    }
}
```
(ToolFilter must derive Clone — it already does where the model's filter is stored per
state; drift-check at execution.)

Kernel call-site table (one concern: the filter consult):

| file | old (verbatim, from the audit) | new | count |
|---|---|---|---|
| src/agent/dispatch.rs | `        let filter = wf.allowed_tools();` (:253) | the dispatch fn that holds this line gains `policy: Arc<crate::agent::policy::AgentPolicy>`; the line becomes the gate consult: `let surface = policy.tool_surface().await;` + the gate filter: `let filter = match surface.gate { Some(f) => f, None => wf.allowed_tools() };` — the None arm is dead once the factory attaches WorkflowPolicy, but keeps NoPolicy-shaped callers honest (kernel default = the workflow's own filter? NO — NoPolicy means no workflow surface; None falls back to the RAW loop filter path only where the loop still owns one. DRIFT: verify at execution that dispatch has no other allowed_tools source; the fallback arm then reads the pre-2a behavior when policy is NoPolicy — which only tests hit.) | 1 |
| src/agent/turn.rs | the schema-consult block :553-561 (read the block; anchor = its first line, exact at execution) | the same `surface.advertise` consult replaces the direct wf read | 1 |

`gate_call` stays `Allow` in 2a (the filter does all of today's work; the plan-mutation gate
and the research write verdict keep their tool-side/loop-side homes until 2b/2c — documented
in the impl with the master-spec :215-216 reference). ACCEPTANCE: the dispatch/backlog tool
families' tests pass unmodified; a `grep -c "allowed_tools"` diff vs the 2a.0 baseline in
dispatch.rs is −1.

## Step 2a.4 — stable_head + volatile_tail (the head/tail builders)

(a) HEAD call site: find the production callers of `build_stable_head(` (

the audit: prompt.rs :505 def; the turn-side assembly calls it — grep `build_stable_head(`
across src/ at execution; expected count ≥1 production + tests). Each PRODUCTION call of
`prompt::build_stable_head(&constitution)` becomes
`let head = loop_policy.stable_head(&crate::agent::policy::KernelPromptBlocks::workflow(), &constitution).await;`
— `stable_head` is SYNC in the trait, so the form is
`loop_policy.stable_head(&KernelPromptBlocks::workflow(), &constitution)` (no await).
WorkflowPolicy::stable_head already delegates to `assemble_stable_head` (2a.2 skeleton) —
byte-identical ✓ (the golden test holds the line). The workshop count of call-site edits:
expected 1 (the turn's messages[0] builder — drift-check at execution).

(b) TAIL: `build_volatile_tail(caps: &Capabilities, workflow: &Workflow, memories: Option<&str>)`
(:550-553+). The workflow-dependent half (the workflow-section assembly + CURRENT PLAN /
GOAL / PROGRESS / CURRENT STEP / skill goal / sub-agent notes) MOVES verbatim into
`WorkflowPolicy::volatile_tail(caps)` (lock the wf; assemble the SAME bytes). The kernel
keeps: recall block + CAPABILITIES + the caller's ordering. Signature becomes:

```rust
pub fn build_volatile_tail(
    caps: &Capabilities,
    app_tail: &str,
    memories: Option<&str>,
) -> String
```
— body: the memory block (unchanged bytes) + `app_tail` where the workflow section used to
ride + CAPABILITIES (unchanged bytes; the tail's exact section order must NOT change — the
existing workflow_section tests :1138/:1534 pin content, not order; the ORDER pin is the
turn-side assembly — drift-check at execution which test pins the ORDER and extend it to
2a.0's golden set BEFORE flipping if none does).

Call sites of `build_volatile_tail(` across src/ (grep at execution; expected: the
turn-side assembly + `install_system_messages`-adjacent pops per prompt.rs :664-668 doc):
each flips to composing `let app_tail = loop_policy.volatile_tail(&caps).await;` then
`prompt::build_volatile_tail(&caps, &app_tail, memories)`.

`on_prompt`'s tail_rewrite (the plan nudge) rides in 2a.5 — 2a.4 leaves the nudge call
(turn.rs :3512) kernel-side.

ACCEPTANCE: the 2a.0 head golden green; workflow_section content tests green; a
`grep -c "&Workflow"`-style check on prompt.rs shows `build_volatile_tail` no longer takes
the workflow (count 0 in its signature).

## Step 2a.5 — on_prompt + on_turn_end (the intake + the auto-continue gate)

WorkflowPolicy full delegates land here:

```rust
async fn on_prompt(&self, text: &str) -> PromptHook {
    // The unattended dispatch marker (channels.rs :128-139) — the SAME
    // predicate the runtime arms use today (:805, :1331), now read via the
    // hook. Kernel arms consume `unattended: Some(..)` 1:1.
    let unattended = Some(text.starts_with(crate::runtime::channels::UNATTENDED_PROMPT_PREFIX));
    // The Complete-state plan nudge — delegate to today's fn (turn.rs :3512
    // moves from a direct call to this delegate; same inputs, same bytes).
    let wf = self.workflow.lock().await;
    let tail_rewrite = if wf.state() == WorkflowState::Complete {
        Some(crate::agent::prompt::append_plan_nudge(/* args — transcribe turn.rs :3512's exact arguments at execution */))
    } else {
        None
    };
    PromptHook { unattended, tail_rewrite }
}
```
(+ `workflow_policy::tests::auto_continue_moved_behaviour_identical` is NOT this step — that
test name (:917) belongs to 2b's move; 2a.5 gates on the EXISTING arm tests around green.)

Kernel call-site table:

| file | old (anchor at execution) | new | count |
|---|---|---|---|
| src/runtime/agent.rs | the Prompt arm :795-805: `:805 self.unattended = text.starts_with(UNATTENDED_PROMPT_PREFIX);` | consult `self.agent_loop.policy().on_prompt(text).await` FIRST: `if let Some(u) = hook.unattended { self.unattended = u; }` + stash `hook.tail_rewrite` for the next turn's tail (the nudge call in turn.rs :3512 changes to consuming the stashed hook — one turn of latency equals today's: the nudge is computed at PROMPT time for the turn that follows; DRIFT: verify at execution that :3512's call is for the turn AFTER the prompt — if it is same-turn, keep the :3512 call site and let the hook be advisory-only for the nudge in 2a) | 1 |
| src/runtime/agent.rs | the compaction-buffer arm :1324-1331 (:1330/:1331) | the same hook consult | 1 |
| src/runtime/agent.rs | :899 (the third reset arm — read its fn at execution) | the same hook consult | 1 |
| src/runtime/agent.rs | the gate :328-367 | the STATE predicate moves: build `TurnSummary { success, stop_reason }`, call `policy.on_turn_end(&summary).await`; on `Continuation::Continue` run the EXISTING streak mechanics (:343-357: MAX cap + consolidation cadence + the note :365-367); on `Stop` park. The predicate inside WorkflowPolicy::on_turn_end replicates loop_impl `workflow_expects_progress`'s state logic verbatim (read :2400-2426 fully at execution and copy the predicate; loop_impl's fn stays for kernel diagnostics) | 1 |

The emit_workflow_state site (:1811-1956) is NOT moved (D4). ACCEPTANCE: the
runtime/agent.rs arm tests (:6634, :6733, :6861, :7048, :7291 + chains to :7196) green;
the plan-nudge family (prompt.rs :1204-1248) green.

## Step 2a.6 — route (ModelContext → { route: RouteKey, is_subagent }; [models.free])

This is the riskiest step (master-spec :957-958): keep it whole, gated by the 2a.0 golden
table (spec :787: "routing slot per (state, skill, plan kind, subagent) (golden table), and
… identical (golden table test)").

(a) `WorkflowPolicy` gains the loop back-reference (for `is_subagent`, set later by the
spawn path — host/runtime.rs :163's `set_is_subagent`):
- field: `loop_ref: std::sync::Mutex<Option<std::sync::Weak<crate::agent::AgentLoop>>>`
  (Weak — no cycle); `pub fn attach_loop(&self, weak: Weak<AgentLoop>)`;
  `pub fn loop_ref(&self) -> Option<Arc<AgentLoop>>` (upgrade).
- factory: right after attaching the policy, `policy.attach_loop(Arc::downgrade(&loop))`.
- loop_impl.rs: add `pub fn is_subagent(&self) -> bool` next to `set_is_subagent`
  (find the field/flag — same struct block as D2's field; drift-check).

(b) `RouteKey` construction: WorkflowPolicy::route() replicates today's slot-chain assembly —
DELEGATE, don't copy: extract the chain-build from loop_impl::resolve_turn_provider_routed
(:1518-1630) into `pub(crate) fn build_route_key(state, skill, plan_kind) -> RouteKey` in
model_resolver.rs (the SAME body that builds today's ordered list; resolve_turn_provider_routed
then calls it too — one source of truth), and WorkflowPolicy::route() = that builder on its
own wf state (+ is_subagent → the `subagent` slot from the loop_ref flag, mirroring today's
chain).

(c) `ModelContext` becomes `{ route: RouteKey, is_subagent }` (master-spec :241):
- model_resolver.rs :68-84: replace the struct's state/skill/subagent/plan-kind FIELDS with
  `pub route: RouteKey,` + `pub is_subagent: bool` (the resolver's walked chain then reads
  `ctx.route.slots` — the walk itself is unchanged byte-for-byte).
- `ModelContext::new(...)`'s signature: keep a COMPAT constructor
  `ModelContext::new(state, skill, is_subagent, plan_kind)` that BUILDS the RouteKey via the
  same builder (2b) — in 2a, `new` keeps its old signature and constructs `{ route:
  build_route_key(state, skill, plan_kind, is_subagent), is_subagent }` — THE POINT: zero
  call-site churn, the resolver walks identical chains, and every existing call
  (loop_impl :1547/:1630/:1723/:1747, turn.rs :1200, model_resolver's own tests) compiles
  UNTOUCHED. jobs 2b/2c then deprecate `new` in favor of `{ route, is_subagent }` literals.
- `[models.free]`: `ModelsConfig` + its DTO gain `free` with `#[serde(default)]`
  (routing contract :244-246) — find the struct in model_resolver.rs (drift-check the exact
  block) and mirror in src-tauri's DTO if one exists (grep; count at execution).

(d) The test transcription (NOT weakening — a 1:1 shape migration): every model_resolver
test constructing `ModelContext::new(state, skill, is_sub, kind)` keeps working through (c)'s
compat constructor — so the lattice tests pass UNTOUCHED ✓ (that is the whole reason for (c)).

ACCEPTANCE: the 2a.0 routing-table golden green (the table walked from the
build_route_key output); `grep -c "ModelContext::new(" src/model_resolver.rs src/agent/turn.rs
src/agent/loop_impl.rs` unchanged from the 2a.0 baseline; `cargo test -p mnemo` green.

## Step 2a.7 — spawn_roles (the parent snapshot via the policy)

| file | old (audit :374's exact line — read the fn at execution) | new | count |
|---|---|---|---|
| src/host/runtime.rs | the parent's `wf.allowed_tools()` consult inside `compute_subagent_allowlist` — the block reads the parent loop's workflow handle then filters | the parent's allowed set comes from `parent_loop.policy().tool_surface().await.gate` (the SAME filter today — the 2a.3 flip makes this identical); the child's installs (:189/:191) stay direct (D5) | 1 |

`WorkflowPolicy::spawn_roles()` stays `&[]` (the reviewer allow-list computation —
REVIEWER_BASE_TOOLS/ALWAYS_SAFE_ROLE_TOOLS + the intersection — is kernel-hosted and stays
kernel-hosted through 2b; `RoleSpec` wiring is 3a's CLI). ACCEPTANCE: the factory state tests
(:1877-1898, :3558) + the host::runtime spawn tests (job-1's seven) green.

## Step 2a.8 — spend_tags + after_tool (the last surface; verification-first)

This step's FIRST action is a verification sweep (the audit did not enumerate these sites —
DRIFT #8): grep `Spend`/`Savings` row construction and `step_index` stamping across
src/agent/ + src/runtime/ (expected: the loop's spend-row assembly puts plan_id/step_index
from the workflow today); then:

- WorkflowPolicy::spend_tags() delegates to the SAME extraction (read the wf's current
  plan/step — the exact fields the row assembly reads today).
- The row assembly consults `policy.spend_tags().await` — plan_id/step_index now opaque
  tags (master-spec :226-227) — byte-identical rows when the policy mirrors today's fields.
- `after_tool`: an empty-consult hook call after each tool result in the loop's tool-execution
  path (find the single post-execution point — loop_impl's dispatch result handling; one
  insert; NoPolicy/NoWorkflow both no-op — measured non-issue: one `.await` per tool call).

ACCEPTANCE: the existing spend/steering stat tests green (list their names in the report —
the worker greps `#[test]`+`spend` at execution); zero diff in Spend-row wire payloads.

## Verify commands (run once each, EVERY step boundary; unpiped, read $LASTEXITCODE)

1. `cargo build --workspace`
2. `cargo test --workspace`
3. `cargo test -p mnemo`
4. `cargo test -p mnemo --features browser,embeddings`
(1 & 2 gate every step; 3 & 4 gate the step boundaries 2a.2, 2a.4, 2a.6, 2a.8 minimum — the
full four at every boundary is the default.)

## Stop rule VERBATIM (master-spec :1028-1031)

"If any quoted anchor is not found exactly once, or the code you find does not match this
file, or a verify command still fails after 3 fix attempts, stop and report what differs.
Don't redesign."

## Forbidden (master-spec :1024) + the 2a-specific notes

- No renames beyond the tables; no doc rewrites; no `#[allow]`; no test deletion or
  weakening; no edits outside the listed files.
- Compiler-driven edits allowed: exactly the unused-import deletions / missing-field
  initializations / the threaded `policy:` parameters the compiler flags (both crates
  `#![deny(warnings)]`), each reported.
- The kernel→app import direction: AFTER 2a.2 the factory imports
  `crate::app::workflow_policy` — EXPECTED and required (the import-flip test
  `policy::tests::kernel_does_not_import_app`, :789-790, lands with 2b, not 2a).

## Mechanical acceptance (the step-file greps; run per boundary)

1. The two 2a.0 golden tests exist and pass: `workflow_stable_head_bytes_unchanged`,
   `workflow_policy_routing_table_unchanged`.
2. After 2a.3: `grep -c "allowed_tools" src/agent/dispatch.rs` — baseline−1 (the direct read
   moved behind the surface).
3. After 2a.4: `build_stable_head`'s body is the single `assemble_stable_head` call
   (`grep -c "push_section" src/agent/prompt.rs` — baseline−6+1 helper); no
   `&Workflow` in either builder's signature.
4. After 2a.5: `grep -c "starts_with(UNATTENDED_PROMPT_PREFIX)" src/runtime/agent.rs` → 0
   (the arms consult the hook), and the append_plan_nudge CALL count unchanged by value.
5. After 2a.6: the `ModelContext::new` call counts unchanged; `ModelsConfig.free` present
   (`grep -c "free" src/model_resolver.rs` — ≥1 in the config region).
6. Existing families green at every boundary: the audit's §11 list verbatim.
7. Leak checks at every boundary: `git status --short` clean of unlisted files; the branch
   `wt/mnemo` untouched by other work.

## Report format (per step, the worker reports before finishing)

1. For every table row: file, the anchor line as-found, the applied replacement (or the
   exact deviation), the compiler-driven edits it required.
2. The verify-command outputs (the exit codes; the failing lines if any).
3. The acceptance grep results vs the expected counts.
4. Any DRIFT encountered + its resolution (never silently).
5. The leak check (worktree list, `git branch --list "alf/*"`, `git status --short`) unchanged.
