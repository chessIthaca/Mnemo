## Verdict: PASS

Round-2 verification of plan 2e89f955 (direct unit tests for the agentState helpers, quality review LOW 3) on `wt/agenticcoding`. Both fixes verify correct and complete against the current source: the round-1 HIGH 1 import split is applied exactly as prescribed, the additional tsc-only `over`-widening defect is fixed with the correct contextual annotation, no other type-level defects or silencing patterns remain in the file, the uncommitted delta is exactly the expected five items with zero production code touched, and the 24 tests are otherwise identical to the round-1-verified state.

## Fix verification

**1. Import split (round-1 HIGH 1) — applied exactly as prescribed.**
`frontend/src/hooks/agentState.test.ts:34-35` now reads:

```ts
import type { ActivityEntry, AgentState, MainRunningSnapshot } from "./agentState";
import type { TranscriptEntry } from "../lib/types";
```

- All three agentState-local names are real exports of `./agentState`: `ActivityEntry` (agentState.ts:148), `AgentState` (:155), `MainRunningSnapshot` (:352) — confirmed against the module's full export surface.
- `TranscriptEntry` is a real export of `../lib/types` (types.ts:319, `export type TranscriptEntry = (`), and the import line matches the sibling pattern `agentState.images.test.ts:23` verbatim.
- The 15 value imports (test lines 17-33) also all resolve: MAX_ACTIVITY_ENTRIES (:390), MAX_TRANSCRIPT_ENTRIES (:389), aggregateTokPerSec (:113), allocEntryId (:500), capActivityLog (:528), capTranscript (:411), didMainTurnEnd (:369), emptyAgentState (:290), flushStreamingText (:592), getOrCreate (:324), isActivityEntry (:545), isKnowledgeActivityEntry (:570), pushTranscriptEntry (:609), selectMainAgentId (:339), stampEntryIds (:515). No invalid imports remain.

**2. `over` annotation (the additional tsc-only defect) — fixed correctly.**
`agentState.test.ts:142`: `const over: ActivityEntry[] = [...atCap, { kind: "error", text: "newest", timestamp: 999 }];`
- The annotation supplies the contextual type, so the fresh literal's `kind` narrows to the literal `"error"` instead of widening to `string` — the correct fix for the TS2345 (unannotated, `[...atCap, { … }]` infers `(ActivityEntry | { kind: string; … })[]`, not assignable to `capActivityLog`'s `ActivityEntry[]` parameter).
- `"error"` is a member of `ActivityEntry`'s kind union (agentState.ts:149: `"reasoning" | "answer" | "error" | "info"`); the `atCap` fixture's `"info"` pushes (:139) are likewise valid members.
- The fix is type-erased at runtime — it cannot change test outcomes, consistent with the unchanged 971-passed count.

**3. No other type-level defects; nothing silenced.**
- A pattern search of the whole test file for `as any`, `as unknown`, `@ts-ignore`, `@ts-expect-error`, `@ts-nocheck`, and `eslint-disable` returns zero matches; the full 290-line file was also read end-to-end — no casts, suppressions, or non-null assertions anywhere.
- Every remaining object/array literal is safely typed: annotated consts (`entries: TranscriptEntry[]` :107/:112/:120/:166/:185, `atCap`/`over: ActivityEntry[]` :137/:142, `parents: Record<number, number | null>` :195/:200, `empty: MainRunningSnapshot` :225), contextually-typed call arguments (`pushTranscriptEntry` :93/:100, `stampEntryIds` :176, `isActivityEntry`/`isKnowledgeActivityEntry` :244-288, `capActivityLog` :141/:143), and `toEqual` arguments. `over` was the file's only spread-with-fresh-literal array — the one widening trap — and it is now annotated.
- `MainRunningSnapshot` fixtures match the interface (agentState.ts:352-355: `agentParents: Record<AgentId, AgentId | null>`, `agents: Record<AgentId, AgentState>`); `AgentId = number` (types.ts:7) keeps the `Record<number, …>` maps compatible. The `snap` helper's `{ ...emptyAgentState(), running }` spread (:214) is a valid `AgentState` (running: boolean, :159).

**4. Delta scope — exactly the expected five items, no production code.**
`git status` shows precisely: `M .coding/backlog.jsonl` (status flip pending→in_flight with the plan-linkage note/plan_id — the standard bookkeeping flip), `M frontend/vitest.config.ts` (the one-line registration `"src/hooks/agentState.test.ts",` at :46, immediately after `agentState.images.test.ts` :45), untracked `frontend/src/hooks/agentState.test.ts`, untracked `.coding/plans/2e89f955.md`, untracked round-1 report `.coding/reviews/2027-01-07-agentstate-direct-tests-review.md`. Diff stat: 2 files changed, 2 insertions, 1 deletion. `agentState.ts`, `lib/types.ts`, and every other production source are absent from the delta.

**5. The 24 tests are otherwise identical to the round-1-verified state.**
Ten describe blocks with per-block counts 3+2+2+3+1+4+3+2+2+2 = 24 — exactly the round-1 count. Every semantic detail the round-1 report verified is present and unchanged: the eight `emptyAgentState` defaults (:38-48); `getOrCreate` by-reference + pure-read no-insert (:50-62); `flushStreamingText` no-op same-reference + append/clear (:66-84); `pushTranscriptEntry`'s `["assistant", "tool"]` flush-then-append order + idle append (:88-102); `capTranscript` empty/at-cap same-reference + MAX+1 drops exactly t0 (:106-132); `capActivityLog` no-op at 300 + 301 drops oldest (:136-151); `allocEntryId` relative-only monotonic assertions (:155-163); `stampEntryIds` same-array / stamps-only-missing / counter-sync-past-1_000_000 (:165-190); `selectMainAgentId`'s three cases (:193-209); `didMainTurnEnd`'s four transitions + missing-main-as-not-running (:211-229); `aggregateTokPerSec` null-at-zero + the 100-not-1000 request-count trap (:231-240); both classifiers' full matrices (:242-290). The only deltas from the round-1 file are the two type-level fixes (import block 6 lines → 2; the `over` annotation), both erased at runtime — test outcomes are necessarily identical, matching the unchanged 68-file/971-passed run.

## Project review checks

- **Documentation sync:** test-only, type-level change; no README/PLAN.md/module-doc updates needed (round-1 check unchanged).
- **Multi-platform neutrality:** pure node-env test file; no platform-specific code.
- **Round-1 note (not a finding) status:** the nine-vs-ten describe-block prose miscount in the plan file remains cosmetic; no action needed.
