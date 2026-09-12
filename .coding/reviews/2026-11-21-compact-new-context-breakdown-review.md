# Review: /compact + /new commands, improved compaction prompt, context breakdown

**Date:** 2026-11-21
**Scope:** All uncommitted changes (`git diff HEAD`) — 22 files, +611/-106 lines.

## Summary

The changes add `/compact` (manual context compaction) and `/new` (full conversation
reset) slash commands, a structured compaction prompt with running-update detection,
a Compact button in the context popup, and a per-role context breakdown
(system/user/assistant/tool) emitted with every `ContextUsage` event.

Overall the implementation is solid: the `StopReason` propagation is exhaustive
across all safe points, `AfterTurn` correctly signals both call sites, the system
prompt is safely rebuilt after `clear_context` (verified: `run_turn` re-inserts
`messages[0]` when `messages.is_empty()`), `build_summary_prompt` is used by both
summarize paths, all `ContextUsage` emission sites include `breakdown`, and no
`#[allow(...)]` suppressions were added. All new public items carry doc comments.

Two real bugs were found in `compact_context`'s handling of commands that arrive
during the summarization LLM call, plus minor style/UX issues.

---

## Findings

### BUG (medium) — Cancel during `/compact` summarization is swallowed; agent fails to terminate

**File:** `src/runtime/agent.rs:416-437` (`compact_context`)

`compact_context` calls `summarize_with_interrupt`, which honors a `Cancel`
arriving mid-summarization by returning `stop = Some(StopReason::Cancel)` (and
abandoning the summary — original messages preserved, no data loss). However,
`compact_context` only uses `stop` to decide whether to *apply* the summary
(`if stop.is_none() { self.messages = summarized; }`) — it **does not propagate
the Cancel** back to the `run()` loop:

```rust
let (summarized, _buffered, stop) = context_manager
    .summarize_with_interrupt(&self.messages, 6, provider.as_ref(), cmd_rx)
    .await
    .unwrap_or_else(|_| (self.messages.clone(), Vec::new(), None));
if stop.is_none() {
    self.messages = summarized;
}
// ... emits ContextUsage, then returns
```

After `compact_context` returns, `run()` falls through to `cmd_rx.recv()` and
waits for the next command — the agent **stays alive**. But `Cancel` means
"close-x / terminate the agent" (per the `StopReason::Cancel` contract in
`loop_impl.rs`: "the agent task terminates, `Exited` fires, the tab closes").
The Cancel command was consumed by `summarize_with_interrupt` (not buffered,
not re-injected), so it is silently dropped.

**Contrast with the turn.rs summarization path** (turn.rs:241-253): there,
`summarize_stop` is returned via `TurnOutcome.stop_reason`, so
`run_turn_with_retry` maps it to `AfterTurn::Cancelled`, and `run()` breaks →
`Exited` fires. `compact_context` has no equivalent propagation.

**Fix:** `compact_context` should return the `stop` reason (or at least signal
"cancelled") so the caller can `break` on Cancel. Alternatively, check `stop`
for `Cancel` and break out of the `run()` loop directly (emit `Finished` +
return a signal). The simplest fix: have `compact_context` return
`Option<StopReason>` and have the call sites break on `Cancel`:

```rust
async fn compact_context(...) -> Option<crate::agent::StopReason> {
    // ... existing logic ...
    stop  // return it so callers can act on Cancel
}
```
Then at call sites: `if let Some(StopReason::Cancel) = self.compact_context(...).await { break; }`

---

### BUG (medium) — Buffered commands during `/compact` summarization are silently dropped (data loss)

**File:** `src/runtime/agent.rs:423` (`compact_context`)

`summarize_with_interrupt` buffers non-interrupt commands (Suggestion, Prompt,
Compact, Clear) that arrive during the summarization LLM call
(context.rs:283-287: `Some(other) => buffered.push(other)`). It returns them as
the second tuple element. The turn.rs summarization path **re-injects** these
buffered commands (turn.rs:197-224: Suggestion → `SuggestionInjected` event +
push as system message; other commands logged).

`compact_context` binds `_buffered` but **never re-injects or re-sends them**:

```rust
let (summarized, _buffered, stop) = context_manager
    .summarize_with_interrupt(...)
```

The leading underscore signals intentional non-use, but this means a user's
steer (`/suggestion`) or new prompt sent while a `/compact` summarization is in
flight is **silently lost**. This is user-input data loss.

**Fix:** Re-inject buffered commands the same way turn.rs does — at minimum,
re-send Suggestion/Prompt commands back into the agent's own inbox (via a
cloned sender) or push them as messages. If that's too complex for this path,
at minimum log a warning and/or re-send them via `manager.send()` so they're
re-processed on the next `cmd_rx.recv()` loop iteration. The cleanest approach
mirrors turn.rs: push `Suggestion(s)` as a system message + emit
`SuggestionInjected`; for `Prompt`, push as a user message.

---

