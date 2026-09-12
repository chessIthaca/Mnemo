## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of commit 9bd839b (HEAD on wt/agenticcoding, working tree clean — every file read reflects the committed tree): all three round-1 findings are genuinely resolved in the committed code, the hardening introduced no new defects (lock discipline, guard logic, and all retained behaviors verified case-by-case), the four regression tests pin the shipped behavior assertion-by-assertion, and every intervention guarantee holds. One residual: a single stale doc-comment line in agent.rs — an instance of round-1 finding 1's class that round 1 did not cite and the fix therefore missed — still states the superseded "end_run clears run_all" mechanism. Doc-only, no behavior impact.


## (a) Round-1 findings — resolution status in the committed tree

**Finding 1 (docs) — RESOLVED, with one residual (→ new Finding 1 below).** Every spot round 1 cited was verified fixed by reading the committed files (not the diff):
- `run_all.rs` `halt_run_all` doc IDENTIFICATION bullet (now :2005–2008): "annotates, hands the item id to the intervention latch, and KEEPS the run stopped … the same kept-run mirror" — matches the code.
- `run_all.rs` module doc "run halts" bullet (:80–83): now carries the stopped-run nuance ("a STOPPED run is kept alive until its item resolves, backlog b83e891f; only a naturally halted run ends") — matches the spec twin.
- `run_all.rs` module doc escape-hatch line (:116–119): now qualified "the single-dispatch intervention requeue — a run-all item is kept InFlight with its run stopped instead" — matches the amended spec.
- `agent.rs` `send_suggestion` doc (:247–252), the three body comments (:261–263 stop-flag+latch ordering, :272–278 kept-InFlight disposition, :285–289 MERGE rationale), and the `interrupt` doc (:330–339, "stops the run's dispatch loop (the stop flag) without ending it") — all state the pause contract, consistent with the code.
- `agent.rs` test comments/assertion messages: three of the four cited spots fixed (halt-ordering assertion message :974–976 "stop flag + latch are in place"; latch-test doc :980–985 and assertion :1006 "dispose of the item non-terminally"; interrupt-test doc :1023–1027). The fourth cited spot (old 967–968, the assertion message) was fixed — but its uncited sibling, the same test's doc comment, was missed (Finding 1).
- `state.rs`: `UserIntervention::item_id` (:159–164, "the latch is the reliable capture"), `reason` (:166–168, "intervention note"), `BacklogContext::user_intervention` (:191–198, kept-InFlight/single-dispatch split) — all updated.
- The how record (`.coding/knowledge/how/2027-01-07-close-run-all-dispatched-items-reliably-check-th.md`): rewritten to "Belt-and-braces only: the root cause … is FIXED as of 2027-01-07 … KEEPS a run-all item InFlight with its run stopped (identity-guarded stop flag, never end_run)" — accurate against the code and no longer reads as if the gap were open.
- PLAN.md (:81–90), README.md (:78), the spec (escape-hatch split + stopped-run nuance), the superseded marker on the 2026-09-01 merged decision, the new 2027-01-07 decision record, and the BUG record — all accurate and mutually consistent.

**Finding 2 (stale-latch edge) — RESOLVED.** The keep-InFlight disposition is now `BacklogStatus::InFlight if from_run_all && interrupted_run_active` (run_all.rs:2260), where `interrupted_run_active` is computed from a live identity check against the run's `current_item` (:2225–2248). A drained run (agent exit between halt and resolution) yields `interrupted_run_active = false` and falls to the plain `InFlight` arm (:2278–2289), which requeues to Pending with the checkpoint sha preserved — the single-dispatch rationale, exactly as round 1 suggested. Case analysis of the guard (all traced against the code): latch id + live identity-matched run → keep InFlight + stop (mainline); latch id + run gone → requeue (the fixed edge); latch id + run holds a different item → requeue (pointer gone — unreachable while the item is InFlight, since the run never dispatches past a non-terminally-resolved item); interrupt path (no latch id) + run holds the item → keep InFlight + stop; deferred run (`current_item == None`) → not stopped, single-dispatch arm applies; idle-agent intervention (no item) → no disposition, any active run left alone. All correct.

**Finding 3 (lock hardening) — RESOLVED.** The identity check and the stop-flag set now happen under a single `run_all` hold (run_all.rs:2227–2242): guard acquired → `current_item` read (std lock, no await) → `interrupted == id` match → `r.stop.store(true, Ordering::Relaxed)` inside `if matched` → guard dropped at block end. No re-acquisition window; the result feeds the disposition guard as required.

## (b) The hardening introduced no new defects

- **Lock discipline.** In `handle_user_intervention` the locks are strictly sequential: run_all (:2188–2196, dropped) → run_all again (:2227–2242, dropped) → store (:2251, held only across sync `annotate`/`transition` calls, no await inside) → `emit_backlog_changed` after the store guard drops. No nesting, no await while holding beyond the acquisition itself. `halt_run_all`'s pre-existing run_all → store nesting (the note snapshot, :2040–2051, `drop(guard)` at :2055 before the arms) is untouched by this commit and remains the sanctioned, uninverted order (no store → run_all acquisition exists anywhere — re-verified). `on_main_turn_resolved` reads `stopped` under run_all (:1733–1745, dropped) before the store lock (:1748); the post-resolution run_all hold (:1854) contains only the `current_item` std lock.
- **Guard logic.** `from_run_all && interrupted_run_active` verified case-by-case above; the conjunct only *narrows* the keep-InFlight arm (drained runs now requeue), never widens it.
- **Retained behaviors.** The single-dispatch arm still requeues (`store.transition(id, BacklogStatus::Pending`, :2288, sha preserved); the Pending arm is unchanged (annotate only, :2290–2294); the stop flag is set only under the identity match (:2238–2242); the no-item idle arm of `halt_run_all` still ends the run (:2056–2063).
- **Residual window (note, not a finding).** Between the identity check (run_all dropped :2248) and the store read (:2251) the run could in principle end. Unreachable in practice: all run_all/current_item mutators run on the same sequential event forwarder as this handler, and the only spawned mutator (`compact_then_dispatch_next`) waits in a window where `current_item` is already `None` — which cannot produce a keep-InFlight disposition in the first place. Even if hit, the end state is the accepted drain design. Not a defect.

