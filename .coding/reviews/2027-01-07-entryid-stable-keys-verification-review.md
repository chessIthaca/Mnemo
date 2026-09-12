## Verdict: FINDINGS (0 high, 1 low)

Verification pass for plan 225e0dad (stable entryId keys — mem-perf review HIGH 3): re-review of the full uncommitted delta on `wt/agenticcoding` after the three findings from `.coding/reviews/2027-01-07-entryid-stable-keys-review.md` were fixed. All three fixes are verified correct against the code, and the core change remains sound. One new low finding: the LOW 1 fix (vision-retry spread) has no regression test that fails if it is reverted — the project constitution requires one per fixed defect.

### Fix verification

**HIGH 1 — /load foreign-id counter-sync: FIXED, correctly and completely.**

- `stampEntryIds` (`frontend/src/hooks/agentState.ts:429-439`) runs the sync loop (`if (e.entryId !== undefined && e.entryId >= nextEntryId) nextEntryId = e.entryId + 1;`) BEFORE the `every` early-return, so a /load of an already-stamped conversation still syncs the counter — the exact code-order requirement. The sync only ever advances (never regresses) the counter, so loading a save whose max id sits below the current counter is a harmless no-op, and loading into one agent cannot collide with another agent's entries (single module counter).
- Only-import-path audit re-done from scratch: `stampEntryIds` is called solely from the /load site (`InputBar.tsx:533`) and the tests. `allocEntryId` callers: the identity pass (`agentEventReducer.ts:1431` — stamps only id-LESS entries, never overwrites, and only objects not in the pre-event transcript, which the reducers create fresh), `reduceQuestionAnswered` (:1011), and InputBar's direct pushes (:171 `msg.entryId ?? allocEntryId()` — all eight `pushTranscriptMessage` call sites at :456/:463/:474/:488/:493/:503/:538/:543 pass fresh literals with no entryId, so it always allocates; :339 handleSend). The store's `applyToolCallArgDeltas` (`useAgentStore.ts:1087`) only spreads in-store entries; `clearConversation` sets `[]`. No other restore path exists (searched). Foreign ids can therefore only enter through `stampEntryIds`, which now syncs.
- The regression test ("stampEntryIds advances the counter past foreign ids", `useAgentStore.test.ts:2198-2210`) genuinely pins the fix: without the sync, `out[1].entryId` is ≤ the counter (1 in isolation) and `toBeGreaterThan(5000)` fails; with it, the foreign 5000 is preserved, the id-less sibling is stamped above it, and a subsequent `allocEntryId()` returns > 5000.

**LOW 1 — vision-retry spread: FIXED in code; not pinned by a test (finding 1 below).**

- `agentEventReducer.ts:888` now reads `transcript[runningIdx] = { ...transcript[runningIdx], ...entry };`. The `entry` literal (:870-878) carries only kind/index/total/query/description/success/running — no `entryId`/`ts` keys — so the previous entry's identity fields survive the spread (a spread overrides only keys present in the later object).
- Fresh-literal replacement audit re-done across every `transcript[i] =` site: :555 (tool merge, `...last`), :594, :602, :643, :668 (all `...entry`), :888 (now a spread), :929 (vision_described, `...entry`), `useAgentStore.ts:1087` (batched arg deltas, `...entry`), and `sweepRunningCards` (:1210/:1218/:1222, all `...entry`). No fresh-literal replacement site remains.

**LOW 2 — order-dependent assertion: RESOLVED.**

- The `toBeGreaterThan(stamped[0].entryId!)` (41) assertion now holds in any execution order: in isolation the foreign 41 advances the counter to 42 so the id-less sibling stamps above it; after earlier describes the counter is already past 41. The in-test comment records the reasoning.

### Finding

**LOW 1 — the vision-retry entryId stability has no pinning regression test.**

**Location:** `frontend/src/hooks/useAgentStore.test.ts:1394-1406` ("vision_describe: a re-announced index replaces its running entry (no duplicate)").

The existing retry test asserts only `toHaveLength(1)`; the "entry replacements keep their entryId" test pins the tool arg-delta site, not the vision reducer. Reverting `agentEventReducer.ts:888` to `transcript[runningIdx] = entry` passes the entire suite — the LOW 1 defect (React key changes mid-stream on provider retry, row state resets) could silently reappear. The project constitution requires a regression test per fixed defect ("must fail without the fix and pass with it").

**Fix (extend the existing test — no new describe needed):** capture the entry's entryId after the first announce and assert equality after the second:

```ts
useAgentStore.getState().handleAgentEvent({
  agent_id: ID,
  event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
});
const id = agent(ID).transcript.find((e) => e.kind === "vision")!.entryId;
useAgentStore.getState().handleAgentEvent({
  agent_id: ID,
  event: { kind: "vision_describe", index: 1, total: 1, query: "q" },
});
const cards = agent(ID).transcript.filter((e) => e.kind === "vision");
expect(cards).toHaveLength(1);
expect(cards[0]!.entryId).toBe(id); // retry keeps the id — key stable mid-stream
```

### Re-confirmed (overall change)

- **Stamping completeness** — every transcript write site stamps: the applyAgentEvent identity pass (restructured to `if (old.has(e)) continue;` then stamp ts + entryId on new id-less entries — behavior-preserving for ts, adds entryId), `reduceQuestionAnswered` for the direct-invoke path, InputBar's `pushTranscriptMessage` / handleSend user entry / /load. No other write sites exist in the tree.
- **Key expressions** — `turn[0][0]`/`run[0]` are guaranteed non-empty (grouping seeds `[entry]` at Conversation.tsx:165; chunkRuns seeds `[e]`); fallback keys `t${ti}`/`r${ri}`/`i${i}` are prefixed strings, unique per level, and can never collide with numeric entryId keys; `renderEntry`'s key param is typed `number | string` (Conversation.tsx:175); the activity-run div and the non-activity `renderEntry(run[0], …)` share the same per-run expression over disjoint runs.
- **The six updated toEqual assertions** (useAgentStore.test.ts:246, ~710, 1172-1173, 1575, 1583) add `entryId: expect.any(Number)` and remain strict about kind/text/ts. A sweep for `toEqual({ kind:` across all frontend test files found no other entry-shape assertion the new field breaks (remaining hits are indexing-overlay and answer-parse shapes).
- **Test quality** — the cap test (idBeforeCap captured at index MAX-1, asserted at `t[length-4]` post-cap) pins id-attached-to-entry rather than position; the arg-delta test pins id survival across replacement; the recordQuestionAnswer test pins the bypass path; the source-contract test pins all four key expressions plus the absence of `key={ti}`/`key={ri}`.
- **No regressions elsewhere in the diff** — the only non-code change is the backlog.jsonl status flip (bookkeeping); the memoized grouping (HIGH 2) is untouched (keys live in the render output, not the useMemo); Message's arePropsEqual consumes kind/text/calls only.
- **Constitution** — doc comments on the new exports (`allocEntryId`, `stampEntryIds`), the type field, and the rationale comments; multi-platform neutral (frontend-only TS, no Rust files in the delta, no platform APIs); docs sync — README/PLAN contain no mentions of the transcript cap or keys (carried over from the prior review; this delta adds nothing user-facing), so no doc update is required.
- **Tests** — reported green by the parent (root cargo 2003 passed, src-tauri 191+4 passed, `npm test --workspace frontend` exit 0, `npx tsc --noEmit` exit 0). This read-only review cannot execute them; nothing in the delta touches Rust, and every frontend assertion affected by the new field was verified updated by reading.
