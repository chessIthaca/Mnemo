## Verdict: FINDINGS (5 high, 3 low)

**Scope**: round-2 verification of the uncommitted changes on `wt/agenticcoding` for plan ffd7a86f (parallel run-all) — each of the 7 round-1 fixes re-verified in context, plus a defect hunt over the fix code itself (races, lock ordering, logic inversions, termination).

**Summary**: All seven round-1 fixes are present and their named mechanics are correct — the five gated `end_run` sites, the main-lane exclusion, the worktree-aware plans dir, the tests, the shared-prefix helpers, the docs, and the skip-set fill. But the H1 fix is incomplete and its wind-down variant is broken in two independent ways: the wind-down never clears `current_item` (so it can never terminate), and — because the event forwarder routes by a live `main_agent_id()` comparison while spawned run-all agents are parentless — the wind-down actively misroutes a spawned lane's resolutions into the MAIN resolution path (cross-stamping plus a run that can never end). Three further `end_run` sites were never gated, the H2 exclusion still has a selection-vs-record TOCTOU that can double-dispatch an item, and `drain_spawned_on_exit` never re-drives the run (a crashed lane with an idle main lane stalls the run; the last lane crashing during a wind-down leaks the run state forever).


---

## Round-1 fix verification (all 7)

### H1 — premature `end_run` orphans in-flight lanes → **fix present at the five named sites; incomplete overall (see R1–R3)**

All five sites round 1 named are gated, verified in the working tree:
- (a) `run_all_dispatch_next` no-item arm — run_all.rs:2621-2632: `if !any_lane_in_flight(state).await { end_run(...) }` with the correct "InFlight items are invisible to the Pending-only selector" rationale. ✓
- (b) `on_main_turn_resolved` stopped path — run_all.rs:3950-3959. ✓
- (c) `halt_run_all` no-item arm — run_all.rs:4219-4225. ✓
- (d) `compact_then_dispatch_next` stop path — run_all.rs:2549-2559. ✓
- (e) `drain_run_all_on_main_exit` — run_all.rs:4456-4479: sets the stop flag when spawned lanes are in flight instead of ending. Present — **but the wind-down is defective in two independent ways (R1, R2)**.

`any_lane_in_flight` (3078-3081) / `lanes_in_flight` (3090-3096) correctly cover both lane kinds (main `current_item` OR any spawned entry), and the spawned sibling's stopped check (3407-3421) matches. The verification criterion "no path leaves the run state `Some` forever" **fails**: the stale-`current_item` wind-down (R2), the wind-down misroute (R1), `drain_spawned_on_exit`'s stopped path (R5), and the send-failure strand (R6) all leave it `Some` forever. Three further `end_run` sites were never gated (R3).

### H2 — main-lane double-dispatch → **fix present; TOCTOU residual (R4)**

The exclusion is built under the run-state lock (run_all.rs:2594-2614): current_item + all spawned ids, selection via `next_pending_eligible_excluding(&exclude)` (2615-2620). The no-item arm still ends the run correctly when nothing is excluded and nothing is in flight — an empty exclude is semantically identical to the plain selector (`exclude.contains` on an empty slice), so the sequential path is byte-identical. ✓ Lock-order note: the exclude block takes `spawned` then `current_item` (sequentially — the spawned guard is a statement temporary dropped at the semicolon, so the two are never held simultaneously; no deadlock is possible either way), which contradicts the "current_item BEFORE spawned" comments in `fill_spawned_window`/`lanes_in_flight` — cosmetic (R8). The deterministic double-dispatch is closed; a selection-vs-record race window remains (R4).

### H3 — `orphan_work_landed_at` wrong plan path → **fix complete ✓**

