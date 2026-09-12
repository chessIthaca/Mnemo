## Verdict: FINDINGS (0 high, 4 low)

Review of the uncommitted changes on `wt/agenticcoding` for plan 70d2283d ("Bound the over-threshold re-compaction loop + cross-turn re-emission guard"): both guards are correctly implemented, well-isolated, and genuinely regression-tested — all four findings are low-severity polish (event-contract consistency, error-message accuracy, a reset-path coverage gap, and a backlog-linkage artifact to verify at finish). No high findings; nothing blocks the commit.

## Findings

### L1 (low) — Terminal-event contract inconsistency: new abort paths emit Error + Finished

`src/agent/turn.rs:661-685` (repetition guard) and `src/agent/turn.rs:1555-1580` (compaction abort) both emit `Error { retrying: false }` followed by `Finished`. The codebase's documented contract for terminal turn failures is Error-only — `turn.rs:766-771` (MAX_RETRIES): *"Terminal exclusivity: emit **only** final Error (no trailing Finished)… the emission contract is: one terminal outcome per turn failure"* (the review-report-failure abort at `:740-761` follows it too). The IPC forwarder defends against double resolution via `TurnResolveLatch` (`src-tauri/src/ipc/events.rs:25-33`) and the `notified_children` set (`events.rs:545-554`), so there is **no functional bug today** — but the new paths re-introduce exactly the shape that comment warns about, making the latch load-bearing for them. Fix: drop the `Finished` emission from both new abort paths (the forwarder's Error arm already flips running state and cleans up, `events.rs:576-582` — the MAX_RETRIES path proves Error-only works), or extend the terminal-exclusivity comment to document the new pairing.

### L2 (low) — Abort error message counts attempts, not compactions

`"context stuck over threshold after {compact_attempts} compactions"` (`turn.rs:1559-1562`) reports **5** when at most **4** compactions ran (the 5th attempt aborts before `summarize_with_interrupt`), and **0** in the Rule-4-blocked shape: a pure tool-loop conversation where `summarize_with_interrupt` returns unchanged *without* a summarizer call (`context.rs:300-311` — both the `len <= keep_recent + 1` and `cut <= 1` guards return before `provider.complete`), so the counter climbs to 5 with zero compactions ever executed. The README says "re-compacts at most 4 times" — the message disagrees with both the code behavior and the README. Fix: say "after N compaction attempts" (or track actual compactions), and ideally distinguish the Rule-4-blocked diagnosis ("compaction blocked by open tool loop") so the user knows what actually happened instead of inferring a broken summarizer.

### L3 (low) — Coverage gap: the counter reset and the keep_recent=1 escalation are not behaviorally exercised

The reset (`turn.rs:1712-1714`) is the semantic that keeps legitimate long turns (crossing the fill rate repeatedly, each compaction succeeding) from being punished — but no test covers a turn with ≥2 successful compactions. A future regression of the reset (removed as dead code, comparison inverted) would silently abort long legitimate turns at the 5th threshold crossing and **no test would fail**. Likewise, in `over_threshold_turn_bounds_compaction_attempts` the `keep_recent=1` escalation is *selected* but has no observable effect: Rule 4's `retreat_past_open_tool_loop` retreats the cut to the last user message regardless of `keep_recent` (`context.rs:439-458`), so the whole open tool loop (the ~12K-token results) is kept verbatim either way — the abort comes from the attempt counter alone. A second test variant with the over-threshold bulk in the *text* region (not the tool tail) would exercise both the escalation's shrinking effect and the reset.

### L4 (low) — Backlog linkage artifact: verify item d84bb99b's disposition at finish

The uncommitted `.coding/backlog.jsonl` change flips item d84bb99b ("Files/Diff tabs can't display .coding/reviews — os error 5 should be agent-only") from pending to in_flight and attaches `plan_id 70d2283d` — this plan, whose topic is unrelated to the item (a steer artifact; the note records the interrupt/steer history). Per the repo's own run-all semantics ("only plan completion (finish) marks an item done" — item 6c6966b9's text), finishing this plan risks auto-resolving d84bb99b as done and silently dropping the Files/Diff-tabs work from the queue. Before finish, verify the item returns to pending (or re-queue it) rather than being marked done for work it never received.

## Verified correct (no findings)

