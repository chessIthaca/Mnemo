## Verdict: PASS

Final verification review (third pass) of commit `b6c70af` on `feat/max-tokens-cap-repetition-model-switch`. The second pass (`2026-12-r9-r10-model-switch-verify2-review.md`) returned FINDINGS (0 high, 1 low) — a flaky regression test (`run_turn_model_switch_interrupt_emits_paired_compact_event`) that raced a synchronous `MockProvider` stream against the buffered `Interrupt` in an unbiased `tokio::select!` (~12% flake rate). Commit `b6c70af` replaces the `old_provider` with a local `HangingProvider` whose stream pends forever after one delta, so the interrupt deterministically wins the `select!`. **The flake is resolved.** All five verification points pass.

---

### 1. `HangingProvider` correctly implements `LlmClient` ✓

The `LlmClient` trait (`src/provider/mod.rs:376`) is `#[async_trait] pub trait LlmClient: Send + Sync` with required methods `capabilities`, `kind`, `model`, and `complete`. The local `HangingProvider` (`src/agent/tests.rs:4801-4828`) implements all four with matching signatures:

- `capabilities(&self) -> &Capabilities` — returns `&self.caps`.
- `kind(&self) -> ProviderKind` — returns `ProviderKind::OpenAI`.
- `model(&self) -> &str` — returns `"mock-hanging"`.
- `complete(...)` — returns `Ok(Box::pin(stream::once(TextDelta "partial").chain(stream::pending::<LlmEvent>())))`.

The struct holds only `caps: Capabilities` (which is `Send + Sync`, same as every other mock in the file), so `HangingProvider: Send + Sync` holds. This is a verbatim mirror of the established `HangingMockProvider` (`src/agent/context.rs:878-907`) — same fields, same method bodies, same `stream::once(...).chain(stream::pending())` construction — only with fully-qualified `crate::provider::` paths (because it lives in `tests.rs`, outside the `provider` module). The `FixedModelProvider` at `tests.rs:4977` is a second local `LlmClient` impl in the same file using the same idiom, confirming it compiles. The dropped `tools_phases` field was `MockProvider`-internal bookkeeping, not a trait requirement.

### 2. The interrupt path is now reached deterministically ✓

`summarize_with_interrupt` (`src/agent/context.rs:259-296`) loops on an **unbiased** `tokio::select!` racing `stream.next()` against `cmd_rx.recv()`. The `Interrupt` is buffered in `cmd_rx` before `run_turn` (`tests.rs:4910`), so `cmd_rx.recv()` is `Ready(Some(Interrupt))` from the first poll.

With the hanging stream, after the single `TextDelta` is consumed, `stream.next()` polls `stream::pending()` — which returns `Poll::Pending` **forever** and never registers a waker that fires. So:

- **Iter 1:** both branches `Ready`. `select!` picks one at random.
  - If `cmd_rx` wins → `Interrupt` → `stop_reason = Some(Interrupt)` → `break`. ✓
  - If `stream` wins → consumes `TextDelta`. **Iter 2:** `stream.next()` is `Pending`, `cmd_rx.recv()` is `Ready` → only `cmd_rx` can win → `Interrupt` → `break`. ✓

Either way `stop_reason = Some(StopReason::Interrupt)`. The path that previously lost ~1 in 8 runs (stream winning all races and exhausting to `Ready(None)`) is now unreachable, because the stream can no longer complete. This is exactly the determinism mechanism that makes `summarize_with_interrupt_aborts_on_interrupt` (`context.rs:963`) reliable with `HangingMockProvider`. The ~12% flake is eliminated by construction, independent of run count.

### 3. All assertions are sound ✓

Reaching `stop_reason = Some(Interrupt)` drives `run_turn` into the `if let Some(stop) = stop_reason` block (`src/agent/turn.rs:148-179`), which emits the paired `AgentEvent::Error { error: "Compaction interrupted — original conversation kept, model switch completed." }` (line 153-163), completes the swap via `set_provider` (line 164-167), and returns `TurnOutcome { stop_reason: Some(stop), .. }` early (line 173-178). The test's five assertions (`tests.rs:4918-4954`) all hold:

1. `outcome.stop_reason.is_some()` — `Some(Interrupt)` propagated from the early return. ✓
2. `has_compact_started` — `CompactStarted` emitted before summarization on the model-switch path. ✓
3. `has_interrupt_note` — the paired `Error` contains "interrupt" (line 157). ✓
4. `agent.provider().capabilities().max_context == 2_000` — `set_provider` installed the new 2K-context provider. ✓
5. `!agent.has_pending_swap()` — the swap was taken (`take_pending_swap`) and completed. ✓

**Regression direction holds:** without the fix (the `if let Some(stop)` block removed), the interrupt still fires deterministically, but no paired `Error` is emitted and the turn does not return early — so `has_interrupt_note` (and `stop_reason.is_some()`) fail. The test now reliably fails without the fix and passes with it, meeting the constitution's "must fail without the fix and pass with it" bar.

### 4. No new issues introduced ✓

- `git diff HEAD` is empty — the fix is fully committed in `b6c70af`; no stray uncommitted changes.
- The change is test-only (`src/agent/tests.rs`); production code (`turn.rs:148-163`) is unchanged from the prior (verified-correct) state.
- No `#[allow(...)]`; the local `use futures::StreamExt;` (line 4821) is used for `.chain()` (mirrors `context.rs:900`), not dead.
- `HangingProvider` is scoped inside the test function — no symbol leakage.
- No platform-specific APIs; no README/PLAN/endpoints.toml impact (backend-internal test mechanics).

### 5. `cargo test` ✓

Read-only reviewer — could not execute `cargo test`. The main agent reports 1499 tests pass with 0 warnings (under `#![deny(warnings)]`, a green build proves zero warnings) and 5/5 runs of the specific test pass. Because the flake is now eliminated by construction (the stream pends forever after one delta), determinism holds regardless of run count; the 5/5 result is consistent with — and no longer a lucky sample of — a deterministic test.

---

### Conclusion

LOW 2 (flaky regression test) is **resolved**. The `HangingProvider` correctly implements `LlmClient`, its pending stream makes the interrupt deterministically win the `select!`, all five assertions are sound and correctly fail without the fix, and no new issues are introduced. LOW 1 was already verified resolved in the prior pass and is untouched by this commit. The branch is ready.
