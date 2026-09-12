## Verdict: FINDINGS (0 high, 4 low)

Review of ALL uncommitted changes on wt/agenticcoding for plan 96e2862a ("Run-all gate re-checks at the next resolution instead of halting on non-closures"). The core gate change is correct and well-pinned: keeping the run armed introduces no spurious dispatch, no infinite loop, and every stuck shape retains a recovery path; the stopped-run (b83e891f) semantics are preserved exactly. The 4 findings are documentation-sync misses (LOW-1) and three narrow behavioral observations on the new guard/restore (LOW-2..4) — none block the fix.


## Scope reviewed

- `src-tauri/src/ipc/run_all.rs` (the fix): `on_main_turn_resolved` non-closure + turn-failure arms, `plan_open_note`, `RUN_ALL_STEER`, module doc, the three new source-contract tests + `plan_open_note_names_what_happened`, removal of `stopped_runs_survive_open_plan_and_error_turn_ends`.
- Interaction paths read in full: `end_run`, `halt_run_all` (incl. `already_halted`), `handle_user_intervention`, `drain_run_all_on_main_exit`, `adopt_orphaned_in_flight`, `finish_captured_item_done`, the single-dispatch branch, `BacklogStore::annotate`/`set_note`/`transition` (src/backlog.rs:480-538).
- Bookkeeping (not fix code): `.coding/plans/96e2862a.md`, `.coding/plans/3ddf7041.md`, `.coding/backlog.jsonl`, `.coding/knowledge/*` (BUG record, HOW rewrite, SPEC amendment, prior-plan records).
- Doc-sync sweep: repo-wide regex for `no further items dispatched|run halts|halts the run|halt the run|run halted` (59 hits triaged; historical `.coding/plans`/`.coding/reviews` and superseded specs excluded).

## Findings

### LOW-1 — Documentation sync: stale "the run halts" claims at 7 live sites the fix didn't update

The fix updated the module doc (both sites), `RUN_ALL_STEER`, the note wording, and the two live knowledge records — but these live claims still describe the old halting semantics:

1. **`src-tauri/src/ipc/run_all.rs:1973-1975`** — `on_main_turn_resolved`'s OWN doc header: "anything else → no transition + halt the run), then dispatch the next (unless stopped)." The doc comment of the very function this plan changed still states the removed behavior. Most important site of this finding.
2. **`src-tauri/src/ipc/run_all.rs:2448`** — `halt_run_all`'s doc: "resolves the item under the plan-tied rules (gate → Done, abandoned → Failed, still open → stays InFlight + halt)."
3. **`src-tauri/src/ipc/run_all.rs:1087`** — test comment in `on_main_turn_resolved_is_plan_tied`: "Every other turn end: no transition — annotate + halt the run." (Assertions are fine; the comment is stale.)
4. **`src-tauri/src/ipc/events.rs:975`** — `try_flush_deferred_main_resolution` doc: "Delivering it then would close the turn as a non-closure — the run halts with the item left in_flight". The justification (premature delivery while Reviewing) still stands; the stated consequence is the old semantics.
5. **`src/runtime/turn_resolve.rs:10-12` and `src-tauri/src/ipc/events.rs:30-32`** — the latch module docs: "today the failure resolution would halt the run and the success resolution would then act on stale state."
6. **`PLAN.md:78`** — "and the run halts (the next item may only start once this item's loop verifiably closed; a resumed session continues the plan…)". PLAN.md is explicitly on the project's doc-sync checklist; the plan's step 7 named README.md only (README verified clean — no matches).
7. **`docs/why-mnemo-deck.md:821`** — "stays in flight, the work stays in the tree, and the run halts for the user to resume" (this deck's wording was previously curated for accuracy in the 45dcf577 review rounds).

Minor adjacent nit: `src/runtime/agent.rs:6091-6092/6101-6102` (test comment + panic message in `unattended_planning_auto_continues`) says "the run-all run halts until a manual 'c'" — it describes the pre-3ddf7041 state, but under the new semantics a parked run-all run WAITS (armed), it does not halt; worth a one-word touch if touching that file anyway.

Not findings: `.coding/knowledge/spec/2026-12-06-backlog-status-plan-lifecycle.md:14` carries the old "the run halts on the Run-All path" text but is properly marked `status = "superseded"` with a header pointing at the amended 2027-01-07 spec — historical record, correctly handled. All `.coding/plans/*` and `.coding/reviews/*` hits are historical artifacts.

**Fix:** reword the 7 sites to the waiting semantics (e.g. "the run waits — the next turn resolution re-checks"), mirroring the module doc's new phrasing.

### LOW-2 — Single-dispatch restore lacks the `already_waiting` guard → unbounded note growth

`on_main_turn_resolved`'s single-dispatch None arm (run_all.rs:2325-2347) annotates on EVERY non-closure resolution, and the new pointer restore makes every subsequent main turn resolution re-check the same item — so while a ▶-dispatched item sits in flight, **every main-agent turn end (including the user's ordinary interactive chat turns) appends another `plan_open_note(...)` line** to the item's note. `BacklogStore::annotate` APPENDS (`" | addition"`, src/backlog.rs:517-518), so the note grows once per turn, unbounded — exactly the problem the fix's own `already_waiting` guard was added to prevent on the run-all arms (the fix's rationale: "repeated non-closures would grow the note unboundedly"). Before the fix the pointer was consumed on the first non-closure (one annotation total); the restore makes it per-turn. The run-all arms and their mirror diverge.

**Fix:** apply the same guard in the None arm (skip the annotate when the note already contains the waiting marker), or compare the exact note text.

### LOW-3 — `already_waiting` marker is coarse: different-flavor waiting notes are suppressed after the first

The guard `n.contains("run waits")` (both arms) suppresses ANY subsequent waiting-note annotate once ANY waiting note exists:
- A later **turn failure's abort reason is never recorded** after an earlier non-closure note — e.g. a "3 consecutive tool errors" abort following a "plan loop did not close (workflow: Executing)" note would leave the abort reason invisible (the a6a7727a incident's abort text would have been lost had a non-closure note preceded it).
- **Workflow-state progression is frozen** — the note keeps saying "workflow: Executing" even when the run later waits in Reviewing.
- Theoretical false positive: any unrelated note text containing "run waits" would suppress a legitimate annotate. Mitigated in practice: the note field is harness-written, and dispatch-time `set_note` REPLACES the note (src/backlog.rs:525-538), so cross-run contamination is excluded — verified.

