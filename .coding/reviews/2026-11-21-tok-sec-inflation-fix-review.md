# Review — tok/sec inflation fix (commit 463ddd4)

**Branch:** `fix/aggregate-tok-sec-inflation`
**Commit:** 463ddd4 — "fix: aggregate tok/sec used avg time not total (N× inflation)"
**Scope:** Frontend-only (TS/TSX). Reviewed by reading the current state of every
changed file + searching for every aggregate site.

## Verdict

The core fix is **correct**. The shared helper `aggregateTokPerSec(tokens, msTotal)`
(`agentState.ts:108-113`) implements the right formula — `total tokens / total time`
(`tokens / (msTotal / 1000)`), returning `null` when `msTotal <= 0`. All 5 aggregate
sites now call it; the per-request rate in `reduceUsage` (`agentEventReducer.ts:345-348`)
is correctly **left unchanged** (it uses the single request's own ttft/gen, which is
correct). Null handling is consistent across all callers (each guards `!== null` or
`? : "—"`), and is in fact *better* than before: the old avg-per-request formula
divided by `generation_ms_total / timed_requests`, which produced `Infinity` when
`generation_ms_total === 0` but `timed_requests > 0` (a request reporting ttft but
null generation); the helper now returns `null` and the rate is hidden.

Two low-severity cleanup items were left behind by the fix. Neither affects
correctness of the displayed rates.

---

## Findings

### Correctness — low (stale doc comment)

**`frontend/src/hooks/agentState.ts:75-76`** — The `SessionTiming` interface doc
comment still documents the **buggy** formula this fix just removed:

```
 * Mirrors the StatsView computation: avgInputRate = prompt_tokens /
 * (ttft_ms_total / timed_requests / 1000).
```

That is exactly the `tokens / avg-per-request-time` formula the fix replaced. It now
**contradicts** the new `aggregateTokPerSec` doc comment immediately below it
(`agentState.ts:91-107`), which explicitly warns *against* dividing by
`ms_total / timed_requests`. A reader of `SessionTiming` is told the code divides by
average per-request time when it actually divides by total time.

**Fix:** update the comment to describe the correct computation, e.g.
`avgInputRate = aggregateTokPerSec(prompt_tokens, ttft_ms_total)` (total tokens /
total time), or drop the "Mirrors the StatsView computation" line since the helper is
now the single source of truth.

### Bugs / constitution compliance — low (dead code)

**`frontend/src/components/views/StatsView.tsx:90, 93`** — The `tot.timed`
accumulator field in `ModelTable` is now **dead**: it is computed
(`timed: acc.timed + mb.timed_requests`, line 90) and initialized
(`timed: 0`, line 93) but **never read**. A search for `tot.timed` / `.timed` across
all `.tsx` files returns only line 90.

Before the fix, `tot.timed` fed the buggy totals-row formula
(`tot.prompt / (tot.ttft / tot.timed / 1000)`); now that `totIn`/`totOut` use
`aggregateTokPerSec(tot.prompt, tot.ttft)` / `aggregateTokPerSec(tot.completion, tot.gen)`
(lines 95-96), the `timed` divisor is no longer needed and the field is orphaned.

`tsconfig` has `noUnusedLocals: false`, so this won't fail `tsc --noEmit`, but it is
dead state left behind by the fix — the kind of thing the constitution's "remove the
dead code" guidance targets, and the task explicitly asked to flag dead locals as a
code smell.

**Fix:** drop `timed` from both the reducer body (line 90) and the initial accumulator
(line 93).

---

## Verified correct (no action)

- **Helper formula** (`agentState.ts:108-113`): `msTotal > 0 ? tokens / (msTotal / 1000) : null`
  — correct throughput; `<= 0` guard covers zero and negative. Thorough doc comment.
- **Re-export** (`useAgentStore.ts:84` import, `:143` export): present in both blocks;
  placed alphabetically within the lowercase group (`aggregateTokPerSec` < `applyCodeColors`).
- **InflightBar** (`InflightBar.tsx`): `aggregateTokPerSec` imported (line 4);
  `sessionOutputRate` uses `completion_tokens` / `generation_ms_total` (lines 54-57);
  `avgTtftMs`/`avgGenMs` **kept** and still referenced in the outer tooltip (line 191) —
  no dead locals; rate-span title updated to "Session output tok/sec (completion / total
  generation time)" (line 198).
- **StatsView ModelTable**: totals (lines 95-96) + per-model (lines 119-120) all use the
  helper; null guarded with `? fmtRate : "—"` (lines 134-135, 149-150).
- **StatsView SessionCard** (`StatsView.tsx:194-197`): `avgTtft`/`avgGen` **kept** (used in
  tooltips at lines 238, 246); `avgInputRate`/`avgOutputRate` use the helper; tooltips
  updated to mention "total TTFT"/"total generation".
- **StatsView ProjectCard** (`StatsView.tsx:277-278`): `avgTtft`/`avgGen` **removed**
  entirely (its `StatRow`s at lines 307-312 have no `title` prop, so they'd have been
  dead); `avgInputRate`/`avgOutputRate` use the helper. No remaining `avgTtft`/`avgGen`
  references in `ProjectCard`.
- **Per-request rate** (`agentEventReducer.ts:345-348`): correctly **unchanged** — uses
  the single request's own `ttft`/`gen`, which is the right per-request throughput.
- **Null handling**: all 9 call sites guard `null` (InflightBar `!== null &&`; StatsView
  `? fmtRate : "—"` / `!== null &&`). No regression; the `generation_ms_total === 0`
  case is now handled (null) instead of producing `Infinity`.
- **Tests** (`useAgentStore.test.ts`): `aggregateTokPerSec` imported (line 13); the
  `sessionTiming` test's inline assertion switched from the buggy 400 to the correct
  200 (300 tokens / 1500ms = 200 tok/s, lines 457-465); new `describe("aggregateTokPerSec")`
  block (lines 777-794) covers null-on-zero, 100/1000ms=100, and the N× regression
  (10×100 tok/1000ms = 100, not 1000). Math verified.
- **No missed aggregate sites**: searched all `.ts`/`.tsx` for `timed_requests`,
  `ttft_ms_total`, `generation_ms_total`; the only divisions by `timed_requests` that
  remain are the intentionally-kept tooltip averages (`avgTtftMs`/`avgGenMs` in
  InflightBar, `avgTtft`/`avgGen` in SessionCard) — those are per-request averages
  shown in tooltips, not aggregate rates.
- **Security**: N/A (pure frontend display math). Nothing off.
- **Constitution**: frontend-only change; `#![deny(warnings)]` (Rust) doesn't apply.
  No `#[allow(...)]`-style suppressions added (N/A for TS). The one `eslint-disable`
  in the diff (`StatsView.tsx:437`, react-hooks/exhaustive-deps) is pre-existing, not
  added by this fix. Style matches the surrounding code.
