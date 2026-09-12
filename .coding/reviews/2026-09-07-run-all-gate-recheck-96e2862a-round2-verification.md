## Verdict: FINDINGS (0 high, 2 low)

Round-2 verification of commit 0283504 ("fix: run-all gate re-checks at the next resolution instead of halting (96e2862a)", HEAD of wt/agenticcoding, clean tree) against the round-1 report (.coding/reviews/2026-09-07-run-all-gate-recheck-96e2862a-review.md). **All 4 round-1 findings are genuinely fixed** — every cited doc site reworded, all three guards keyed on the exact note text, the check-and-set restore correct in both race orderings — and the fixes introduce no behavioral regressions; the strengthened test pins hold against the live code. Two stale doc sites of the round-1 LOW-1 class survive at phrasings round-1's line-based sweep could not see (one line-split across PLAN.md 940/941, one matching no sweep pattern). Both LOW, doc-sync only.

## Scope verified

- Commit 0283504 (HEAD, clean tree — `git diff HEAD` empty, `git status` empty).
- All four round-1 findings at their cited sites, re-read verbatim in the current code: the three `already_waiting` guards (run_all.rs:2187-2195, :2233-2241, :2375-2383), the check-and-set restore (:2393-2406), and the reworded doc sites (run_all.rs:2001-2009, :2425-2459, :1117; events.rs:28-36, :972-980; turn_resolve.rs:10-12; PLAN.md:75-83; agent.rs:6091/:6102; docs/why-mnemo-deck.md:820-822).
- The three strengthened regression tests (run_all.rs:933-987, :990-1031, :1034-1077) — anchors checked against the live source.
- Doc-sync re-sweep: `halt` / `halts` / `halted` / `run halts|halts the run|halt the run|run halted|annotate + halt` across src-tauri/src, src/runtime, PLAN.md, docs/why-mnemo-deck.md — every remaining hit triaged (approval/steer-halt sites, `halt_run_all`'s own doc/body, historical incident descriptions, test fixtures).

## Round-1 findings — all verified fixed

### LOW-1 (7 stale "the run halts" doc sites) — FIXED at every cited site

1. **`on_main_turn_resolved` doc header** (round-1 :1973-1975 → now :2001-2009): "anything else → no transition + the run waits — the next turn resolution re-checks), then dispatch the next (unless stopped)." ✓
2. **`halt_run_all` doc** (round-1 :2448 → now :2425-2459): the "still open → stays InFlight + halt" claim is gone; the doc now says the post-approval turn resolution "still sees the item and resolves it under the plan-tied rules" — accurate. ✓
3. **`is_plan_tied` inline test comment** (round-1 :1087 → now :1117): "Every other turn end: no transition — annotate + the run waits (the next turn resolution re-checks)." ✓ (the opening comment 10 lines above still carries a stale phrase — new finding LOW-5 below)
4. **`try_flush_deferred_main_resolution` doc** (events.rs:972-980): "the item is left in_flight with a premature 'plan loop did not close (workflow: Reviewing)' note even though the main agent hasn't finished its work yet" — the stated consequence now matches the new semantics. ✓
5. **Latch module docs** (turn_resolve.rs:10-12, events.rs:28-36): "the failure resolution would take the non-closure disposition and the success resolution would then act on stale state". ✓
6. **PLAN.md:75-83**: "the run waits — the next turn resolution re-checks (the next item may only start once this item's loop verifiably closed; …)". ✓ (a second, summary paragraph still carries the old claim — new finding LOW-6 below)
7. **docs/why-mnemo-deck.md:820-822**: "the run waits — it resumes when the plan closes (or the user nudges)". ✓ Note: `docs/` is gitignored by design (.gitignore:66-67, "Presentation deck — excluded from repo"), so this fix is working-tree-only and cannot appear in any commit — verified fixed in the current file, which is the only place it lives.
8. **agent.rs nit** (:6091, :6102): "the run-all run stalls until a manual 'c'" — "stalls" is the accurate word under the new gate (parked → run armed but stalled). ✓

### LOW-2 (single-dispatch restore lacked the annotate guard) — FIXED

The None arm (run_all.rs:2361-2392) now guards its annotate: `already_waiting` re-fetches the item under the store lock and checks the exact note text (:2375-2383); annotate only when `!already_waiting` (:2384-2391). Mirrors the run-all arms exactly; the comment (:2368-2373) documents the per-turn re-check rationale. ✓

### LOW-3 (coarse "run waits" marker suppressed different flavors) — FIXED

All three guards key on the EXACT note text — `n.contains(note.as_str())`:
- run-all still-open arm: :2194
- run-all error arm: :2240
- single-dispatch None arm: :2382

A different flavor (a later abort reason, a workflow-state progression) produces different note text → not contained → annotates; only exact repeats are suppressed. ✓

### LOW-4 (unconditional pointer restore could clobber a concurrent ▶ dispatch) — FIXED

Check-and-set under a single lock hold (run_all.rs:2393-2406): `let mut slot = …single_in_flight.lock().expect(…); if slot.is_none() { *slot = Some(in_flight_id); }`. ✓

## New-issue analysis (the three verification questions)

### (a) Exact-text guard semantics — no material information loss

- The note formats are three fixed-structure strings (`plan_open_note`, :306-321) whose only variable is the workflow-state name, plus the error arm's `"{reason} — item left in flight; …"` wrapper. A different-flavor note is never a substring of an existing one in practice: the full note (prefix + state + suffix) must appear contiguously, and differing state names / abort reasons / suffixes break the match. The notes carry no counts, so count-digit collisions cannot arise.
- Even in a hypothetical containment edge, only a redundant note line is skipped — control flow is unaffected (the run stays armed, the pointer stays/restored, the next resolution re-checks regardless of the guard).
- The residual growth (a genuinely different note per turn, e.g. a state progression Executing → Reviewing) is LOW-3's fix working as intended: different information is recorded; identical information is not duplicated. The guards re-fetch the item under the store lock (no stale snapshot); the check→annotate TOCTOU is benign (turn resolutions are latch-serialized per turn).

### (b) Check-and-set restore interactions — correct

- Concurrent ▶ dispatch landing inside the take→restore window: sets the slot to the NEW item → the restore sees `slot.is_some()` → skips → the dispatch wins. ✓
- Dispatch landing after the restore: overwrites the restored pointer → the dispatch wins (correct — an explicit user dispatch supersedes). ✓
- `Some(status)` arm: the restore lives inside the `None` arm only — after a terminal resolution the slot stays empty and auto-feed (gated on `resolved_terminally`, which the None arm sets false at :2407) may dispatch the next. No path restores a terminally-resolved item's pointer. ✓

### (c) Strengthened test pins — anchors hold

- `non_closure_resolution_keeps_the_run_armed` (:933-987): `still_open_arm` spans the first `plan_open_note(main_state)` (:2179 — the doc header contains no such literal, so `find` anchors on the arm) to the arm's `return;` (:2205); asserts no `end_run`, `already_waiting`, `n.contains(note.as_str())`, `emit_backlog_changed`, and that the stopped check (:2270) follows. All verified present in the span. ✓
- `turn_failure_resolution_keeps_the_run_armed` (:990-1031): error arm spans the first `"turn failed"` (:2228) to its `return;` (:2251); asserts no `end_run`, "run waits", `already_waiting`, exact-text guard. ✓
- `single_dispatch_non_closure_restores_the_pointer` (:1034-1077): the `take` anchor lands on the `single_in_flight` comment at :2176 (before the real take at :2309 — the same imprecision round-1 already documented as acceptable); the span [take .. `resolved_terminally = false` (:2407)] contains `Some(in_flight_id)` (:2404), `already_waiting` (:2375), the exact-text guard (:2382), and `slot.is_none()` (:2403). ✓
- Red-before/green-after still holds: the pre-fix arms contained `end_run` and lacked all three guards and the restore.

## Findings

### LOW-5 (round-2) — run_all.rs:1106: `is_plan_tied` opening comment still says "(annotate + halt)"

The test's opening comment (round-1 cited the inline comment at :1087, now fixed at :1117) still reads: "leaves the item untouched **(annotate + halt)** on every other turn end — never `CantResolve`, never a rollback." (:1104-1107). Under the shipped semantics the resolution annotates and **the run waits** — the two comments in the same test now contradict each other. The phrasing "(annotate + halt)" matches none of round-1's sweep patterns (`run halts|halts the run|halt the run|run halted`), which is why both the round-1 sweep and the fix missed it. Doc-sync only; the assertions below it are correct.

**Fix:** reword to "(annotate + the run waits)" mirroring :1117.

### LOW-6 (round-2) — PLAN.md:939-941: backlog-subsystem summary still says "the run halts" (line-split)

The feature-list paragraph still reads: "every other turn end (loop open, crash, halt-for-approval) leaves the item untouched — no rollback, **the run halts**, a resumed session continues the plan." Under the shipped semantics, loop-open and crash (the non-closure and turn-failure dispositions this plan changed) leave the run **waiting** — only halt-for-approval actually halts (`halt_run_all`). The phrase "the run / halts" is split across lines 940/941, so round-1's line-based sweep pattern `run halts` matched neither line — missed by both the sweep and the fix. PLAN.md is explicitly on the project's doc-sync checklist. Doc-sync only.

**Fix:** reword to the waiting semantics, e.g. "…no rollback, the run waits (the next turn resolution re-checks); an approval halt stops it for the user; a resumed session continues the plan."

## Changeset correctness re-verification (round-1 core verdict holds)

- **No spurious dispatch:** both run-all non-closure arms `return` (:2205, :2251) before the done-counter/dispatch section; the single-dispatch None arm sets `resolved_terminally = false` (:2407), suppressing auto-feed (:2412). No dispatch path is reachable from a non-closure. ✓
- **No infinite loop:** waiting is event-driven (the next turn resolution); the arms perform no dispatch. ✓
- **Recovery paths:** run-all non-closure keeps `current_item`; single-dispatch non-closure restores the pointer; a stopped run still ends via the post-resolution `if stopped { end_run; return; }` (:2270-2273); agent exit → drain; app restart → adoption. ✓
- **Stopped-run semantics preserved:** the stopped check is reached only after a terminal resolution (the non-closure arms return earlier) — exactly the b83e891f contract. ✓
- **Multi-platform neutrality:** pure async Rust logic — no paths, no OS APIs, plain-text notes. ✓

## Verification status

Tests not re-run by this reviewer (read-only). The task's stated matrix (root cargo test 2083/0/4, src-tauri 247+4+2/0, zero warnings under `#![deny(warnings)]`; frontend untouched by the finding fixes — confirmed: no frontend files in commit 0283504) is consistent with the source state: all asserted anchor strings exist at the asserted positions in the live code, and no other test pins the old halting wording (repo-wide sweep — the only "halt" claims remaining are approval/steer-halt sites, `halt_run_all`'s own doc/body, historical incident descriptions, and the two findings above).

## Summary

All 4 round-1 findings are genuinely fixed and correctly pinned; the exact-text guards and the check-and-set restore are sound with no new behavioral issues; the core gate change remains correct (no spurious dispatch, no infinite loop, every stuck shape retains a recovery path, stopped-run semantics preserved). Two stale doc sites of the LOW-1 class survive at phrasings invisible to a line-based sweep — fix LOW-5/LOW-6 (two one-line rewords) before finishing.