This mirrors `halt_run_all`'s equally coarse `already_halted` ("halted" anywhere), so it is consistent with the established pattern — hence LOW. If the information loss matters, key the guard on the exact note string or a per-arm marker.

### LOW-4 — Unconditional pointer restore can clobber a concurrent `single_in_flight` mutation

The restore `*single_in_flight = Some(in_flight_id)` (run_all.rs:2340-2345) writes unconditionally after several await points (store lock, annotate persist — file I/O — emit). A user ▶ dispatch (an IPC command thread, concurrent with the event forwarder by construction) landing inside the take→restore window sets the slot to the NEW item; the restore then overwrites it with the old id — the new dispatch's completing resolution goes blind (the pre-fix stranded shape; recoverable via run-all-start adoption / exit drain / the escape hatch, but a regression to the bug this plan fixes). Same class if the main-agent exit drain (which clears `single_in_flight`) can interleave — though if both run inline on the same forwarder loop they are serialized (the auto-compact comment at :2243-2246 indicates `on_main_turn_resolved` runs inline on the forwarder). Tiny window, recoverable failure mode.

**Fix (cheap hardening):** check-and-set under a single lock hold — `let mut slot = ...lock(); if slot.is_none() { *slot = Some(in_flight_id); }` — so a concurrently-set new pointer wins.


## Verified correct (review-focus items)

**(a) Gate correctness — no spurious dispatch, no infinite loop, no unrecoverable stuck run.**
- Both arms `return` before the dispatch section; dispatch happens only after `item_resolved` (gate/abandon arms) via `run_all_dispatch_next` / `compact_then_dispatch_next`. No dispatch path is reachable from a non-closure. ✓
- No self-retriggering loop: resolutions are turn-end-driven events, and the arms perform no dispatch. ✓
- Stuck-forever analysis: every armed-wait shape retains recovery — (1) user nudge (any next turn resolution re-checks), (2) stop + restart, (3) main-agent exit → `drain_run_all_on_main_exit` requeues the item to Pending + ends the run (read in full, :2504-2536), (4) app restart → `adopt_orphaned_in_flight` requeues crash orphans (:2569-2605). The park shapes are covered by auto-continue (Executing/Reviewing always; Planning in unattended mode per 3ddf7041). Residual: a resting-Complete freeform answer in run-all mode parks with the run armed (Complete is not auto-continue-covered) — the item stays Pending, the stall is visible (waiting note + Parked evidence event), and the trade-off is deliberate and documented in the module doc. Acceptable.
- User's chat prompt while the run is armed: an interactive turn that doesn't close a plan re-hits the non-closure arm (annotate skipped, no dispatch); one that closes the item's plan resolves it (the intended fix); one that abandons it → Failed (correct). The gate's plan-identity limitation (any plan closing resolves the item) is pre-existing and unchanged by this fix.
- Known residual, already queued: an escape-hatch-done item held by an armed run + agent exit → drain requeues Done→Pending → re-dispatch (backlog 6c6966b9, the done-orphan guard). The armed-wait widens that window vs the old halt; covered by the queued item, not a new finding.

**(b) Stopped-run semantics preserved exactly.** The arms return without ending the run — stopped or natural alike; the post-resolution `if stopped { end_run; return; }` (:2234-2237) still ends an intervention-stopped run after its item resolves, WITHOUT dispatching. `handle_user_intervention`'s stop-flag + kept-pointer mechanics and the latch path's `ours`/end_run are untouched. The removed `stopped_runs_survive_open_plan_and_error_turn_ends` coverage is subsumed by the stronger no-`end_run` assertions (they cover stopped AND natural runs). ✓

