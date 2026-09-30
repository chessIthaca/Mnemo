## Verdict: PASS

Round-2 delta review of plan 52ac5966 (escalation lane ladder, backlog ad56c7bd). Scope: `git diff 810bf51..3059042` — commit 3059042 only, i.e. the fix for round-1 finding L1. Everything up to 810bf51 was verified PASS in round 1 and was not re-reviewed. Read: the commit's full diff (stat + hunks), the touched regions of `src/agent/step_lanes.rs` (160–328, 445–544), `src/agent/turn.rs` (1140–1303), `src/agent/tests.rs` (14460–14569, 14860–14949), `PLAN.md` line 901, and the pre-existing `Workflow::plan_id` / `create_plan_with_kind_and_detail` (src/workflow/mod.rs 300–684) on which the fix's uniqueness premise rests.

**L1 fix verification — keying the lane memo by plan id, never title:**

1. **step_lanes.rs — complete.** `LaneState.plan_title` is gone; the field is `plan_id: String` (line 173) with a doc comment recording the L1 rationale. `needs_classify(state, plan_id, step_index, epoch)` (lines 269–283) returns `Fresh` when `current.plan_id != plan_id`, `ReClassify` on an epoch bump, `Reuse` otherwise — ids only. A literal search for "title" in the file hits only comments explaining why the title is NOT used (lines 168–171, 266–268, 489–492, 504); nothing title-shaped remains in the memo path, and both constructors (`decided`, `fallback`) take `plan_id`.

2. **turn.rs::step_lane_target — correct, fail-safe, lock discipline preserved.** The snapshot block (lines 1170–1175) takes `wf.plan_id()?.to_string()` under the workflow lock with no title fallback — a step without a plan id makes the `?` return `None`, so no lane routing happens at all. The guard is scoped in a block that ends before any await: the first `await` after the block is `handle.decide` at line 1195; only short-lived `std::sync` `lane_state`/`lane_failure` lock scopes sit in between. Same discipline as round 1 verified. Both match arms write the memo with the same `plan_id` they compared against (decided at 1234, fallback at 1265).

3. **Test evidence — genuinely red before the fix, no weakening.** Unit test `a_new_plan_id_is_fresh_even_at_the_same_step_and_epoch` (step_lanes.rs 488–506): a plan-b memo probe at plan-a's step 1 / epoch 3 asserts `Fresh` — exactly the L1 shape. Loop test `lane_memo_is_fresh_for_a_new_plan_that_repeats_the_title` (tests.rs 14872–14949): it creates plan "Lane plan", runs a turn (medium lane enforced, `passes == 1`), then creates a SECOND plan with the same title, same single step, unchanged epoch and asserts `passes == 2` with an explicit message naming the memo. Under the old title-keyed memo the second plan hit `Reuse`, served the memoized target without a classifier call, and `passes` stayed 1 — the assertion at 14944–14947 fails. Red before, green after. The `lane_agent_with_workflow` refactor is a pure seam extraction: `lane_agent` (14516–14537) still creates the workflow + plan and now delegates to the new helper, which performs the identical `AgentLoop::new(...).with_model_resolver/with_routing_gate/with_reflex` construction — the five pre-existing lane tests run the full path unchanged.

4. **PLAN.md — synced.** Line 901's memo sentence now reads "the same plan id + step (keyed by the plan's ID, NEVER its title…)". No other spot in PLAN.md claims the title keys the memo.

5. **Nothing else in the delta.** Commit 3059042 touches only PLAN.md (2 lines), step_lanes.rs, tests.rs, turn.rs — all fix-bearing. No unrelated behavior edits.

**Fix-correctness premise checked:** `create_plan_with_kind_and_detail` (src/workflow/mod.rs 650–661) mints a fresh UUID v4-based, collision-checked id per plan creation and pushes it on the stack; `plan_id()` returns the top frame's id. Unique per creation, stable across resume — the memo key's uniqueness premise holds.

**Constitution checks:** docs synced (PLAN.md updated in-commit); no platform-specific code in the delta; no shell-based file mutation; warning-free build is proven by the green `cargo test` under `#![deny(warnings)]` (dispatch context, not re-run); regression test covers the fixed path.

**Bookkeeping accuracy (one line):** the uncommitted `.coding/` items (`backlog.jsonl` update, `plans/52ac5966.md`, the round-1 report) match what shipped on the branch.

**Process remark:** the `.coding/` bookkeeping hunks remain uncommitted carry-over (round-1 verified them as accurate; not re-line-reviewed).

Reviewed-state: 3059042dfe735fa6664ca8c6f7e57c46d109d484
