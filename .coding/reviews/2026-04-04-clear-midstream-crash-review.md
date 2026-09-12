# Review: Fix crash on clear conversation mid-stream

**Date:** 2026-04-04
**Reviewer:** read-only subagent
**Scope:** all uncommitted changes (`git diff`)

## Files changed
- `frontend/src/components/layout/InputBar.tsx` — `/clear` now calls `interrupt(agentId)` before `clearConversation(agentId)` when `running`.
- `frontend/src/hooks/useAgentStore.ts` — doc comment on `clearConversation`.
- `frontend/src/hooks/useAgentStore.test.ts` — regression test `testClearConversationWipesTranscript`.
- `.coding/plans/stack.json` — plan bookkeeping (ignored).

## Summary

The fix adds a fire-and-forget `interrupt()` before `clearConversation()` in the `/clear` slash-command handler, gated on the `running` flag. The backend `Interrupt` makes the streaming `select!` break and emit `Finished` (`src/agent/turn.rs:343-347`), so (in principle) no further events repopulate the cleared state. The direction is correct and the only caller of `clearConversation` is this `/clear` path (confirmed via search — no other callers). However, the fix has two concrete gaps relative to its own stated goal, detailed below.

---

## Findings

### BUG (high) — `clearConversation` does not clear `pendingApproval`

`frontend/src/hooks/useAgentStore.ts:1276-1285`:

```ts
clearConversation: (id) =>
  set((s) => {
    const agent = getOrCreate(s.agents, id);
    return {
      agents: {
        ...s.agents,
        [id]: { ...agent, transcript: [], streamingText: "", steers: [] },
      },
    };
  }),
```

The wipe clears `transcript`, `streamingText`, and `steers` — but **not** `pendingApproval`. The fix's own root-cause analysis (the new doc comment at `useAgentStore.ts:393-403` and the InputBar comment at `InputBar.tsx:234-242`) explicitly names a *dangling `pendingApproval`* as the crash vector ("leave a dangling `pendingApproval` (its tool card was just wiped)"). Yet neither `clearConversation` nor the interrupt clears it.

Consequence: if the user presses `/clear` while an approval is pending (a very plausible mid-stream state — the agent is `running` and blocked on approval), the `ApprovalPrompt` card remains visible after the clear, referring to a `toolCallId` whose tool card was just wiped from the transcript. The interrupt does not clear frontend `pendingApproval` (it only stops the backend). So the exact inconsistent state the fix set out to eliminate can still occur.

**Recommended fix:** add `pendingApproval: null` (and likely `streamingReasoning: ""`, `activityLog: []`) to the spread at `useAgentStore.ts:1282` so the wipe is complete. This is the single most important gap.

### BUG (medium) — fire-and-forget interrupt leaves a race window; events in flight still repopulate cleared state

`frontend/src/components/layout/InputBar.tsx:243-248`:

```ts
if (running) {
  interrupt(agentId).catch((e) =>
    console.error("failed to interrupt before clear:", e),
  );
}
clearConversation(agentId);
```

`interrupt()` is an async IPC (`invoke("interrupt", ...)` in `frontend/src/lib/tauri.ts:39-41`). It is **not awaited** — `clearConversation` runs synchronously on the next line, before the interrupt has reached the backend, let alone before the backend's `select!` breaks. The comment at `InputBar.tsx:240-242` claims "no more events arrive to repopulate the cleared state," but that overstates the guarantee:

1. **rAF text buffer (`frontend/src/hooks/useAgentEvents.ts:118-124, 84-93`):** `text_delta` fragments are buffered in `buffersRef` and flushed once per animation frame via `requestAnimationFrame`. `clearConversation` sets `streamingText = ""`, but a pending rAF flush will call `appendStreamingText(id, text)` (`useAgentStore.ts:1315-1324`) on the next frame, re-populating `streamingText` with whatever was buffered before the interrupt took effect. The cleared streaming text reappears.

