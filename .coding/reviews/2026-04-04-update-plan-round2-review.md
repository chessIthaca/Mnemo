# Review: `update_plan` tool + `abandon_plan` re-framing — Round 2 (M1/M2/N1 fixes)

Date: 2026-04-04
Reviewer: read-only reviewer subagent (`spawn_agent`)
Scope: **all** uncommitted changes in the working tree (`git status` / `git diff HEAD`):

- `.coding/plans/026be09d-...md` (plan step 5 checked off — bookkeeping)
- `.coding/plans/stack.json` (active plan id changed — bookkeeping)
- `.coding/plans/57e7ab3a-...md` (new plan file, untracked — bookkeeping)
- `src/agent/factory.rs` (register `UpdatePlanTool`)
- `src/agent/prompt.rs` (Executing prompt mentions `update_plan` / `abandon_plan`)
- `src/tool/mod.rs` (ToolFilter allowlist + module doc + 3 filter tests)
- `src/tool/workflow/plan.rs` (`UpdatePlanTool`, wording fix, partial move, abandon description)
- `src/workflow/mod.rs` (`Workflow::update_plan` incl. out-of-order guard + 5 tests)
- `.coding/reviews/2026-04-04-update-plan-review.md` (round-1 report, untracked)

What the plan set out to do: add a non-destructive `update_plan` tool to adjust an
in-flight plan in place (preserving completed steps); re-frame `abandon_plan` as a
destructive last resort (keep it `NeedsApproval`); and this round fix the four round-1
findings (M2 out-of-order data loss → reject with error + test; M1 misleading "N steps
added" wording → "before → total steps"; N1 avoidable `args.steps.clone()` → move;
N2 `wf.plan().unwrap()` after Ok → justified skip, no change). `cargo test --lib`
re-verified by this reviewer: **400 passed; 0 failed**.

---

## Findings

**No findings.**

All four round-1 findings are resolved correctly, and no new correctness/bug/security
issues were introduced by the fixes. Detailed verification below.

---

## Verification of the round-2 fixes

### M2 — Out-of-order completion guard (was silent data loss) ✅ FIXED
**File:** `src/workflow/mod.rs:239-252` (`Workflow::update_plan`)

The new guard runs after computing `split` (the index of the first not-done step, or
`len` if all done) and before the prefix is taken:

```rust
if frame.plan.steps[split..].iter().any(|s| s.done) {
    return Err(crate::error::Error::Workflow(
        "cannot update plan: steps were completed out of order ..."
    ));
}
```

Traced every case the task asked about; all behave correctly:

| Steps (d=done, o=open) | `split` | `steps[split..]` | `any(done)` | Result |
|---|---|---|---|---|
| `[a(d), b(d), c(o)]` (in-order) | 2 | `[c(o)]` | false | **OK — no misfire** |
| `[a(d), b(o), c(d)]` (out-of-order) | 1 | `[b(o), c(d)]` | true | **error** (correct — `c` would be lost) |
| all done `[a(d),b(d),c(d)]` | 3 (=len) | `[]` | false | would *not* fire, **but unreachable**: `state==Complete` is rejected at line 196 (`self.state != WorkflowState::Executing`) before `split` is computed. So the guard can never run on an all-complete plan. |

- The guard is **not dead code**: `PlanFile::complete_step`
  (`src/workflow/plan_file.rs:158-168`) marks *any* in-range index done with no ordering
  check, and `Workflow::complete_step` (`src/workflow/mod.rs:289-309`) just forwards the
  index. So `[a(d), b(o), c(d)]` is genuinely constructible via `complete_step(2)` —
  confirmed by the new test `update_plan_rejects_out_of_order_completion`
  (`src/workflow/mod.rs:789-799`) which does exactly `complete_step(2)` then asserts the
  `Workflow` error variant. ✅
- The error variant chosen is `Error::Workflow` (a workflow-state issue), which matches
  the sibling `state != Executing` rejection at line 197 and the "no plan exists" case
  at line 204. Consistent. ✅
