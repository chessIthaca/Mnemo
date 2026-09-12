## Verdict: FINDINGS (1 high, 2 low)

**Scope**: round-3 verification of the uncommitted changes on `wt/agenticcoding` for plan ffd7a86f (parallel run-all) — each of the 8 round-2 fixes re-verified in context against the working tree (events.rs, run_all.rs, worktrees.rs, spawn.rs, backlog_cmds.rs, the tests), plus a defect hunt over the fix code itself: termination of every wind-down path, the DISPATCH_LOCK's interactions with the forwarder, the deferred-resolution flush, and the adoption cleanup.

**Summary**: All eight round-2 fixes are present and their named mechanics are correct — the ownership-first routing in all four forwarder arms, the R2 pointer clear (exactly the specified placement), the three additional gated `end_run` sites, the DISPATCH_LOCK (no re-entrancy, no lock-order cycle), the drain's re-drive tail, the send-failure cleanup, the adoption worktree sweep (outside the store lock, derivable paths matching provision), and the R8 lock-order alignment. But the R3c intervention wind-down is defective in its closed-loop arm: the `ours` arm resolves the steered item Done via `finish_captured_item_done` and then sets the stop flag WITHOUT clearing `current_item` — the stale pointer keeps every end-of-run check true forever, so the run hangs active after the last lane drains (the exact R2 defect class, inside the R3c fix itself). Two further low findings: a spawned lane's deferred turn resolution has no flush path (recovery rides on a best-effort notification send), and `remove_item_worktree`'s early-return ordering can skip the branch delete.


---

## Round-2 fix verification (all 8)

### R1 — the wind-down misroutes a spawned lane's events into the MAIN path → **fix complete ✓**

All four arms verified in events.rs:
- **Executing-stamp** (465-493): `owns_spawned_run(&app, agent_id)` checked FIRST → `stamp_spawned_in_flight`; only the else branch computes the live `is_main` → `stamp_backlog_in_flight`.
- **Error** (606-652): `owns_spawned_run` first → `on_spawned_turn_resolved(false, Some(n), …)`; else `is_main` → `on_main_turn_resolved`.
- **Finished** (670-736): `owns_spawned_run` first → `on_spawned_turn_resolved` (Success/Failure); else `is_main`; the deferred-main-failure flush stays in the final `else` (children only).
- **Exited** (745-799): `let owns_spawned = owns_spawned_run(…)` then `was_main = !owns_spawned && mgr.main_agent_id() == Some(agent_id)` — captured BEFORE `mgr.remove` (the ordering pin holds).

A spawned lane's events can never reach the main paths, even after the main agent's exit (when the smallest parentless id is a lane): every main-path entry is behind the ownership check, and the Exited arm's `was_main` is ownership-gated. The `is_main` computation is preserved unchanged for non-spawned agents (the else branches). The reorder's effect on the deferred-main-failure flush is correct: spawned lanes are parentless — never the main's descendants — so their finish was never a meaningful flush trigger, and the Exited arm's unconditional flush (769) is unchanged. Residual live-comparison sites (compact-signal arms 511-512/526-527, the approval-halt arm 825-836) are provably benign in the only state where the misroute is live — the post-main-exit wind-down: stop is already set and `current_item` is None (the R2 clear), so both the halt and a spurious compact-signal bump are no-ops. Defensive ownership checks there would be nice-to-have, not required.

### R2 — the wind-down never terminates (drain never clears current_item) → **fix complete ✓**

`drain_run_all_on_main_exit`: the item handling (requeue / landed→Done, 4578-4622) → the pointer clear (4630-4635, unconditional — covers every outcome including the not-in-flight skip) → the wind-down decision (4643-4659: spawned lanes in flight → stop, else end_run). Exactly the specified placement: after the item transition, before the spawned_in_flight check. The wind-down now terminates: each lane's resolution/exit reads stop → `any_lane_in_flight` (current_item None + spawned empty) → end_run.

### R3 — three more ungated end_run sites → **(a) ✓ (b) ✓ (c) present but defective — see HIGH-1**

