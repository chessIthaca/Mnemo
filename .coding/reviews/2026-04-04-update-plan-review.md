# Review: `update_plan` tool + `abandon_plan` "destructive last resort" re-framing

Date: 2026-04-04
Reviewer: read-only reviewer subagent (`spawn_agent`)
Scope: **all** uncommitted changes in the working tree (`git status` / `git diff`):

- `.coding/plans/026be09d-...md` (plan step 5 checked off — bookkeeping)
- `.coding/plans/stack.json` (active plan id changed to a new plan — bookkeeping)
- `.coding/plans/57e7ab3a-...md` (new plan file, untracked — bookkeeping)
- `src/agent/factory.rs` (register `UpdatePlanTool`)
- `src/agent/prompt.rs` (Executing prompt mentions `update_plan`/`abandon_plan`)
- `src/tool/mod.rs` (ToolFilter + module doc + tests)
- `src/tool/workflow/plan.rs` (`UpdatePlanTool` + abandon description)
- `src/workflow/mod.rs` (`Workflow::update_plan` + 4 tests)

What the plan set out to do: add a non-destructive `update_plan` tool so the agent
can adjust an in-flight plan (preserving completed steps) instead of abandoning;
re-frame `abandon_plan` as a destructive last resort; keep `abandon_plan`
`NeedsApproval` (unchanged). `cargo test` reported passing (399+9+5, 0 failures).

---

## Findings

### Minor — `appended` count / "N step(s) added" wording is misleading when steps are trimmed or replaced
**File:** `src/tool/workflow/plan.rs`, `UpdatePlanTool::execute` (≈ lines 206-225)

`appended = total.saturating_sub(before)` where `before` = step count *before* the
update and `total` = step count *after*. The output says
`"{appended} step(s) added"`. The schema description tells the user they can
"provide fewer to trim," but:

- If the agent *replaces* the remaining steps with a different (equal-length) list,
  `total == before` → `appended == 0` → output says "0 step(s) added", even though
  the plan *did* change (steps were rewritten). That reads as "nothing happened."
- If the agent *trims* (fewer remaining steps than before), `total < before` →
  `appended == 0` (saturating) → again "0 step(s) added", while steps were actually
  *removed*.

The value `appended` only means "net increase in total step count," which is a poor
proxy for "steps changed." Not a correctness bug (the plan is rewritten correctly),
but the human-readable output can mislead the agent into thinking the update was a
no-op when it wasn't. Suggested fix: report e.g. `"{remaining} remaining step(s) set"`
(the length of the new `steps` list provided), or `"{before} → {total} steps"`, and
drop the word "added." Low severity because the `data` block and the plan file are
authoritative; only the prose is affected.

### Minor — Out-of-order completion causes silent data loss of later-done steps
**File:** `src/workflow/mod.rs`, `Workflow::update_plan`, the completed-prefix split
(≈ lines 230-248)

The completed-prefix logic is:
```rust
let split = frame.plan.steps.iter().position(|s| !s.done)
    .unwrap_or(frame.plan.steps.len());
let mut combined: Vec<Step> = frame.plan.steps[..split].to_vec();
// ... append new_steps ...
```
This preserves `steps[..split]` and discards `steps[split..]`. Under **in-order**
completion this is exactly the done steps and is correct (covered by tests). **But
`complete_step` does NOT enforce in-order completion.** `PlanFile::complete_step`
(`src/workflow/plan_file.rs:158-168`) marks *any* in-range index done regardless of
order, and `Workflow::complete_step` just forwards it. The `CompleteStepTool` takes
an arbitrary `step_index` and the prompt only *advises* the agent to complete steps
in order ("When a step is done, call complete_step") — there is no guard preventing
the model from completing step index 2 while step 0 is still unchecked.

