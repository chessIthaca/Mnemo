## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of commit d0798f9 (HEAD of `wt/agenticcoding`, clean tree) against the four round-1 findings. All four fixes are correctly applied and verified — L1, L2, L3 fully resolve their findings; L4's re-queue is correctly applied but leaves one conditional residual path (the low finding below). The two guards themselves are unchanged in logic by the fix round and remain correct; no regressions found.

## Fix verification

### L1 (event contract) — VERIFIED
- Repetition guard (`turn.rs:640-692`): emits `Error { retrying: false }` then returns `Ok(TurnOutcome)` immediately — no trailing `Finished`. Placement unchanged (after the text-only early-return at 623, before `execute_tool_batch` at 694).
- Compaction abort (`turn.rs:1559-1584`): emits `Error { retrying: false }` then returns `Some(TurnOutcome)`; the `run_turn` call site (`turn.rs:324-340`) returns `Ok(outcome)` directly on `Some` — no fall-through emission.
- Both now match the MAX_RETRIES terminal-exclusivity contract (`turn.rs:772-797`, comment + code), and each new site carries its own comment citing that contract. The remaining `Finished` emissions in turn.rs (418, 623, 724, 1770, 2648, 2827) are all on other, pre-existing paths (hard-stop gate, normal text-only end, stop-signal, summarize-interrupt, session-level) — none reachable from the two abort paths.
- Forwarder re-check: the Error arm (`events.rs:576-599`) flips running state, cleans up pending approvals/questions, resolves run-all via `on_final_error`, and notifies parents of child agents — Error-only is proven by the shipping MAX_RETRIES path.
- Tests still assert the right events: the over-threshold test's `saw_stuck_error` matches the reworded message ("context stuck over threshold after 5 compaction attempts — aborting turn" contains "stuck over threshold"); the repetition test's `saw_guard` matches "repetition guard: 3 identical consecutive responses…". Informational (not a finding): no test pins the *absence* of a trailing `Finished` — the contract is comment-enforced; a mutation re-adding it would not fail a test.

### L2 (message accuracy) — VERIFIED
`turn.rs:1569-1573` now reads "context stuck over threshold after {} compaction attempts — aborting turn" with `state.compact_attempts`, which is exactly 5 at the abort (incremented by 1 per `maybe_compact` invocation, checked `>= 5` — it cannot overshoot). "attempts" is accurate for both shapes: the seeded shape runs at most 4 summarizer calls before the 5th-attempt abort; the Rule-4-blocked shape makes 5 attempts with 0 compactions executed. Consistent with README's "re-compacts at most 4 times" (compactions executed vs attempts made — both quantities now correctly worded).