- (a) The main-lane checkpoint-failure arm (2782-2789): gated on `if !any_lane_in_flight(state).await` — the failed item stays Pending (annotated), the lanes' resolutions end the run when the last one lands. ✓
- (b) The no-main-agent arm (2830-2836): gated the same way. ✓
- (c) The user-intervention `ours` arm (3831-3853): winds down — checks the spawned lanes only (the steered main-lane item does not gate the end), sets the stop flag when lanes are in flight, else `end_run`. Present as specified — **but the arm leaves a stale `current_item`, so this wind-down can never terminate (HIGH-1)**. Note the arm's comment ("The steered item (the main lane) stays InFlight for the user's manual work by design") describes the OTHER intervention arm (`handle_user_intervention`, the loop-not-closed case) — in this arm the item was just resolved Done two lines above.

### R4 — selection-vs-record TOCTOU (double dispatch) → **fix complete ✓**

`let _dispatch_guard = DISPATCH_LOCK.lock().await;` at the top of `run_all_dispatch_next` (2674), held across the whole body including the spawned-window fill. Re-entrancy audit: nothing in the dispatch call tree calls `run_all_dispatch_next` — the tree is checkpoint → selection → manager busy checks → send → emit → fill → `dispatch_spawned_item` → provision / checkpoint / recalled_context_block / resolve_images / `spawn_run_all_agent` / record / send / emit; none re-enter. Lock-order audit: DISPATCH_LOCK is acquired only at dispatch_next's top; the run-state/store/manager/project-root locks inside are all scoped (acquired and dropped, never held across anything that acquires DISPATCH_LOCK); the three concurrent caller classes (IPC start task, compact task, forwarder arms) are all outside the lock. No cycle, no self-deadlock.

### R5 — drain_spawned_on_exit never re-drives the run → **fix complete ✓**

The drain's tail (3677-3696) mirrors `on_spawned_turn_resolved`'s: read the stop flag; stopped → `end_run` only when `!any_lane_in_flight`; otherwise `run_all_dispatch_next` (which dispatches the requeued item, refills the window, and ends the run when nothing eligible remains). Deadlock audit: the drain runs in the forwarder's Exited arm with NO locks held up that stack (the manager guard is dropped at 758; the agent_loops guard at 759 is a statement temporary); DISPATCH_LOCK is not held up that stack, and its holders never wait on the forwarder (sends are try_send, emits are synchronous, spawns don't wait on first events) — so the Exited arm's full dispatch_next await is safe (see the informational note below on its latency cost).

### R6 — a failed prompt send strands the recorded entry → **fix complete ✓**

`dispatch_spawned_item`: record (3110-3123) → send (3126-3137, `if let Err(e)`) → on failure: `remove_spawned_run` (3144) → `retire_spawned_agent` (3145) → `remove_item_worktree` with the LOCAL worktree/branch (3146-3151 — the same values cloned into the record) → `return Err` (3152). The fill's skip arm annotates the item (3016-3022). The retired agent's later Exited is a clean no-op: the entry is gone (`owns_spawned_run` false), the agent is gone from the manager (`was_main` false), the drain finds no entry.

### R7 — adoption sweep not worktree-aware + stale worktrees never cleaned → **fix complete ✓ (residual LOW-2)**

`adopt_orphaned_in_flight` collects the requeued ids (4787-4789), `drop(store)` (4792), then best-effort `remove_item_worktree` per requeued id (4800-4813) with the derivable paths — `.worktrees/runall-<item8>` + `wt/runall-<item8>` — matching provision exactly (worktrees.rs:33-50, 77-93). The cleanup runs OUTSIDE the store lock (blocking git ops never under it ✓). The landed check still uses the main-tree predicate (4747) — the safe direction (requeue) for spawned items, since the agent id is unknown at adoption. A main-lane orphan has no runall worktree → the remove fails at the worktree step and no-ops. Residual: the early-return ordering inside `remove_item_worktree` can skip the branch delete (LOW-2).

### R8 — exclude-block lock order vs the documented invariant → **fix complete ✓**

The exclude block (2682-2704) locks `current_item` BEFORE `spawned`, with the invariant comment — now matching `fill_spawned_window` (2966-2986) and the documented order in `lanes_in_flight`/`any_lane_in_flight`.


---

## Round-3 findings

### HIGH-1. The R3c intervention wind-down never terminates: the `ours` arm leaves a stale `current_item` after resolving the item Done

run_all.rs:3809-3854 (`on_main_turn_resolved`, the intervention closed-loop branch). When a steer on the main agent is absorbed AND the plan loop still closes in that same turn, the path calls `finish_captured_item_done(app, &state, &id)` — which commits and transitions the item **Done** (4983-5011) but never touches `current_item` — and then the `ours` arm (3831-3853) winds down: spawned lanes in flight → set the stop flag → return. **Neither `finish_captured_item_done` nor the ours arm clears the pointer.**

