## Verdict: FINDINGS (1 high, 0 low)

The hybrid drain/cancel fix is correctly implemented and well-tested for its stated scope — steers drain the batch, hard stops record cancelled calls in history with sanitized args, mid-execution stops grace-drain fast calls with their real result. One real gap: the new 2-second grace window makes a pre-existing `StopReason::fold` asymmetry practically hittable — a steer typed during the window is silently dropped end-to-end, which the new test `commands_arriving_during_the_grace_window_are_not_lost` claims cannot happen.

**Scope reviewed:** all uncommitted changes on wt/agenticcoding (git diff HEAD + untracked) — src/agent/turn.rs (post-stream block rewrite, null-turn/bad-JSON-MAX stop_reason fixes, bad-JSON retry steer gate, stream-error retry steer guard), src/agent/loop_impl.rs (`StopReason::is_hard_stop`), src/agent/dispatch.rs (`DRAIN_GRACE_MS` + grace-window drain), src/agent/tests.rs (SleepTool + 7 new/rewritten tests), frontend/src/hooks/useAgentStore.test.ts (1 regression test), and the .coding knowledge/backlog/plan updates.


## Verified correct

1. **Steer fall-through — no drop, no double-injection.** A mid-stream bare Steer folds into `stop_reason`, the stream runs to Finish (only hard stops break it), the post-stream block falls through the `is_hard_stop` gate, the batch drains, and the post-batch return (turn.rs:2139-2153) carries the steer — pinned by `mid_stream_steer_drains_the_announced_tool_call` and `mid_stream_steer_drains_a_multi_call_batch`. The steer is consumed from cmd_rx exactly once and pushed as a user message exactly once (agent.rs:186-208); no path both carries it in `stop_reason` and re-queues it.

2. **Bad-JSON retry gate.** With a steer pending, the retry returns (turn.rs:1656-1674) *after* the assistant message + per-call error results were re-injected — history stays consistent for the follow-up turn, and the steer is carried. The MAX-retries and null-turn returns now carry `stop_reason.take()` instead of hardcoded `None`.

3. **Stream-error retry guard.** The `Err` return requires `stop_reason.is_none()` (turn.rs:1448-1452) — a pending steer falls through to the non-fatal note path and is carried.

4. **Hard-stop history recording.** turn.rs:1366-1445: truncated args sanitized to `"{}"` (bad-JSON precedent), `raw` dropped when any args are bad, the assistant message carries the accumulated tool_calls, one `"interrupted: not run (turn stopped)"` tool_result per call id, UI synthetic events + Finished preserved. The every-call-has-a-result invariant is asserted in tests.

5. **Provider safety of the sanitized record.** OpenAI: `raw` is kept only when all args parse, and `raw_is_usable` (openai.rs:2276) independently validates the raw's args, falling back to field construction. Anthropic: field construction skips empty text (anthropic.rs:474) and parses args into a tool_use `input` object with a `{}` fallback (anthropic.rs:494-495) — `Message::assistant("", calls)` with `raw: None` produces a valid tool_use-only content array. No dangling-tool_calls 400 path remains.

6. **Grace-window mechanics.** dispatch.rs:546-565: the pinned future is awaited bounded by `DRAIN_GRACE_MS`; completion returns the real result through the same consolidation-note/steering-metrics/escalation pipeline as the normal path; timeout drops the future (kill_on_drop at function exit) and returns the synthetic error. Stop is delayed at most 2s. Commands arriving during the window queue in cmd_rx (bounded channel — senders await; no loss at the dispatch level).

7. **Constitution.** `is_hard_stop` is pub with a doc comment; `DRAIN_GRACE_MS` documented; zero `#[allow]` in the tree; the new tests use only `tokio::time`/`std::time` (platform-neutral, no Windows-only APIs); the regression tests fail without the fix (the old post-stream block ended the turn pre-batch; the old dispatch arm dropped the future immediately).

8. **Documentation sync.** Knowledge files properly superseded/added (bug `-2.md` supersedes the original with markers; decision record added); README.md/PLAN.md carry no stale turn-lifecycle prose (checked — the interrupt/steer hits there are backlog/steering-marker prose, unrelated); the behavior contracts live in the updated turn.rs/dispatch.rs/loop_impl.rs doc comments.


