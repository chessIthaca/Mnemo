## Verdict: FINDINGS (1 high, 2 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 225e0dad (stable entryId keys — mem-perf review HIGH 3). The core fix is correct and well-tested: entryId stamping is complete across every transcript write site, the spread discipline preserves ids at all replacement sites but one, the key expressions are sound (non-empty `turn[0][0]`/`run[0]`, unique fallbacks), and the regression tests genuinely pin the fix. One high finding: `/load` imports foreign entryIds from a saved conversation without advancing the module counter, so the next allocation can collide — duplicate React keys, the exact misattribution class this plan eliminates, in the standard save→load→resume flow.

### HIGH 1 — /load imports foreign entryIds without advancing the counter → duplicate keys on resume

**Location:** `frontend/src/hooks/agentState.ts:411-430` (`nextEntryId` / `allocEntryId` / `stampEntryIds`), `frontend/src/components/layout/InputBar.tsx:533` (/load).

**Root cause:** `stampEntryIds` returns already-stamped entries untouched and never advances `nextEntryId` past the ids it passes through. `/load` is the only path that imports entries with pre-existing entryIds into the store — a post-fix `/save` serializes them, and the plan intends ids to round-trip.

**Impact (guaranteed, not just possible):** In a fresh app process `nextEntryId = 1`, and a conversation saved from any fresh process starts at entryId 1. Load that save into another process (the standard "resume later" flow): the transcript holds ids 1..N while the counter still sits at 1. The next user message calls `allocEntryId()` → 1 → the new user entry collides with the loaded first entry. Both are turn-starting entries, so `turns.map` renders two sibling turns keyed `1` — React "Encountered two children with the same key" plus undefined reconciliation between the two positions: row state can transfer between unrelated turns. That is precisely the state-misattribution class this plan set out to eliminate, reintroduced through the round-trip the plan explicitly supports. Also reachable in-session whenever the loaded save's max id exceeds the current counter position.

**Fix (one loop):** in `stampEntryIds`, advance the counter past the max id seen so future allocations can't collide:

```ts
export function stampEntryIds(entries: TranscriptEntry[]): TranscriptEntry[] {
  for (const e of entries) {
    if (e.entryId !== undefined && e.entryId >= nextEntryId) nextEntryId = e.entryId + 1;
  }
  if (entries.every((e) => e.entryId !== undefined)) return entries;
  return entries.map((e) => (e.entryId === undefined ? { ...e, entryId: allocEntryId() } : e));
}
```

Add a regression test: stamp an array containing a high foreign id (e.g. 5000), then assert `allocEntryId()` returns > 5000. This also makes the existing `> 41` assertion in the stampEntryIds unit test hold in any execution order (see LOW 2).

### LOW 1 — vision retry replaces the running entry with a fresh literal → new entryId mid-stream

**Location:** `frontend/src/hooks/agentEventReducer.ts` `reduceVisionDescribe` (~870-886).

The retry path (`runningIdx >= 0`) does `transcript[runningIdx] = entry` where `entry` is a fresh literal (`{ kind: "vision", index, total, query, description: null, success: true, running: true }`) — no spread of the previous entry. The identity pass therefore stamps a NEW entryId (and ts), so the vision card's React key changes on provider retry: the row unmounts/remounts and local expanded state resets. The plan explicitly listed vision retry among the replacement sites that must spread so ids survive; every other replacement site (tool_call_start merge, arg deltas in both the reducer and the store's batched `applyToolCallArgDeltas`, tool_result finalization ×2, vision_described, sweepRunningCards ×3) correctly spreads. ts has the identical pre-existing behavior, so this is consistent with the ts mechanism — but it deviates from the plan's stated stability goal.

**Fix:** `transcript[runningIdx] = { ...transcript[runningIdx], ...entry };` — the literal's explicit fields win, entryId/ts carry over.

### LOW 2 — order-dependent assertion in the stampEntryIds unit test

**Location:** `frontend/src/hooks/useAgentStore.test.ts:2179-2192`.

