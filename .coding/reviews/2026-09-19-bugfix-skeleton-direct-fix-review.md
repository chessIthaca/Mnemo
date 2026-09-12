## Verdict: PASS

The revert of the bug_fixing skeleton's locked step 3 from "Minimal fix via sub-plan" back to a direct minimal-fix step is correct, complete, and well-tested. All focus areas verified — zero findings.

### Scope reviewed

`git diff HEAD` over the working branch. Core change (the plan goal):
- `src/tool/workflow/plan.rs` — `BUG_FIXING_SKELETON` step 3 + doc comment + retargeted test pin.
- `PLAN.md` — bug_fixing bullet restored to direct-fix description.
- `.coding/knowledge/decision/2026-08-28-...md` — marked `status = "superseded"`.
- `.coding/knowledge/decision/2026-08-30-...md` (new) — successor stating the current direct-fix decision.

Also present but unrelated to this plan (not flagged): `.coding/backlog.jsonl` state churn, `.coding/plans/ca136f5b.md` + `.coding/plans/2c1fed06.md` + `.coding/reviews/2026-09-19-tcp-nodelay-verify-review.md` (a separate tcp_nodelay plan/review), and untracked `docs/` presentation materials.

### Focus-area verification

**1. New step-3 text + doc comment match (no stale "via sub-plan").**
`BUG_FIXING_SKELETON[2]` (plan.rs:44-45) now reads:
> `**Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.`

The const's doc comment (plan.rs:32-36) reads `...document the root cause → minimal fix → verify...` — "minimal fix", no "via sub-plan". The two are consistent. ✓

**2. Retargeted test genuinely pins the new wording (fails on revert, passes on new).**
`create_plan_bug_fixing_forces_skeleton_and_persists_symptom` (plan.rs:1709-1710) asserts:
```rust
plan.steps[2].text.contains("Minimal fix")
    && plan.steps[2].text.contains("regression test pass")
```
- New step-3 text contains **both** substrings ("**Minimal fix**" + "makes the regression test pass") → assertion holds.
- Old step-3 text ("**Minimal fix via sub-plan** — Push a sub-plan...LEAST amount of change...") contains "Minimal fix" but **not** "regression test pass" → the `&&` fails. The test would correctly fail if step 3 were reverted.

*Observation (not a finding):* the substring "Minimal fix" is non-discriminating in isolation — the old text also began "**Minimal fix via sub-plan**", so it matches both old and new. Discrimination rests entirely on the second substring "regression test pass". This is adequate (the `&&` makes the assertion fail-on-revert / pass-on-new as required), just slightly less robust than the prior pin which had two independently-discriminating substrings. No action needed.

**3. No stale "sub-plan" references in the skeleton const, its doc comment, or the PLAN.md bullet.**
A full-tree search for `via sub-plan` / `LEAST amount of change` / `fix step pushes a` confirms: the only live `.rs` "minimal fix" references are the const (plan.rs:44), the doc comment (plan.rs:34), and the test failure message (plan.rs:1711) — all correct. The 8 `sub-plan` hits inside plan.rs are all legitimate sub-plan mechanics (create_plan pushing sub-plans, branch handling, plan-depth assertions) — none in the skeleton const or its doc comment. All remaining "via sub-plan" hits live in immutable historical records (`.coding/plans/*.md`, the superseded decision file, the 2026-02-13 review) which must not be edited. The arrow shorthand `reproduce→root-cause→fix→verify` in README.md / prompt.rs:75 is intentionally left and stays accurate. ✓

**4. The two append-refused tests are correctly left unchanged.**
- `update_plan_tool_append_refused_for_bug_fixing_skeleton` (plan.rs:2745) asserts `result.output.contains("locked 4-step skeleton")`.
- `update_plan_append_refused_for_bug_fixing_skeleton` (src/workflow/mod.rs:1979) asserts `err.to_string().contains("locked 4-step skeleton")`.

Both assert on the lock error message, not step wording, so they are unaffected by the step-3 text change. Neither appears in the diff. ✓

**5. Doc-sync: PLAN.md bug_fixing bullet matches the const.**
PLAN.md:253 reads `document root cause → minimal fix → verify` — identical shorthand to the const doc comment (plan.rs:34). ✓

**6. No `#[allow(...)]` suppressions; build stays warning-free.**
The diff adds no `#[allow]`. The crate is `#![deny(warnings)]` at both roots, so the reported green `cargo test` (1652 passed, 0 failed, 16 ignored, exit 0) already proves zero warnings. ✓

**7. Multi-platform neutrality.**
The change is a pure string-constant edit + a test-assertion retarget. No Windows-only APIs, paths, or shell syntax introduced. ✓

**8. Knowledge supersession is clean.**
- Old file `2026-08-28-...md`: `status = "superseded"` added to frontmatter; original 2026-02-13 content retained verbatim as history.
- Successor `2026-08-30-...md`: frontmatter carries `supersedes = "2026-08-28-..."`; body states the current decision (step 3 applies the minimal fix DIRECTLY, no sub-plan), cites DECISION memory 544111c5 as the authoritative rationale, explicitly notes it REVERSES the 2026-02-13 decision (plan 1ef2bfa3, commit f46076e), and gives the root rationale (a bug_fixing sub-plan can only itself be bug_fixing → plan-loop generator; user aborted plan b840f00c). Code pointer (plan.rs ~:44-45) is accurate. ✓

This supersede-with-successor approach (keep old as history + new successor) is the correct memory-hygiene pattern and matches the focus area's expectation.

### Side observations (not findings, no action required)

- **`.coding/backlog.jsonl`**: the backlog item describing this very task (4bd98d68) is marked `status: "failed"` with note "plan loop never ran this turn (workflow rested in Complete) — task not done", even though the work is clearly complete. This is a symptom of the known deferred-success bug in `TurnResolveLatch` (memory 22e1f2cc), not a defect introduced by this change. Several completed backlog items were also pruned in the same churn — workflow-managed state, not hand-edited.
- **Unrelated untracked files**: `docs/` presentation materials (`.pptx`, `brand.md`, `build_deck.py`, `__pycache__/`, `_preview/`, and a PowerPoint lock file `~$Mnemo-...pptx`) plus the tcp_nodelay plan/review are present in the working tree but are not part of this plan's change set. The `~$` lock file is a temp artifact from an open presentation and should not be committed, but that is outside this plan's scope.

### Conclusion

The revert faithfully restores the pre-1ef2bfa3 direct-fix wording in the locked skeleton, syncs the doc comment and PLAN.md, retargets the regression test so it genuinely pins the new text, leaves the append-refused tests and the arrow shorthand untouched, and records the decision reversal cleanly via knowledge supersession. No correctness, bug, security, documentation-sync, or multi-platform findings. PASS.
