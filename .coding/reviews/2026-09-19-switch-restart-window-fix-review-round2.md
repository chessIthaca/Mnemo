## Verdict: PASS

Round-2 verification for bug-fixing plan 3f87686a ("Fix switch-restart window: same place, same size, on top") on `wt/mnemo`. Both round-1 findings (0 high, 2 low) are verified fixed in commit `0ea1f41` — the branch tip, with a clean working tree (`git diff HEAD` / `git status` empty), so the committed tree is the tree reviewed. The fix delta introduces no new issues, and the new regression test's anchors match the actual source. Nothing remains open.

## Scope reviewed

The round-2 delta on top of the round-1-reviewed state — the two finding fixes plus the new test — verified in the committed tree (`git show 0ea1f41` + the on-disk files, which are identical since the tree is clean): the reworded leading comment on the App.tsx geometry effect, the fire-and-forget `lastMaximized` refresh in `debouncedSave`, and the "keeps the maximized flag fresh between debounced saves" contract in `frontend/src/lib/windowRestore.test.ts`. The rest of the commit was verified in round 1 and spot-checked here (restore block, save path, flusher registration, beforeunload, effect cleanup, vitest include).

## Round-1 finding fixes — verified

### LOW 1 (stale leading comment) — FIXED

`frontend/src/App.tsx:484-492` now reads: "if we saved geometry last time, re-apply it via the Tauri window API — CLAMPED … — and re-maximize when it was saved maximized. Then listen for the window's own resize/move events (debounced) and save the current bounds, persisting the maximized flag on the last *normal* bounds so a restart reopens exactly like the window that closed." Both stale phrases are gone ("it wasn't maximized", "skip saving while maximized"), and every claim maps to the code: re-apply + re-maximize ↔ `if (saved)` (line 509) + `if (saved.maximized) { await win.maximize(); }` (565-567); persist-the-flag-on-normal-bounds ↔ the save path's `{ ...base, maximized: true }` with `base = lastBounds ?? snapshotBounds()` (603-605). Accurate.

### LOW 2 (stale `maximized: false` on beforeunload within the 400 ms debounce) — FIXED

`debouncedSave` (App.tsx:615-627) now refreshes the flag fire-and-forget on every invocation — and it is invoked on every resize/move event (`onResized(() => debouncedSave())` / `onMoved(() => debouncedSave())`, 646-659), so a maximize (which emits a resize) refreshes `lastMaximized` within IPC latency instead of waiting out the debounce. This is exactly the remedy round 1 suggested. Safety of the delta:

- The rejection handler `() => {}` swallows IPC failures (window destroyed mid-flight, etc.) — no unhandled rejection; the `void` prefix marks the floating promise intentional.
- `debouncedSave` stays synchronous — the clear/reschedule timer logic is untouched, and the flush path (`registerWindowGeometryFlusher(() => { …; return save(); })`, 633-636) is unaffected: `save()` queries `isMaximized()` fresh itself.
- A late-resolving `.then` after effect teardown merely assigns a closure-local boolean (the beforeunload listener is already removed) — harmless.
- Considered and dismissed: an out-of-order IPC resolution could in principle overwrite `lastMaximized` with an older value after a newer `save()` set it. Tauri IPC to the same window resolves in issue order in practice, any subsequent move/resize re-fires the refresh, and the residual exposure (the single-round-trip window between the maximize event and the flag landing, vs. the 400 ms window this closes) is strictly narrower than pre-fix. Not a finding.

## New regression test — anchors verified

"keeps the maximized flag fresh between debounced saves" (`windowRestore.test.ts:324-337`) slices App.tsx between `const debouncedSave` and `registerWindowGeometryFlusher(() => {` and asserts the block contains `isMaximized()` and `lastMaximized = m`. Verified against the source: the slice (App.tsx:615-633) contains `void win.isMaximized().then(` (621) and `lastMaximized = m;` (623). The slice cleanly isolates the fire-and-forget refresh — `save()`'s own `await win.isMaximized()` (595) sits before `const debouncedSave` and is excluded. The test fails without the fix (neither string exists in the slice pre-fix) — a genuine regression guard for LOW 2. The file was already registered in `vitest.config.ts` include (round 1), so the new test runs. All other source-contract anchors re-checked against the committed tree and still hold: the import line, exactly one `clampRestoredGeometry(` call, the restore block spanning "Restore saved geometry once on mount"…"let saveTimer" with no `setMinSize`, `new LogicalSize(restored.width, restored.height)` / `new LogicalPosition(restored.x, restored.y)`, `if (saved.maximized)` + `win.maximize()` in the restore block, and `registerWindowGeometryFlusher(() => {` / `registerWindowGeometryFlusher(null)`.

## Suite state

Read-only reviewer (no shell): suite results are as recorded in the commit message and the round-2 task — `cargo test` (root, canonical) green with 2492 tests, `cd frontend; npm test` green; the `cargo test --workspace` `tools_array_stays_within_context_budget` failure is pre-existing (round 1 reproduced it with these changes stashed) and touches nothing in this diff (marker read + window focus + tests only). Statically verified here: both Rust test modules and all frontend test files are present in the committed tree and correctly wired.

## Verdict

Both round-1 findings are fixed correctly in `0ea1f41`, the fix delta is safe (error-swallowing fire-and-forget, no interference with the debounce/flush paths, accurate reworded comment), and the new regression test pins the LOW 2 fix with anchors that match the source. No new findings. Ready to land.
