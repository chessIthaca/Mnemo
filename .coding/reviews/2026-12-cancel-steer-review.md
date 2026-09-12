## Verdict: FINDINGS (0 high, 4 low)

The cancel-steer implementation is **correct across all injection paths**. I verified every path the plan's architecture context flagged: the streaming `select!` (fold), the provider-request buffer (`retain`), the between-tool-call drain (fold), the approval re-injection (fold), the three summarization re-injection sites (`drop_cancelled_steers`), the compact aftermath (`drop_cancelled_steers`), the pre-inject drain in `run_turn_with_retry` (fold), and the between-turns no-op. No race window where a cancelled steer still gets injected was found; no non-steer command is accidentally dropped. All four low findings are documentation/test-completeness items — none block correctness.

---

### What was verified (correctness — PASS)

**StopReason::fold CancelSuggestion arm** (loop_impl.rs:347-378): removes matching text from `Steer`/`InterruptWithSteers`/`CompactWithSteers` via `texts.retain(|t| t != &s)`, collapses empty lists correctly (`Steer([])→None`, `InterruptWithSteers([])→Interrupt`, `CompactWithSteers([])→Compact`), and leaves non-steer stop reasons + `None` untouched via the `other => *current = other` catch-all. ✓

**drop_cancelled_steers** (loop_impl.rs:425-445): HashSet-based, order-independent (cancel may arrive before or after the steer it cancels). Removes all `CancelSuggestion(_)`, removes matching `Suggestion`/`Prompt` texts, keeps all other commands. ✓

**Provider-request select!** (turn.rs:949-953): `buffered_steers.retain(|t| t != &s)` removes the matching steer from the in-flight buffer. ✓

**Streaming select!** (turn.rs:1235-1243): `CancelSuggestion` falls into the `Some(cmd)` catch-all → `fold`. A cancel that collapses `Steer` to `None` does NOT break (only hard stops break), so the turn continues normally — exactly the intended behavior. ✓

**Between-tool-call drain** (turn.rs:1553-1558): `try_recv` loop folds every command including `CancelSuggestion`. ✓

**Approval re-injection** (turn.rs:1640-1657): widened fold arm includes `CancelSuggestion(_)`. ✓

**Summarization re-injection ×3** (turn.rs:127, turn.rs:484, runtime/agent.rs:767): each calls `drop_cancelled_steers(&mut buffered)` before the re-injection loop. ✓

**Pre-inject drain** (runtime/agent.rs:127-138): `while let Ok(cmd) = cmd_rx.try_recv() { fold(...) }` closes the window between turn-end and injection. No double-processing risk — commands consumed by the turn's `select!` are already gone from the channel; only commands arriving in the gap are caught. Folds every command kind, so nothing is silently dropped (an `Interrupt` arriving in the window preserves steers via `InterruptWithSteers`; a `CancelSuggestion` collapses a matching `Steer` to `None`). ✓

**Between-turns no-op** (runtime/agent.rs:566-572): correct — a between-turn `Suggestion` is consumed immediately as a turn, so there's nothing queued to cancel. ✓

