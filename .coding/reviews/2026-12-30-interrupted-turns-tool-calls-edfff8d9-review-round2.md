## Verdict: FINDINGS (0 high, 1 low)

The round-1 finding is fully resolved by commit 40eadad: the fold promotion is correct and exhaustive (no fold arm can still silently drop a steer except the documented Cancel/Clear drops and the user-initiated CancelSuggestion), the unguarded buffered re-injection can neither double-inject nor mis-order steers, the new tests genuinely pin the end-to-end contract and fail without the fix, and no other fold call site regresses. One low documentation-sync finding: the durable bug record (-2.md) predates the round-1 fix and doesn't mention the grace-window seam, its 4 new regression tests, or the current test count.


## Scope and method

Reviewed commit 40eadad (verified HEAD with a clean tree — `git diff HEAD` and `git status` both empty) on wt/agenticcoding: the fold promotion + 3 unit tests (src/agent/loop_impl.rs:431-446, 1697-1733), the unguarded buffered re-injection + updated comments (src/agent/turn.rs:1818-1874), the end-to-end test `interrupt_with_buffered_steer_carries_the_steer` + the dispatch-test comment precision (src/agent/tests.rs:5386-5570), plus a full re-read of every `StopReason::fold` call site, every `InterruptWithSteers` consumer, the summarization re-injection paths (turn.rs:92/455, agent.rs `compact_context`), and the rest of the commit (knowledge files, backlog, plan, frontend test — unchanged from round-1).

Method note: the reviewer surface is read-only (no shell), so the reported test runs were cross-checked for internal consistency rather than re-executed: round-1 reported 1942 lib tests; the fix adds exactly 4 (3 fold unit tests + 1 end-to-end test) → 1946, matching the reported count; the frontend count (786) is unchanged and no frontend file is touched by the fix. Consistent. The code-level verification below is exhaustive by inspection.

## 1. Fold promotion — correct and complete (no arm can still silently drop a steer)

Exhaustive enumeration of `fold` (loop_impl.rs:423-513), every (command, current) pair:

- **Suggestion/Prompt** into: `None` → `Steer([s])`; `Steer`/`InterruptWithSteers`/`CompactWithSteers` → push; **`Interrupt` → `InterruptWithSteers([s])` (new)**; **`Compact` → `CompactWithSteers([s])` (new)**; `Cancel`/`Clear` → drop. The `_ => {}` catch-all now covers ONLY Cancel/Clear, with the inline comment "the task/tab is gone — documented drop".
- **Interrupt** into any steer-carrying state → `InterruptWithSteers(texts)` (preserved); else → `Interrupt`. No drop.
- **Compact** → `CompactWithSteers` analog. No drop.
- **Cancel/Clear** → override, dropping queued steers — intentional and documented in the doc comment (Cancel exits the task, steers moot; Clear wipes the conversation, running a queued command afterwards would be wrong), pinned by `fold_cancel_drops_queued_steers`, `fold_clear_drops_queued_steers`, and the new `fold_suggestion_into_cancel_still_drops`.
- **CancelSuggestion** → removes only the matching text (user-initiated "x"), collapses empty lists correctly (`Steer([])→None`, `InterruptWithSteers([])→Interrupt`, `CompactWithSteers([])→Compact`), leaves non-steer reasons untouched. Not a silent drop.

The doc comment (loop_impl.rs:413-422) accurately states the new contract ("Hard signals preserve any already-buffered steers where the queued command must still run: Interrupt → InterruptWithSteers and Compact → CompactWithSteers… Cancel and Clear intentionally DROP queued steers"); "a Cancel/Interrupt is never downgraded" remains true — promotion is not a downgrade, the stop still happens. The three new unit tests each fail without the promotion (the old `_ => {}` arm kept `Some(Interrupt)`/`Some(Compact)` unchanged).

## 2. Unguarded re-injection — no double-injection, no mis-ordering

**Single-consumption invariant.** A steer is consumed from cmd_rx exactly once, on exactly one path: dispatch's steer arm moves it out of the channel into `buffered` (dispatch.rs `Some(other) => buffered.push(other)`); the grace-window `tokio::time::timeout` polls only the tool future, so a steer arriving during the window stays queued in cmd_rx. The two paths are mutually exclusive. The merge's re-injection loop (turn.rs:1860-1874) consumes `buffered` **by value**, folding each command into `stop_reason` exactly once; the TurnOutcome then carries `stop_reason` to `run_turn_with_retry`, which pushes each steer as a user message exactly once + one `SuggestionInjected` each (agent.rs:186-208). No path both carries a steer in `stop_reason` and re-queues it in cmd_rx — double-injection is structurally impossible.