**(c) Single-dispatch restore.** No double-dispatch: auto-feed stays suppressed (`resolved_terminally = false`), and the restore only affects the next resolution's visibility. One narrow clobber race — LOW-4 above.

**(d) "run waits" marker.** Safe against cross-run false positives (dispatch-time `set_note` replaces the note — verified src/backlog.rs:525-538); coarse within a run — LOW-3 above.

**(e) Tests pin the behavior.** Verified the anchors against the actual source: `plan_open_note(main_state)` first occurrence is the run-all arm (:2147; the single-dispatch use at :2303 comes later, so `find` anchors correctly); `"turn failed"` first occurrence is the run-all error arm (:2193; single-dispatch at :2313 later); both regions run to the arms' `return;` and assert no `end_run` + `already_waiting` + `emit_backlog_changed`; the stopped-check positional assertion holds. Red-before/green-after confirmed by reasoning: the pre-fix arms contained `end_run`, lacked the guard, and the restore/wording did not exist — all three tests fail on the pre-fix source. One minor anchor imprecision, not a gap: `single_dispatch_non_closure_restores_the_pointer`'s `take` anchor actually lands on the :2060 comment mentioning `single_in_flight` (before the real take at :2273) — the region-based restore assertion still pins the contract, and the take itself is compile-covered by the `in_flight_id` binding. `plan_open_note_names_what_happened` updated to the new wording. Source-contract style is the repo's established pattern for AppHandle-bound logic (documented in the plan's context). ✓

**(f) Documentation sync.** Module doc (both sites), `RUN_ALL_STEER` rule 4, the HOW record (rewritten to HISTORY + fixed state), the SPEC 2027-01-07 amendment, and the BUG record are accurate and consistent with the code. README verified clean (no stale "halt" claims — the task's claim holds). Misses: LOW-1 (7 live sites, incl. PLAN.md:78 and the changed function's own doc header). The 2026-12-06 spec is properly marked superseded. ✓ with LOW-1.

**(g) Multi-platform neutrality.** Pure async Rust logic — no paths, no OS APIs, no shell, plain-text notes. Neutral on macOS and Windows. ✓

**(h) Bug-plan checklist.** Regression tests exercise the changed path (source-contract on the exact arms changed) ✓. Root cause documented: the plan's Bug section (96e2862a.md:43) + the BUG record file (symptom → root cause → fix → regression test names) ✓. BUG memory updated with the fix state: the record file carries the full fix; the semantic record's supersede-to-fixed is scheduled at finish per the plan's own step-2 note ("At finish, supersede with the fixed state + commits") — the file is the truth, so this is per-plan, not a miss. ✓

## Bookkeeping (not fix code — nothing wrong found)

- `.coding/backlog.jsonl`: the a6a7727a escape-hatch done stamp is consistent with the incident (abort → auto-continue resume → plan 3ddf7041 Complete, commits c08c177+c92c4cf+8ea23e7); the clear-finished `deleted_at` stamps touch only terminal items (no InFlight cleared — matches the clear_finished contract); the two newly queued items (ffd4bac3 auto-compact dial, 77ff8f45 bug-fixing plan steps) are well-formed. 207dc316's note describes the old halting behavior — accurate at stamp time, fine as history.
- `.coding/plans/3ddf7041.md`: the appended regression-test name (`unattended_planning_auto_continues`) matches the actual test in src/runtime/agent.rs. ✓
- `.coding/plans/96e2862a.md`: the resumption document accurately embeds the fix design; all 4 steps checked; the recorded regression test (`turn_failure_resolution_keeps_the_run_armed`) exists and pins the changed path. ✓
- Knowledge records: the HOW rewrite (RELIABLE TRIGGER → HISTORY + fixed, habit kept as safety net) and the SPEC amendment are accurate against the shipped code. ✓

## Verification

The stated matrix (root cargo test 2083/0/4, src-tauri 247+4+2/0, tsc clean, vitest 1046/75 files, zero warnings under `#![deny(warnings)]`) is consistent with the source state reviewed — the changed code has no unused bindings/imports, the tests exist with correct anchors, and no other test pins the old halting behavior (repo-wide sweep found no live source matches for the old wording). Not re-run by the reviewer (read-only); the parent should re-run after fixing LOW-1..4.

## Summary

The amplifier fix is correct: the run stays armed on non-closures and turn failures, the completing turn's resolution now finds the run and stamps Done, the stopped-run and drain/adoption/intervention interactions all verify, and the three source-contract tests genuinely pin the new behavior (red before, green after). Fix LOW-1 (doc sync — 7 stale "halt" sites, incl. the changed function's own doc header and PLAN.md:78) and LOW-2 (add the `already_waiting` guard to the single-dispatch restore path) before commit; LOW-3/LOW-4 are cheap hardening at the author's discretion.
