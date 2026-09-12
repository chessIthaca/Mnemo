## Verdict: PASS (0 high, 3 low)

The finish↔update_plan deadlock fix is **correct and secure**. Both fix paths (A: relax `update_plan` for regression_test-only in Reviewing; B: `finish` accepts the param directly) are soundly implemented, the security guard is airtight, reviewer isolation is preserved, and both regression tests are meaningful. The 3 findings are all low-severity documentation/accuracy nits — none block the change.

### Scope reviewed
Uncommitted diff on `wt/agenticcoding` (5 source files): `src/tool/mod.rs`, `src/workflow/mod.rs`, `src/tool/workflow/plan.rs`, `src/agent/factory.rs`, `src/error.rs`. The `.coding/` changes (backlog, plan, knowledge files) are bookkeeping and were not reviewed for correctness.

---

### Security: can `update_plan` in Reviewing mutate steps/title/goal/context? — NO (airtight)

The `ToolFilter::Reviewing` arm is static and cannot inspect args, so the workflow method's guard is the **only** protection. Tracing the full path:

1. `ToolFilter::Reviewing` (mod.rs:435) now allows `name == "update_plan"` — the tool passes the static filter.
2. `dispatch.rs` filter check passes.
3. `UpdatePlanTool::execute` (plan.rs:628-635) passes **all** args straight through to `wf.update_plan(args.title, args.goal, args.context, steps, args.append, args.regression_test)` — **no pre-filtering** that could be wrong.
4. `Workflow::update_plan` (mod.rs:514-527) guard runs **before any mutation**:
   ```rust
   let has_structural_fields = title.is_some() || goal.is_some()
       || context.is_some() || steps.is_some();
   match self.state {
       WorkflowState::Executing => {}
       WorkflowState::Reviewing if !has_structural_fields => {}
       _ => return Err(WorkflowWrongState { .. });
   }
   ```
   In Reviewing, any `Some` structural field → `WorkflowWrongState`. Only all-`None` (regression_test-only) proceeds.

The frame is private; `update_plan` is the sole mutation entry point, so no caller can bypass the guard. `append` is correctly excluded from `has_structural_fields` — it's a no-op when `steps`/`context` are both `None`, and either being `Some` trips the guard independently. **The reject path is tested**: `update_plan_errors_outside_executing` (mod.rs:2016) asserts `update_plan(Some("T"), …)` in Reviewing → `WorkflowWrongState`. The allow path is tested by the new `update_plan_regression_test_allowed_in_reviewing` (mod.rs:2024).

### Reviewer isolation — preserved
`ToolFilter::Reviewer` (mod.rs:506-516) is a strict allow-list (`current_plan` || explicitly named). `reviewer_filter_is_a_strict_allow_list` (mod.rs:1885) still asserts `!names.contains("update_plan")` — **unchanged**. The relaxation is in the `Reviewing` *base-state* filter only; reviewers never see `update_plan` (or `finish`). ✓

### Lock safety — safe
`finish` changed `let wf` → `let mut wf` (plan.rs:1204) and calls `wf.update_plan(...)` (plan.rs:1220) while holding the workflow lock. `update_plan` is a **sync** method (`&mut self`, no `await`, no re-acquire of the `Arc<Mutex<Workflow>>`) — no deadlock. It does a sync `std::fs::write` under the lock, but that matches the existing `complete_step`/`abandon_plan` pattern (the "never hold a lock across an *await*" rule at plan.rs:1200 is not violated — `update_plan` doesn't await). ✓

### Regression test validity — both meaningful
- `update_plan_regression_test_allowed_in_reviewing` (mod.rs:2024): old code was `if state != Executing { err }` → fails in Reviewing; new guard allows it. Fails before fix, passes after. ✓
- `finish_accepts_regression_test_param` (plan.rs:3165): `FinishArgs` lacks `deny_unknown_fields`, so before the fix serde silently drops the unknown `regression_test` field → the gate (plan.rs:1287) blocks with "no regression test recorded" → `res.success == false`. Not spurious: it pre-records **no** test (`bug_plan_to_reviewing(&wf, None)`) and relies solely on the param. ✓

---

### Findings (low severity)

**L1 — Budget-ceiling comment doesn't match the code** · `src/agent/factory.rs:1430-1431` vs `:1440`
The comment reads `Reviewing 18_500 → 20_500 ON PURPOSE`, but the actual ceiling at line 1440 is `20_700` (a +2200 delta, not +2000). The comment also omits that `ExecutingResearch` was raised `20_300 → 20_400` (`:1439`). Fix the comment to state `20_700` and mention all three raised ceilings. (The ceilings themselves are reasonable — tight buffers for the intended schema growth: update_plan's full schema joins the Reviewing array, finish gained ~37 chars, update_plan's description grew. No context-bloat risk; the cap still catches *unintended* growth above it.)

**L2 — `finish` silently swallows `update_plan` errors** · `src/tool/workflow/plan.rs:1219-1222`
`let _ = wf.update_plan(None,None,None,None,false,Some(rt));` discards the `Result`. The comment ("the gate below catches a missing test") is sound for the normal case, and the realistic error modes are all impossible here (state was just checked Reviewing + lock held; a finished plan is on the stack; `rt` is non-empty so `changed=true`; `steps=None` so no skeleton-lock rejection). The one residual is a `write_to_dir` disk error: the in-memory frame still gets the regression_test (mutation precedes the write), so the snapshot/gate pass, but the plan *file* isn't persisted with it — a partial-disk-failure edge case where the plan is marked Complete yet its on-disk file lacks the test name. Acceptable (finish's own `persist_stack` would also fail on a truly broken disk), but consider logging the error (`eprintln!`) so a real write failure isn't fully silent, mirroring the capture-failure logging at plan.rs:1375.

**L3 — Test name `update_plan_errors_outside_executing` is now slightly stale** · `src/workflow/mod.rs:2003`
The name implies `update_plan` errors universally outside Executing, but regression_test-only is now *allowed* in Reviewing. The assertions remain correct (title in Reviewing → `WorkflowWrongState`), so this is a naming/clarity nit, not a behavioral bug. Consider renaming to `update_plan_rejects_structural_fields_in_reviewing` (or adding a one-line comment noting regression_test-only is the now-allowed exception) so a future reader isn't misled.

---

### Constitution checks
- **Warning-free under `#![deny(warnings)]`**: the diff introduces no obvious warning sources (`let _ =` is intentional; `mut wf` is used; no unused imports/dead code). Could not run `cargo test` directly (read-only reviewer), but the reported 1705 passed / 0 warnings is consistent with the diff. ✓
- **Multi-platform neutral**: pure Rust workflow/filter logic — no Windows-only APIs, paths, or shell syntax. ✓
- **Doc comments on public items**: `FinishArgs.regression_test` (plan.rs:1039), `update_plan` method comment (mod.rs:508-513), `error.rs` doc, and both tool schemas are updated. ✓
- **Documentation sync**: no stale "Executing only" claim for `update_plan` exists in `README.md` or `PLAN.md` (verified — `PLAN.md:256` describes the primary verify-step `update_plan regression_test` path, still accurate; the finish-param fallback is an edge-case relaxation that doesn't require a doc update). The code-level docs (schema descriptions, error.rs) carry the change. No README/PLAN.md update required. ✓