## (c) Regression tests pin the shipped behavior

All four verified assertion-by-assertion against the committed source, and all are logically red against the pre-fix code visible in the diff (the old handler called `end_run(`, had no `r.stop.store(true`, and requeued InFlight → Pending unconditionally; the old steer arm called `end_run`; the old arms ended the run unconditionally):
- `intervention_handler_is_never_terminal_and_never_continues` (:658–703): no terminal stamp, no dispatch/auto-feed, no `end_run(` — all true of the handler; the NEW no-reacquire assertion (:697–702, no `run_all.lock()` between `interrupted ==` at :2236 and `r.stop.store(true` at :2240) genuinely pins the single-hold property; the identity-guard-before-stop-set ordering (:690–696) matches :2236 < :2240.
- `intervention_keeps_a_run_all_item_in_flight_with_its_run` (:749–793): the `BacklogStatus::InFlight if from_run_all` arm exists (:2260), annotates only (:2270), never transitions, contains `interrupted_run_active` (the guard itself), and the single-dispatch requeue follows (:2288).
- `steer_halt_keeps_the_run_state_for_the_in_flight_item` (:706–746): approval block (:2065–2084) and steer block (:2085–2116) contain no `end_run(`; the no-item arm still does (:2056–2063).
- `stopped_runs_survive_open_plan_and_error_turn_ends` (:796–833): `if !stopped` present in both the still-open arm (:1807–1820) and the terminal-error arm (:1837–1851).

## (d) Intervention guarantees hold

No terminal stamp (the handler's only transition is the single-dispatch/drain InFlight → Pending requeue; no `Failed`/`Done`/`CantResolve` anywhere in it); no dispatch, no auto-feed, no done-counter bump in the handler. No auto-continue past an intervention: the latch path returns before the run-all branch (closed-loop false → handler → return; closed-loop true → `finish_captured_item_done` + end-if-ours → return); the post-resolution stopped check ends the run before any dispatch (:1870–1873); `compact_then_dispatch_next` ends the run on `Some(true)` (:1415–1420) and skips on `None` (:1410–1414); `backlog_run_all` refuses while a run is active (backlog_cmds.rs:424–426). The `if !stopped` semantics mirror `backlog_stop_all`'s documented "stop after the current in-flight item resolves".

## (e) Documentation sync

Module/function docs in run_all.rs, agent.rs, state.rs; PLAN.md; README.md; the spec (2026-12-06-backlog-status-plan-lifecycle.md, both amendments); the decision records (superseded marker + new 2027-01-07 decision); the BUG record; the how record — all verified accurate against the committed code. One gap → Finding 1.

## (f) Multi-platform neutrality + security

The round-2 changes touch only tokio locks, std locks, atomics, `format!`, and comments — no paths, no shell, no Windows-only APIs. No `unsafe`, no injection (note text from fixed reason literals), no secrets. Clean.

## Findings

**1. LOW — Documentation sync: one residual stale statement of the superseded "end_run clears run_all" contract (agent.rs:946–948).**
The doc comment of `send_suggestion_halts_run_all_before_sending` still reads: "The halt must precede `send_cmd` so `end_run` clears `run_all` before the soft-stop `Finished` arrives (race avoidance)." Under the committed code the steer halt never calls `end_run` while an item is in flight — it sets the stop flag + latch and KEEPS the run — so the stated mechanism is false, and it contradicts both the code and the same test's updated assertion message 25 lines below (:974–976, "so the stop flag + latch are in place"). Round 1 cited the assertion message (old 967–968) but not this doc comment, and the fix addressed every cited spot — this uncited sibling was missed. Doc-only; the ordering requirement itself (halt before send_cmd) remains correct and tested. Fix: reword to the pause mechanism, e.g. "…so the stop flag + intervention latch are in place before the soft-stop `Finished` arrives — the intervention path then keeps the item InFlight with its run stopped (race avoidance)."

## Bug-plan checklist
- Regression tests exercise the changed paths: **yes** — all four are source-contract tests over the exact changed functions/arms, each logically red pre-fix (see (c)); the two round-1 hardening fixes each gained a pinning assertion (the `interrupted_run_active` guard; the no-reacquire window).
- Root cause documented: **yes** — BUG record + plan 28bc06a2 Context, accurate against the committed code.
- BUG memory written: **yes** — BUG: "Run-All interrupted items re-dispatch forever — intervention destroys the dispatch context" (c88ee1f5), content matches the shipped fix.

## Notes
- Read-only reviewer: the test matrix was verified by inspection (every source-contract assertion matched against the committed source; working tree clean at 9bd839b = HEAD), not re-executed; the parent's in-session runs (root cargo test 2063 passed exit 0, src-tauri 237 + 4 passed exit 0, frontend vitest 1011 tests + tsc --noEmit clean) stand as the execution evidence — and under `#![deny(warnings)]` those green runs also prove the build is warning-free.
- Bookkeeping reminder (not a finding, carried from round 1): backlog item b83e891f is still `pending` in `.coding/backlog.jsonl` — stamp it done via `backlog_status` when this plan closes, per the how record's own closure procedure.
