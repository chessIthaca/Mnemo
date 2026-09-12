## Verdict: PASS

Round-2 verification of the fixes for `.coding/reviews/2026-indexing-progress-overlay.md` (FINDINGS 1 high / 2 low), at commit `2c56c36` on `wt/agenticcoder`. Working tree is clean (`git status --short` empty) — all fixes are in the commit. Each fix verified against the actual current files, not just the commit message.

## HIGH 1 — vitest include entry — FIXED CORRECTLY

- `frontend/vitest.config.ts:57` contains exactly `"src/components/projects/indexingOverlay.test.ts",` in the `include` array.
- The suite is valid vitest under that entry: imports `describe`/`expect`/`it` from `vitest`; imports `applyIndexProgress` (exported, `IndexingOverlay.tsx:26`) and `IndexingOverlayState` (exported, `:15`) from `./IndexingOverlay`; `IndexProgressEvent` is exported at `frontend/src/lib/tauri.ts:909`. Environment is `node` and the suite is a pure reducer fold — no DOM needed.
- All 7 cases match the reducer semantics (`_state` ignored; `started`→0/0 progress, `progress` carries counters, `done`→null including the from-null no-op, `failed`→error card, `started` after failure re-shows). The event literals match the TS union at `tauri.ts:909-913`, which is pinned to the Rust wire shape.
- Reported post-fix run confirms the expected delta: 617 passed / 46 files (round 1 observed 610/45 → +7 tests, +1 file — exactly this suite). No longer silently skipped.

## LOW 1 — switch-mode double-mount — FIXED CORRECTLY

- The switch-mode Dialog return branch (`ProjectPicker.tsx:265-287`) no longer mounts `<IndexingOverlay />`; a grep over `frontend/src/**/*.tsx` confirms exactly two mount sites remain: `App.tsx:653` and `ProjectPicker.tsx:246` (startup branch). Lines 260-264 carry a comment explaining why (App-tree instance covers switch mode; second copy would stack the failed card — cites review LOW 1).
- The startup-mode branch (`ProjectPicker.tsx:241-258`, mount at :246) still mounts it — required, because `App.tsx:561-563` early-returns `<ProjectPicker />` (no `onClose` → startup mode) before the normal tree exists in needsProject mode.
- `IndexingOverlay.tsx:44-62` doc comment rewritten: the inaccurate "Double-mounting is harmless" claim is gone; it now documents the exactly-one-live-mount rule (App tree covers startup pass + every switch-mode create flow; startup-mode picker covers the needsProject case where App's tree is not mounted; switch-mode picker must NOT mount a second copy or the failed card needs two Dismiss clicks).

## LOW 2 — hide native child WebView2 — FIXED CORRECTLY

- `IndexingOverlay.tsx:70` calls `useBrowserOverlay(state !== null)`, imported at `:8` from `../../hooks/useBrowserOverlay`, placed unconditionally before the `if (state === null) return null;` early return at `:89` — rules of hooks satisfied.
- `useBrowserOverlay.ts:27` takes `open: boolean`; `:29` `if (!open) return;` makes it a no-op while hidden; enter fires on mount-while-open / open-flip, exit on close/unmount, balanced by the backend's saturating depth counter (`:18-23`). Interaction with ProjectPicker's own `useBrowserOverlay(true)` (`ProjectPicker.tsx:45`) during a switch-mode create flow is safe: overlay enter bumps the counter to 2, exits balance — no underflow, no wedged-hidden webview.
- Consistency: App-tree mount (`App.tsx:653`) is inside the normal tree (the `return` at `:591`), immediately before `<Sidebar />` (`:654`), and now hides the child WebView2 like every other full-viewport modal; the startup-mode ProjectPicker mount was already covered by the picker's own hook and remains consistent.

## Regression check

- App.tsx mount unchanged and correct (`:653`, normal tree, pre-Sidebar); the needsProject early-return topology is untouched.
- Plan goal intact: `IndexingOverlay.tsx:102-126` renders the reconcile-style dialog with an animated progress bar and the mono `${done}/${total} files indexed` counter ("…" while indeterminate); `done` self-hides, `failed` keeps the error card with a Dismiss button.
- Commit `2c56c36` includes all expected files (Rust stream + gate, `seed_stores` progress param, frontend overlay + test + tauri.ts listener, vitest entry, README bullet, plan/review bookkeeping).
- Reported post-fix test matrix (read-only, not re-run): root `cargo test` 1545/0 exit 0 warning-free; src-tauri 172/0; `npm test` 617 passed (46 files); `npx tsc --noEmit` exit 0. Consistent with the fixes.

All three round-1 findings are fixed correctly and nothing regressed. PASS.
