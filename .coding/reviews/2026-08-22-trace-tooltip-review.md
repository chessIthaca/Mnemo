# Review — Multiline aligned tooltips for the Trace tab graphs

**Plan:** 09dc4448-5102-4ef8-bdfd-a9b24113024d · **Branch:** fix/compact-bugs · **Scope:** uncommitted diff vs HEAD only (branch tip 564ce64 excluded) · **Date:** 2026-08-22

## Verdict

The implementation is **correct and well-tested**. The builders are pure and their math checks out; the portal positioning math is sound; the wiring, docs, and README are in sync; nothing platform-specific or unsafe. Findings below are all minor (no correctness blockers); nits are explicitly labeled.

## Verified (no findings)

- **Builder math** (`frontend/src/lib/traceStats.ts:263-316`): `phaseTipRows` total/stack order/em-dash semantics confirmed against `phaseSeries` (prep 120−compact 20=100; total 9177ms → "9.2s"; answer 8400−2000=6400 → "6.4s"); `tokenTipRows` cached % clamped (`Math.min(100, Math.round(...))`, 6789/12345 → 55%); `cacheTipRows` thresholds (≥50 emerald / ≥20 amber / else red) **exactly match** the column classes in `CacheChart` (`TraceStats.tsx:302-306`). Token dot colors match the chart segments + legend.
- **Portal math** (`TraceStats.tsx:124-152`): `bottom: window.innerHeight - anchor.top + 6` places the tooltip's bottom edge 6px above the column top; the flip branch (`top: anchor.bottom + 6`) is correct; `-translate-x-1/2` centering on `cx` is correct; `pointer-events-none` prevents hover-flicker; `onMouseLeave` is on the same element as `onMouseEnter`; portal children unmount with the column (ring-buffer eviction safe).
- **Moved symbols / imports**: `fmtInt`/`fmtPhase`/`PHASE_SEGMENTS` now live in traceStats.ts with doc comments; `fmtInt` is neither imported nor used anywhere in TraceStats.tsx (full-file read); the removed `fmtDuration` import has no remaining use there; every import in the new import block is used. Consumers of traceStats are only TraceStats.tsx, LlmTraceView.tsx (`cacheHitPct` only), and the test — no collisions.
- **Contract test** (`TraceStats.tooltip.test.ts`): `?raw` pattern matches the InflightBar.test.ts precedent (typed by `vite/client`, present in `frontend/src/vite-env.d.ts`); the negative assertions `"· prep ${"` / `"(cached ${fmtInt("` were literal substrings of the old title templates and are genuinely gone from the current source; test is registered in `vitest.config.ts` (line 27).
- **fmtDuration expectation**: `fmtPhase(65_000)` → `fmtDuration` yields "1m 5s" (`format.ts:78-90`) — test and doc-comment example ("1m 3s") match.
- **Docs**: Column doc comment, traceStats.ts doc comments (all new public items documented per constitution), and the README bullet ("multiline, tab-aligned tooltip (portal-rendered…)") match behavior; summary-strip titles deliberately retained.
- **Security**: tooltip renders formatted numbers / fixed labels / locale time strings as React text only; no `dangerouslySetInnerHTML`; all Tailwind classes are compile-time string literals (scanner-visible).
- **Multi-platform**: pure frontend; `window`/`document`/`toLocaleString` are webview-neutral; no Rust changes, no `cfg(windows)`, no `#[allow(...)]`.

## Findings

### Low

1. **Tooltip not horizontally clamped — can clip at the viewport edges.**
   `TraceStats.tsx:126-133` centers a `min-w-[170px]` tooltip on the column center (`left: anchor.cx` + `-translate-x-1/2`) with no left/right clamp. The newest request is always the rightmost column, so its center sits near the panel's right edge; a ≥85px half-width tooltip extends past the viewport edge and is visually cut off (fixed positioning creates no scrollbar). Same at the far-left column. Suggest clamping `left` to, e.g., `[8 + tipWidth/2, innerWidth - 8 - tipWidth/2]`, or reducing the translate near edges.

2. **`aria-label` on a plain div isn't real screen-reader parity.**
   `TraceStats.tsx:96-101` — the comment claims "Screen-reader parity for the removed native title", but `aria-label` on a non-interactive, role-less `<div>` is ignored by most screen readers. The old `title` was equally weak, so this is not a regression, but the comment overstates the effect. If parity is the intent, add `role="img"` (or `role="figure"`) to the column div so the label is announced.

### Nits (optional)

3. **Flip threshold `170` is a hand-tuned constant** (`TraceStats.tsx:129`) ≈ the tallest (8-row phase) tooltip's pixel height. If rows are added or font metrics shift, an `anchor.top` just above 170 can clip a few px at the viewport top. Acceptable heuristic; a one-line comment stating what 170 approximates would make the magic number self-maintaining.
4. **Palette duplication in PhaseChart** (`TraceStats.tsx:176-182`): the segment classes are still a hardcoded inline array instead of deriving from the now-exported `PHASE_SEGMENTS` (which feeds the legend and tooltip dots). Values match today; a future palette edit to `PHASE_SEGMENTS` would silently desync the columns from legend/tooltip dots.
5. **Include-list ordering** (`vitest.config.ts:27-28`): `TraceStats.tooltip.test.ts` is placed before `LlmTraceView.test.ts`, breaking the views block's rough alphabetical order. Cosmetic only.

### Noted, accepted by design (no action)

6. **Anchor rect captured once on hover entry** (`TraceStats.tsx:102-105`): a 1.5s trace poll can add a column and shift `flex-1` widths while the user hovers, leaving the tooltip at stale coordinates until re-hover. Given short hover durations and `pointer-events-none`, this is a reasonable trade-off and matches the plan's stated assumption.

## Constitution checks

- Documentation sync: **pass** (README bullet, component docs, lib doc comments all match behavior).
- Multi-platform neutrality: **pass** (pure frontend, platform-neutral APIs).
- Public items documented: **pass** (`TipRow`, `PHASE_SEGMENTS`, `fmtInt`, `fmtPhase`, `phaseTipRows`, `tokenTipRows`, `cacheTipRows` all carry doc comments).
- Warning-free / no `#[allow]`: **pass** (Rust untouched; `npm run build` reported clean).
- Regression tests: **pass** (8 unit tests + source-contract test covering the change; the contract test's negative assertions pin the old format's removal).
