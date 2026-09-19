## Verdict: FINDINGS (0 high, 2 low)

Review of all uncommitted changes on `wt/mnemo` for plan 660fdedc (bug_fixing, backlog 1d0332ca — skip the RECALLED CONTEXT rider for reviewer spawns). The code fix is correct, complete, and well-tested — zero code defects. Both findings are closing-sequence bookkeeping: the plan's `regression_test` field is not yet recorded, and no BUG: memory record exists yet for the defect.

## Scope

- `src/tool/agent/spawn_agent.rs` — the fix (role-gated rider skip), three synced doc comments, one new regression test.
- `.coding/backlog.jsonl` — item 1d0332ca flipped pending → in_flight with plan_id 660fdedc (app-managed, expected).
- `.coding/plans/660fdedc.md` — untracked plan file (must be committed with the change).
- Test status as reported by the main agent: full suite green (2485 passed, 0 failed, 5 ignored), warning-free, verified unpiped (exit=0). This reviewer is read-only and cannot re-run the suite; the logic review below is consistent with green.

## Correctness — the fix (PASS)

1. **The role check is exactly right.** `role` is resolved and validated at spawn_agent.rs:232-242: any non-empty role other than "reviewer" returns an error, so the only values that can reach the rider are `Some("reviewer")` (trimmed) and `None`. The guard `role.as_deref() == Some("reviewer")` (:334) therefore covers exactly the reviewer spawns and nothing else, and mirrors the existing reviewer-pin check at :295 — consistent style.
2. **Byte-identical pass-through holds.** The reviewer arm moves `args.task` into `task` untouched — no trim, no clone-and-modify, no normalization; the same deserialized String reaches `spawner.spawn(&args.name, &task, role.clone())` / `spawn_with_parent`. The else arm is the unchanged rider computation (`recalled_context_block(self.memory.as_ref(), None, &args.task, None)` → `format!("{}{rider}", args.task)`). Only one arm executes — no double move. Nothing else touches the task between deserialization (:216) and the spawner calls (:357-377).
3. **Placement preserves the existing invariants.** The skip sits after every rejection path (name/task/role/model/parent-aware), so "a rejected spawn never pays for a store round-trip" is kept — and the reviewer arm extends it: a reviewer spawn now skips the recall round-trip entirely, even on success.
4. **Reviewer-with-model edge (failed-reviewer retry):** the skip is independent of the model path, so a user-sanctioned retry reviewer also gets a clean task. Correct.
5. **Scope is complete — no other reviewer-prompt path.** `crate::tool::steering::recalled_context_block` has exactly two callers: `src/tool/workflow/plan.rs:899` (create_plan's result rider — plans, not reviewer prompts; unchanged) and the fixed spawn_agent site. The `recalled_context_block` in src-tauri/src/ipc/run_all.rs:426 is a separate function enriching run-all dispatched backlog-item prompts (worker agents — the rider's intended purpose); reviewers are always spawned through the spawn_agent tool, including from run-all items. The defect's only path is fixed.

## Regression test (PASS)

`reviewer_task_skips_the_recalled_context_rider` (spawn_agent.rs:1184-1248):

- Reuses the exact fixture of the existing green `task_seeded_with_recalled_context_rider` test (HashEmbedder + `MemoryStore::open_in_memory` + a fresh-timestamp Semantic seed titled "SPEC: the kettle safety valve opens above 2 bar", task "work on the kettle safety valve") — the identical store/task combination is proven to produce a matching hit, so the byte-identical assertion is meaningful, not vacuously green on "no hits".
- The reviewer spawn's task is asserted with `assert_eq!` against the exact task string — strictly stronger than a "does not contain RECALLED CONTEXT" check — and the captured role is asserted `Some("reviewer")` (MockSpawner records `(name, task, role)`; the tool passes `role.clone()`).
- The normal spawn on the same store still carries the rider (contains "RECALLED CONTEXT" and the seed title) — existing behavior pinned in the same test.
- **Fails-without-the-fix is logically certain:** the pre-fix code (the diff's removed lines) computed the rider for every spawn with no role check, and the identical fixture is proven to yield a rider by the existing green test — so pre-fix, `calls[0].1` would carry the rider and the `assert_eq!` would fail. (Matches the main agent's reported pre-fix failing run.)
- Existing tests are unchanged (the diff is pure addition in the test module); `task_seeded_with_recalled_context_rider` and `task_unchanged_without_store_or_hits` stay green per the reported suite run.

## Decision documentation (PASS)

The rider comment (:323-333) names the rationale (self-contained reviewer protocol; noise / review bias / prompt bloat) and the decision (role-based ONLY, not an exposed spawn option — no schema growth for no current consumer; unrestricted sub-agents keep the rider). This answers the backlog item's "consider whether the skip should be role-based only or also exposed as an explicit spawn option; document the decision in the rider's comment". The three doc comments (module doc :12-19, `memory` field doc :106-111, `with_memory` doc :143-148) are synced and accurate.

## Constitution checks (PASS)

- **Warning-free:** no new imports, no dead code, no unused bindings; `#![deny(warnings)]` + the reported green suite.
- **Doc comments on public functions:** `with_memory` updated; no new public items.
- **Multi-platform neutrality:** pure logic, no platform surface.
- **File-tools-first:** no shell-based file mutation in the diff.
- **Documentation sync:** the spawn_agent schema description (:165-204) carries no rider text — nothing to update, no budget impact (verified). PLAN.md:273-275 ("recalled_context_block … shared by create_plan and spawn_agent") remains accurate — spawn_agent still consumes the helper for non-reviewer spawns. steering.rs's module doc likewise remains accurate; the exemption policy is documented at the consumer, which is the right place. No README mention to update.

## Findings

### LOW 1 — the plan's `regression_test` field is not recorded yet

`.coding/plans/660fdedc.md` has no `## Regression test` section, while a completed bug_fixing plan that recorded the field renders one (995436c2.md: `## Regression test` → `repeat_failure_injects_schema_correction_on_second_identical_error`). Skeleton step 4 — checked — includes "Record the regression test name via update_plan (regression_test field) — finish is blocked without it."

**Fix:** one `update_plan` call recording `reviewer_task_skips_the_recalled_context_rider` before finish (otherwise the finish gate will block).

### LOW 2 — no BUG: memory record exists for backlog 1d0332ca

Two targeted memory searches (bug-typed, then title-phrase) found no BUG record for this defect — only the unrelated reviewer-model / null-model / correction-event bugs. Skeleton step 2 — checked — contracts "memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars)". Precedent: plan 995436c2's round-1 review flagged exactly this (its L2) and the records were written during fix-findings.

**Fix:** memory_write the BUG record before finish — symptom: every spawned reviewer's first prompt carried the recalled-context rider; root cause: the rider block ran for every spawn with no role check; fix: role-gated skip at spawn_agent.rs:334; regression test: `reviewer_task_skips_the_recalled_context_rider`. If the finish-time BUG auto-capture is the sanctioned path for this plan, state that justification explicitly when closing instead.

## Notes (no action required)

- The untracked `.coding/plans/660fdedc.md` must be included in the commit (the task already says so; restated so it isn't missed).
- The unchecked "Detailed steps" boxes in 660fdedc.md match the machinery's normal rendering for bug_fixing plans (the completed 995436c2.md likewise leaves its detailed steps unchecked) — not a finding.
- This reviewer's own task prompt arrived carrying a RECALLED CONTEXT block — the running binary predates the fix, as the task note predicted; the code and tests were judged, not the live spawn behavior.
