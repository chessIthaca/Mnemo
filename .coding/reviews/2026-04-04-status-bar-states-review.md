# Review: Status bar shows Planning/Skill states + always up-to-date

**Date:** 2026-04-04
**Scope:** All uncommitted changes (`git diff HEAD` + untracked files).
**Plan:** Make the status-bar workflow label show all four states (Planning,
Executing, Complete, Skill) with consistent capitalization.

## Files changed

- `frontend/src/components/layout/StatusBar.tsx` — the substantive change
  (`stateLabel` computation, lines 304–325).
- `.coding/plans/1c2857d9-….md` — bookkeeping (step 6 checked off).
- `.coding/plans/stack.json` — bookkeeping (active plan id swapped).
- `.coding/plans/fecec2c8-….md` (untracked) — the new plan file.

## Verdict: no findings

The diff is clean. Detailed correctness analysis below.

### Correctness

- **All four states render with correct capitalization.** `workflowState` is
  typed `WorkflowState | null` where `WorkflowState = "planning" |
  "executing" | "complete" | "skill"` (lowercase, `types.ts:41`). The store
  writes only lowercase values — `event.state as WorkflowState`
  (`useAgentStore.ts:927`) and `wf.state` from `getWorkflowState`
  (`StatusBar.tsx:122`). `st = (workflowState ?? "").toLowerCase()` normalizes
  to lowercase, so the explicit branches (`st === "planning"` → "Planning",
  `st === "skill"` → "Skill", `st === "executing"` → "Executing",
  `st === "complete"` → "Complete") match correctly. ✅

- **"executing" with no plan is handled.** A brand-new plan with 0 steps, or
  the transient window where `WorkflowStateChanged(Executing)` has fired but
  `refreshPlan()` hasn't fetched the plan yet (`plan === null` → `total === 0`),
  skips the `st === "executing" && total > 0` branch and falls to the bare
  `if (st === "executing") return "Executing"` (line 318). No x/y shown when
  there's no plan data — correct. ✅

- **Branch order is correct.** `executing && total > 0` (line 313) precedes the
  bare `executing` (line 318), so the x/y form is preferred whenever a plan
  exists. Reversing them would always short-circuit to "Executing". ✅ The
  `complete` branch (line 312) is first, which is correct — `st === "complete"`
  wins regardless of plan, and `total > 0 && completed >= total` also catches
  the all-steps-done transient. This is pre-existing behavior, not a
  regression. ✅

- **Unknown-value fallback is null/empty-safe.** When `workflowState` is
  `null`, `st = ""`, no branch matches, and the fallback
  `return workflowState ? … : ""` yields `""` (the `&&` in
  `{stateLabel && <span>…}` then renders nothing). No crash. For a non-empty
  unknown string it capitalizes the first char. ✅

- **No division-by-zero / empty-plan edge.** `total > 0 && completed >= total`
  short-circuits when `total === 0`, so a 0-step plan can't trigger "Complete"
  via the step-count path. ✅

### Bugs
None. No case produces a wrong or empty label (empty only when no state is
known, which correctly renders no label).

### Security
None expected — pure display. The label is rendered as text content in a
`<span>`; React escapes it. No injection surface.

### Constitution compliance
- Frontend-only change; no Rust touched. Windows/PowerShell rules N/A.
- No commit to `main` (changes are uncommitted on the feature branch).
- `tsc --noEmit` clean and `cargo test` passes (459 tests) per the task
  description.
- Plan bookkeeping files are valid (markdown checkbox flip; `stack.json` is
  valid JSON; new plan file is well-formed). No corruption.