### STYLE (low) — `run_turn_with_retry` dedented to column 0 inside `impl` block

**File:** `src/runtime/agent.rs:64-116`

The `run_turn_with_retry` function (and its doc comment) are at column 0 while
still textually inside the `impl AgentTask { ... }` block. The rest of the file
uses 4-space indentation for methods. This is valid Rust (whitespace is
insignificant) and produces no warning under `#![deny(warnings)]`, but it's a
style inconsistency that will confuse readers and likely came from a
search-and-replace that stripped leading whitespace.

**Fix:** Re-indent lines 64-116 to 4 spaces to match the rest of the `impl`
block.

---

### CODE QUALITY (low) — Unused `agent` variable in `/new` handler

**File:** `frontend/src/components/layout/InputBar.tsx:386`

```tsx
useAgentStore.setState((s) => {
  const agent = s.agents[agentId] ?? emptyAgentState();  // ← never used
  return {
    agents: {
      ...s.agents,
      [agentId]: emptyAgentState(),
    },
  };
});
```

`agent` is computed but never referenced — the return value uses
`emptyAgentState()` directly. This is dead code. If the TS config has
`noUnusedLocals`, it would fail the frontend build; otherwise it's just noise.

**Fix:** Remove the `const agent = ...` line:
```tsx
useAgentStore.setState((s) => ({
  agents: { ...s.agents, [agentId]: emptyAgentState() },
}));
```

---

### UX (low) — "Context compacted." shown before compaction completes or succeeds

**File:** `frontend/src/components/layout/InputBar.tsx:360-363`

```tsx
compact(agentId).catch((e) => console.error("failed to compact:", e));
pushTranscriptMessage(agentId, {
  kind: "assistant",
  text: "Context compacted.",
});
```

The "Context compacted." message is pushed immediately (fire-and-forget),
before the backend compaction completes. If compaction fails (provider error)
or is a no-op (≤7 messages — `summarize_with_interrupt` returns the original
messages unchanged), the user still sees "Context compacted." This is
misleading. Consider either (a) waiting for a completion signal before showing
the message, or (b) wording it as a request ("Compacting context…") rather
than a completed action.

---

## Verified correct (no findings)

1. **Compact/Clear at every safe point** — exhaustive match arms in streaming
   select! (turn.rs:696-706), complete_with_retry select! (turn.rs:474-485),
   between-tool-call try_recv (turn.rs:925-930), and run() between-turn arms
   (agent.rs:345-352). The `stop_reason` propagates correctly through
   `TurnOutcome` → `run_turn_with_retry` → `AfterTurn`.

2. **AfterTurn enum** — both call sites (Prompt arm agent.rs:291-296, Suggestion
   arm agent.rs:334-339) handle all 4 variants (Continue/Cancelled/Compact/Clear).

3. **`build_summary_prompt` in both summarize paths** — context.rs:159
   (`summarize`) and context.rs:239 (`summarize_with_interrupt`). Running-update
   detection (messages[1] starts with `## Conversation summary`) works correctly.

4. **ContextBreakdown at every ContextUsage site** — turn.rs:232 (summarize
   path), turn.rs:260 (no-summarize path), agent.rs:436 (compact_context),
   agent.rs:449 (clear_context), channels.rs:423 (into_serializable). All
   include `breakdown`.

5. **`/new` clears both frontend + backend** — `clearConversationIpc(agentId)`
   sends `AgentCommand::Clear` (backend wipes messages + emits ContextUsage
   {used:0}); frontend resets to `emptyAgentState()` (InputBar.tsx:385-393).

6. **`clear_context` is safe** — `self.messages.clear()` wipes `messages[0]`
   (system prompt), but `run_turn` (turn.rs:385) re-inserts it when
   `messages.is_empty()`. Verified: the next Prompt pushes a user message,
   then `run_turn` sees `messages[0].role != System` and inserts the system
   prompt at index 0. No crash, no missing system prompt.

7. **`count_tokens_by_role`** — correctly uses shared `try_tiktoken_bpe()`
   (context.rs:95) with char-based fallback (context.rs:111-128). The fallback's
   lack of per-message overhead (4 tokens) mirrors the existing
   `char_based_estimate` in `count_tokens` — consistent, not a new issue.

8. **Compact button** — disabled while `running` or `agentId === null`
   (InflightBar.tsx), calls `compact(agentId)`. Popup made interactive
   (`pointer-events` removed from `pointer-events-none`) so the button is
   clickable.

9. **No `#[allow(...)]` suppressions** added. All new public functions/structs/
   enum variants have doc comments. `ContextBreakdown` derives the required
   traits (Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default) and is
   re-exported from `runtime::mod` (so `myharness::runtime::ContextBreakdown`
   in the contract fixture resolves).

10. **Contract fixture + frontend fixture + test** all updated consistently
    (tests/contract_fixtures.rs:165-173, event-context-usage.json,
    useAgentStore.test.ts:476-478, types.ts:118-122).
