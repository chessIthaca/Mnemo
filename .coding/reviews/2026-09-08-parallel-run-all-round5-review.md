## Verdict: FINDINGS (0 high, 1 low)

**Scope**: round-5 verification of the uncommitted changes on `wt/agenticcoding` for plan ffd7a86f (parallel run-all) — the two round-4 fixes re-verified in context against the working tree (the ours arm's DISPATCH_LOCK serialization in `on_main_turn_resolved`; the flush-ordering pin in `end_run_is_gated_on_lanes_in_flight_everywhere`), plus a regression sweep of the two touched regions: a deadlock/lock-order audit of the new lock hold, the sequential path, the test's logic and determinism, and an enumeration of every DISPATCH_LOCK acquirer.

**Summary**: R4-L2 is complete — the pin now asserts the exact flush-call sequence ["lane", "main", "lane", "main"] in source order, fails on any reversed ordering, and the current source satisfies it. R4-L1 is correct but incomplete: the lock wraps the clear + spawned-read + wind-down decision (closing race (b) BY CONSTRUCTION — every SpawnedRun record is written under the same lock), the arm is deadlock-free (end_run acquires no dispatch lock; the lock ordering matches dispatch_next's; the arm returns before the tail), and the sequential path is unchanged — but the OURS determination sits OUTSIDE the lock, so the finding's race (a) is narrowed, not closed by construction. One low finding.

---

## R4-L1 — the ours arm under DISPATCH_LOCK → correct, incomplete for race (a)

run_all.rs:3871-3913 (`on_main_turn_resolved`, the intervention closed-loop `ours` arm):

**Verified ✓ (the prescribed points):**
- **The lock wraps the determination + clear + wind-down decision** — as implemented, "determination" = the `spawned_in_flight` determination (3890-3903): one run-state hold (`guard.as_mut()`) containing the clear (3897-3898) and the spawned read (3899); the wind-down decision (3904-3911: stop-flag vs `end_run`) follows under the same guard, acquired at 3889 before all of it.
- **Nothing in the arm calls run_all_dispatch_next** — the arm's only call is `end_run` (3910). `end_run` (229-232) is exactly `*run_all = None` + `emit_backlog_changed` — no DISPATCH_LOCK, no dispatch, no re-entrancy. Safe to call under the hold.
- **The lock is released before the function's tail** — the arm ends in `return;` (3913), inside `if let Some(id) = iv.item_id`; the tail's compact spawn / dispatch (4211-4212) is unreachable from this path, and the guard drops at the return.
- **The sequential path (no spawned lanes) still ends the run exactly as before** — `spawned` empty → `spawned_in_flight = false` → `end_run`, byte-equivalent to the round-3 shape, now merely serialized.
- **Deadlock / lock-order audit — clean on every axis**: (1) DISPATCH_LOCK → run-state is the same order `run_all_dispatch_next` uses (2714 → the exclude read); no path holds the run-state lock across a DISPATCH_LOCK acquisition (every run-state acquisition in the new code — `on_spawned_turn_resolved`, `drain_spawned_on_exit`, `fill_spawned_window`, `dispatch_spawned_item`, `owns_spawned_run`, `any_lane_in_flight`, `backlog_run_all` — is a scoped block dropped before any dispatch_next call). (2) No caller of `on_main_turn_resolved` (the forwarder's Error/Finished arms, `try_flush_deferred_main_resolution`) holds DISPATCH_LOCK — no self-deadlock from the arm's acquisition. (3) The inner std Mutexes (current_item, spawned) are statement-scoped temporaries with no awaits inside the tokio-guard hold; `current_item` BEFORE `spawned` matches the documented invariant (R8).
- **Race (b) — the SpawnedRun-record vs end_run race — closed BY CONSTRUCTION**: the spawned-read and end_run run under DISPATCH_LOCK, and every SpawnedRun record is written by `fill_spawned_window`, whose only callers are `run_all_dispatch_next`'s three exits (2790, 2920, 2982) — all under the lock acquired at 2714. A fill's record can no longer land between the spawned-read and end_run.

### LOW-1. Race (a) is narrowed, not closed: the ours determination sits outside the lock

run_all.rs:3858-3870 (the ours determination) vs 3889 (the lock acquisition). The round-4 finding scoped THREE unserialized holds — "the ours determination, the clear, and the wind-down decision" — and its race (a) is precisely the ours-check→clear seam: "a concurrent dispatch's `current_item` write landing between the ours check and the clear would wipe the NEW item's pointer". The implemented fix serializes the clear + wind-down but leaves the ours determination a separate run-state hold BEFORE the lock, and the clear's hold does not re-verify the pointer against the interrupted id — so a dispatch whose write completes between the ours check (3858-3870) and the lock acquisition (3889) is still wiped by the clear (3897): the new item is worked by the main agent with no run pointer, its resolution cannot stamp it Done, and it strands InFlight until the next run start's adoption sweep requeues it.

Reachability (verified by enumerating every `run_all_dispatch_next` caller — the only DISPATCH_LOCK acquirer besides the ours arm):
- The three forwarder-task tails (`on_main_turn_resolved` :4212, `on_spawned_turn_resolved` :3370, `drain_spawned_on_exit` :3591/:3734) cannot interleave: the forwarder is ONE task looping a fan-in channel for all agents (events.rs:332-409), and the ours arm runs inline in it.
- The IPC start task (`backlog_run_all`, backlog_cmds.rs:566) refuses while a run is active — and a run is active (the ours determination just read its current_item).
- The compact task (`compact_then_dispatch_next` :2687) is the residual writer: its dispatch follows its compaction wait, and the compaction is triggered by `AgentCommand::Compact` (2638), which the agent task processes only when idle — so a normally-signaled compaction completes before the next turn starts and the task is gone before the ours arm runs. The one surviving path is the lost-signal timeout (2650, `wait_for_compact_signal` → `AUTO_COMPACT_WAIT`): a wall-clock deadline that can fire at any instant, including inside the ours-check→lock window; the ours arm's `lock().await` then suspends on the dispatch's hold, the dispatch writes the new pointer, and the resumed arm clears it.

The window is nanoseconds wide and needs the lost-signal conjunction — far narrower than the pre-fix race, and the same "not demonstrably reachable" class the round-4 finding acknowledged before filing. But the finding's own fix menu offered two mechanisms that close (a) by construction — the lock across the whole arm, or the merge ("read `current_item`, compare against the interrupted id, clear, read `spawned` — one acquisition") — and neither is present for the determination.

**Fix** (either, both trivial): hoist `let _dispatch_guard = DISPATCH_LOCK.lock().await;` above the `let ours = {...}` block (3858) — the determination acquires only the run-state lock, same order as the wind-down, no re-entrancy; or re-verify inside the clear's hold (skip the clear AND the wind-down when the pointer no longer equals the interrupted id — the run is not ours to wind down).

---

## R4-L2 — the flush-ordering pin → fix complete ✓

run_all.rs:1147-1168 (`end_run_is_gated_on_lanes_in_flight_everywhere`):

- **The pin collects every flush CALL occurrence in events.rs in source order**: `match_indices("try_flush_deferred_")` yields exactly six occurrences — the four calls (738, 744, 783, 789) and the two definitions (1100, 1165) — verified by search; no comment or doc occurrence exists in the file.
- **The definitions are skipped correctly**: both are `async fn try_flush_deferred_main_resolution(` / `async fn try_flush_deferred_spawned_resolutions(` — `events[..i].ends_with("async fn ")` is true for them and false for the indented call sites. A reformatted definition (e.g. `pub async fn`) would be counted and fail the sequence loudly — conservative, not silent.
- **The current source satisfies it**: the Finished arm's child else-branch runs the lane flush (738-743) BEFORE the main flush (744-745); the Exited arm runs the lane flush (783-788) BEFORE the main flush (789). Sequence: ["lane", "main", "lane", "main"] — matches the assertion.
- **The pin fails on a reversed ordering**: swapping either site's pair changes the sequence (the Exited arm → ["lane","main","main","lane"]; the Finished arm → ["main","lane","lane","main"]; both → ["main","lane","main","lane"]) — `assert_eq` fails in every case. A future reorder of the two flush calls can no longer stay green: the load-bearing BEFORE-main-flush ordering is pinned, which was exactly the round-4 gap (the old count-only pin stayed green on a reorder).
- **Deterministic and compile-sound**: `include_str!` embeds events.rs at compile time; `match_indices` iterates in byte order; the types check out (`Vec<&str>` vs `vec!["lane","main","lane","main"]`). The count assertion (>= 3: definition + two call sites) is retained ahead of the sequence pin; the rest of the test (the `any_lane_in_flight` gate counts, the R2 drain-clear pin, the R3-H1 marker pin) is unchanged.

---

## Also checked (no findings)

- **The R3-H1 marker survives the round-4 edit**: the comment "R3-H1: the steered item was just resolved" is present at 3894-3895 inside the locked hold — the existing `body.contains(...)` pin still matches.
- **`dispatch_decisions_are_serialized` unaffected**: its `body.contains("let _dispatch_guard = DISPATCH_LOCK.lock().await;")` assertion is a contains-check; the ours arm's second acquisition site keeps it true (and the test's intent — dispatch_next holds the lock — is untouched).
- **The stop-flag set (3904-3908)** is a separate run-state acquisition under the lock — same order, no awaits held inside, fine.
- **No regression in the ours arm's surroundings**: `finish_captured_item_done` still runs before the determination; the pointer-less intervention fall-through (3915-3916) is unchanged; the run-all branch below (3919+) is unreached from the ours path.
- **Multi-platform / security / docs**: both round-4 changes are Rust-side ordering/serialization + test logic — no new git invocations, no platform-specific code, no user-facing surface; README/agent.md describe the feature, not the wind-down internals (unchanged since round 4, still accurate).

## Tests

Not executed by this reviewer — the reviewer tool surface is read-only (no shell). The claimed matrix (root cargo test 2131/0/4, src-tauri cargo test 273/0/0, frontend npm test 1060/1060) is the parent's to verify before commit; both round-4 changes are Rust-side in run_all.rs (the frontend files in the diff are the earlier rounds' parallel-UI work, untouched by the round-4 fixes), consistent with the claim. The new sequence pin verified sound by inspection; a green src-tauri run implies it passes against the current source, consistent with my reading. LOW-1 lives in a serialization seam no current test exercises — the same gap class the round-4 LOWs lived in.