If that ever happens, e.g. steps `[a(done), b, c(done), d]`, then `split = 1` (first
not-done = `b`), so the preserved prefix is `[a]` only; the already-completed `c`
lies at index 2 ≥ `split` and is **dropped**, its done-state lost. The agent would
see its completed work disappear from the plan. The doc comment on `update_plan`
acknowledges this implicitly ("Under in-order completion (the normal flow) this is
exactly the done steps; any later not-done step is part of the remaining work and is
replaced") but frames it as a feature, not a hazard.

Assessment: out-of-order completion is *possible* by the code (no enforcement), even
if *unusual* in practice. This is a latent data-loss footgun. Recommended mitigations
(pick one): (a) preserve *all* done steps regardless of position by keeping every
`step.done` entry and inserting the new steps at the first not-done position (more
forgiving); or (b) at minimum, detect the out-of-order case (`steps[split..]`
contains any `done == true`) and return an error telling the agent to complete steps
in order before updating, so the loss is never silent; or (c) add an in-order
assertion to `complete_step` itself so the precondition always holds. I'd flag this
**Major** if out-of-order were common, but since completion is advised in-order and
the tooling nudges that direction, I'm recording it as **Minor** with a request to
at least make the failure non-silent. Worth a follow-up test that constructs the
out-of-order case and asserts the chosen behavior.

### Nit — `args.steps.clone()` is an avoidable allocation
**File:** `src/tool/workflow/plan.rs`, line 204

`args.steps.clone()` is passed to `wf.update_plan(..., args.steps.clone())` while
`args` is otherwise consumed only for `title/goal/context` via `.as_deref()`. Since
`args` isn't used again after this call, you could move `args.steps` directly
(`wf.update_plan(args.title.as_deref(), args.goal.as_deref(), args.context.as_deref(), args.steps)`)
and drop the clone. Trivial; no behavioral impact.

### Nit — `wf.plan().unwrap()` after a successful `update_plan`
**File:** `src/tool/workflow/plan.rs`, line 207

After `update_plan` returns `Ok(())` the workflow is guaranteed `Executing` with a
non-empty stack, so `wf.plan().unwrap()` cannot panic. The unwrap is safe by
construction. Recording only for completeness — no change needed. (A `let Some(plan)
= wf.plan() else { ... }` would be more defensive but is unnecessary.)

---

## Explicit verifications (all pass)

- **`abandon_plan` safety level genuinely unchanged:** `AbandonPlanTool::safety`
  returns `SafetyLevel::NeedsApproval` (`src/tool/workflow/plan.rs:350-353`) —
  identical to pre-change. Only the *description* was updated to "destructive last
  resort / prefer update_plan." The user's requirement (abandon needs approval) was
  already satisfied and remains so. ✅
- **AutoRun safety for `update_plan` is consistent:** `UpdatePlanTool::safety`
  returns `AutoRun`, matching `CreatePlanTool`. `update_plan` only rewrites the
  sandboxed `.coding/plans/<id>.md` file in place (same id), preserves completed
  steps, keeps `stack.json` unchanged, and never mutates project source. This is
  the same non-destructive posture as `create_plan`. ✅
- **ToolFilter visibility:** `update_plan` is listed **only** in the
  `ToolFilter::Executing` branch (`src/tool/mod.rs:151-156`). The `Planning`
  branch is an allowlist of `name == "create_plan"` only (line 138) and `Complete`
  is `name == "create_plan"` only (line 167), so `update_plan` is correctly
  **hidden** in both. Tests assert hidden in Planning (line ~409) and Complete
  (line ~451), visible in Executing (line ~430). ✅
- **Serde `Option` + `#[serde(default)]`:** `UpdatePlanArgs` uses
  `#[serde(default)] Option<...>` on all four fields. With `serde_json`, an
  **absent** key → `None` (via `default`), and an explicit **`null`** → `None`
  (serde maps JSON null to Option::None by default). Both deserialize cleanly; the
  blank-string case is handled by the `!t.trim().is_empty()` guard in
  `Workflow::update_plan`. ✅
- **Completed-prefix logic under in-order completion:** correct — `position(!done)`
  yields the first unchecked index; `steps[..split]` is exactly the done prefix;
  new steps appended with fresh contiguous indices. The unit test
  `update_plan_appends_steps_preserving_completed_prefix` and the tool test
  `update_plan_appends_steps_preserving_completed` both confirm this. ✅
- **State guard:** `update_plan` errors with `Workflow` error when state !=
  `Executing` (Planning: no plan; Complete: plan done). Tested by
  `update_plan_errors_outside_executing`. ✅
- **Noop / empty / blank guards:** empty `steps` → `InvalidInput`; all-`None` →
  `InvalidInput("changed nothing")`; blank-only strings treated as noop → error.
  Tested by `update_plan_rejects_empty_or_noop` and the tool test
  `update_plan_noop_errors`. ✅
- **Re-indexing:** after the edit, `combined.iter_mut().enumerate()` reassigns
  `s.index = i`, so step indices stay contiguous and match display order. ✅
- **Plan id + stack.json unchanged:** `update_plan` writes via
  `frame.plan.write_to_dir(&self.plans_dir, &frame.id)` (same `frame.id`) and never
  calls `persist_stack`. The id-preservation test confirms. ✅
- **Constitution — doc comments:** `Workflow::update_plan` has a full doc comment;
  `UpdatePlanTool` struct is documented; `UpdatePlanTool::new` lacks a doc comment
  — **but this matches the pre-existing style**: `CreatePlanTool::new`,
  `CompleteStepTool::new`, and `AbandonPlanTool::new` are *all* undocumented
  `pub fn new` constructors (lines 58, 238, 319). The constitution says "follow the
  existing code style," and the existing style is that these trivial constructors
  carry no doc comment. Consistent, not a violation. ✅ (flagging only so it's a
  conscious decision; if the team wants strict doc-coverage, all four `new`s should
  be done together in a separate pass — out of scope here).
- **No Windows-path / shell-safety issues:** no shell or path-construction code in
  the diff; `write_to_dir`/`plans_dir` are pre-existing and unchanged. ✅
- **No security issues:** no user-controlled path interpolation, no eval, no
  unbounded input growth beyond plan size (same bounds as `create_plan`). ✅

---

## Verdict

**Minor findings only — no Major or correctness-breaking issues.** The change is
sound, well-tested, and consistent with the existing `create_plan`/`abandon_plan`
design. Two things worth addressing before/after merge:

1. The `appended`/"N step(s) added" wording misrepresents trim/replace-equal cases
   (Minor, output-only).
2. The completed-prefix split silently drops later-done steps if completion ever
   happens out of order, which the code permits (Minor today, latent data-loss;
   recommend making it non-silent).

Both are non-blocking for the stated goal. `abandon_plan` remains correctly
`NeedsApproval`. The plan's deliverables are met.