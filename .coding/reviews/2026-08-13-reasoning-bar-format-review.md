# Code Review — Reasoning bar: non-cached sent tokens, output-only tok/s, shared number format

**Date:** 2026-08-13
**Reviewer:** read-only reviewer subagent
**Scope:** ALL uncommitted working-tree changes (`git diff HEAD` + untracked): `frontend/src/lib/format.ts` (new), `frontend/src/lib/format.test.ts` (new), `frontend/src/components/chat/InflightBar.tsx`, `frontend/src/components/views/StatsView.tsx`, `frontend/vitest.config.ts`, plus `.coding/` bookkeeping (`backlog.json`, plan files, `stack.json`).

## Summary of what was verified

- **Plan goal 1 (shared formatter):** `format.ts` exports `fmtTokens`/`fmtRate` (both have doc comments), delegating to a private `fmtHybrid` with sign handling, `<10K` raw comma-grouped (≤1 decimal), `10K–1M` K suffix, `≥1M` M suffix. Structure matches the plan exactly.
- **Plan goal 2 (InflightBar):** `sessionInputRate` and its display span are removed; `avgTtftMs` is retained *and still used* (tooltip at line 187) — no unused variables. The ↑ counter target is `Math.max(0, tokenUsage.prompt - tokenUsage.cached)` with title "Prompt tokens sent (excludes cached)" (lines 56, 171) — accurate. Only the ↓ output rate remains (lines 193–199). All `formatTokens`/`formatRate` call sites replaced with shared imports; local functions deleted. `useCountUp` rounds every animation tick to an integer, so no fractional count-up values reach the formatter.
- **Plan goal 3 (StatsView):** local `fmtTokens`/`fmtRate` deleted; `fmtInt`/`fmtCost`/`fmtTime` kept; shared imports added. Every token display site (SessionCard, ProjectCard, ModelTable incl. totals row, per-day, session list) uses `fmtTokens`; rate sites use `fmtRate` inside `!== null` guards (lines 238, 246, 314, 317) — no `null` can reach the formatter.
- **Missed token-display sites:** searched all `.ts`/`.tsx` for `toLocaleString`, `tokenUsage`, `prompt_tokens`/`completion_tokens`. The only remaining non-shared token formatter is `LlmTraceView.tsx:34` (`fmtInt`), which is deliberately exempt. `max_output_tokens` in the settings UI is a user-edited config input, not a usage display — correctly out of scope. No missed sites.
- **Tests (plan goal 4):** `format.test.ts` is registered in `vitest.config.ts` include list. It covers 0/999 raw, comma-grouping (1000, 1234.5, 9999), K boundary (10000→"10K", 12345, 999949), M (1e6, 1234567, 1234567890), sign (−1234), and rate cases (42.55→"42.6", 1234.5, 12000).
- **Constitution:** no `#[allow(...)]`-style suppressions, `ts-ignore`, or `eslint-disable` added by this diff (the two `eslint-disable` lines in `useCountUp.ts:71` and `StatsView.tsx:444` are pre-existing and untouched). Both public functions have doc comments. No mixed line endings introduced in the frontend files.
- **Security:** none — pure display formatting, no new I/O or IPC.

## Findings

### Correctness (minor)

1. **`frontend/src/lib/format.ts:21-38` — rounding happens *after* the threshold branch, so displayed values can spill across the K/M thresholds.**
   - Inputs in **[9,999.95, 10,000)** hit the `<10K` branch, round to 10000, and render as **"10,000"** — a displayed value ≥ 10,000 without the K suffix the rule mandates for that range (e.g. `fmtRate(9999.96)` → "10,000" instead of "10K").
   - Inputs in **[999,950, 1,000,000)** hit the K branch, `round1(abs/1000)` returns exactly 1000, and render **"1,000K"** — a 1M-equivalent value shown with a K suffix instead of "1M". Same pattern produces **"1,000M"** for inputs in [999,950,000, 1e9).
   - Fix suggestion: round the *full value* first (or detect a mantissa of exactly 1000) and branch/roll over on the rounded magnitude — e.g. if the rounded K mantissa is 1000, emit `1M`; if the rounded raw value is ≥ 10000, emit the K form.
   - Practical reachability is narrow (token counts are integers; only rates are fractional), but this is the canonical shared utility for every token display, so the edge should be well-defined.

2. **`frontend/src/lib/format.ts:22` — negative values that round to zero render "-0".** `fmtTokens(-0.04)` → sign "-", `round1(0.04)` = 0 → **"-0"**. Cannot occur from current call sites (all inputs are non-negative or clamped via `Math.max(0, …)`), but the module doc claims sign preservation, so either normalize `-0` to `"0"` or document it.

### Tests (minor)

3. **`frontend/src/lib/format.test.ts` — the hybrid thresholds are covered, but not the rounding-across-threshold edges**, which is exactly where finding 1 lives. Missing cases: `9999.96` (expect "10K" once finding 1 is fixed), `999950` / `999999.5` (expect "1M"), and optionally a `-0.04` → "0" case. The existing boundary cases (9999→"9,999", 10000→"10K", 999949→"999.9K", 1e6→"1M") are correct as written.

### Bugs

None found. The InflightBar rework has no unused variables (`avgTtftMs` is still consumed by the session tooltip), the ↑ tooltip accurately describes prompt−cached, the output-only rate layout is intact, and StatsView's null-guards around `fmtRate` are sound.

### Constitution compliance

No violations found in the code changes. Informational notes only:

- `git` warns that `.coding/backlog.json` and `.coding/plans/54844dd0-….md` have LF endings in the working copy (they are written by the plan/bookkeeping tools) and will be CRLF-normalized at commit. The files are pure LF, not mixed — this is machine-managed bookkeeping state, not a line-ending violation to fix by hand.
- `.coding/plans/stack.json` now references plan `44ec4abe-…` (the active plan) and the previous plan's closing step is marked complete — consistent bookkeeping for a plan hand-off; no action needed beyond what the plan tools manage.

## Test/build status

Not run by the reviewer (read-only, no shell access). The main agent must still run the frontend vitest suite and `cargo test` (warning-free under `#![deny(warnings)]`) before committing.
