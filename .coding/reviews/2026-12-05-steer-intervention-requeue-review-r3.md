## Verdict: PASS

Both round-2 LOW findings are fixed correctly and completely in the working tree — verified against the source, not just the diff hunks. R2 LOW A (run identity): `handle_user_intervention` and the closed-loop arm both end a run only under a `matches!(…, Some(interrupted) if interrupted == id)` pointer-equality guard, the no-item case never ends a run, the `item_id` Option survives the requeue block via `as_deref()` borrowing, lock nesting is clean (clone-then-release, no await while held), and both regression tests pin the guard. R2 LOW B (doc drift): PLAN.md continuation lines are uniformly 2-space and `halt_run_all`'s doc comment is trimmed and scope-corrected to match the code. Stated test status (app 189/0 +4 doc-tests, lib 1718/0) is consistent with the code but not independently re-run by this reviewer (read-only).

---

## R2 LOW A — run identity: FIXED

### `handle_user_intervention` (run_all.rs:1310-1391)

- **Borrow fix confirmed.** `let mut item_id = iv.item_id.clone()` (1315) → latch id, run-all `current_item` fallback (1316-1325), `single_in_flight.take()` fallback (1326-1333). The requeue block binds `if let Some(id) = item_id.as_deref()` (1335) — a borrow, not a move — so the Option is still alive for the identity check at 1372. Without `as_deref()` this would not compile; the previously-moved shape is gone.
- **Identity guard.** 1372-1386: `match item_id.as_deref()` — `Some(id)` locks `run_all` (tokio), clones the run's `current_item` (std mutex, clone-then-release), and matches `live.as_ref().and_then(|o| o.as_deref())` against `Some(interrupted) if interrupted == id`; `None => false` — the no-item case never ends a run. `end_run` (1388) fires only under `interrupted_run_active`. Traced cases: steer-halt (run already ended by `halt_run_all` → live `None` → no double end), interrupt-during-run (live `current_item == Some(item)` → ends exactly that run), deferred Run-All started mid-turn (`current_item == None` → `Some(None)` collapses to no-match → run survives to its own next-resolution dispatch), single-dispatch pointer (never equals a run's `current_item` → run survives). All correct.
- **Lock nesting — clean.** The identity block holds the `run_all` tokio guard only across the synchronous `current_item` lock+clone (1374-1377); no await inside; the guard drops at the block end (1382) **before** `end_run` is awaited (1388). The fallback block (1316-1325) holds `run_all` only across a clone. The requeue block (1336-1362) holds the store tokio guard only across synchronous `transition`/`annotate` calls. Sequential acquire/release throughout, consistent with the file-wide `run_all → store` ordering; std mutexes (`current_item`, `single_in_flight`, `user_intervention`) are never held across an await.

### Closed-loop arm in `on_main_turn_resolved` (941-963)

After `finish_captured_item_done` (942), the `ours` block (950-959) applies the same pointer-equality guard (`Some(interrupted) if interrupted == id.as_str()`) with identical clone-then-release lock discipline (guard dropped at 959 before `end_run` at 961), then unconditionally `return`s — a deferred Run-All (`current_item == None`) is not ours and dispatches on its own next resolution, and pointer-less (single-dispatch / live-run interrupt) interventions still fall through to the normal path (965-966), which the live run-all / `single_in_flight` branches resolve. No regression in the fall-through.

### Module doc + tests

- Module doc intervention bullet (25-35) documents the run-identity rule verbatim ("the resolution only ever ends the run whose in-flight pointer matches the intervened item — a Run-All the user started while the intervention turn was still running (deferred, `current_item == None`) is left to its own next-resolution dispatch") — matches the code exactly.
- `intervention_handler_is_never_terminal_and_never_continues` (546-582) asserts the `interrupted ==` guard is present **and positioned before** `end_run` (handler: guard at 1380 < `end_run` at 1388 ✓), plus requeue-before-end and the never-terminal/never-continues contracts.
- `closed_loop_with_captured_item_commits_and_marks_done` (599-640) asserts `interrupted ==` inside the intervention block (spans 929-968 in the real function — verified to contain `iv.item_id` at 941, `finish_captured_item_done(` at 942, `interrupted ==` at 957). Both tests fail if the guard is removed — genuine pins.
- Re-checked the `fn_body` needle-composition against the file layout (tests module ends at 641, precedes the code): no test-module text contains any composed `fn <name>(` needle, so extraction always lands on the real function body.