Every subsequent end-of-run check reads it: `on_spawned_turn_resolved`'s stopped path (3536-3545: `current_item.is_some() || !spawned.is_empty()`), `drain_spawned_on_exit`'s (3689), `halt_run_all`'s no-item arm (4390), `dispatch_next`'s no-item arm (2719). With the stale `Some`:

- Each lane's terminal resolution → stopped → `any_in_flight` true (the stale pointer) → no end_run.
- The LAST lane's resolution/exit → same → **the run state stays `Some` forever**: the UI shows the run active, `backlog_run_all` refuses new runs, and the stop button doesn't recover (`backlog_stop_all` only sets the already-set stop flag; `halt_run_all` with `current_item` Some takes the annotate-and-keep arms). A second steer doesn't recover either — the latch path re-runs, the item is already Done, the pointer survives again.

This is the exact R2 defect class (a stale `current_item` blocks wind-down termination) — inside the R3c fix itself. The arm's comment ("The steered item (the main lane) stays InFlight for the user's manual work by design — only the spawned lanes gate the end") is wrong for THIS arm: the item was just marked Done two lines above; that rationale belongs to `handle_user_intervention` (the loop-not-closed case), whose kept pointer is live and whose wind-down terminates correctly (the steered item resolves via the normal main resolution → the pointer clear at 4112 → the stopped path → the lanes' exits).

Recovery: close the main agent's tab (its Exited → `drain_run_all_on_main_exit` → the R2 clear → end_run) or restart the app.

**Fix**: in the `ours` arm, clear `current_item` after the `ours` determination and before/inside the wind-down — mirroring `drain_run_all_on_main_exit`'s R2 clear (4630-4635). The `else` (no lanes) branch already ends the run (`end_run` drops the state); only the stop branch needs it. Add a pin to `end_run_is_gated_on_lanes_in_flight_everywhere` — the R2 clear pin currently covers only `drain_run_all_on_main_exit`, which is exactly the gap this slipped through.

### LOW-1. A spawned lane's deferred turn resolution has no flush path

`try_flush_deferred_main_resolution` (events.rs:1080-) flushes only `main_agent_id()`'s latch. A spawned lane's turn that ends (final Error, or Finished) while its reviewer subagent still runs is deferred under the LANE's id (`on_final_error`/`on_finished` with `descendants_running`) — and nothing ever flushes it: both flush call sites (the Finished arm's child else-branch 731-736, the Exited arm 769) target the main agent only. During a normal run (main alive) the lane's deferred resolution is recovered only by the reviewer's completion notification (`notify_parent_on_completion` → a best-effort `try_send` Suggestion) waking the lane into a new turn — if that send is dropped (inbox full / handle gone), the lane's item stalls InFlight with the run waiting: no re-prompt, no drain until app restart. The main agent has the flush as its safety net for exactly this case; the lane does not.

Inverse edge (currently harmless, but the R1 misroute class): after the main's exit, `main_agent_id()` resolves to a spawned lane, so the flush would consume a LANE's deferred failure and deliver it through `on_main_turn_resolved` — the main path. It no-ops only because the R2 clear guarantees `current_item` is None post-main-exit; the lane's failure disposition is silently discarded either way.

**Fix direction**: route the flush by ownership as well — when the finishing/exiting agent owns a spawned run, flush ITS latch and deliver through `on_spawned_turn_resolved` (or give the spawned arms their own descendant-cleared flush).

### LOW-2. `remove_item_worktree`'s early-return ordering can skip the branch delete

worktrees.rs:109-117: `git worktree remove --force` then `git branch -D`, with `?` after the first — a worktree-remove failure (the worktree directory already gone, e.g. deleted manually, or a locked worktree) returns early and the branch delete never runs. In the R7 adoption cleanup this means a stale `wt/runall-<item8>` whose worktree is already gone survives the sweep → the next spawned-lane dispatch of that item fails provision ("branch already exists") → the fill's skip arm → the item is permanently undispatchable via lanes (the main lane still works). Same shape in every `remove_item_worktree` caller.

**Fix**: run both ops independently (attempt the branch delete even when the worktree remove fails), or `git worktree prune` before the remove.


---

## Also checked (no findings)

