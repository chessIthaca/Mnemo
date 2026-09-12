## Verdict: FINDINGS (0 high, 1 low)

Final verification review of commit `fecc607` on `feat/max-tokens-cap-repetition-model-switch`.
This is the second verification pass. The first (`2026-12-r9-r10-model-switch-verify-review.md`)
returned FINDINGS (0 high, 2 low) — both *missing regression tests*. Commit `fecc607` adds
both tests. **LOW 1 is fully resolved. LOW 2's test is added and exercises the right path when
it reaches it, but it is FLAKY** — it can fail ~1 in 8 runs even with the fix present, so it
does not satisfy the constitution's "must fail without the fix and pass with it" bar. The
shipped production code (turn.rs:148-163) remains correct; this is a test-reliability defect.

---

### LOW 1 — RESOLVED ✓

**Test:** `set_explicit_provider_clears_stale_deferred_swap_on_immediate` (`src/agent/tests.rs:4559`).
**Fix under test:** `*self.pending_swap.lock() = None` in the immediate-swap path (`src/agent/loop_impl.rs:1016-1019`).

Verified the test genuinely fails without the fix and passes with it:

1. **The clear is necessary.** `set_provider` (`src/agent/loop_impl.rs:958-966`) touches *only*
   `self.provider` and `self.context_manager` — it never touches `pending_swap`. So the explicit
   `*pending_swap = None` at line 1016-1019 is the *only* thing that clears a stale deferred swap
   on the immediate path. Removing it leaves B in place.
2. **The test exercises the deferred→immediate sequence.** Step 1 defers B (`max_ctx 64_000 <
   current 200_000` → deferred branch, `pending_swap = Some(B)`, early `return`). Step 2 supersedes
   with C (`max_ctx 200_000 ≥ current 200_000` → immediate branch). The context manager is still
   the original 200K at step 2 (the deferred path never swaps it), so `new_max < current_max` is
   `200_000 < 200_000` = false → immediate. Correct path selection.
3. **Without the fix:** `has_pending_swap()` returns true → `assert!(!has_pending_swap())` fails;
   `take_pending_swap()` returns `Some(B)` → `assert!(...is_none())` fails. Two independent
   assertions fail. ✓
4. **With the fix:** both assertions pass; `provider().model() == "large-C"` confirms C is active.
5. **Deterministic** — `set_explicit_provider` is synchronous (no `await`, no `select!`), so there
   is no racing. This is a clean regression test.

LOW 1 is correctly and completely resolved.

---

### LOW 2 — FINDING (flaky regression test)

**Test:** `run_turn_model_switch_interrupt_emits_paired_compact_event` (`src/agent/tests.rs:4785`).
**Fix under test:** the paired `AgentEvent::Error { error: "Compaction interrupted — …" }` emitted
in the `if let Some(stop) = stop_reason` block (`src/agent/turn.rs:148-163`).

The test is well-intentioned and, **when the interrupt fires**, it correctly verifies the fix:
`stop_reason.is_some()`, `CompactStarted` present, a paired `Error` containing "interrupt",
swap completed (`max_context == 2_000`), pending swap consumed. The assertions are sound.

**The defect: the test does not reliably reach the interrupt path.** It races a *completing*
synchronous stream against the buffered interrupt, so a non-trivial fraction of runs take a
different code path where `stop_reason` stays `None` and the paired-event block is never entered.

**Mechanism:**

- The test's `old_provider` is a `MockProvider`. `MockProvider::complete` (`src/agent/tests.rs:102`)
  returns `futures::stream::iter(events)` — a **synchronous** stream whose `poll_next` returns
  `Poll::Ready` immediately (no `Pending`, no waker). Its queue is `[TextDelta "Partial summary",
  Finish Stop]` — two events, then `None`.
- The `Interrupt` is buffered in `cmd_rx` *before* `run_turn` is called, so `cmd_rx.recv()` is
  also `Ready` on the first poll.