## R2 LOW B — doc drift: FIXED

1. **PLAN.md:46-65** — every continuation line of the Backlog + Run-All bullet now uses exactly 2 spaces, including the eight new intervention/InFlight lines (58-64); the surrounding pre-existing lines (47-57, 65) were already 2-space. Uniform. The second edited region (PLAN.md ~818-824, run-all subsystem bullet) is also uniformly 2-space per the diff and consistent with the same style.
2. **`halt_run_all` doc (1183-1210)** — the stale "IDENTIFICATION (Phase 3b)" claims are gone. The paragraph now says the sha is set at dispatch "via `set_note`" (matches `run_all_dispatch_next`'s `set_note(&item.id, Some(checkpoint))`), that approval halts (`stamp_failed = true`) stamp `Failed` and — because the halt clears the run state — that stamp IS final (resolution can no longer see the item), and that the steer arm leaves the status untouched and hands the id to the intervention latch with a `[handle_user_intervention]` reference. Each sentence matches the corresponding code branch (1258-1267 approval transition; 1268-1291 steer annotate + latch fill). The `stamp_failed` parameter paragraph (1199-1210) is accurate, including the annotate-only steer semantics and the no-op-when-inactive contract (`if let Some(r)` at 1214).

## Regression glance (surrounding resolution paths)

- **Run-all resolution branch (969-1118)** — unchanged from the round-2-verified state: plan-gate Done/CantResolve/rollback arms, done-bump, `stopped → end_run`, next dispatch. The new intervention block only ever returns early or falls through with a pointer-less latch; the branch is reached exactly as before.
- **Single-dispatch branch (1120-1171) + auto-feed (1172-1177)** — untouched by this round (git diff shows no hunks there); still-Pending items resolve terminally through the guarded `Pending → Done/Failed/CantResolve` rows only on non-intervention turn ends, which is the designed behavior.
- **Plan goal holds end-to-end:** the handler never stamps a terminal status (`InFlight → Pending` with sha-bearing note, `Pending → annotate` only, terminal `_ => {}` untouched), the Executing-entry stamp (`should_stamp_in_flight` 1438-1440, `stamp_backlog_in_flight` 1452+) still lifts only still-Pending items, and the latch is consumed first at every resolution.
- **New-this-round `already_halted` guard in `halt_run_all` (1236-1238, 1273)** — idempotence for the steer-arm annotation (skips a second "— halted" suffix if the note already carries one). Benign: cosmetic note dedup, no status semantics; the store read at 1226-1235 follows the established `run_all → store` ordering. Cosmetic false-positive edge (a note legitimately containing the word "halted" skips one annotation) is harmless.

## Non-findings (observations, no action needed)

- The closed-loop arm's `ours` guard is conservative: a captured id implies a steer halt, which already called `end_run`, so in practice the guard never fires `end_run` (a later run cannot re-dispatch the same item — it was `InFlight`, dispatch requires `Pending`). It is exactly the invariant the fix description asked for and costs nothing.
- A steer on an *idle* main agent with an active deferred Run-All still halts that run at `halt_run_all` time (pre-existing, unconditional halt semantics — "steer received during unattended run"; the latch itself is correctly not set when not running). Out of round-3 scope and unchanged by this fix.

## Scope notes

Verified: both round-2 LOW fixes, the matches! guard logic, lock nesting (tokio `run_all` + std `current_item`, clone-then-release, no await while held), doc-vs-code agreement, and a regression glance over the resolution paths. Test figures (mnemo-app 189/0 +4 doc-tests, mnemo 1718/0) are consistent with the working tree; not independently re-run (read-only reviewer).
