# Job 2b — the app module graph moves under `mnemo::app`

Master-spec anchors: §5.2 :266-269 (the 2b sketch), :255-258 (the boundary
test `kernel_does_not_import_app` and its scan tokens), U7 (the kernel never
knows the workflow's type — the trait inverts the dependency). Status: NOT
STARTED (this file is the step file; the audit step runs verification-first,
the discipline of §14).

## Mandate

Relocate the workflow, backlog, skill, review and auto-continue code into
`mnemo::app` behind the trait. The factory takes `Arc<dyn PolicyFactory>`,
and backlog wiring (`set_backlog*`) moves to the app's policy factory. The
boundary test turns on — a single-crate module boundary; a later
`mnemo-kernel` crate split then stays mechanical (master-spec :257-258,
:277-278).

Rejected (the spec, :273-276): `WorkflowState::Free` inside the workflow;
`Option<Workflow>` on `AgentLoop` (the kernel must not know the workflow
type); an immediate crate split (the module boundary + the test gives the
same guarantee for now).

## 2b.0 — the audit (verification-first; DRIFT #8 territory)

Audit BEFORE any move, with every claim probed against the tree. Record the
census in this file's `## Audit` section (append-only) as steps 2b.1-2b.3
consume it.

### The move-set (the app code relocating under `crate::app`)

- `src/workflow/` — mod.rs (~146 KB) + plan_file.rs (~59 KB).
- `src/backlog.rs` (~120 KB).
- `src/skill/` — the module dir (mod.rs + shipped skills; verify the file
  list at execution).
- `src/tool/workflow/` — ask_user.rs (~10 KB), backlog.rs (~60 KB), plan.rs
  (~297 KB), skill.rs (~53 KB), mod.rs.
- The review machinery: `src/agent/review_scope.rs` + wherever the review
  tool surfaces (locate at execution — note tool/workflow has NO review.rs;
  the review surface rides plan.rs/tool-side).
- The auto-continue mechanics: the gate/streak code in
  `src/runtime/agent.rs` (:297-373 region as of 2a.5's tree) — decide at
  execution what "the auto-continue code" means for this pass: the
  streak/consolidation mechanics may ride the policy from 2c (2a.5 put the
  state predicate in `WorkflowPolicy::on_turn_end`); the kernel keeps only
  the budget/count math. Audit, then decide, then document the decision
  here.

### The kernel's current import sites (verified census as of `4569974`)

Production source files importing `crate::workflow` / `crate::backlog` /
`crate::skill` / `crate::app` / `tool::workflow` — the exact tokens the
boundary test scans (master-spec :255-258):

- `src/agent/loop_impl.rs` — the `workflow` FIELD (:144, :783 — the kernel
  HOLDS `Arc<Mutex<Workflow>>`), `workflow_handle` (:2401), the
  `pinned_state` (:272), the `ModelContext` compat ctor params
  (:1526/:1545-47), the routing's Skill probe (:1563),
  workflow_expects_progress (:2441-2446, zero live callers since 2a.5 —
  the keeper fns stay or retire at execution), the :2454 read.
- `src/agent/turn.rs` — the :50 import, `read_plan_scope` (:1383-87, the
  kernel's own wf read feeding the budget gate + the spend stamp), the
  Complete+success read (:1920), `step_lane_target` (:481).
- `src/agent/prompt.rs` — the :18/:629 imports; :386-393 (the state-gated
  section fn); :1054-1108 (the STATE consts' state-gating — the kernel's
  prompt-section selection); the nudge (:1093); :1656 (a PlanKind probe).
- `src/agent/dispatch.rs` — the :29 import + the filter/gate logic reads.
- `src/model_resolver.rs` — the :44 import (`PlanKind`, `WorkflowState`
  for `build_route_key`).
- `src/runt` ime/` — turn_resolve.rs (:28/:293), channels.rs (:24),
  agent.rs (:1497 + the test imports).
- `src/host/runtime.rs` — the :751 import + :795/:809-813 + the tests.
- `src/memory/` — indexer/plans.rs (:11 PlanFile), finish_capture.rs (:46
  PlanFile/PlanKind) — the memory module (kernel-adjacent) reads plan
  files; decide at execution whether it stays kernel (a PlanFile trait
  needed) or rides app-side plumbing.
- `src/tool/` — mod.rs (:31 + the BacklogStore opens :1821/:1831/:1841),
  agent/spawn_agent.rs (:38), and the whole `tool/workflow/` dir.
- `src/skill/mod.rs` — the :31 import.
- Doc-only mentions (no import — retire the links as they touch):
  model_routing.rs :230, review_scope.rs :22, project/git_ops.rs :161.
- Tests (`#[cfg(test)]` modules inside the above files + agent/tests.rs):
  the boundary scan's treatment of test code decides at 2b.0 — the working
  proposal is that the scan excludes `#[cfg(test)]` blocks (mechanical:
  the test harness imports app types freely), documented before 2b.3
  turns the test on.

### The audit's decisions to make (document in `## Audit` when resolved)

1. What stays kernel-side verbatim: `WorkflowState` is an APP type (U7) —
   the kernel's state-gated prompt consts and route slots need either (a)
   the policy selecting among kernel-supplied blocks (the head/tail seam
   from 2a.4 already does this for the head — extend it), or (b) the
   kernel defining its own neutral enum mapped by the factory. Decide per
   site; document with evidence.
2. The kernel's `workflow` FIELD removal: `AgentLoop` holds
   `Arc<Mutex<Workflow>>` — behind the trait this becomes the policy's
   concern (the 2a accessors already expose it via `workflow_handle()`).
   Enumerate every `self.workflow` consumer: loop_impl :2402/:2441/:2454 +
   read_plan_scope (turn.rs, via its own field?) — audit who owns what.
3. `build_route_key` (2a.6) takes `WorkflowState`/`PlanKind` — the routing
   walk is policy-shaped already; decide whether it moves app-side behind
   the policy or the kernel defines the slot vocabulary neutrally.
4. The `tool/workflow` tools: they belong to the app (the boundary token
   `tool::workflow`). The registry wiring (tool/mod.rs :1821-1841) moves
   to the app's policy factory; the kernel's `register_tool` surface stays.
5. The memory module's plan-file reads (indexer/finish_capture): either a
   kernel-side filesystem reader (no PlanFile type) or an app-side hook.
6. The auto-continue mechanics' split (the budget/count math stays
   kernel; the state predicate already consults the policy since 2a.5).

## 2b.1 — the app relocation (zero behavior)

Move `workflow/`, `backlog.rs`, `skill/`, and `tool/workflow/` under
`crate::app::` with re-exports keeping every existing path compiling
(`pub use crate::app::workflow::*;` in the old spots or the mechanical
import rewrites — pick per the compiler's least-diff rule). Files:
`src/app/mod.rs` (+ the re-exports), `src/app/workflow/**`, `src/app/
backlog.rs`, `src/app/skill/**`, `src/app/tool/workflow/**` — plus the
mechanical `crate::workflow::` → `crate::app::workflow::` rewrites ONLY in
the moved modules themselves (the kernel's sites stay untouched in this
step). Verify: the battery green, the goldens green, zero diffs beyond
moves/imports.

## 2b.2 — the PolicyFactory + the wiring move

`src/agent/factory.rs`: the factory takes `Arc<dyn PolicyFactory>` (the
trait: build the policy + the app-side tools); `set_backlog`/
`set_backlog_notifier` (:686/:698 — their IPC callers live in
src-tauri/src/main.rs :129) move to the app's factory; the `tool/workflow`
tool registrations (tool/mod.rs's opens :1821-1841) ride the same factory.
Verify: battery green, the factory state tests green.

## 2b.3 — the kernel decoupling (the trait inversions)

Per-site inversion of every kernel import site in the 2b.0 census, each
with its own verified anchor + drift documentation. The 2a seam already
routed most behavior; the remaining reads (read_plan_scope, the state
gating, the field) invert behind the policy or the kernel-neutral types.
After the last flip: `kernel_does_not_import_app` turns ON (the token scan
per master-spec :255-258, the test treatment settled at 2b.0 point 6).
Verify: the battery, the goldens, the boundary test green, zero
regressions in the auto-continue/unattended/routing tests.

## Acceptance (job-level)

The goldens trio green at every boundary
(workflow_stable_head_bytes_unchanged, workflow_policy_routing_table_
unchanged, volatile_tail_section_order_pinned); the full four-command
battery (the step file :778-785) at every step boundary; the boundary
test green at the end of 2b.3 with the scan tokens clean in kernel
sources; every kernel import site from the 2b.0 census either relocated,
inverted behind the trait, or explicitly retired (with the census line
crossed off and the drift documented in the commit message).

## Conventions

- The verbatim-move contract where it applies (job-1 rules); the
  edit-steps discipline (only the step's listed files); anchors match on
  quoted text; every deviation in the `--no-verify` commit message.
- The review rounds to a terminal PASS per step; the finish gate
  fail-closes on FINDINGS.
- The goldens gate every phase; the boundary test is add-only green.

## Audit — the per-site records (2b.0; append-only)

### A1. The kernel's workflow-field consumers (verified read 2027-01-11, tree d340acb)

- `src/agent/loop_impl.rs:144` + `AgentLoopParams` `:783` — AgentLoop HOLDS
  `Arc<tokio::sync::Mutex<Workflow>>` ("Each agent owns its own Workflow
  (built by the AgentLoopFactory)", the doc :2397-2400). Every consumer
  below flows from this field.
- `loop_impl.rs:2401-03` `workflow_handle()` — the IPC-layer getter
  (`get_workflow_state(agent_id)`), returning the Arc clone. 2b shape: the
  handle moves app-side (the policy implements whatever the IPC needs —
  the kernel stops returning the concrete type).
- `loop_impl.rs:2440-47` `workflow_expects_progress(unattended: bool)` —
  matches Executing|Reviewing || (unattended && Planning). ZERO live
  callers since 2a.5 (the keeper doc :2419-25: "the state predicate moved
  verbatim into WorkflowPolicy::on_turn_end (the auto-continue gate now
  consults the policy); this fn has zero live callers"). → feeds DECISION
  2: retire the fn or keep it reading a policy-supplied label; nothing
  real depends on it.
- `loop_impl.rs:2453-56` `workflow_state_label()` —
  `format!("{:?}", wf.state())` — the Parked event's evidence label
  (consumed by `runtime/channels.rs` per the doc :2450). 2b shape: the
  policy supplies the label (a `current_state_label()` query returns a
  String; the kernel never names the enum).
- `src/agent/turn.rs:1390-95` `read_plan_scope()` — locks `self.workflow`
  ("under ONE workflow lock", the doc :1390-94); feeds the budget gate +
  the spend attribution; 2a.8 already flipped the CALLER to the policy
  consult (the kernel fn stays as today's implementation the app-side
  policy calls INTO via the workflow handle).
- `src/agent/turn.rs:1920` — the finish-capture guard
  `wf_state == WorkflowState::Complete && result.success` (the Complete-
  plan capture; state VALUE comparison).
- `loop_impl.rs:272` `pinned_state: Mutex<Option<WorkflowState>>` — the
  STATE-SCOPED explicit picker pin (:1589-1634: stamped on first serve,
  holds in its own state, cedes on a state change with a configured slot;
  :2093-2106 `set_explicit_provider` clears the stamp on a fresh pick).
  The model-picker needs the CURRENT state VALUE to scope pins → DECISION
  3 overlap: a policy query returning an opaque state value can replace
  the typed field.
- `src/agent/dispatch.rs:29 → :143` — `denial_message(name: &str, state:
  WorkflowState, verdict: ResearchWrite)` — a VALUE parameter rendering
  state-dependent denial text; no field access.
- `src/agent/factory.rs:90` — the `Workflow` import wiring AgentLoopParams;
  `:86-88` imports `crate::tool::workflow::skill` tools (the boundary
  token `tool::workflow` hits the KERNEL here — a real scan hit);
  `:1776` sits inside the `#[cfg(test)]` module (test-scoped).
- `factory.rs:686-98` `set_backlog`/`set_backlog_notifier` — the factory
  holds `BacklogWiring{store: Arc<Mutex<BacklogStore>>, on_changed}`
  (`:97`'s type from crate::backlog); called by the IPC layer
  (src-tauri/src/main.rs:129). → DECISION 4 evidence: the wiring slot +
  the two setters are exactly what the app-side policy factory absorbs.
### A2. The prompt state-gating (verified read 2027-01-11; CENSUS CORRECTION)

- Census correction: the cited ":1054-1108 (the STATE consts' state-
  gating)" region is `crate::agent/prompt.rs`'s `#[cfg(test)] mod tests`
  (starts :625 — the nudge tests :1048-1099). It is NOT production
  state-gating. Production prompt.rs state-touch is FAR smaller than the
  census implied. (The test module also imports
  `crate::app::workflow_policy::workflow_section` :631 — boundary-hit
  tokens in test code confirmed.)
- `prompt.rs:434-436` `build_stable_head(&Constitution)` — STATE-FREE
  (2a.4-complete: `assemble_stable_head(&KernelPromptBlocks::workflow(),
  constitution)`); `kernel_prompt_blocks()` :443-452 hands the six
  file-private consts to the policy. No workflow dependency.
- `prompt.rs:475-509` `build_volatile_tail(caps, app_tail: &str, memories)`
  — STATE-FREE: the workflow section rides in as the policy-assembled
  `app_tail` string (the doc :454-461: "assembled by the agent's policy
  (2a.4: WorkflowPolicy::volatile_tail wraps the same bytes the
  kernel-side workflow_section produced pre-2a.4)").
- `prompt.rs:522-535` `build_system_prompt(caps, workflow: &Workflow,
  ...)` — the BACK-COMPAT wrapper (the doc :511-521: "tests and any
  single-message caller"; production composes head+tail directly). It
  imports `crate::app::workflow_policy::workflow_section` at :531 → the
  BOUNDARY tokens `crate::app` + `&Workflow` both hit kernel sources.
  2b shape: the wrapper MOVES under crate::app (it is a test
  convenience; the app owns both the type and workflow_section).
- `prompt.rs:384-398` `append_plan_nudge(tail, state: WorkflowState,
  user_message)` — fires only on `WorkflowState::Complete` + a work-verb
  message. The ONE production state VALUE dependency: turn.rs:3544-48
  calls it with `self.workflow.lock().await.state()`. 2b shape: the nudge
  (fn + looks_like_work_intent + PLAN_NUDGE_TEXT) moves under crate::app,
  and turn.rs's call flips to a policy consult
  (`policy.append_plan_nudge(&mut tail, latest_user_msg.as_deref())`) —
  the policy reads its own workflow for the state; the kernel stops
  touching the type (feeds DECISIONS 1 + 2).
- Verdict for DECISION 1: the prompt seam is ALREADY policy-shaped
  (2a.4); the remaining flips are the wrapper relocation + the nudge
  consult. WorkflowState does not need to enter any kernel fn signature
  after these two moves.
### A3. The peripheral sites (verified read 2027-01-11; census corrections)

- `src/model_resolver.rs:106-136` `build_route_key(workflow_state:
  WorkflowState, skill_name, plan_kind, is_subagent) ->
  crate::agent::policy::RouteKey` — the 2a.6 slot-walk extractor (skill →
  subagent → bug_fixing → state slot), doc :100-101: "WorkflowPolicy::route
  can emit exactly the same list (the routing-table golden pins both)". The
  `:44` import `{PlanKind, WorkflowState}` feeds exactly this fn. 2b shape
  (DECISION 3): build_route_key MOVES under crate::app (the policy's route
  delegate owns it); the kernel's `ModelContext::new` compat ctor stops
  building route keys from typed state (2a.6 already made it accept the
  prebuilt RouteKey); `RouteKey` itself stays kernel-side (it lives in
  crate::agent::policy — the policy's type).
- `src/runtime/turn_resolve.rs` — production WorkflowState uses are VALUE
  params of the transition bookkeeping (`workflow_transition_counts(from:
  Option<WorkflowState>, to: WorkflowState)` feeding TurnResolveLatch);
  tests :464-477. Run-All/auto-feed bookkeeping consumes state values
  (DECISION 6 family).
- `src/runtime/channels.rs:24, :262-263, :592-593, :843-844` —
  `AgentEvent::WorkflowStateChanged { state: WorkflowState, top_plan_id }`
  — the IPC EVENT WIRE TYPE carries the state VALUE end-to-end (the
  serialization shim :843-844 forwards it; its Serializable twin :592-593).
  2b shape: the event carries an opaque representation (a String label or
  u8 tag), the app maps its enum into it (DECISION 6 family).
- Census correction — `src/runtime/agent.rs`: the PRODUCTION import block
  (:11-24) carries NO workflow dependency (failure_triage, AgentLoop,
  consolidation, provider, channels, correction only). BOTH census hits
  (:1497, :3138) are `#[cfg(test)]`-module (the mock harness imports +
  a :3130-41 test fixture's `create_plan_with_kind`). The auto-continue
  gate reads ONLY AgentLoop accessors — production-clean already.
- Census correction — `src/host/runtime.rs`: ALL hits (:751, :795,
  :809-813) sit inside `#[cfg(test)]` (the `parent_in_state` test harness
  :793+ driving AgentLoop into states). Production host/runtime.rs has NO
  workflow import — 2a.7's consult flipped the last production read.
- `src/memory/indexer/plans.rs:11` `use crate::workflow::PlanFile` —
  PRODUCTION: the indexer parses `.coding/plans/*.md` through PlanFile
  (`scan_plans(dir, out, errors, config)`, :15-21).
- `src/memory/finish_capture.rs:46` `use crate::workflow::{PlanFile,
  PlanKind}` — PRODUCTION: the finish-capture digests read the completed
  plan file and test `PlanKind::BugFixing` for the BUG digest (:59-63).
- DECISION 5 evidence: the memory module (kernel-adjacent) depends on the
  plan-file FORMAT parser + one PlanKind VALUE test. Both usage shapes are
  parse-and-classify (no workflow SEMANTICS). Recorded inversion shapes:
  (a) the plan-file format parsing relocates kernel-side (a neutral
  parser module, e.g. crate::memory::indexer's own plan_format or a
  kernel-side crate::plan_parse), with the app's workflow/plan_file.rs
  keeping plan semantics and delegating serialization; (b) a source-record
  trait the app implements. Final pick at 2b.1 planning; both keep the
  boundary tokens out of kernel sources.
### The decisions — resolved (2b.0, 2027-01-11; evidence: A1-A3)

1. **WorkflowState ownership** — WorkflowState/PlanKind are APP types (U7).
   After 2b.3 the kernel names them NOWHERE: no typed fields, params, or
   fns. State flows reach the kernel only as (a) policy consults (the
   2a.5/2a.7/2a.8 pattern), or (b) opaque representations (String label /
   u8 tag on the wire events + the transition bookkeeping) mapped by the
   app at the emit sites.
2. **The kernel field removal path** — AgentLoop's `workflow: Arc<Mutex<Workflow>>`
   (:144) + the AgentLoopParams mirror (:783) LEAVE the kernel. Consumers:
   `workflow_expects_progress` (zero live callers since 2a.5, A1) → RETIRE
   at 2b.3; `workflow_state_label` + `read_plan_scope` + :1920's
   Complete-and-success capture guard + the nudge read (:3546) → policy
   consults; `workflow_handle()` (the IPC getter) → the app factory/policy
   implements what the IPC needs. Which side holds the Arc<Mutex<Workflow>>
   post-move is 2b.3's mechanical detail (probed then; today WorkflowPolicy
   reaches the workflow via the loop back-ref).
3. **Route-key placement** — `build_route_key` moves under crate::app (the
   policy's route delegate owns it; the routing-table golden travels with
   it). The kernel's `ModelContext::new` compat ctor stops building typed
   route keys (2a.6 already made it accept the prebuilt key). `RouteKey`
   STAYS kernel-side — it lives in crate::agent::policy (the policy's own
   type; the routing-table golden pins parity both sides). The
   `pinned_state` stamp (:272) consults the policy for the state value it
   scopes pins by.
4. **The tool/workflow relocation** — the five modules (ask_user, backlog,
   plan, skill, mod) move under crate::app; the kernel's imports flip to
   D5's register pattern: the app-side policy factory attaches the app
   tools post-construction (WorkflowPolicy::register_tools has been a
   deliberate no-op since 2a.1 — 2b.2/2b.3 gives it its real body or moves
   the wiring to the policy factory impl). `set_backlog`/
   `set_backlog_notifier` (:686-98) leave the kernel factory for the app's
   policy factory; src-tauri/src/main.rs:129's IPC call re-targets it.
5. **The memory module's plan-file reads** — indexer/plans.rs (:11) and
   finish_capture.rs (:46) consume the plan-file FORMAT parse-only. Audit
   recommendation (final pick at 2b.1 planning): the plan-file format
   parsing relocates kernel-side as a neutral parser; the app's
   workflow/plan_file.rs keeps plan semantics and delegates serialization.
   Alternative: a source-record trait the app implements. Either keeps
   boundary tokens out of kernel sources.
6. **The auto-continue split + the boundary test's test-code treatment** —
   the split already holds post-2a.5 (the gate consults on_turn_end; the
   kernel keeps budget/count mechanics; runtime/agent.rs production is
   workflow-type-free per A3). Remaining kernel state-value consumers
   (turn_resolve's transition counts, channels' WorkflowStateChanged wire
   field) receive the opaque representation. Boundary test: the scan
   EXCLUDES #[cfg(test)] module contents (balanced-close strip before
   token-scanning); tokens stay exactly master-spec :255-258's list
   (`crate::workflow`, `crate::backlog`, `crate::skill`, `crate::app`,
   `tool::workflow`); the scanner reads CARGO_MANIFEST_DIR/src at test
   time; the test's home is decided at 2b.3.

Census corrections carried: host/runtime.rs and runtime/agent.rs are
production-clean (test-only hits); prompt.rs's :1054-1108 was test code
(A2). The remaining PRODUCTION kernel sites = loop_impl (field, pinned
stamp, handle, label, keeper), turn.rs (read_plan_scope, :1920, :3546,
:50), dispatch.rs (:29→:143), model_resolver.rs (:44+build_route_key),
runtime/{turn_resolve, channels}, memory/{indexer/plans, finish_capture},
factory.rs (:86-88 tool::workflow tools, :90, :686-98), tool/mod.rs (:31
+ the opens), skill/mod.rs (:31), tool/agent/spawn_agent.rs (:38),
prompt.rs (the :531 app import + the wrapper).
### A4. The console-mode shape (settled during the 2b.2 review round 1)

Console mode (`run_console`, src-tauri/src/console.rs) wires the app
`WorkflowPolicyFactory` too — the reviewer's HIGH finding: the kernel factory
constructed WorkflowPolicy UNCONDITIONALLY pre-2b.2, so a GUI-arm-only
wiring would have silently regressed console agents to NoPolicy (no
workflow tail section, the `["free"]` routing chain, while the plan tools
stayed registered). The wiring sits right after `build_brain`'s Ready arm,
before the first agent build. NO backlog wiring on the console path:
console opens no store (review finding 7 keeps the `backlog_add` trio out
of console registries — the policy factory's None-wired backlog slot
reproduces that exactly, while `ask_user` stays registered). The kernel
test helpers (factory.rs make_factory, console.rs make_factory,
config_io.rs, events.rs) stay unwired (NoPolicy) — every one of them
asserts bookkeeping, not policy semantics (reviewer-2b2's checklist + the
battery; no test covers console policy semantics, noted as the accepted
gap the boundary test's 2b.3 pass will not narrow).