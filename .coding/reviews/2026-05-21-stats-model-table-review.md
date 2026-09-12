# Review: Stats per-model table + unified US number formatting

**Date:** 2026-05-21
**Scope:** All uncommitted changes (`git diff HEAD` + untracked files).
**Feature file:** `frontend/src/components/views/StatsView.tsx` (only code change).
**Bookkeeping (non-feature, reviewed for correctness):** `.coding/plans/stack.json`, `.coding/plans/0afb2240-*.md`, untracked `.coding/plans/ffe38b93-*.md` — all benign plan-stack pointer updates; no code impact.

## Summary

The change replaces three local formatters with US-style versions (`fmtTokens`/`fmtInt`/`fmtRate`/`fmtCost`), adds a `ModelTable` component (9-column scrollable table + totals row), and swaps the old 2-line per-model blocks in `SessionCard`/`ProjectCard` for `<ModelTable>`. `SessionCard`'s per-model gate changed `> 1` → `> 0` (intended; single-model sessions now show the table). `ProjectCard`'s gate was already `> 0`.

The totals-row aggregation is **correct**: the `reduce` initializer is all-zeros, every accumulator field matches its source field, and the aggregate In/s = `tot.prompt / (tot.ttft / tot.timed / 1000)` is the proper timing-weighted aggregate (not a naive average of per-row rates) — consistent with the existing `SessionCard`/`ProjectCard` StatRow math (lines 206–209, 289–292). `fmtCost` sub-cent branch is correct (`$0.0023` for `< 0.01`, `$1,234.50` via `toLocaleString` otherwise). Table overflow handling is adequate (`overflow-x-auto` wrapper, `whitespace-nowrap` cells, `max-w-[10em] truncate` + `title` on the model-name cell). Doc comments present on all new functions; module comment updated. No commits to main (working-tree-only). NaN/Infinity cannot arrive from the backend (JSON doesn't serialize them), so the only way to reach a non-finite rate is the division-by-zero path below.

## Findings

### Bugs

**B1 — Division-by-zero renders "Infinity" in In/s / Out/s cells (per-row + totals).** `StatsView.tsx:125-132` (per-row) and `:101-102` (totals).

The rate guards check only `timed_requests > 0`:
```ts
const inRate = mb.timed_requests > 0
  ? mb.prompt_tokens / (mb.ttft_ms_total / mb.timed_requests / 1000)
  : null;
```
If `timed_requests > 0` but `ttft_ms_total === 0` (e.g. a sub-ms TTFT rounded to 0, or a timed request whose first-token timing wasn't captured), the denominator is `0` → `inRate = Infinity`. `fmtRate(Infinity)` then renders the literal string **"Infinity"** in the cell (`Infinity < 100` is false → `Math.round(Infinity).toLocaleString("en-US")` → `"Infinity"`). Same for `outRate` when `generation_ms_total === 0`, and for the totals row (`totIn`/`totOut`) when the summed timing total is 0 but `tot.timed > 0`.

This is an **inconsistency** with the existing `SessionCard`/`ProjectCard` StatRow logic, which *does* guard the zero case — `StatsView.tsx:208`: `avgTtft && avgTtft > 0 ? stats.prompt_tokens / (avgTtft / 1000) : null` (and `:209` for `avgGen > 0`). The new table regressed that guard.

**Suggested fix** (mirror the existing pattern): also require the timing total to be positive, e.g.
```ts
const inRate = mb.timed_requests > 0 && mb.ttft_ms_total > 0
  ? mb.prompt_tokens / (mb.ttft_ms_total / mb.timed_requests / 1000)
  : null;
```
and analogously for `outRate` (`generation_ms_total > 0`) and the totals (`tot.ttft > 0` / `tot.gen > 0`). Alternatively, guard inside `fmtRate` with `Number.isFinite(n)`. The per-row guard is the more targeted fix and keeps the table consistent with the cards.

### Consistency / minor

**C1 — Top-level "Requests"/"Sessions" StatRow values still use `String()`, not `fmtInt`.** `StatsView.tsx:220` (`String(stats.request_count)` in `SessionCard`), `:299` (`String(stats.session_count)`), `:300` (`String(stats.request_count)` in `ProjectCard`).

The plan's stated goal was to "unify all number formatting to US-style comma-grouped integers", and `fmtInt` was added specifically "for request counts" and is used in the new table (`:141`, `:156`). But the top-level `Requests`/`Sessions` StatRow rows were left on raw `String()`, so a 10,000-request session shows **"10000"** in the card header row while the table's totals row shows **"10,000"**. Not a functional bug, but a visible inconsistency that undercuts the unification goal. Suggested fix: `value={fmtInt(stats.request_count)}` / `fmtInt(stats.session_count)` at the three call sites. (The `SessionList` `:375` `{s.request_count}r` and per-day `:351` `{d.request_count} reqs` are outside the StatRow scope the plan called out, so leaving those is defensible — but the three StatRow rows are in-scope and should be grouped.)

**C2 — Table Cost column shows "$0.0000" for unpriced models / zero totals (cosmetic).** `StatsView.tsx:148` (per-row), `:163` (totals).

`modelCost` returns `0` when a model has no `[[pricing]]` entry (`:61`), and the table renders `fmtCost(...)` unconditionally. `fmtCost(0)` hits the `usd < 0.01` branch → `"$0.0000"`. The `StatRow` Cost rows avoid this by gating on `cost > 0` (`:261`, `:327`), but the table has no such gate, so every unpriced model row — and an all-unpriced totals row — shows "$0.0000". Not wrong, just not pretty. Optional: render `"—"` (or `"$0.00"`) when `modelCost(...) === 0`. Low priority.

## No findings (verified clean)

- **Totals aggregation** — reduce initializer all-zeros, accumulator fields all correct; aggregate In/s/Out/s recomputed from summed timing totals (weighted, correct). `:87-102`.
- **`fmtCost` sub-cent branch** — `$0.0023` for `< 0.01`; `1234.5` → `"$1,234.50"` via `toLocaleString("en-US", {min:2,max:2})`. `:48-51`.
- **Table overflow** — `overflow-x-auto` wrapper + `whitespace-nowrap` cells + `max-w-[10em] truncate`/`title` on model name; adequate at the 300px panel min-width. `:104-167`.
- **No regressions to StatRow token/rate/cost rows** — Prompt/Completion/Reasoning/Cached use `fmtTokens` (6000 → "6,000", intended), rates use `fmtRate`, cost uses `fmtCost`. `:225-265`, `:304-328`.
- **Constitution** — doc comments on `fmtTokens`/`fmtInt`/`fmtRate`/`fmtCost`/`ModelTable`/`modelCost`; module comment updated; `StatsView.tsx` line-ending style preserved (the LF→CRLF git warning is on the `.coding/plans/*.md` bookkeeping file, not the feature file); no commits to main.
- **`key={mb.model}`** — safe; backend aggregates per-model so names are unique.
- **Empty/single `per_model`** — `> 0` gate renders nothing for empty; a single-model session renders 1 row + a totals row that duplicates it (expected for a totals row).