2. **Events already in the Tauri channel:** events emitted by the backend before the `Interrupt` is processed (a `tool_call_start` / `approval_request` / `tool_result` already sent through `agent://event`) are still delivered to `activeDispatch` and dispatched to the store *after* `clearConversation` has run, re-populating the transcript / setting a fresh `pendingApproval`.

3. **Failed interrupt:** if `interrupt()` rejects (backend error, agent gone), the `.catch` logs and swallows it; `clearConversation` has already run. The agent keeps running and emitting events into the cleared store — the exact condition the fix exists to prevent.

The interrupt *narrows* the window but does not *close* it. For a true fix, `clearConversation` (or the `/clear` handler) would need to also drain/cancel the rAF buffer and tolerate late-arriving events — e.g. by clearing `pendingApproval` (see finding above) and by having the event dispatcher ignore events for a just-cleared agent, or by awaiting the interrupt before clearing.

(Note: from reading the consumers — `Conversation.tsx:144-146`, `ApprovalPrompt.tsx`, `DiffViewer.tsx:22-83` — a dangling `pendingApproval` does not itself throw; `ApprovalPrompt`/`DiffViewer` read args from the approval object, not the transcript. So the original "crash" may be milder than the root-cause narrative implies. Regardless, the repopulation race is a real correctness defect.)

### CORRECTNESS (low) — the regression test does not exercise the fix and is not executed

`frontend/src/hooks/useAgentStore.test.ts:204-226` (`testClearConversationWipesTranscript`):

- The test exercises only the pure store action (`clearConversation`). It does **not** exercise the actual fix (the `interrupt`-before-`clear` ordering in `InputBar`), because `InputBar` is a React component not covered here. The test comment acknowledges this, so it is a contract pin, not an end-to-end regression. Acceptable, but the "regression test" label overstates coverage.
- The test seeds `started` + `text_delta` + `addSteer` but does **not** seed an `approval_request`, so it does not assert `pendingApproval` is cleared — consistent with the implementation gap in the high-severity finding above. If the test seeded an approval and asserted `pendingApproval === null` after clear, it would fail and expose the gap.
- Per the file header (`useAgentStore.test.ts:4-8`), these tests are **not executed by any runner** (no vitest/jest configured); they are only type-checked via `tsc --noEmit`. The regression test therefore provides no runtime guarantee. This is a pre-existing limitation of the test setup, not introduced by this change, but worth noting since the constitution requires "run `cargo test` before marking complete" and these frontend tests do not run at all.

### CONSTITUTION COMPLIANCE (low, pre-existing) — `interrupt` and other public functions in `tauri.ts` lack doc comments

The project constitution requires "All public functions must have doc comments." The fix relies on `interrupt` (`frontend/src/lib/tauri.ts:39-41`), which has no doc comment. This is pre-existing (not introduced by this diff — the diff only adds a call site), but since the fix depends on the function and the doc-comment contract on `clearConversation` was just added, the asymmetry is worth noting. Other undocumented public functions in the same file (`cancel`, `approve`, `setSafetyMode`, `getSafetyMode`, `listAgents`, `readFile`, `listFiles`, `getGitBranch`) are likewise pre-existing.

### OBSERVATION (info) — `/load` has the same class of bug

`InputBar.tsx:301-334` (`/load` case) replaces the transcript directly via `useAgentStore.setState` without interrupting when running. If the agent is mid-stream, subsequent events re-populate/append to the replaced transcript — the same pattern this fix addresses for `/clear`. Out of scope for this change, but the same root cause.

---

## Verdict

The fix is directionally correct (stopping the backend before clearing is the right instinct, and `running` at `InputBar.tsx:27` correctly reflects mid-stream state). But it is **incomplete**:

1. `clearConversation` does not clear `pendingApproval` — the very field the root-cause analysis blames for the crash (high severity, one-line fix).
2. The fire-and-forget interrupt leaves a real race where in-flight events (rAF-buffered `text_delta`, channel-queued events) re-populate the cleared state (medium severity).

Recommend addressing finding #1 (clear `pendingApproval` in `clearConversation`) at minimum before merging; finding #2 ideally by awaiting the interrupt and/or draining the rAF buffer.