**Guard A — compaction-attempt budget (`turn.rs:1544-1581`, `1596-1607`, `1707-1714`)**
- Increment placement: past the threshold check (`turn.rs:314` gates the call) and the announce block; every invocation that reaches it counts, including summarizer-failure iterations (bounded retry burn — correct).
- Escalation ladder matches the plan: attempts 1-2 → 6 (or 3 over the hard ceiling), 3-4 → 1 (strictly more aggressive than 3, so overriding the hard-ceiling tier is safe), 5 → abort.
- Reset semantics: uses the post-compaction recomputed count from the same `TokenAccounting` (tools-schema overhead survives `reset()`) and the same `effective_summarize_at()` as the trigger — the two sides cannot disagree. `used == threshold` counts as stuck, matching the `>=` trigger. A summarizer failure leaves messages unchanged → still over → no reset → climbs. Correct.
- Abort path: no dangling `CompactStarted` (announce latched at attempt 1, which got its `Compacted` pair on completion); `Phase::Compacting` → Error → Finished leaves the inflight bar cleared. `TurnOutcome` (Stop, empty text, `stop_reason: None`) matches the existing `summarize_stop` precedent; `stop_reason` is provably `None` at that point (a pending stop reason ends the turn at the post-batch check before the next iteration).
- Rule-4-blocked shape (pure tool loop): now bounded — abort at attempt 5 instead of ballooning to the provider 400 (the incident's secondary path). Strictly better than pre-fix; only the message wording is off (L2).

**Guard B — repetition guard (`turn.rs:630-686`)**
- Placement: after the `tool_calls.is_empty()` early-return (text-only responses end the turn and can never trip it), after `handle_bad_json`'s `continue` (malformed args have their own counter), before `execute_tool_batch` — the third response's call is NOT executed and its assistant message is NOT recorded, so `messages` ends on a valid [assistant-call, tool-result] pair (no orphaned tool result).
- Signature: text + `name:arguments` per call, `\u{1e}`-joined, `\u{1f}`-delimited from the text; provider ids excluded (matches the live incident's fresh ids). Control-character delimiters make cross-field collisions practically impossible.
- Ring: sliding window of 3; trip requires all three equal.
- `tool_error_count` clear: that counter resets to 0 on any success (TurnState doc), so the clear fires only for the response immediately following a failed batch — a legitimate retry starts a fresh ring and needs 3 fresh identical responses to trip; MAX_RETRIES owns the error-loop failure mode. Correct.
- The ring is NOT cleared on compaction — correct: the incident's re-emission happened *on* the compacted context.
- `stop_reason` carried via `take()` — a pending steer is not dropped.

**Guard interaction:** in the live pattern guard B trips at the 3rd iteration (before guard A's attempt-5 abort); guard A bounds the varied-text variant; guard B bounds the under-threshold variant. The tests isolate them correctly (varied text in test A; small context in test B).

**Tests (`src/agent/tests.rs:7477-7747`)** — both exercise the changed paths and fail without the fix (verified by tracing, not just the task claim): pre-fix, the over-threshold test runs ~12 summarizer calls (BranchMock caps model responses at 12 so the turn terminates instead of hanging) and never sees the stuck error; the repetition test executes all 10 calls → 10 tool results ≠ 2. Post-fix traces: 4 summarizer calls then the attempt-5 abort; exactly 2 tool results then the guard. The Rule-4 retreat analysis confirms the over-threshold invariant holds (the open tool loop is always kept verbatim, so the count stays over and the ladder must abort). Note: as a read-only reviewer I could not run `cargo test` myself; the stated green suites are consistent with my code-level verification.

## Bug-plan checks

- **Regression tests exercise the changed paths and fail without the fix:** yes — both by design and by my independent pre-fix trace (above).
- **Root cause documented:** yes — `.coding/knowledge/bug/2027-01-07-cross-turn-re-emission-tool-loop-pattern-continu.md` (symptom → root cause → fix + both regression test names + evidence pointers: traces.jsonl id 398, provider-errors.jsonl:405), the BUG memory (e3da7716), and the plan file's Bug + Regression-test sections.

## Constitution checks

- **Documentation sync:** README's context-compaction bullet extended and accurate ("at most 4 times" matches the code; the repetition-guard description matches the signature semantics). PLAN.md's compaction mentions (Rule 4, retention) don't describe the re-compaction loop — no update needed there. Module/field doc comments are thorough.
- **Multi-platform neutrality:** pure Rust, no platform-specific code, no path/shell assumptions; the `as u32`/`as usize` round-trip is portable.
- **Doc comments:** all new fields, the BranchMock helper, and both tests carry doc comments.
- **Warning-free build:** no dead code or unused imports introduced; per the stated green `cargo test` under `#![deny(warnings)]`.
- **Security:** no new attack surface; error strings carry only counters; signatures contain only already-in-memory content.
