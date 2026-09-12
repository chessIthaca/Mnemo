# Review — Strengthen system-preamble workflow guidance

**Date:** 2026-04-09
**Plan:** Strengthen system-preamble workflow guidance (aed26b02)
**Scope:** All uncommitted changes (`git diff HEAD`): `src/agent/prompt.rs` (the deliverable), `src-tauri/src/main.rs` (prior-session `config.clone()` fix), and `.coding/` bookkeeping.

## Correctness

The three new preamble bullets (`src/agent/prompt.rs:19-30`) accurately describe the real state machine, cross-checked against `src/workflow/mod.rs` and `ToolFilter` in `src/tool/mod.rs`:

1. **Tool gating (lines 19-22)** — "only read tools + create_plan are visible" in PLANNING/COMPLETE. Confirmed: `ToolFilter::Planning`/`Complete` expose AutoRun (read) agent tools + `create_plan` (+ intentional exceptions `spawn_agent`/`skill_start`/`ask_user`/`current_plan`/memory). The phrasing is a deliberate simplification and is **identical to the pre-existing `WORKFLOW_LIFECYCLE` text** (lines 68, 86), so it introduces no new inaccuracy. The spirit ("you cannot edit files, run shell, or commit without a plan") is correct. ✓
2. **Research kind skips review (lines 23-26)** — Confirmed: `complete_step` routes a root `PlanKind::Research` plan → `Complete` directly with `reviewed=true` (`workflow/mod.rs:521-524`); backed by `research_plan_skips_reviewing_on_completion`. ✓
3. **Sub-plans any time during EXECUTING, parent resumes (lines 27-30)** — Confirmed: `create_plan_with_kind` does not clear the stack in Executing (`workflow/mod.rs:343`), pushing a sub-plan; `complete_step` pops it and resumes the parent at its next unchecked step (`workflow/mod.rs:507-511`); backed by `completing_sub_plan_pops_and_resumes_parent`. ✓

**`src-tauri/src/main.rs:242-245`** (`config.clone()`, prior session): Correct. `config` is a borrowed `&Config` (the match borrows `brain`), so it cannot be moved into `Mutex::new`; `.clone()` dereferences and produces the owned `Config` required. The explanatory comment is accurate. ✓

## Bugs

**Low — test robustness / potential false pass** (`src/agent/prompt.rs:374-403`):
The new test `preamble_emphasizes_workflow_tool_gating_research_and_subplans` calls `build_stable_head`, which concatenates **both** `CODING_SYSTEM_PREAMBLE` (the new bullets) **and** the pre-existing `WORKFLOW_LIFECYCLE` const (lines 64-98). The test's comment claims to verify the preamble carries the emphases "not rely on the later WORKFLOW LIFECYCLE map alone," but 4 of its 5 assertions would also pass from the lifecycle text alone:

| Assertion | In new preamble? | Also in `WORKFLOW_LIFECYCLE`? |
|---|---|---|
| `"gates your tools"` | yes (line 19) | **no** — unique anchor ✓ |
| `"enforced"` | yes (line 19) | yes (line 66) |
| `"research"` | yes (line 23) | yes (lines 70, 92, 96) |
| `"sub-plan"` | yes (line 27) | yes (line 76) |
| `"AT ANY POINT"` | yes (line 27) | yes (line 75) |

Only `"gates your tools"` genuinely ties the pass to the new preamble. The test passes for the right reason *today* (the preamble does contain all five), but it would **not** catch a regression that stripped the research/sub-plan/"AT ANY POINT"/"enforced" text from the preamble while leaving the lifecycle intact — i.e. 4/5 assertions are potential false passes relative to the test's stated intent.

**Suggested fix:** tighten those four assertions to preamble-unique substrings (e.g. `"Plan even for research/investigation work"`, `"create sub-plans AT ANY POINT during EXECUTING"`, `"this is enforced, not a suggestion"` as it appears in the preamble bullet), so each genuinely guards the preamble rather than the lifecycle. (The existing sibling test `lifecycle_encourages_research_planning_and_subplans` already covers the lifecycle, so the two tests should be disjoint.)

## Security

No findings. The change is prompt text + a unit test + a startup-fallback `Config` clone. No injection surface, path handling, or secrets involved.

## Constitution compliance

- **`#![deny(warnings)]` / no `#[allow(...)]`:** No `#[allow(...)]` suppressions added anywhere in the diff. ✓
- **Doc comments on public functions:** No new public functions added; the new test is private. Existing `build_stable_head` retains its doc comment. ✓
- **Line-ending style:** No mixed CRLF/LF introduced in source files. The git `LF will be replaced by CRLF` warnings are on `.coding/backlog.json` and `.coding/plans/28331246-*.md` (bookkeeping JSON/MD) — that is git's autocrlf on Windows normalizing on checkout, not a mixed-ending introduction in source. ✓
- **Build warning-free:** Presumed green per the plan's `cargo test` (reviewer is read-only and cannot run the build); nothing in the diff would introduce a warning.

## Bookkeeping sanity

- `.coding/backlog.json`: item 28 status `pending`→`in_flight`; its text matches this plan's goal. ✓
- `.coding/plans/stack.json`: active id switched to `aed26b02`, `reviewed:false` — correct for a new unreviewed plan. ✓
- `.coding/plans/28331246-*.md`: step 10 checkbox flipped `[ ]`→`[x]` — a prior plan's completion, consistent. ✓

## Summary

One low-severity finding: the new test's assertions are not all preamble-specific (4/5 would pass from the pre-existing lifecycle text), so it doesn't fully verify what its comment claims and could be a false pass under regression. Everything else — preamble correctness, the `config.clone()` fix, constitution compliance, security, bookkeeping — is clean.
