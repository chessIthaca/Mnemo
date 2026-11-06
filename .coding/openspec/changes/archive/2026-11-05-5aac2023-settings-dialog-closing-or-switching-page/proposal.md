Leaving a settings page or the dialog with unsaved edits always asks first, through one tested guard at dialog level and one inline confirmation; the related Endpoints, load-gating, re-read and warning defects are fixed.

Source: a read-only architecture review of the dialog (2026-11-05, findings F1-F8), verified against the code by the lead. History: the React-era dialog had a dialog-level dirty map; the Svelte 5 rewrite moved to per-page Save/Discard and dropped the guard everywhere except Tiers.

Current code (mnemo2/src/components unless noted):
- Sheet.svelte: `{#if open}` renders the body, so closing unmounts every page. X button, Escape (svelte:window keydown) and the backdrop <button> all call `onclose()` unconditionally. Sheet is shared with Composer, StartScreen, TopBar and WorkerDeleteSheet: DO NOT change Sheet.svelte; the guard wraps the onclose it is given.
- SettingsDialog.svelte: passes `{onclose}` straight to Sheet; rail buttons do `page = entry.id`; `{#if page === ...}` chain unmounts the old page.
- Draft state per page: EndpointsPage `dirty` (line 47); TiersPage `hasUnsaved()` (line 603: ladderChanged, dispatchDirty, gatesDirty, flagsDirty); ModelsPage compactionDirty, visionDirty, indexDirty, budgetDirty (45-48); SavingsPage optimizerDirty, pricingDirty (59-60); SafetyPage modeDirty, rulesDirty (43-44); McpPage `dirty` (61) plus `formOpen` (an open add/edit form holds typed values that `dirty` does not count); SpecsPage artifactDirtyNow (75), configDirty (89). ProjectPage and AppearancePage apply immediately: nothing to guard.
- TiersPage guard: lines 612-644 (`pageEl`, capture-phase click/keydown, beforeunload, `confirm("Discard unsaved Tiers edits?")`), uses `leavesPage` from lib/layaSettings.ts:474 (tested in lib/layaSettings.test.ts ~155-186, imported only by TiersPage). TiersPage's other confirm() at line 202 (force-on gate) is a different question: leave it.
- EndpointsPage inline banner: lines 313-319, styles `.inline-confirm` at 577-578. It keeps its own `guard()`/`pendingAction` for switching endpoints inside the page.

Decisions:
1. One registry through Svelte context, new file mnemo2/src/lib/unsavedEdits.ts (plain .ts; a Set of callbacks, no runes needed because deriveds read fresh outside effects):
```ts
export interface UnsavedRegistry { register(check: () => boolean): () => void; hasUnsaved(): boolean; }
export function createUnsavedRegistry(): UnsavedRegistry
export function provideUnsavedRegistry(): UnsavedRegistry   // createUnsavedRegistry + setContext; call during component init
export function trackUnsaved(check: () => boolean): void    // getContext; no registry (a page rendered alone in a test) = no-op; else onDestroy(registry.register(check))
```
2. The guard lives in SettingsDialog only. One function `requestLeave(action: () => void)`: if `registry.hasUnsaved()` set `pendingLeave = action`, else run it. Sheet gets `onclose={() => requestLeave(onclose)}`; rail buttons call `requestLeave(() => (page = entry.id))`, and clicking the page already showing does nothing. While a banner is showing, Escape/X/backdrop just keep the existing pendingLeave (except a different rail click, which replaces it).
3. One confirmation idiom: new InlineConfirm.svelte (props `message`, `onkeep`, `ondiscard`; buttons "Keep editing" and "Discard changes"; role="alert"; the `.inline-confirm` styles moved from EndpointsPage). EndpointsPage's banner and the dialog's banner both use it. The dialog renders it at the top of `.settings-page`, above the page, message "Discard unsaved changes?". When it appears, focus moves to "Keep editing" so a stray Enter or Escape loses nothing. Discard clears pendingLeave and runs the action (the page unmounts, dropping its drafts). Keep clears pendingLeave.
4. `beforeunload` moves from TiersPage to SettingsDialog: while open, a window listener that calls preventDefault + `returnValue = true` only when `registry.hasUnsaved()`. Whether Tauri's webview shows that prompt on window close stays unverified; do not build more on it.
5. TiersPage's guard, `pageEl` and its bind, `leavesPage` and its tests are removed.
6. `useDraft()` composable (review F7) is NOT in this plan: the pages have multi-section drafts, and the registry already gives the single plug point. Sheet.svelte, ProjectPage and AppearancePage do not change.

Verification, in mnemo2/: `npx vitest run src/components src/lib`, `npm test`, `npx svelte-check --tsconfig ./tsconfig.json`, then `npx vite build` must print no assignment_value_stale warning after step 2. Preview page for a look: /preview.html?scene=<settings scene> if one exists in src/test/previewScenes.ts.