**Ordering.** The merge folds the Interrupt signal FIRST (turn.rs:1840), then buffered commands in arrival order (Vec::push in dispatch preserves order); the promotion APPENDS, so the final list is arrival-ordered. A grace-window steer (still in cmd_rx, necessarily sent after the buffered ones) is appended at the pre-inject drain (agent.rs:182-184). Mixed scenarios all yield arrival order, e.g. mid-stream steer s1 + buffered s2 + Interrupt → `fold(Interrupt)` gives `InterruptWithSteers([s1])`, re-injection appends s2 → `[s1, s2]`; a grace-window s3 then appends at the drain → `[s1, s2, s3]`.

**Interactions under hard_stop.** The re-injection loop runs BEFORE the `if hard_stop` break (turn.rs:2107), so the promoted stop_reason is carried by the TurnOutcome return at 2145-2159. A buffered `CancelSuggestion` still works under hard_stop: folding it into `InterruptWithSteers([s])` collapses to `Interrupt` — the "x" cancels the steer even when Stop was pressed. Under a Cancel signal, buffered steers fold into `Some(Cancel)` and drop — documented intent, consistent. The `hard_stop` flag itself is computed from the signal before the loop, so re-injection cannot accidentally clear it.

## 3. The new tests genuinely pin the end-to-end contract