`expect(out[1].entryId!).toBeGreaterThan(stamped[0].entryId!)` (41) passes only because earlier describes in the same file have already advanced the module counter past 41 (the cap test alone allocates 1003 ids). Run the test in isolation (`vitest -t "stamps id-less"`) and the counter starts at 1 → `out[1].entryId === 1` → the assertion fails. The test also doubles as the canary for HIGH 1: it feeds a foreign id (41) into the sequence below the counter.

**Fix:** drop the `> 41` comparison and assert only what is order-independent (out[0] is the same object; out[1] < out[2]; re-stamping returns the same array) — or land HIGH 1's counter-sync, which makes the assertion hold in any order.

### Verified clean (plan checklist)

- **Stamping completeness** — every transcript write site audited by search (`transcript:`, `.transcript =`, `transcript.push`, `transcript[i] =`) plus full reads of the reducer, store, and InputBar:
  - Reducer-internal, stamped by applyAgentEvent's identity pass: flushStreamingText (fresh assistant), all pushTranscriptEntry callers (compacted, compact_started, suggestion_injected, prompt_dispatched, skill_started, qa), tool_call_start (memory + tool entries), memory_recalled, error, child_finished, vision_describe. All 16 flushStreamingText/pushTranscriptEntry call sites live in agentEventReducer.ts — no external callers.
  - Direct store actions: `applyToolCallArgDeltas` (useAgentStore.ts:1087) only replaces via `{ ...entry, calls }` — id survives, no new entries created; `recordQuestionAnswer` stamps at creation (agentEventReducer.ts:1006); `clearConversation` sets `[]`.
  - InputBar: pushTranscriptMessage (`msg.entryId ?? allocEntryId()`), handleSend's user entry, /load (`capTranscript(stampEntryIds(parsed))`).
  - No other write sites exist in the tree.
- **Identity-pass restructure** — `if (old.has(e)) continue;` is behavior-preserving for ts (carried-over entries always have ts) and adds entryId stamping; mutating pre-write is safe (per-kind reducers return fresh objects; nothing in `old` is aliased).
- **The keys** — `turn[0][0]`/`run[0]` are guaranteed non-empty (grouped turns seed `[entry]`; chunkRuns seeds `[e]`); fallback keys `t${ti}`/`r${ri}`/`i${i}` are prefixed strings, unique per level, and can never collide with numeric entryId keys; `renderEntry`'s param is typed `key: number | string`; the activity-run div and the non-activity `renderEntry(run[0], …)` share the same per-run expression and runs are disjoint, so sibling keys are distinct.
- **No behavior change beyond the keys** — the memoized grouping (HIGH 2) is untouched (keys live in the render output, not the useMemo); Message's arePropsEqual consumes kind/text/calls only, so stable keys now route the same entry object to its row and the equality check hits; /save serializing entryId is intended, and the Rust save/load handlers (src-tauri/src/ipc/files.rs:1308/1360) are verbatim string write/read with no JSON parsing — the field round-trips with no schema risk, and older builds loading new saves ignore the extra field (blind cast on the frontend).
- **Test quality** — the cap test (idBeforeCap captured at index MAX-1, asserted at `t[length-4]` post-cap) genuinely pins id-attached-to-entry rather than position; the arg-delta test pins id survival across replacement; the recordQuestionAnswer test pins the bypass path; the source-contract test pins all four key expressions plus the absence of `key={ti}`/`key={ri}`; the six updated toEqual assertions use `entryId: expect.any(Number)` and remain strict about content.
- **Constitution** — doc comments on all new exports, the type field, and the rationale comment above turns.map; multi-platform neutral (frontend-only TS, no platform APIs, no Rust changes in the diff); docs sync — README.md/PLAN.md contain no mentions of the transcript cap or keys (searched), so no doc update is required for this internal fix; reported test runs (root cargo 2003 passed, src-tauri 191+4 passed, npm test exit 0, tsc exit 0) are consistent with the frontend-only diff — not re-run by this read-only review, but nothing in the diff touches Rust.
