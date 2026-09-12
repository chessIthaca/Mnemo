# Review — Workflow output + status bar UX tweaks

**Date:** 2026-08-12
**Branch:** `feat/reviewing-workflow-state` (not main ✓)
**Scope:** All uncommitted changes per `git status` / `git diff HEAD`:
- `src/tool/workflow/plan.rs` — two output format strings + one test assertion
- `frontend/src/components/layout/StatusBar.tsx` — removed `header`/`headerSuffix` computation in the executing label
- `.coding/plans/0beb83ef-*.md`, `.coding/plans/stack.json`, `.coding/plans/c0387ccf-*.md` (untracked) — plan/bookkeeping state (skimmed, no source impact)

## Correctness

**complete_step output (plan.rs:337).** The new format string
`"Complete Step {n} ({completed}/{total}, {title}) — state: {state}"`
uses Rust implicit named-argument capture. All four captured locals are in
scope at the call site: `state` (329), `completed` (331), `total` (332),
`title` (333), and `n` is supplied explicitly via `n = args.step_index + 1`
(340). The em-dash `—` is a literal character in the string, not a format
argument. Interpolation is correct. ✓

**Test assertion (plan.rs:953-957).** `complete_step_echo_includes_plan_title`
creates a 1-step plan titled "Build Feature X" and completes step_index 0.
Expected values: `n = 0 + 1 = 1`, `completed = 1`, `total = 1`. Completing the
only step of a root plan transitions to `Reviewing` (verified at
`src/workflow/mod.rs:431`), so `state = Reviewing`. The actual output is
`Complete Step 1 (1/1, Build Feature X) — state: Reviewing`. The assertion
`result.output.contains("(1/1, Build Feature X)")` matches. ✓

(Note: the plan's prose mentioned a trailing period `Reviewing.` but the
actual format string has none — the original string had no trailing period
either, so this is consistent with prior behavior and the `contains` test
does not depend on it. Not a finding.)

**finish output (plan.rs:542).** `"Finish review step — review passed, state →
Complete (report: {})"` — the `{}` positional arg is filled by
`args.review_report` (543). Correct. The `data` JSON (545-548) still carries
`state` and `review_report` unchanged. ✓

**StatusBar executing label (StatusBar.tsx:379-385).** The IIFE now returns
`Executing ${x}/${total}` where `x = Math.min(completed + 1, total)`. `total`
(373) and `completed` (374) are still computed and still used (the
`completed >= total` Complete check at 378 and the `x` computation at 381).
No leftover references to the removed `current` / `header` / `headerSuffix`
locals (confirmed via search — the only remaining `header`/`step.header`
references are in the untouched popout dropdown at 595-598, which still shows
`step.header ?? step.text`). The current step's headline remains visible in
the dropdown, so no information is lost. ✓

**Structured data unaffected.** The `complete_step` `data` JSON still carries
`plan_title` (plan.rs:346), so any frontend consumer of the structured field
is unaffected by the `output` string change. ✓

No findings.

## Bugs

**Other tests asserting on old strings.** Searched the repo for
`Plan closed out`, `of '`, `done, state:`, and `Complete Step`. The only test
that asserted on the old `complete_step` output text was
`complete_step_echo_includes_plan_title`, which has been updated. No test
asserts on the `finish` output text — the three finish tests
(`finish_tool_closes_out_review`, `finish_without_review_report_errors`,
`finish_from_executing_errors`, plan.rs:720-779) assert only on `res.success`
and `wf.state()`. No integration test in `tests/` references these strings.
No frontend test asserts on the StatusBar `stateLabel` string (searched
`*.test.*` for `Executing`/`stateLabel`/`headerSuffix` — no matches). ✓

**Unused variables.** None. The removed `current`/`header`/`headerSuffix`
locals were the only consumers of the `plan?.steps.find(...)` lookup in that
branch; removing them entirely leaves no dangling binding. `total` and
`completed` remain used. ✓

No findings.

## Security

Output-string-only changes; no new I/O, no path handling, no privilege
changes. The `finish` tool's review-report path validation (under
`.coding/reviews/`, non-empty) is unchanged. No findings.

## Constitution compliance

- **Public function doc comments:** `CompleteStepTool` (plan.rs:250) and
  `FinishTool` (plan.rs:445) struct doc comments are intact; the `Tool` trait
  methods (`name`/`category`/`schema`/`safety`/`execute`) inherit the trait's
  documentation contract. No doc comments were removed. ✓
- **Windows 11 / PowerShell:** No shell commands introduced by this diff. ✓
- **No commits to main:** Working tree is uncommitted on
  `feat/reviewing-workflow-state`; no merge/push to main in the diff. ✓
- **Line-ending style:** The `file_edit`/`file_write` tools normalize to the
  detected style; the diff shows clean LF→LF for the source files (the only
  CRLF warning is on the `.coding/plans/*.md` bookkeeping file, which is
  git's autocrlf notice, not a mixed-ending introduction). ✓

No findings.

## Overall verdict

**Ship.** The diff is a clean, minimal, three-point UX tweak. Both format
strings interpolate correctly with all captured locals in scope; the one
affected test assertion matches the actual new output; the StatusBar has no
leftover variable references and loses no information (the headline stays in
the dropdown); the structured `data` JSON (including `plan_title`) is
unchanged so frontend consumers are unaffected; no other test asserts on the
old strings; doc comments and the never-commit-to-main rule are respected.
No findings across all four categories.