### L3 (coverage gap) — VERIFIED
`legitimate_long_turn_compactions_reset_budget` (`src/agent/tests.rs`) drives `maybe_compact` directly with pure TEXT messages via the `compact_if_over!` macro mirroring `run_turn`'s trigger (`count >= effective_summarize_at()`). Traced both failure modes:
- **Reset removed:** phase 1 climbs to 3 (attempt 3 lands under, no reset); phase-2 crossing 1 runs attempt 4 (keep_recent=1, lands under), crossing 2 hits attempt 5 → abort → `Some(TurnOutcome)` → the `assert!(outcome.is_none())` fails. Requires ≥2 crossings in 16 rounds — satisfied (each round ≈ 12.4K chars ≈ 3K tokens against a 20K-token effective threshold; the estimator is deterministic, so this is not flaky).
- **Escalation removed (keep_recent stays 6/3):** phase-1 attempts 1-4 keep ≥3 big notes (~78K tokens) over threshold → attempt 5 aborts → the 5th macro call returns `Some` → assert fails.
- Post-fix trace: phase 1 = attempts 1-2 (keep_recent 6) over, attempt 3 (keep_recent=1) keeps one small message → lands under → reset; phase 2 = each crossing compacts at attempt 1 and resets; final `summarizer_calls >= 5` holds (3 + ≥2).
- The direct-drive rationale is sound (a `run_turn`-shaped test cannot oscillate: the tool loop grows monotonically and Rule 4's retreat keeps it verbatim — stuck by design).
- `TurnState::fresh()` is field-by-field identical to the old `run_turn` literal (same nine fields, same values) plus the two new fields zeroed; `run_turn` now constructs via `fresh()`; the green build proves no other exhaustive struct-literal site exists. `pub(crate)` on `TurnState`, `fresh()`, and `maybe_compact` is the minimal visibility for the test module — no leak.

### L4 (backlog linkage) — VERIFIED, with one residual (finding below)
- The re-queue is committed in d0798f9 (before finish — the plan is still in Reviewing; this review is the finish gate): item d84bb99b is `pending` with an accurate explanatory note, `plan_id` retained for traceability.
- `BacklogStore::transition`/`annotate` call `reload_from_disk()` first (`backlog.rs:472, 513`) — the store sees the re-queued status; the edit is authoritative for file-backed state.
- Every status-keyed auto-resolve path skips a `Pending` item: the exit drain's `still_in_flight` check (`run_all.rs:2796-2799`), run-all-start adoption's `InFlight` filter (`run_all.rs:2886`), and the intervention dispositions' arms. No code path scans items by `plan_id` to mark Done (the linkage is used only for ownership checks and landed-evidence). The intervention latch (`finish_captured_item_done`, `run_all.rs:2188-2189`) is consumed at the first turn resolution after the intervention (`take()` at 2164-2169) — long gone, and it captures run-all items only.
- Residual: see the finding.

## Findings

### R1 (low) — L4 residual: the re-queue cannot clear the in-memory dispatch pointer; a live pointer still routes the finish resolution to the item

The re-queue blocks every *status-keyed* auto-resolve path, but the single-dispatch success arm keys off the in-memory `single_in_flight` pointer, which a `.coding/backlog.jsonl` edit cannot touch:

- The Executing-entry stamp that round 1 observed (the `in_flight` flip + `plan_id 70d2283d` linkage) proves the pointer referenced d84bb99b at this plan's creation (`stamp_backlog_in_flight`, `run_all.rs:3167-3208`, reads `single_in_flight`/`run_all.current_item`).
- The turn-resolution mirror (`run_all.rs:2521-2530`) restores the pointer on every open-loop resolution, so within one continuous app instance it stays live until a terminal resolution, a main-agent exit, or an app restart — none of which the fix round performed.
- At the finish resolution, the non-run-all branch takes the pointer and transitions the referenced item to `Done` with **no status guard** (`run_all.rs:2433-2484`), and `Pending → Done` is a legal row (`backlog.rs:478-480`); `transition` reloads from disk (`backlog.rs:472`) so the re-queued status is seen — and does not protect. The transition also overwrites the note with `None`, wiping the explanatory note.

So the claim "finishing plan 70d2283d cannot auto-resolve d84bb99b as done" holds only if the app instance restarted since the stamp (pointer then `None`, finish resolves nothing). Conditional on runtime state I cannot observe read-only — but the default (no restart mid-closing-sequence) leaves the pointer armed.

**Fix (cheap, post-finish):** after calling `finish`, verify item d84bb99b is still `pending` (Backlog tab or `.coding/backlog.jsonl`); if it flipped to `done`, re-queue it — `Done → Pending` is the sanctioned requeue row (`backlog.rs:485-488`), one click in the UI or one `backlog_status` call. That converts the silent drop into a detectable-and-recoverable event. **Durable follow-up (backlog candidate, not this changeset):** the success arm should not transition an item the harness never stamped `InFlight` for the completed plan (or should check the item's `plan_id` linkage) — the exact steer-pivot artifact this incident produced; the linkage field already exists for it.

## Standard checks re-verified over the full commit

- **Guard A (compaction budget):** logic untouched by the fix round — increment past the threshold gate and announce block (`turn.rs:1559`), abort at exactly 5 (`1560-1584`, now Error-only), ladder `1599-1610` (attempts 1-2 → 6/3, 3-4 → 1, overriding the hard-ceiling tier safely), reset `1715-1717` (post-compaction count from the same `TokenAccounting` + `effective_summarize_at()` as the trigger; `used == threshold` counts as stuck, matching the `>=` trigger). Round-1's semantic verification carries over; the only changes are the message and the removed `Finished`.
- **Guard B (repetition guard):** logic untouched — signature keys tool `name:arguments` with provider ids excluded, `\u{1e}`/`\u{1f}` delimiters, ring of 3, `tool_error_count` clear, `stop_reason` via `take()`, ring not cleared on compaction. Only change: the removed `Finished`.
- **No regressions from the fixes:** the four fixes are (a) two `Finished` removals matching the MAX_RETRIES contract, (b) a message reword, (c) a new test + behavior-identical `fresh()` refactor + minimal `pub(crate)` visibility, (d) the backlog JSONL re-queue. None alter guard logic, event flow, or public API.
- **Three regression tests exercise the changed paths:** over-threshold (budget + abort, asserts ≤5 summarizer calls/Compacted events — actual 4 each — and the stuck error), repetition (exactly 2 tool results + the guard error), and the new reset/escalation test (traced above; fails on both targeted mutations). All three assert the post-fix event shapes.
- **Root cause documented:** `.coding/knowledge/bug/2027-01-07-cross-turn-re-emission-tool-loop-pattern-continu.md` (symptom → root cause → fix + both original regression test names + evidence pointers), the BUG memory (e3da7716), and the plan file's Bug + Regression-test sections. Informational (not a finding): the doc lists the two original test names; the third (reset/escalation coverage) isn't mentioned — its existing claims remain true, a one-line addition if desired.
- **README:** the compaction bullet stays accurate — "at most 4 times" matches the executed-compaction count, the escalation and repetition-guard descriptions match the code.
- **Multi-platform neutrality:** pure Rust in library paths; no platform APIs, paths, or shell syntax; the JSONL/knowledge/review files are data.
- **Suites:** stated green (lib 2087+16, app 250+4+2, warning-free under `#![deny(warnings)]`). As a read-only reviewer I could not run `cargo test` myself; the stated greens are consistent with my code-level verification (no compile-level concerns: the test's `super::turn::TurnState::fresh()` / `pub(crate)` accesses are correctly scoped, the macro and mock usage mirror the existing tests).
