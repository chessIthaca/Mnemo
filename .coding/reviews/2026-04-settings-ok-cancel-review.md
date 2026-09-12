# Review: Settings dialog — standard OK/Cancel buttons

**Date:** 2026-04
**Branch:** `feat/search-read-tool`
**Scope:** All uncommitted changes (`git diff HEAD`) for the Settings OK/Cancel
redesign. Files: `SettingsDialog.tsx`, `types.ts`, `types.test.ts`, and the 7
section components (`Appearance`, `Safety`, `Vision`, `Pricing`, `Models`,
`Advanced`, `Providers`).

## Verification performed

- `npx tsc --noEmit` → exit 0 (type-check clean).
- `npm run build` (tsc + vite) → exit 0, built in 752 ms.
- `npm test` (vitest) → 89 passed / 89 (7 files), including the new
  `dirtySectionIds` suite (5 cases).
- Line-ending check: all 5 sampled changed files are pure CRLF (Windows style),
  no mixed `\r\n`/`\n`. ✅ constitution-compliant.
- Branch is `feat/search-read-tool`, not `main`. No commits to main. ✅
- New public exports (`SettingsSectionHandle`, `dirtySectionIds`) both carry
  doc comments; `save` field is documented. ✅ constitution-compliant.

## Findings

### Correctness — no findings

The redesign is correct on all six review points:

1. **`useImperativeHandle` deps (no stale closures).** All 7 sections call
   `useImperativeHandle(ref, () => ({ save: ... }))` with **no deps array**.
   Per React semantics, omitting the array recreates the handle on every
   render, so the `save` closure always captures fresh state (`draft`,
   `mode`, `rules`, `endpoints`, etc.). The inline-arrow form
   (`AppearanceSection.tsx:132`, `VisionSection.tsx:59`) and the
   `handleSave`-reference form (`SafetySection.tsx:130`,
   `PricingSection.tsx:84`, `ModelsSection.tsx:177`, `AdvancedSection.tsx:105`,
   `ProvidersSection.tsx:220`) are equivalent here — both rebuild per render.
   No stale-closure bug.

2. **Cancel/discard with always-mounted sections.** `active={open}` drives
   every section. On close (`open=false`):
   - `AppearanceSection` restores committed CSS from the store
     (`AppearanceSection.tsx:106-109`) — draft/snapshot are overwritten on
     next open via `readDraftFromStore()` (`:96-99`).
   - `ProvidersSection` clears `apiKeys` + `snapshot` and reports
     `onDirtyChange(false)` (`ProvidersSection.tsx:91-98`).
   - `Safety`/`Vision`/`Pricing`/`Models`/`Advanced` each reload from the
     backend on `active=true` (`useEffect(() => { if (active) void load(); },
     [active])`), so a stale draft from a cancelled edit is overwritten on
     reopen. Drafts persist in memory between close and reopen but are never
     shown stale. ✅
   - `requestClose` (`SettingsDialog.tsx:118-132`) confirms discard via
     `window.confirm` only when `anyDirty`, then clears `dirtyMap` and calls
     `onClose()`. Correct.

3. **`handleOk` sequential save** (`SettingsDialog.tsx:135-162`). Reads
   `dirtyMap` at call time via `dirtySectionIds(dirtyMap)`; iterates dirty
   sections in insertion order; `await handle.save()` per section. On partial
   failure (A ok, B fails): A's snapshot is updated inside its `save` → A's
   `dirty` recomputes false → `onDirtyChange(false)` fires → `dirtyMap`
   updated; B's snapshot unchanged → stays dirty. `failures > 0` → dialog
   stays open with a red error line. Matches the plan's intended behavior.
   `setSaving(true/false)` brackets the loop and disables both buttons
   during the save. ✅

4. **OK disabled when `!anyDirty`** (`SettingsDialog.tsx:374`). OK is
   `disabled={!anyDirty || saving}`; Cancel is always enabled (except during
   `saving`). User can close with no changes via Cancel. Acceptable per plan.

5. **ProvidersSection save path.** Uses `saveEndpoints` (not `saveSettings`)
   at `ProvidersSection.tsx:198`, updates `snapshot` (`:205-207`), returns
   `true`/`false` correctly. Works through the imperative handle. ✅

6. **Unused imports/variables.** `Save` icon removed from all 7 sections.
   `RotateCcw` retained in `AppearanceSection` — still used at `:201`
   (`handleResetDefaults` button, distinct from the removed per-section Reset).
   `saving` state retained in every section — still used inside each `save`
   closure. `handleReset` (Appearance's old snapshot-restore) fully removed;
   no dangling references (search confirms only `handleSave` remains, each
   defined + referenced exactly once in `useImperativeHandle`). ✅

### Bugs — no findings

- No race condition in the sequential save loop: it `await`s each section
  before proceeding, and `setSaving` prevents re-entry (both buttons disabled).
- `React.RefObject<SettingsSectionHandle>` at `SettingsDialog.tsx:77` resolves
  via the UMD global `export as namespace React` from `@types/react` even
  though the file imports only named hooks — confirmed by `tsc --noEmit`
  passing. Not an error. (Optional nit: importing `type React` would be more
  explicit and match `dialog.tsx`/`tabs.tsx`, but it is not required.)
- `dirtySections` (`:101-104`) and `dirtySectionIds` (`:136`) compute the same
  thing two ways (inline `Object.keys(...).filter` vs. the helper). Harmless
  duplication, not a bug — the helper exists for testability of `handleOk`.

### Security — no findings

- No new network/IPC surface. `saveEndpoints`/`saveSettings` calls are
  unchanged from the pre-existing per-section save paths.
- `ProvidersSection` still clears API keys from React state on deactivate
  (`:91-98`), so secrets are not retained in memory after the dialog closes.
- No `dangerouslySetInnerHTML`, no eval, no untrusted-input interpolation.

### Constitution compliance — no findings

- **Doc comments:** `SettingsSectionHandle` interface, its `save` field, and
  `dirtySectionIds` are all documented (`types.ts:203-223`). ✅
- **Line endings:** all changed files are consistent CRLF; no mixed endings. ✅
- **No commits to main:** work is on `feat/search-read-tool`. ✅
- **Tests run before marking complete:** `npm run build` + `npm test` both
  green (89/89). ✅
- **Code style:** matches existing patterns (`forwardRef` + named function
  inside, `useImperativeHandle` with no deps, Tailwind class conventions). ✅

## Summary

The diff is clean. The OK/Cancel redesign is correct: no stale closures, no
discard regressions, the sequential save loop handles partial failure as
specified, and all removed-button cleanup is complete with no dangling
references. Build and tests pass. No findings to act on.
