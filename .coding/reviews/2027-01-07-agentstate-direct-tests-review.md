## Verdict: FINDINGS (1 high, 0 low)

Review of all uncommitted changes on `wt/agenticcoding` for plan 2e89f955 (quality review LOW 3 — direct unit tests for the agentState helpers). The 24 tests faithfully pin every helper's actual semantics, cover the finding's named boundary cases, are order-tolerant where the module counter demands it, and touch no production code — but the test file's type-only import of `TranscriptEntry` from `./agentState` is invalid (that module never re-exports it), which fails `tsc` and therefore breaks `npm run build` even though the vitest run stays green.

## HIGH 1 — invalid type import: `TranscriptEntry` is not exported by `./agentState`; breaks `tsc` / the frontend build

**Where:** `frontend/src/hooks/agentState.test.ts:34-39`

```ts
import type {
  ActivityEntry,
  AgentState,
  MainRunningSnapshot,
  TranscriptEntry,   // ← not an export of ./agentState
} from "./agentState";
```

**Evidence:**
- `agentState.ts` *imports* `TranscriptEntry` from `../lib/types` (agentState.ts:14-19) but never re-exports it. The exhaustive export surface of the module (31 `export` declarations — every interface/function/const in the file) contains no `export type { … }` / `export * from` re-export of it. The other three names in the import are fine: `ActivityEntry` (:148), `AgentState` (:155), `MainRunningSnapshot` (:352).
- The sibling file does it correctly: `agentState.images.test.ts:23` — `import type { TranscriptEntry } from "../lib/types";`.

**Why the green test run didn't catch it:** vitest transpiles with esbuild, which erases `import type` statements without resolving them — so `npm test` (971 passed) is fully consistent with this defect existing. But `frontend/package.json:9` defines `build` as `tsc && vite build`, and `frontend/tsconfig.json:19` has `"include": ["src"]`, which covers `src/hooks/agentState.test.ts`. `tsc` fails with TS2305 ("Module './agentState' has no exported member 'TranscriptEntry'"), so the production frontend build (and any tauri build that runs it) is broken by this change.

**Fix (one line, test-only):** split the import —

```ts
import type { ActivityEntry, AgentState, MainRunningSnapshot } from "./agentState";
import type { TranscriptEntry } from "../lib/types";
```

— matching agentState.images.test.ts. After the fix, re-run `npm test` AND `npx tsc` (or `npm run build`) in `frontend/` to prove both are green; `npm test` alone cannot detect this class of error.

## Verification detail (all checked against source, no findings)

**1. Tests match the helpers' actual semantics (agentState.ts:290-613):**
- `emptyAgentState` defaults (:290-321) — all eight spot-checked fields match.
- `getOrCreate` (:324-329) — existing state returned **by reference** (`agents[id] ?? emptyAgentState()`); fresh state on first touch **without inserting** (the `??` is a pure read — `agents[7]` stays undefined). Both pinned correctly.
- `flushStreamingText` (:592-599) — no-op on empty `streamingText` with same-transcript-reference assertion (early return at :593); appends the assistant entry + clears when set. ✓
- `pushTranscriptEntry` (:609-612) — flush-then-append order pinned as `["assistant", "tool"]`: the flushed text lands ABOVE the pushed entry, exactly the load-bearing ordering the source doc comment (:587-590) describes. Append-without-flush when idle also covered. ✓
- `capTranscript` count edges (:411-417) — empty → same reference; exactly `MAX_TRANSCRIPT_ENTRIES`=1000 → same reference (no-op; `capTranscriptImages` fast path at :449 returns the same array when no image chars); 1001 → `slice(-1000)` drops exactly t0, keeps t1..t1000. Both off-by-one sides pinned. ✓
- `capActivityLog` (:528-532) — no-op at 300 (same ref), 301 drops exactly the oldest. ✓
- `allocEntryId` (:500-502) — monotonic via relative assertions only. ✓
- `stampEntryIds` (:515-525) — same array when all stamped (:521); stamps only missing ids (stamped entry keeps its reference, :522-524); syncs the counter past foreign `/load` ids (:516-520) so the next allocation can't collide. ✓
- `selectMainAgentId` (:339-349) — smallest parentless; fallback to smallest known while parent info loads; null when both maps empty. ✓
- `didMainTurnEnd` (:369-378) — all four transitions asserted, fires only on true→false; missing main agent reads as not-running (`?? false` at :375), matching the doc comment (:364-367). ✓
- `aggregateTokPerSec` (:113-118) — null when `msTotal`=0; the request-count inflation trap pinned: 10×100tok/1000ms → 100 tok/s, NOT 1000. ✓
- `isActivityEntry` (:545-552) / `isKnowledgeActivityEntry` (:570-575) — all classifications match the source exactly (tool/memory/vision/skill true; memory + `graph_*` tools true; shell/vision false for knowledge). ✓

**2. Boundary cases = the finding's named ones:** empty transcript, `MAX_TRANSCRIPT_ENTRIES` off-by-one (exactly-at-cap no-op AND cap+1 drops oldest), `flushStreamingText`, `pushTranscriptEntry` — all present. ✓

**3. Entry-id tests are order-tolerant:** only relative assertions (`b === a+1`, `toBeGreaterThan(1_000_000)`, `toBeDefined`) — no absolute-value assertions anywhere; the module counter (`nextEntryId`, :497) is file-local state and vitest isolates modules per test file. ✓

**4. No production code changed:** the diff is exactly `.coding/backlog.jsonl` (status flip pending→in_flight) + the one-line `frontend/vitest.config.ts` registration; untracked: the new test file + `.coding/plans/2e89f955.md`. `agentState.ts` is untouched. ✓

**5. vitest registration correct:** `frontend/vitest.config.ts:46`, immediately after `agentState.images.test.ts` (:45) — the include list is the gate, and the file is registered. ✓

**6. Fixture type-shapes:** every entry literal in the test was checked against the `TranscriptEntry` union (lib/types.ts:319-410) — user/assistant/tool/error/steer/skill/qa/memory/vision variants plus the `entryId?` intersection all typecheck; `AgentId = number` (types.ts:7) so the `Record<number, …>` maps are compatible. (The only type-level defect is HIGH 1 above.)

**7. No duplication of agentState.images.test.ts:** the new file's `capTranscript` fixtures carry no images, so the image-budget coverage stays solely in the images file, as the scope note intended. ✓

**8. Multi-platform neutrality:** pure node-env test addition — trivially satisfied. ✓

**9. Documentation sync:** test-only change; no README/PLAN.md/module-doc updates needed. The agentState.ts:5-12 header's "fully unit-testable" claim is now demonstrated by direct tests — nothing stale remains. ✓

## Note (not a finding)

The plan file and spawn description say "nine describe blocks," but the file has **ten** (the plan itself enumerates (a)–(j)); the test count of 24 is correct (3+2+2+3+1+4+3+2+2+2). Cosmetic prose miscount only — no action needed.
