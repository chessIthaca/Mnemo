## Verdict: PASS

Zero findings. The changeset exactly implements the plan's goal — the four inflight-bar token counters render their raw values per event, the dead `useCountUp` hook is deleted with its sole caller gone, and the regression test pins the new contract. All six review-focus axes verified clean.

### Scope reviewed

All uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked):

- `frontend/src/components/chat/InflightBar.tsx` — `useCountUp` import removed; the four display consts now render raw values; comment block states the event-driven contract.
- `frontend/src/components/chat/InflightBar.test.ts` — old count-up-pinning test replaced by the per-event regression test.
- `frontend/src/hooks/useCountUp.ts` — deleted (79 lines, dead code).
- `.coding/backlog.jsonl` — run-all dispatch stamp for item d9b560a0 (pending → in_flight, plan 41f76e7e) — expected bookkeeping.
- Untracked: `.coding/plans/41f76e7e.md`, `.coding/knowledge/bug/2027-01-07-inflight-bar-token-counters-count-up-instead-of.md`.

### 1. Correctness — verified

- The four consts are value-identical to the old `useCountUp` targets: `Math.max(0, tokenUsage.prompt - tokenUsage.cached)`, `tokenUsage.completion + liveCompletionTokens`, `tokenUsage.reasoning + liveReasoningTokens`, `contextUsage.used` (InflightBar.tsx:159-162).
- Every render site unchanged — same variables, same formatting: `fmtTokens(promptDisplay)` :301, `fmtTokens(completionDisplay)` :304, `fmtTokens(reasoningDisplay)` :314; `contextUsedDisplay` feeds `contextPct` :165 and `fmtTokens(contextUsedDisplay)` :357. The variables are function-local to `InflightBarPanel` — nothing outside the component could consume the animated values (full-file read + code graph confirm; the graph's pre-deletion index shows InflightBarPanel as the sole `useCountUp` caller).
- No formatting regression from dropping the hook's `Math.round`: `useCountUp` always ended at the raw target (`setDisplay(target)`), so `fmtTokens` already received exactly these values at steady state; and the live estimates are integers anyway (reducer: `Math.max(1, Math.round(event.text.length / 4))`, agentEventReducer.ts:329 — the completion bucket mirrors it). The only removed behavior is the ~400ms transient, which is the defect.
- Hook-order safety: the removed calls sat before the early return at :218 alongside the remaining unconditional hooks; deleting hooks entirely cannot break the order of those that remain.

### 2. Regression test — verified

- New test "updates the ↓/🧠 token counters per event via the live estimates — no count-up interpolation" (InflightBar.test.ts:118-133): asserts the direct const expressions are present and the component source no longer contains `useCountUp`.
- Fails without the fix: pre-fix source contained the `useCountUp` import + four wrappers (so `not.toContain("useCountUp")` fails) and lacked the direct const expressions (so both `toContain` asserts fail). Passes with it — verified against current source lines 160-161.
- Pattern: the established `?raw` source-contract pattern (node env, no React DOM infra), consistent with the file's other tests and repo precedent (plan 00d88e13).
- Registration: `InflightBar.test.ts` is in `frontend/vitest.config.ts`'s include allow-list (line 22, pre-existing — modified file, no config change; the include-guard requirement is satisfied).

### 3. No dangling references — verified

- Plain tree-walk search for `useCountUp` (2564 files): the only source-tree hits are the regression test's own explanatory comment and the `not.toContain("useCountUp")` assertion — intentional, they pin the absence. No imports, no other components, no config references.
- All other hits are immutable `.coding/` history (the backlog item text, the BUG knowledge record, past plans/reviews/decisions) — historical records, correctly not rewritten.
- `tsc --noEmit` clean independently proves no dangling import of the deleted module.

### 4. Bug-plan checks — verified

- Root cause documented: `.coding/knowledge/bug/2027-01-07-inflight-bar-token-counters-count-up-instead-of.md` — symptom → root cause (cosmetic interpolation on top of already-event-driven values) → fix → regression test name. Complete.
- BUG memory written: semantic-tier record id d461fe1d ("inflight bar token counters count up instead of updating per event") — confirmed via memory_search.
- Regression test recorded on the plan: `.coding/plans/41f76e7e.md` "## Regression test" section names it; all 4 steps checked.

### 5. Constitution — verified

- Doc-sync: `README.md` and `PLAN.md` contain zero count-up/`useCountUp` references (md-glob search — matches only under `.coding/`). The component's comment block documents the new event-driven contract. Nothing stale.
- Multi-platform neutrality: frontend-only change; no platform-specific code introduced (the deleted hook's `window.matchMedia`/`requestAnimationFrame` were cross-platform web APIs; nothing added).
- Warning-free: the deletion removes a pre-existing `eslint-disable-next-line react-hooks/exhaustive-deps` — a net suppression reduction; no new `#[allow]`/`eslint-disable`/`ts-ignore` in the diff. Gates green (tsc clean, vitest green, cargo test 2079 passed / 0 failed).

### 6. Security — verified

Display-only change: one import removed, four numeric consts rendered through the existing pure `fmtTokens` formatter. No new inputs, IPC surface, or injection risk.

### Notes (considered, not findings)

- The regression test pins the ↓/🧠 direct expressions explicitly; ↑/ctx are covered by the blanket `not.toContain("useCountUp")` (the actual defect mechanism). Same scope as the old test it replaces — consistent and adequate.
- The ctx bar's fill span keeps its pre-existing `transition-all duration-300` CSS width transition (untouched by this diff) — a separate affordance from the reported counter count-up, correctly out of scope.
- The historical decision record `2026-01-06-app-shell-stops-re-rendering` mentions "feeds hooks (useCountUp etc.)" as split rationale — immutable history; the rationale (the panel feeds hooks) remains true via the remaining hooks.
