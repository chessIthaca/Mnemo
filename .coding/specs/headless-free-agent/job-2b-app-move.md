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