- **DISPATCH_LOCK × forwarder**: the Exited arm's drain now awaits a full `run_all_dispatch_next` (including the window fill — worktree provisioning + a cold graph index, potentially tens of seconds). Safe — no deadlock (verified: no locks held up the Exited-arm stack; DISPATCH_LOCK holders never wait on the forwarder — sends are try_send, emits synchronous, spawns don't wait on first events). Cost: the single forwarder task blocks for the dispatch duration, delaying all agent event processing (deltas batch, approvals/questions queue). This was already true for the Finished arm's re-drive (round 1); R5 extends it to Exited. If it ever matters, `tokio::spawn` the re-drive (the compact task already uses that pattern).
- **The R1 reorder × the deferred-main-failure flush (the else branch)**: correct — spawned lanes are parentless, never the main's descendants, so their finish was never a meaningful flush trigger; the Exited arm's unconditional flush (769) is unchanged. (The lane-side gap that DOES exist is LOW-1 above — a different, pre-flush-routing issue.)
- **R3c termination via the OTHER intervention arm**: `handle_user_intervention` (loop not closed) terminates correctly — the kept pointer is live (the item is genuinely InFlight), the steered item resolves via the normal main resolution → the pointer clear (4112) → the stopped path → the lanes' exits end the run. Only the closed-loop `ours` arm is broken (HIGH-1).
- **Sequential-path preservation (N=1)**: re-verified — the fill returns at `concurrency <= 1`; `owns_spawned_run`/`stamp_spawned_in_flight`/`drain_spawned_on_exit` no-op on an empty spawned set; the empty-exclude selection is semantically identical to the plain selector; DISPATCH_LOCK is uncontended on the sequential path (single dispatcher).
- **Lock discipline**: the std Mutexes (`current_item`, `spawned`) are scoped, never held across awaits; run_all → inner-mutex nesting is one-way everywhere; the store lock is never held across git subprocesses (snapshot → evidence → re-verify in both drains and both resolution paths); LANDING_LOCK serialization unchanged and correct.
- **Retire→Exited idempotence**: every path that removes a spawned entry also retires the agent (terminal resolution, item-vanished, send-failure), so the later Exited computes owns=false + was_main=false (the agent is out of the manager) and the drain no-ops. A crashed lane (entry present) is ownership-routed (R1) and drained exactly once.
- **Security / multi-platform / docs**: `SpawnedRunView` exposes only agent_id/item_id/branch; git args are fixed strings + derived branch names as Command args; worktrees.rs is std::path + the CREATE_NO_WINDOW-hardened git_raw — no Windows-only APIs; doc comments on all new public functions; no `#[allow]` in the diff; README + agent.md match the verified behavior.

## Tests

All seven round-2/3 tests verified present and pinning what they claim:
- `end_run_is_gated_on_lanes_in_flight_everywhere` (1106-1129): 7 actual guard-form sites ≥ 6 (compact stop, no-item, checkpoint-failure, no-main-agent, on_main stopped, halt no-item, drain_spawned stopped), the two wind-downs (`spawned_in_flight` ≥ 2), and the R2 clear pin.
- `spawned_routing_is_ownership_first_never_live_main_id` (1132-1148): exactly the 4 ownership checks in events.rs + the `was_main` ownership-gate pin.
- `dispatch_decisions_are_serialized` (1151-1161): the DISPATCH_LOCK guard pin.
- `spawned_exit_drain_re_drives_the_run` (1164-1178): the re-drive + the stopped end_run pin.
- `spawned_send_failure_cleans_up_the_recorded_entry` (1181-1190): the remove_spawned_run pin.
- `run_start_adoption_cleans_stale_spawned_worktrees` (1193-1202): the remove_item_worktree pin.
- `main_agent_exit_drains_an_active_run_all` (2047-2114): the was_main pin now matches the ownership-gated line (`!owns_spawned && mgr.main_agent_id() == Some(agent_id)`), still ordered before `mgr.remove`.

Caveat (unchanged from round 2): these are source-contract pins — they verify the named mechanics exist, not behavior. HIGH-1 lives exactly in the gap: nothing pins the ours arm's pointer clear.

## Test matrix

Not executed by this reviewer — the reviewer tool surface is read-only (no shell). The claimed numbers (root cargo test 2130/0/4, src-tauri cargo test 273/0/0, frontend npm test 1060/1060) are the parent's to verify before commit. A green matrix is consistent with these findings — HIGH-1 lives in the intervention wind-down seam that no current test exercises.