- **`interrupt_with_buffered_steer_carries_the_steer`** (tests.rs:5455+): streams one 300ms `sleep_ms` call, sends Suggestion at t=50ms and Interrupt at t=100ms (both while the tool executes — the steer is buffered by dispatch's steer arm, the Interrupt opens the grace window). Asserts (a) `outcome.stop_reason == InterruptWithSteers(["actually do X instead"])` — **fails without the fix** (the old `if !hard_stop` guard skipped the re-injection, leaving plain `Interrupt`); (b) the tool drained with its REAL result (`success` + "slept") — pins the grace-window drain half; (c) the tool_result message in history — pins history recording. Timing is deterministic: the provider stream is `stream::iter` (available on first poll), the 50ms sleeps yield to the current-thread runtime giving the turn task ample time to reach dispatch, and the commands land well inside the 300ms tool sleep — the same pattern as the established sibling tests. The run-loop consumption half (InterruptWithSteers → steers as user messages + follow-up turn) is pre-existing code, unchanged by this commit, verified in round-1 and pinned by the earlier cancel-steer plan's tests.
- **`commands_arriving_during_the_grace_window_are_not_lost`**: the comment is now precise and accurate — it scopes itself to the dispatch-level half (the steer stays queued when dispatch returns) and names the two tests that pin the fold promotion and the end-to-end merge. The round-1 inaccuracy (claiming the pre-inject drain "folds it into the stop reason" when that fold was a no-op) is gone.
- **The 3 fold unit tests** pin the promotion and the documented Cancel drop at the unit level, each failing without the fix.

## 4. No regression at the other fold call sites

All five production `StopReason::fold` call sites re-verified:

1. **Stream select! hard-stop arm** (turn.rs:1279): folds the signal then breaks — no steer is folded after within this arm; the promotion is unreachable. No behavior change.
2. **Stream select! steer arm** (turn.rs:1289): `current` can only be `None` or `Some(Steer)` here (a hard stop breaks the loop immediately after folding), so the promotion arms are unreachable. No change.
3. **Between-call safe point** (turn.rs:1777): `current` can only be `None` or `Some(Steer)` (hard stops break the batch at the pre-execution check or the merge's `hard_stop` break). Promotion unreachable. No change.
4. **Merge Interrupt signal** (turn.rs:1840): pre-existing round-1 behavior (fold preserves steers folded before the signal). Unchanged.
5. **Merge re-injection** (turn.rs:1868) + **pre-inject drain** (agent.rs:183): the two fix sites — the only places `Some(Interrupt)`/`Some(Compact)` can be `current` when a steer folds.

The **summarization paths** (turn.rs:92 model-swap, turn.rs:455 auto-compact, agent.rs:866 `compact_context`) do NOT use fold — `summarize_with_interrupt` buffers commands and the callers re-inject them directly as messages (after `drop_cancelled_steers`). On the interrupted-summarization path the buffered steers are re-injected as messages BEFORE the stop return, so they survive in history — unaffected by the promotion. All `InterruptWithSteers` consumers (agent.rs:187; turn.rs:935, 995-1025 pre-stream check, 1782 pre-execution check, 1834 merge `hard_stop`) treat it consistently as a hard stop that carries steers.

The promotion changes behavior ONLY where a steer previously vanished (fold into `Some(Interrupt)`/`Some(Compact)` was a no-op); no previously-working path changes outcome. In particular there is no unwanted auto-resume: Stop with nothing queued still yields plain `Interrupt` → park.


## 5. Rest-of-diff sanity check (round-1 coverage re-verified with the fix in)

- **Working tree / commit state**: 40eadad is HEAD, tree clean — the fix is fully committed, nothing uncommitted to drift from what was reviewed.
- **Commit message**: documents the review fix explicitly ("a steer folded into Some(Interrupt)/Some(Compact) now promotes… and the buffered re-injection runs even under hard_stop"). Accurate.
- **Knowledge files**: the bug supersession chain is correct (-2.md supersedes the original with front-matter markers; the original marked `status = "superseded"`), the DECISION record matches the implemented behavior, the spec and plan files are consistent (all steps [x]).
- **Backlog**: item d171df98 correctly `in_flight` with `plan_id: edfff8d9` (the main agent closes it at finish).
- **Unchanged-from-round-1 surfaces** (re-skimmed, still correct): the dispatch.rs grace window, the turn.rs post-stream hard-stop block (history recording with sanitized args), the null-turn/bad-JSON-MAX stop_reason carries, the frontend useAgentStore regression test.
- **Constitution**: doc comments present and accurate on the changed public surface (`fold`, the merge block); no `#[allow]` anywhere in the diff; tests use only `tokio::time`/`std::time` (platform-neutral); every new regression test fails without its fix (verified by tracing the old code paths).
- **Security**: no new attack surface — the promotion only moves user-authored steer text into the steer-carrying stop reasons, the same trust level and injection path as the pre-existing `Steer` outcome.

## Finding 1 (LOW): the durable bug record predates the round-1 fix — documentation sync

`.coding/knowledge/bug/2026-12-30-interrupted-turns-silently-drop-in-flight-tool-c-2.md` was written for the original hybrid drain/cancel fix and was not updated when the round-1 finding was fixed in the same commit:

- Its FIX list has three points (steers drain / hard stops record / grace-drain) and does not mention the grace-window seam fix (fold promotion + the unguarded buffered re-injection under hard_stop) — the one defect the round-1 review found in the fix itself.
- Its REGRESSION TESTS list is missing the 4 new tests (`fold_suggestion_into_interrupt_promotes_to_interrupt_with_steers`, `fold_suggestion_into_compact_promotes_to_compact_with_steers`, `fold_suggestion_into_cancel_still_drops`, `interrupt_with_buffered_steer_carries_the_steer`).
- Its VERIFIED line cites the pre-fix count ("cargo 1942 passed"); the current count is 1946.

The seam IS documented in the commit message, the code comments at both fix sites, and the committed round-1 report — but the knowledge file is this project's durable, mergeable bug record ("the file stays the truth") and was an explicit step-7 deliverable of the plan, so the landed state should be reflected there. Suggested fix (docs-only, ~5 minutes): append a point 4 to the FIX list ("Review fix (round-1 finding 1): a steer folded into Some(Interrupt)/Some(Compact) now promotes to InterruptWithSteers/CompactWithSteers, and the buffered re-injection in turn.rs runs even under hard_stop — a steer typed during the grace window or buffered before the Interrupt drives the follow-up turn instead of vanishing"), add the 4 test names to the REGRESSION TESTS list, and update the VERIFIED line to 1946. The BUG memory digest should likewise mention the seam when updated at finish (it currently says only "root causes A-D").

## Notes (non-finding)

- Round-1's non-finding notes still hold: the pre-stream hard-stop check (turn.rs:995-1025) uses an explicit `matches!` list including `InterruptWithSteers` rather than `is_hard_stop` — same semantics, harmless duplication; `tool_calls_made: 0` on the hard-stop TurnOutcome matches old behavior.
- The merge's `hard_stop` flag deliberately excludes Compact/Clear signals (turn.rs:1831-1836) — a Compact signal lets the batch finish and the post-loop check carries `CompactWithSteers` to the compaction path, consistent with the "compact resumes mid-task" design. Unreachable in practice anyway (stop_signal setters produce only Interrupt/Cancel).
- The end-to-end test's one theoretical flake mode (turn task not reaching dispatch within 100ms on a pathologically loaded machine would fold the commands mid-stream instead, failing the "slept" assertion) is the same timing pattern as the established sibling tests (150ms sleeps over instant streams) — acceptable suite risk, not a finding.
- Test-count arithmetic cross-check: 1942 (round-1) + 4 new = 1946 lib; integration 16 passed / 3 ignored and frontend 786 unchanged — all consistent with the diff (no frontend or integration-test changes in the fix).