`spawned_plans_dir(worktree, agent_id)` (4381-4391) = `<worktree>/.coding/plans/agents/<agent_id>/` — **exactly** matching spawn.rs's WorktreeBinding branch (`binding.worktree.join(".coding").join("plans").join("agents").join(agent_id.to_string())`), verified side by side. All three callers updated: `on_spawned_turn_resolved`'s `closed_earlier` (3221), `drain_spawned_on_exit`'s landed check (3467), `stamp_spawned_in_flight`'s title lookup (3593-3594). The main path (`orphan_work_landed`, 4330-4341) still passes `root/.coding/plans`. The `root` argument for spawned callers is the worktree (commits-after-checkpoint runs against the item's branch). ✓ Residual: the run-start adoption sweep still uses the main-tree predicate for spawned items (R7).

### L1 — no behavioral coverage → **delivered, with caveats**

All four tests exist and pin what they claim: `lanes_in_flight_covers_both_lane_kinds` (behavioral, state-level — both lane kinds keep the run alive), `spawned_plans_dir_lives_under_the_agent_subdir` (behavioral), `end_run_is_gated_on_lanes_in_flight_everywhere` + `main_lane_selection_excludes_spawned_items` (source contracts — the repo's established idiom for AppHandle-dependent wiring), plus the behavioral store test `next_pending_eligible_excluding_skips_handed_out_ids` (src/backlog.rs). They would catch a regression that removes any of the five gates or the exclusion call. Caveats: the `>= 4` guard-count pin can only count guard-form sites — it cannot catch `end_run` sites that don't use the guard at all (exactly R3's class), and nothing pins the wind-down's termination (R1/R2), the drain's re-drive (R5), or the selection race (R4).

### L2 — `item_branch` dead code + triplicated derivation → **fix complete ✓**

`item_short_id`/`item_branch` (src/project/worktrees.rs:41-50) are now used by `provision_item_worktree_impl` (77-79, worktree dir + branch) and `dispatch_spawned_item` (run_all.rs:2989-3000, the agent name). The only `chars().take(8)` in the codebase is inside `item_short_id` itself (searched). The shared-prefix test pins it.

### L3 — reviewer reports land in the main tree (undocumented) → **fix complete ✓**

README's parallel bullet and agent.md's branch policy both state that spawned items' reviewer reports land in the main tree's `.coding/reviews/` (shared by design) and are not carried by the item's branch merge — matching the behavior (the factory's main-derived `reviews_dir()` is unchanged; subagent root inheritance passes the parent's plans dir but reviews stay main-tree).

### L4 — `fill_spawned_window` stops on the first dispatch error → **fix works ✓ (residual R6)**

The skip set (2863-2927): a failed dispatch annotates the item ("parallel dispatch failed: …") and the fill continues; `skip` is folded into the next iteration's exclusion. Termination verified: each iteration either returns (no run / sequential / window full / nothing left) or grows the exclude set monotonically (a recorded spawned entry or a new skip), and the item pool is finite — so one stale branch no longer blocks the window. Residual: a send-failure after the record strands the entry (R6).


---

## Round-2 findings

### R1 (HIGH) — The wind-down misroutes a spawned lane's events into the MAIN resolution path (live `main_agent_id()` routing)

The event forwarder routes Finished/Error/Executing-stamp by a **live** comparison: `let is_main = mgr.main_agent_id() == Some(agent_id);` (events.rs:653 for Finished; the Error arm follows the same pattern; the Executing-stamp arm at events.rs:466-488). `main_agent_id()` (src/runtime/mod.rs:137-143) is *the smallest-id parentless agent* — and spawned run-all agents are **parentless** (`spawn_run_all_agent` → `spawn_agent_shared(..., None /* parentless */, ...)`). The Exited arm removes the main agent from the manager (events.rs:731-734) *before* calling `drain_run_all_on_main_exit` (events.rs:763), which now **winds down** (keeps the run, sets `stop`) instead of ending it.

So during the wind-down, whatever parentless agent now holds the smallest id — typically a spawned lane — **is** `main_agent_id()`:

- Its workflow's Executing entries route to `stamp_backlog_in_flight` (the MAIN path), which flips the stale `current_item` item — the requeued main-lane item, now `Pending` — back to `InFlight` and links it to the *lane's* plan (run_all.rs:4862-4874).
- Its Finished/Error route to `on_main_turn_resolved`, which resolves the stale `current_item` — and with the linkage now matching the lane's plan, `may_flip` can pass → the requeued item is marked `Done` with `commit_success` on the MAIN tree, while the lane's *actual* item is never resolved.
- The lane's own `SpawnedRun` entry is never removed by the main path → `spawned` never empties → `any_lane_in_flight` stays true → **the run can never end**; the lane's worktree/branch never land; the agent is never retired.

Round 1's unconditional `end_run` at main exit made this latent misroute a harmless no-op (the run was gone → `run_all_active=false` → single-dispatch branch → no-op). The wind-down makes it live — a NEW defect introduced by the H1 fix's site (e). The wind-down comment itself names the hazard ("`main_agent_id()` would now resolve to a spawned agent — mis-dispatching main-lane items into a worktree") but only guards the dispatch side, not the event-routing side.

**Fix direction**: route by ownership, not by a live `main_agent_id()` — check `owns_spawned_run(&app, agent_id)` BEFORE the `is_main` branch in the Finished, Error, and Executing-stamp arms (a spawned-run owner is never the main lane), or pin the main agent's id in `RunAllState` at run start and compare against that.

### R2 (HIGH) — The wind-down never terminates: `drain_run_all_on_main_exit` never clears `current_item`

run_all.rs:4397-4480: the drain reads `current_item` (4399-4409), requeues/marks the item (4410-4455), then the wind-down sets `stop` and returns — **without clearing the pointer**. Every subsequent end-of-run check reads it: `on_spawned_turn_resolved`'s stopped path (3407-3421, `current_item.is_some() || !spawned.is_empty()`), the no-item arm (2629), `halt_run_all`'s no-item arm (4222), `compact_then_dispatch_next`'s stop path (2556). With the stale pointer `Some`, `end_run` never fires → the run state stays `Some` forever: the UI shows the run active, `backlog_run_all` refuses new runs ("already running"), auto-feed stays off, and the stop button doesn't recover either (`halt_run_all` with `current_item` Some takes the annotate-and-keep arms, not the no-item `end_run` arm). Recovery requires an app restart.

Trigger: the main agent exits (crash/cancel) while its lane is in flight (`current_item` Some) and spawned lanes exist — the primary crash scenario the drain exists for. (When the main exits *between* items, `current_item` is already None and the wind-down terminates correctly.)

**Fix**: clear `current_item` (set `None`) after the drain handles the item, before the wind-down decision.

### R3 (HIGH) — H1 fix incomplete: three more `end_run` sites remain ungated

Round 1 listed five sites; the file has nine `end_run` call sites. Three remain unconditional:

- **run_all.rs:2692** — the main-lane checkpoint-failure arm: `end_run` while spawned lanes may be in flight. A git failure on the main lane during a parallel run ends the run → the lanes' resolutions find `run_all = None` → `owns_spawned_run` false → no-ops; their Exited drains no-op → stuck-InFlight items, unlanded branches, leaked agents (the exact H1 aftermath). Fix: gate it like the others — the failed item already stays `Pending` (annotated), and the run then ends when the lanes drain.
- **run_all.rs:2734** — the no-main-agent arm (edge: reachable only when the manager holds no parentless agent at all, e.g. a lane's Exited removed the last agent while its entry removal is still pending). Same gate applies.
- **run_all.rs:3684** — the intervention `ours` arm: a steer on the main agent during a parallel run, absorbed with the plan loop still closing → `finish_captured_item_done` → `ours` → `end_run` while spawned lanes work. Fix: set the stop flag and let the lanes wind down (mirroring the drain) instead of ending.

The delivered test `end_run_is_gated_on_lanes_in_flight_everywhere` counts `>= 4` guard-form `if !any_lane_in_flight` matches — it structurally cannot catch sites that don't use the guard form.

### R4 (HIGH) — H2 residual: selection-vs-record TOCTOU can still double-dispatch an item

The exclusion lists are only as good as the **recorded** state. `dispatch_spawned_item` (2935-3041) spans tens of seconds between the fill's selection (2899-2904) and the `SpawnedRun` record (3009-3022): a full `git worktree add` checkout + a checkpoint commit + an agent spawn + a cold codegraph index. During that window the item is `Pending` and absent from every exclude list. Symmetrically, the main path's selection→`current_item`-set window (its checkpoint, 2615-2703) is open to the fill's selection. The record-before-send discipline (round-1 verified good) closes the *resolution*-vs-record race, not the *selection*-vs-record race.

Concurrent `run_all_dispatch_next` callers exist: the between-items compact task (`tokio::spawn(compact_then_dispatch_next(...))`, run_all.rs:3984 — runs whenever `auto_compact_on_plan_complete` is on, i.e. the feature's primary overnight configuration), the IPC `backlog_run_all` task, and the forwarder's resolution handlers (serialized among themselves, but not with the other two). Realistic instance: the compact task's dispatch_next is mid-checkpoint on item X when a spawned lane resolves → the forwarder's dispatch_next also selects X (`current_item` not yet set) → X dispatched twice — two prompts to the main agent, or one to the main agent and one to a spawned lane. Or at run start: the IPC task's fill is provisioning item 2 when the main agent's first turn resolves quickly → the forwarder's dispatch_next selects item 2 for the main lane. Consequence: two agents on one item — the exact cross-corruption class H2 exists to prevent (two checkpoints, cross-stamping, the second resolution's `may_flip` guard failing → `remove_worktree = false` → worktree + branch leak). Narrow per-event window, but it accumulates over a long unattended run.

**Fix direction**: serialize dispatch decisions — a run-level `tokio::sync::Mutex` held across the whole `run_all_dispatch_next` (including the fill), or reserve item ids in the run state at selection time (cleared at record/stamp/skip).

### R5 (HIGH) — `drain_spawned_on_exit` never re-drives the run

run_all.rs:3438-3549: after draining a crashed/cancelled spawned lane (requeue to Pending, or landed → Done + cleanup), the function ends at `emit_backlog_changed` — no `run_all_dispatch_next`, no stop check, no `end_run`.

- **Stall**: a spawned lane crashes while the main lane is idle-and-empty — e.g. a 2-item run: item 1 resolves (main idle, run waits on lane 2), lane 2 crashes → the requeued `Pending` item is never dispatched; the run sits active with an idle main agent until the user intervenes (▶ or stop). If the main lane is busy the item is picked up at its next resolution (self-heals) — the stall is the small-backlog / tail-of-run case, and it defeats the unattended-overnight purpose.
- **Leak**: during a stopped wind-down, the LAST lane exiting *without* a turn resolution (crash/cancel) leaves the run `Some` forever — nothing calls `end_run` on this path.

**Fix**: mirror `on_spawned_turn_resolved`'s tail — after the drain, read the stop flag: if stopped and nothing in flight → `end_run`; otherwise `run_all_dispatch_next`.


### R6 (LOW) — `dispatch_spawned_item`'s send-failure strands the recorded entry

The `SpawnedRun` is pushed (3009-3022) BEFORE `manager.send` (3025-3037); if the send fails, the `?` returns Err and the fill's skip arm (2911-2922) annotates + skips the item — but the entry and the live (promptless) agent remain. `lanes_in_flight` is then true forever (the agent never resolves — it got no prompt; its Exited fires only at shutdown), the item is excluded from every selection forever, and the run can never end. Rare (a fresh agent's inbox failing) but the error path exists and leaks. **Fix**: on send failure, remove the entry + retire the agent before returning Err.

### R7 (LOW) — the run-start adoption sweep is not worktree-aware, and stale runall worktrees/branches are never cleaned

After an app crash mid-parallel-run, `adopt_orphaned_in_flight` (4528-4608) requeues spawned items using the MAIN-tree `orphan_work_landed` — always false for them (their plans live at `<worktree>/.coding/plans/agents/<agent_id>/`), so a genuinely-landed spawned orphan is requeued for duplicate re-execution. That is the safe direction (no false `Done`), but it is the same H3 class persisting at this site. Additionally, the stale `.worktrees/runall-<item8>` + `wt/runall-<item8>` (both derivable from the item id — the `SpawnedRun` entries died with the run state) are never removed: a later spawned-lane dispatch of the requeued item fails provision ("branch already exists") → the L4 skip arm annotates + skips → the item is permanently undispatchable via spawned lanes (it still works via the main lane), and the stale worktrees/branches accumulate. **Fix**: at adoption/run-start, remove the derivable stale worktree+branch for requeued spawned orphans (and optionally consult the worktree plans dir for landed evidence).

### R8 (LOW) — the exclude block's lock order contradicts the documented one (cosmetic — no deadlock possible)

`run_all_dispatch_next`'s exclude (2594-2614) locks `spawned` then `current_item`, while `fill_spawned_window` (2865-2884) and `lanes_in_flight` (3090-3096) document "current_item BEFORE spawned". No site ever holds both guards simultaneously (the exclude's spawned guard is a statement temporary dropped at its semicolon; fill's current guard is dropped before the spawned guard is bound), so there is no actual ordering constraint — but the comments imply an invariant the code does not follow. **Fix**: align the order or drop the order comments.

---

## Verified good (re-checked this round)

- **Lock discipline**: the std Mutexes (`current_item`, `spawned`) are scoped, never held across awaits; `run_all` → inner-mutex nesting is one-way everywhere; `emit_backlog_changed`'s `spawned_views` takes `spawned` under the `run_all` guard (consistent); the store lock is never held across git subprocesses (snapshot → evidence → re-verify pattern in both drains and the resolution paths).
- **LANDING_LOCK**: correct serialization (a static tokio Mutex held across the git ops; no other lock taken under it except the brief project-root clone). The landing worktree is reused with `reset --hard main` re-sync; conflicts capture `--diff-filter=U` before `merge --abort`; `--no-ff` keeps main merge-commits-only. All verified in worktrees.rs with real-git tests (provision/refuse-existing/remove/land/conflict-keeps-branch).
- **Resolution-vs-drain interleave**: safe — both run inline on the single event forwarder (Finished/Error precede Exited in the stream); the `still`/`still_in_flight` re-verify under the store lock before transitions is belt-and-braces.
- **Sequential-path preservation (N=1)**: `fill_spawned_window` returns at `concurrency <= 1` before any spawn; `owns_spawned_run`/`stamp_spawned_in_flight`/`drain_spawned_on_exit` no-op on an empty spawned set; the payload additions are additive (`parallel`, `concurrency`, `spawned: []`); the empty-exclude selection is semantically identical to the plain selector.
- **Window accounting**: `spawned_count + 1 >= concurrency` reserves the main lane (under-fills by one while the main lane is deferred — documented intent); the fill runs on all three dispatch exits (busy-guard defer 2660, checkpoint-race defer 2779, success 2841 — pinned by the source-contract test).
- **Spawned blocked-row arm** (3300-3307): mirrors the main path's LOW-2 behavior (annotate, no transition, lane dropped, item left for the next run-start adoption) — consistent, not a divergence.
- **AgentRootSpec plumbing** (factory.rs / loop_impl.rs / spawn.rs): the spec's sandbox/project_root/codegraph bind the worktree agent's tools — proven behaviorally by `build_with_root_spec_binds_tools_to_override_root` (a write lands in the worktree, never the factory root); `current_plan` reads the build's plans dir; the finish gate uses the spec's graph; subagents inherit the parent's root + plans dir (`parent_root_binding`, tested); a `None` spec graph never silently falls back to the main tree's graph.
- **Security / multi-platform**: `SpawnedRunView` exposes only agent_id/item_id/branch; git args are fixed strings + branch names from 8-char UUID prefixes passed as Command args (no shell); worktrees.rs is std::path + git subprocess + the CREATE_NO_WINDOW-hardened `git_raw` — no Windows-only APIs; doc comments on all new public functions; no `#[allow]` in the diff.
- **Frontend wiring**: the checkbox (session-only, never persisted), `backlogRunAll(parallelRunAll ? 3 : undefined)`, `resolve_run_all_concurrency` clamp 1..=8 (tested), the payload/store/fixture updates, and the progress line's "N parallel" are consistent end to end; no new frontend test files (BacklogView.test.ts is an existing registered file — no vitest include-list gap).

## Test matrix

**Not executed by this reviewer** — the reviewer tool surface is read-only (no shell). The claimed numbers (root cargo test 2130/0/4, src-tauri cargo test 268/0/0, frontend npm test 1060/1060) are the parent's to verify before commit. Static support: all new tests live in existing test modules/files (no registration gaps), and the diff introduces no warning-risk constructs (`deny(warnings)` at both crate roots). Note that a green matrix is consistent with these findings — R1–R5 live in the run-lifecycle integration seam (wind-down, crash drains, cross-task dispatch timing) that no current test exercises.