- The doc comment (`src/workflow/mod.rs:179-182`) now documents the rejection
  explicitly ("Out-of-order completion is rejected … complete steps in order before
  updating"). ✅
- No new allocation or mutation before the early return — the check reads only. ✅

### M1 — Wording "{before} → {total} steps" (was "{appended} step(s) added") ✅ FIXED
**File:** `src/tool/workflow/plan.rs:190-227` (`UpdatePlanTool::execute`)

- `before` is captured at lines 196-199 via `wf.plan().map(|p| p.steps.len()).unwrap_or(0)`
  **before** `wf.update_plan(...)` is called at line 200. Confirmed by reading the source:
  the capture precedes the mutating call; no re-read after mutation. ✅
- Output line 216: `"updated plan '{title}' ({completed}/{total} done, {before} → {total} steps) — state: {state}"`.
- Reads correctly for all three shapes:
  - **Append** (3 remaining → 5): `3 → 5 steps` ✓ (clear increase)
  - **Replace-equal** (4 → 4): `4 → 4 steps` ✓ (no longer falsely reads as "nothing happened"; the `{completed}/{total} done` plus the unchanged count still conveys a rewrite occurred, and `data.before != data.total` is not the only signal — the tool's success + the plan file are authoritative)
  - **Trim** (5 → 3): `5 → 3 steps` ✓ (clear decrease, not "0 added")
- The `data` block (lines 220-226) now carries `before` instead of `appended`; `total` is
  the post-update count. A consumer can derive the delta either way. The `appended`/
  `saturating_sub` arithmetic is gone, removing the misleading-zero case entirely. ✅
- One subtlety worth noting (not a finding): on a **pure metadata edit** (only
  `title`/`goal`/`context` changed, `steps` omitted), `before == total` and the output
  reads e.g. `"3 → 3 steps"`. That is accurate — step count is unchanged on a metadata
  edit — and not misleading the way "0 steps added" was, since the arrow explicitly means
  "before → after count" rather than "delta added." Fine.

### N1 — Move `args.steps` instead of `.clone()` ✅ FIXED
**File:** `src/tool/workflow/plan.rs:200-204`

`args.steps` is now moved directly into `wf.update_plan(args.title.as_deref(),
args.goal.as_deref(), args.context.as_deref(), args.steps)`. The `args.title.as_deref()`
/ `args.goal.as_deref()` / `args.context.as_deref()` calls (lines 201-203) are evaluated
**before** the `args.steps` argument (line 204) in Rust's left-to-right argument
evaluation, and even after the partial move of `steps`, reading the still-owned `title`
field via `.as_deref()` is legal (only `steps` was moved out; `args` is otherwise dead).
Confirmed it compiles: `cargo test --lib` → 400 passed, 0 failed. No use of `args` after
line 204. The `Option<String>` `.as_deref()` borrows the still-owned field, so no
use-after-move. ✅

### N2 — `wf.plan().unwrap()` after `Ok` ✅ JUSTIFIED SKIP
**File:** `src/tool/workflow/plan.rs:207`

Unchanged. After `wf.update_plan(...)` returns `Ok(())`, the workflow is guaranteed
`Executing` with a non-empty stack (the function errored out otherwise and we're in the
`Ok` arm), so `wf.plan()` returns `Some` and `unwrap()` cannot panic. Safe by
construction; matches `CreatePlanTool::execute`'s same pattern. The round-1 reviewer
explicitly recorded "no change needed." This is the one permitted skip (factually-justified
no-op). ✅

---

## Whole-diff cross-checks (the prior round's change + this round's fixes hold together)

- **ToolFilter gating** (`src/tool/mod.rs:127-170`): `update_plan` appears **only** in
  the `Executing` branch (line 154). `Planning` (line 138) and `Complete` (line 167)
  allowlists are `name == "create_plan"` only. Tests assert hidden-in-Planning
  (~line 412), hidden-in-Complete (~line 454), visible-in-Executing (~line 432). The
  in-tool state guard at `Workflow::update_plan` line 196 is a **second** defense
  (errors if not Executing) and is independently tested by
  `update_plan_errors_outside_executing`. Defense-in-depth intact. ✅
- **Safety levels unchanged & correct**: `UpdatePlanTool::safety` → `AutoRun`
  (`plan.rs:182-187`, non-destructive in-place rewrite of sandboxed plan doc, same id,
  preserves completed steps, never touches project source — same posture as
  `create_plan`). `AbandonPlanTool::safety` → `NeedsApproval` (`plan.rs:352-354`,
  destructive pop). The requirement "abandon needs approval" is satisfied and was never
  regressed; only its *description* was re-framed to "destructive last resort /
  prefer update_plan." ✅
- **Factory registration** (`src/agent/factory.rs:52-54, 272`): `UpdatePlanTool` imported
  and registered alongside `CreatePlanTool`/`CompleteStepTool`/`AbandonPlanTool`. ✅
- **Prompt** (`src/agent/prompt.rs:131-142`): Executing guidance now steers the agent to
  `update_plan` for scope changes (in-place, preserves completed steps) and reserves
  `abandon_plan` (approval-gated) for fundamentally-wrong plans. Consistent with the
  tool semantics and safety levels. ✅
- **Serde/`Option` handling** (`UpdatePlanArgs`, `plan.rs:39-52`): all four fields
  `#[serde(default)] Option<...>`. Absent key → `None`; explicit `null` → `None`; blank
  string → guarded by `!t.trim().is_empty()` in `Workflow::update_plan` (treated as
  "leave unchanged" → no-op → error). Tested by `update_plan_rejects_empty_or_noop`
  (blank-string case) and `update_plan_noop_errors`. ✅
- **Re-indexing** (`Workflow::update_plan:261-264`): after building `combined`,
  `combined.iter_mut().enumerate()` reassigns `s.index = i`, so indices stay contiguous
  and match display order even after trim/replace/append. ✅
- **Plan id + stack.json unchanged**: writes via
  `frame.plan.write_to_dir(&self.plans_dir, &frame.id)` (same `frame.id`); never calls
  `persist_stack`. Tested by `update_plan_appends_steps_preserving_completed_prefix`
  (`assert_eq!(wf.plan_id().unwrap(), id)`). ✅
- **Constitution — doc comments**: `Workflow::update_plan` has a full doc comment
  (updated this round to document the rejection); `UpdatePlanTool` struct + schema
  documented. `UpdatePlanTool::new` lacks a doc comment, but this **matches existing
  style** — `CreatePlanTool::new`, `CompleteStepTool::new`, `AbandonPlanTool::new` are
  all undocumented trivial constructors. Not a violation. The new test
  `update_plan_rejects_out_of_order_completion` has no doc comment, which the
  constitution does not require for tests. ✅
- **No Windows-path / shell-safety issues**: no shell or path-interpolation code in the
  diff; `write_to_dir`/`plans_dir` are pre-existing and unchanged. ✅
- **No security issues**: no user-controlled path interpolation, no eval, no unbounded
  input growth beyond plan size (same bounds as `create_plan`). The out-of-order guard
  is a *data-integrity* improvement (prevents silent loss), not a security concern. ✅

---

## Verdict

**Clean — no findings (Major / Minor / Nit).** All four round-1 findings are resolved:
M2 (out-of-order silent data loss) is now an explicit `Workflow` error with a dedicated
test and the guard cannot misfire on in-order or all-complete plans (the latter is
rejected earlier by the state guard); M1's misleading "N steps added" wording is replaced
by an accurate "before → total steps" with `before` captured pre-mutation; N1's avoidable
clone is a correct partial move (compiles, tests pass); N2 is a justified, factually-correct
skip. The whole diff — the `update_plan` tool, `abandon_plan` re-framing, ToolFilter,
prompt, factory, and all tests — holds together. `cargo test --lib`: 400 passed, 0 failed.
Ready to commit to `feat/reviewer-report-instructions`.