## Finding 1 (HIGH): a steer typed during the 2-second grace window is silently dropped end-to-end

**Scenario.** The user presses Stop while a tool call is executing, then types a follow-up message within the grace window (the agent is still running — Finished hasn't fired yet).

**Trace (every step verified in code):**
1. dispatch.rs:546-565 — the grace window (`tokio::time::timeout(DRAIN_GRACE_MS, &mut tool_fut)`) polls only the tool future; the steer (`Suggestion`/`Prompt`) queues in cmd_rx. This dispatch-level contract is exactly what the new test pins.
2. The tool completes within the window → real result; `stop_signal = Some(Interrupt)` (latched before the window opened).
3. turn.rs:1829-1843 — merge: `hard_stop = true`; `fold(Interrupt)` into `stop_reason` (None) → `Some(Interrupt)`.
4. turn.rs:2101-2115 — `if hard_stop` → `break`: the between-call safe point (the cmd_rx drain at the top of each batch iteration) never runs again.
5. turn.rs:2139-2153 — the turn returns `TurnOutcome { stop_reason: Some(Interrupt) }`.
6. agent.rs:182-184 — the run loop's pre-inject drain consumes the queued Suggestion and folds it: `fold(Suggestion)` with `current = Some(Interrupt)` hits the `_ => {}` arm (loop_impl.rs:431) — the steer text is discarded.
7. agent.rs:219 — `Some(Interrupt) => AfterTurn::Continue`: the agent parks. The user's message never becomes a user message and never drives a turn. Silent drop.

**Why this is a finding on this change.** The drop mechanism (fold's `_ => {}` asymmetry + the pre-inject drain) is pre-existing, but the old Interrupt arm returned immediately — the vulnerable window between the Interrupt being consumed and the pre-inject drain was microseconds. The new 2-second grace window makes it practically hittable: press Stop, type "actually do X instead", lose the message. This contradicts the `InterruptWithSteers` design intent ("Stop flushes pending commands" — agent.rs:188-195) and is exactly the bug class this plan eliminates. The new test `commands_arriving_during_the_grace_window_are_not_lost` (tests.rs:5386) pins only the dispatch-level contract; its comment claims the pre-inject drain "folds it into the stop reason", but folding a Suggestion into `Some(Interrupt)` is a no-op — the end-to-end contract the test's name asserts does not hold.

**Sibling seam (same scenario, different arrival order).** A steer buffered during tool execution *before* the Interrupt (dispatch.rs:567-568) is dropped by the `if !hard_stop` skip at turn.rs:1852 — the buffered re-injection is skipped when hard_stop is set. So in every "Stop + steer during tool execution" combination the steer is dropped. (Asymmetry: a steer folded *before* the Interrupt — e.g. mid-stream — IS preserved via `fold(Interrupt)` → `InterruptWithSteers`.)

**Suggested fix (small, localized):**
- loop_impl.rs:426-432 (fold's Suggestion arm): promote `Some(Interrupt)` → `InterruptWithSteers(vec![s])` and `Some(Compact)` → `CompactWithSteers(vec![s])`, mirroring the Interrupt arm's steer preservation. The run loop already handles both outcomes (agent.rs:186-208, 214-216). Cancel/Clear keep dropping (documented intent).
- For the buffered seam: fold buffered Suggestion/Prompt commands into `stop_reason` *before* folding the Interrupt signal in the merge block (turn.rs:1836-1842), or let the re-injection loop at turn.rs:1852 run for steers even under hard_stop.
- Extend `commands_arriving_during_the_grace_window_are_not_lost` (or add a companion test) to pin the end-to-end contract: after the pre-inject drain fold, the outcome's stop_reason is `InterruptWithSteers([steer])` and the steer drives a follow-up turn.

## Notes (non-finding)

- The pre-stream hard-stop check (turn.rs:1002-1025) still uses an explicit `matches!` list rather than the new `is_hard_stop` helper — same semantics, harmless duplication.
- `tool_calls_made: 0` in the hard-stop TurnOutcome matches the old behavior (no calls ran); no accounting consumer is affected.
- Security: no new attack surface; the sanitization reduces risk (malformed args are never re-sent; the synthetic result strings are static).