- `summarize_with_interrupt` (`src/agent/context.rs:259-296`) loops on an **unbiased**
  `tokio::select!` racing `stream.next()` against `cmd_rx.recv()`. When both branches are `Ready`,
  tokio picks one at random.
- Trace of the losing path: iter 1 stream wins (consumes `TextDelta`) → iter 2 stream wins
  (consumes `Finish`, a no-op) → iter 3 `stream.next()` returns `Ready(None)` → `break` with
  `stop_reason` still `None`. `summarize_with_interrupt` then returns a *full* summary
  (`"## Conversation summary\n\nPartial summary"`) with `stop_reason = None`.
- Back in `run_turn` (`src/agent/turn.rs:148`), `if let Some(stop) = stop_reason` is `None` → the
  entire interrupt block (the fix under test) is **skipped**. Execution falls through to the
  normal "complete the deferred swap" path (line 181), which emits `Error { error: "Switched to
  'mock' (summarized for smaller context)" }` (no "interrupt"), then runs the turn on the new
  provider and returns `TurnOutcome { stop_reason: None, … }`.
- Result: `assert!(outcome.stop_reason.is_some())` **fails** and `has_interrupt_note` **fails**
  (no event contains "interrupt") — **with the fix present.** This is a false-negative flake.

The probability the stream wins all three races is `(1/2)³ = 1/8 ≈ 12%`, so the test fails on
roughly 1 in 8 runs even though the code is correct. (The main agent's "1499 tests pass" is
consistent with a single lucky run where the interrupt won — it does not refute the flake.)

**Why this is a finding, not a nit:** the project constitution requires a regression test that
"must fail without the fix and pass with it." This test fails *with* the fix ~12% of the time, so
it does not meet that bar and will cause intermittent red CI builds. It also muddies the
regression signal: a future run that breaks the fix could still pass if the interrupt happens to
fire, and a run with the fix intact can still fail.

**Contrast with the established pattern:** the existing `summarize_with_interrupt_aborts_on_interrupt`
test (`src/agent/context.rs:963`) avoids exactly this race by using `HangingMockProvider`
(`src/agent/context.rs:878`), whose `complete` returns
`stream::once(TextDelta).chain(stream::pending::<LlmEvent>())` — it yields one delta then **pends
forever**, so `cmd_rx.recv()` deterministically wins the `select!`. The LOW 2 test should follow
the same pattern.

**Recommended fix:** make the LOW 2 test's `old_provider` use a provider whose stream pends after
its first delta (so the `Interrupt` deterministically wins the `select!` race). `HangingMockProvider`
is currently private to `context.rs`'s `#[cfg(test)]` module, so either (a) define a small local
hanging mock in `tests.rs` mirroring the `stream::once(...).chain(stream::pending())` construction,
or (b) promote `HangingMockProvider` to `pub(crate)` and reuse it. With a pending stream, the
interrupt fires every run, the test reaches the `if let Some(stop)` block deterministically, and
all five assertions pass reliably with the fix (and fail reliably without the paired `Error` event).

---

### No other new issues

- LOW 1's test is deterministic and correctly targets the `pending_swap = None` clear.
- No `#[allow(...)]`, no unused imports/variables, no platform-specific APIs in the new tests.
- The new tests do not affect README/PLAN/endpoints.toml (backend-internal mechanics).
- The production fix code at `turn.rs:148-163` is unchanged and remains correct (verified in the
  prior review).

---

### Note on verification

I could not run `cargo test` (read-only reviewer). The main agent reports 1499 tests pass with 0
warnings; this is consistent with the LOW 2 test passing on a run where the interrupt won the
`select!` race, and does not contradict the flake identified above. LOW 1 is verified sound by
code inspection. LOW 2's flakiness is established by the stream/`select!` mechanics, not by a
test run, so it holds regardless of the reported pass.