**Catch-all buffer sites** (approval.rs:214, context.rs:290, dispatch.rs:428, dispatch.rs:569): all four `Some(other) => buffered.push(other)` sites buffer `CancelSuggestion`, and every re-injection path that consumes them either calls `drop_cancelled_steers` (summarization) or `fold` (approval/ask_user). No non-exhaustive match was missed (Rust's exhaustiveness check + `cargo check` confirm). ✓

**Frontend**: `removeSteer` filters by id (correct); `cancelSuggestion` IPC wrapper mirrors `sendSuggestion`; the X button guards `activeAgent === null` (safer than the `activeAgent!` the plan suggested — an improvement); `cancelSuggestion` is fire-and-forget with `.catch()`. The `X` icon was already imported. ✓

**Security**: `cancel_suggestion` uses the same `send_cmd` helper as `send_suggestion` (same agent_id validation). The `text` parameter is matched against queued steer texts — no execution, no SQL, no shell. No injection/auth concern. ✓

**Constitution**: doc comments on all new public functions (`cancel_suggestion`, `drop_cancelled_steers`, `CancelSuggestion` variant, `removeSteer`, `cancelSuggestion` wrapper). No Windows-only APIs (all cross-platform std: `HashSet`, `Vec`, `retain`). No `#[allow(...)]`. The `cancel` test helper is used by all 4 new fold tests (no dead-code warning). ✓

**Tests**: the 4 fold unit tests exercise the changed `fold` arm (drop-only, preserve-others, InterruptWithSteers collapse, unknown-text noop). The integration test `cancelled_steer_is_not_injected` exercises the streaming path end-to-end through `AgentTask::run`. The `removeSteer` store test is correct. ✓

---

### Finding 1 (Low) — Documentation sync: PLAN.md IPC command list missing `cancel_suggestion`

PLAN.md:518 documents the Tauri IPC bridge command list:
```
- `send_suggestion(agent_id, text)` → `AgentCommand::Suggestion`
```
The new `cancel_suggestion(agent_id, text)` → `AgentCommand::CancelSuggestion` command is **not** listed. Per the project constitution's documentation-sync requirement ("A feature that ships with its docs not updated is an incomplete change"), add it to the list at PLAN.md:518 (right after `send_suggestion`) or to the "Shipped beyond this list" note at line 529.

README.md does not need updating — its "steer" mentions refer to the steering/nudge layer (a different feature), not the steer/suggestion backlog.

### Finding 2 (Low) — `drop_cancelled_steers` cancels `Prompt`s by text (consistency note)

`drop_cancelled_steers` (loop_impl.rs:441-442) removes `C::Prompt { text: s, .. }` whose text matches a cancelled steer. This is **consistent** with how `fold` treats `Suggestion` and `Prompt` identically (loop_impl.rs:340: `C::Suggestion(s) | C::Prompt { text: s, .. }`), so it's a deliberate design choice, not a bug. However, it means a backlog-dispatched `Prompt` with text identical to a cancelled steer (both arriving during a summarization window) would also be silently dropped. This is an extremely narrow edge case and the behavior is arguably correct (the user dismissed that text). The doc comment already says "cancels every matching `Suggestion` / `Prompt` by text" — consider adding a half-line noting *why* Prompt is included (consistency with fold's accumulation rule) so a future reader doesn't mistake it for an oversight.

### Finding 3 (Low) — Missing `CompactWithSteers([]) → Compact` collapse test

The fold unit tests cover `Steer([])→None` and `InterruptWithSteers([])→Interrupt` collapses, but not `CompactWithSteers([])→Compact`. The logic is identical (copy-paste pattern at loop_impl.rs:370-376), so risk is low, but a test would pin the behavior and guard against a future refactor that breaks only that arm. Suggested test:
```rust
#[test]
fn fold_cancel_suggestion_collapses_compact_with_steers_to_compact() {
    let mut reason = Some(StopReason::CompactWithSteers(vec!["queued".into()]));
    StopReason::fold(&mut reason, cancel("queued"));
    assert_eq!(reason, Some(StopReason::Compact));
}
```

### Finding 4 (Low) — Integration test timing dependence (consistent with existing conventions)

`cancelled_steer_is_not_injected` (runtime/agent.rs:1805+) uses 150ms sleeps to ensure the steer and cancel arrive while the `PausingMockProvider` stream is paused on the gate. This matches the existing `mid_turn_steer_soft_stops_and_runs_follow_up_turn` test pattern. The test is **robust to timing variations**: the outcome (no injection, one Finished) is identical whether the commands arrive during the provider-request phase (handled by `buffered_steers.retain`) or the streaming phase (handled by `fold`). The only timing-sensitive point is that the cancel must be processed by the `cmd_rx` arm before `gate.notify_one()` — the 150ms sleep before the notify ensures this. No action required; noted for completeness.
