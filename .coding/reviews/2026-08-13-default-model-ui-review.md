# Code Review — Default-model UI (Settings dialog)

**Reviewer:** read-only reviewer
**Date:** 2026-08-13
**Scope:** All uncommitted changes in the working tree (`git diff HEAD` + untracked).

## Files changed
- `frontend/src/components/settings/sections/ProvidersSection.tsx` (+46) — default-model dropdown in section header.
- `frontend/src/components/settings/sections/EndpointCard.tsx` (+10/−2) — `★ {defaultModel}` chip in collapsed card header.
- `.coding/plans/stack.json` (plan-id pointer) and untracked `.coding/plans/b1bf8a1b-…md` (plan doc) — expected plan bookkeeping, not source.

## What the plan set out to do
Add a discoverable "Default model" `<select>` in the Providers section header (persisting the existing `default_model` via the unchanged `saveEndpoints` path), and surface the current default model as a chip in the collapsed default-endpoint card header. No backend changes.

## Verification performed
- Read both changed files in full; confirmed `serializeProviders` signature in `settings/types.ts` (dm is part of the dirty snapshot, lines 137–144).
- Confirmed dirty tracking: `setDefaultModel` updates `defaultModel` state → feeds the `dirty` memo (ProvidersSection.tsx:103) → `onDirtyChange` → dialog Save/OK flow. Baseline snapshot at load includes `config.general?.default_model`.
- Confirmed save flow: `handleSave` passes `defaultModel` to `saveEndpoints` (line 217). Unchanged and sufficient.
- Confirmed Tailwind 3.4.15 (`frontend/package.json`), so `max-w-40` (spacing-scale max-width = 10rem) is a valid utility.

## Findings

### Correctness
No findings.

- React nesting is valid: the `<select>` renders a single `<option>` or a `<>…</>` fragment of `<option>`s; fragments inside `<select>` are unwrapped by React and are legal here (ProvidersSection.tsx:286–299).
- `useMemo` deps are correct: `defaultModelOptions` depends on `[defaultEndpoint, defaultModel]`; `defaultEndpoint` derives from the stable `endpoints` state, so memo invalidation is correct (no stale options) (ProvidersSection.tsx:119–125).
- No key warnings in the new code (options keyed by model string `key={m}`).
- `defaultEndpoint`/`defaultModelOptions` correctly handle the backend-lenient case by appending a configured `defaultModel` that is absent from the endpoint's model list (lines 121–123).
- Edge cases behave as intended:
  - Default endpoint renamed → `onNameChange` updates `defaultProvider` (line 359) → dropdown re-derives.
  - Model row deleted while default → `deleteModel` clears `defaultModel` when `defaultProvider === ep.name` (lines 203–205) → dropdown shows "(choose a model)".
  - Default endpoint deleted → `deleteEndpoint` clears `defaultProvider` + `defaultModel` (lines 156–159) → dropdown disabled, shows "(set a default endpoint first)".
  - Blank-string models filtered out of options (line 120).
  - Empty endpoint model list → "(no models on this endpoint)" disabled-equivalent state.

### Bugs
No findings. (One pre-existing, non-introduced gap noted for awareness, not a defect of this diff: editing the text of a model row that is currently the default does not update/clear `defaultModel` via `onModelChange` — only `onDeleteModel` does. The new "keep defaultModel as an extra option" logic actually makes this state visible/benign rather than a crash. The per-row "set ★" buttons had the same pre-existing gap. Out of scope for this change; no action required here.)

### Security
No findings. The dropdown/chip render model names as React text children (auto-escaped); `title` attributes carry the raw string safely. No `dangerouslySetInnerHTML`, no new IPC, no secrets handling.

### Constitution compliance
No findings.
- Style matches surrounding code: Tailwind classes, CSS vars (`--text-muted`, `--text-primary`, `--accent-color`), `bg-bg-primary`, `rounded-lg border border-border`, comment style consistent with the effort-select pattern in EndpointCard.
- `max-w-40` is valid under the pinned Tailwind 3.4.15 (spacing-scale max-width).
- No `#[allow(...)]` / eslint suppression added; the change is frontend TSX (Rust `#![deny(warnings)]` not implicated). The plan records `npx tsc --noEmit` and `npm test` (vitest, 131 tests) as passing.

### Unrelated changes
None. The diff contains exactly the two frontend files plus `.coding/plans/` bookkeeping (stack.json pointer + the plan markdown), as expected.

## Verdict
Diff is clean — **no findings**. The implementation matches the plan, the save/dirty path is intact, and edge cases are handled.
