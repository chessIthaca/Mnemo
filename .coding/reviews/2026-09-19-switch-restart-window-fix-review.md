## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on `wt/mnemo` for bug-fixing plan 3f87686a ("Fix switch-restart window: same place, same size, on top"). The fix is correct, minimal, matches the plan, and is fully wired: the peek→set_focus foreground fix, the maximized persist/restore, and the pre-restart geometry flush all check out against the source (including the vendored tao implementations the plan cites). Both findings are LOW: one stale doc comment inside the changed file, one narrow residual edge in the beforeunload path (not the fixed switch path). Neither blocks landing.


## Scope reviewed

Full uncommitted diff on `wt/mnemo` (6 modified files + 2 new frontend files + the plan file), read in full, plus the surrounding context needed to judge it: `src/config/mod.rs` marker functions, `src-tauri/src/main.rs` setup closure + `build_brain_inner`, `frontend/src/App.tsx` geometry effect + `PersistedGeometry`/`readWindowGeometry`, `frontend/src/lib/windowRestore.ts` + its full test file, `ProjectPicker.tsx`, `windowGeometryFlush.ts/.test.ts`, `vitest.config.ts` + the `vitestInclude` guard, `switch_project` in `src-tauri/src/ipc/projects.rs`, and the vendored tao sources the plan cites.

## Findings

### LOW 1 — Stale leading comment on the App.tsx geometry effect (documentation sync)

`frontend/src/App.tsx:484-491` — the effect's leading comment still describes the OLD behavior this diff replaces:

- Line 485: "if we saved geometry last time and **it wasn't maximized**, re-apply it" — the restore guard is now `if (saved)` and re-maximizes when `saved.maximized`.
- Lines 489-491: "save the current bounds — but **skip saving while maximized** so the last *normal* bounds are kept" — the save path now persists `{...lastNormalBounds, maximized: true}` instead of skipping.

The inline comments at the actual changed sites (restore re-maximize, save-while-maximized, flusher) are accurate and good; only this summary comment was left behind. Fix: reword to match (e.g. "re-apply it (re-maximized when it was saved maximized)" / "save the current bounds, persisting the maximized flag on the last normal bounds").

### LOW 2 — beforeunload can persist a stale `maximized: false` (maximize + close within the 400 ms debounce)

`lastMaximized` is updated only inside `save()`, which runs on the debounce. If the user maximizes and closes the window (X button) within 400 ms, `beforeunload` writes `{...lastBounds, maximized: false}` and the restart comes back un-maximized — the plain-close analogue of the defect this plan fixes. Scope note: the **switch-restart path (the fixed defect) is immune** — the picker's `flushWindowGeometry()` calls `save()`, which queries `isMaximized()` fresh. This residual affects only the normal close path in a <400 ms window, is strictly better than the old never-persist-maximized behavior, and matches the plan's specified design (`beforeunload` can't await an IPC round-trip). Optional improvement if desired: keep `lastMaximized` fresh outside the debounce (fire-and-forget `void win.isMaximized().then(...)` in `debouncedSave`, or a maximize/unmaximize window-event listener). Not a blocker.

## Verification evidence

**Foreground fix (Rust).** `peek_pending_project()` at `main.rs:251` before `WindowBuilder::new` (the only "main" window creation — verified single `WindowBuilder::new` in src-tauri), `window.set_focus()` after `.build()` with non-fatal `eprintln!` on failure. Verified against vendored tao: `set_focus` (`vendor/tao/src/platform_impl/windows/window.rs:175`) guards on visible && !minimized && !already-foreground, then `force_window_active` (`window.rs:1500`) = `SetForegroundWindow` + the ALT-key `SendInput` fallback — the main.rs comment's claim is accurate, and the plan's citation is exact. The restore's `setPosition` path uses `SWP_NOZORDER | SWP_NOACTIVATE` (`window.rs:248`), so the frontend geometry restore cannot disturb the foreground fix; `maximize()` activating the same already-foreground window is a no-op.

**Peek-then-take sequence.** `read_marker_at` mirrors `consume_marker_at`'s read+trim semantics and never deletes; `take_pending_project` is consumed only in `build_brain_inner` (`main.rs:1095`), which runs after the setup-closure peek (via `build_brain` at `main.rs:318`). `write_pending_project` has exactly one caller (`switch_project`, `projects.rs:208`), so peek-Some ⇒ switch restart. Unit tests exercise the changed path directly (peek leaves the file; peek-then-take consumes exactly once; missing/empty → None without deleting).

**Maximized persist/restore (App.tsx).** `PersistedGeometry.maximized` exists with a backward-compatible default (`readWindowGeometry` line 78). Restore: clamped normal bounds applied first, then `await win.maximize()` — correct ordering (un-maximize returns to the normal bounds). Save: `{...lastBounds, maximized: true}` with the documented fresh-profile fallback (`lastBounds ?? snapshotBounds()`; the monitor-sized stand-in is clamped to the work area at restore). `snapshotBounds` is cleanly reused by both paths. beforeunload carries `lastMaximized`.

**Flush wiring.** `switchProject` has exactly two callers (graph-verified): `handleOpen` and `handleCreate`, both `await flushWindowGeometry()` immediately before. `save()` never rejects (internal try/catch), so a flush failure can't block the switch. The flusher closure reads `saveTimer` at call time (clears the pending debounce correctly); a stale cleared timer id is a harmless no-op on later `clearTimeout`s. StrictMode double-mount is safe (register → null-cleanup → register; last wins). The `!restoreDone` early return in `save()` is correct for the flush too: pre-restore nothing has changed, so the persisted value is already current (and a user move during the restore's async window is overwritten by the restore itself).

**Source-contract anchors.** All pre-existing `windowRestore.test.ts` anchors verified against the current App.tsx: the import line, exactly one `clampRestoredGeometry(` call (import carries no parens), the restore block still spanning "Restore saved geometry once on mount"…"let saveTimer", no `setMinSize` in the block, `new LogicalSize(restored.width, restored.height)` / `new LogicalPosition(restored.x, restored.y)` unchanged. The three new contracts match the actual source strings. `windowGeometryFlush.test.ts`'s picker contract counts match (2 flushes, 2 switches, in order), and the registry tests are order-independent. The new test file is registered in `vitest.config.ts` include (required by the `vitestInclude` guard — without it the guard would fail listing the unregistered file).

**Regression-test quality (bug-plan checks).** Root cause documented in the plan context (all three prongs, with file/line citations that I re-verified). Every regression test fails without the fix: the Rust peek tests reference a function that doesn't exist pre-fix (compile failure), and every source contract asserts strings only the fix introduces (`peek_pending_project().is_some()`, `window.set_focus()`, `if (saved.maximized)`, `registerWindowGeometryFlusher(() => {`, flush-before-switch ordering). The two-processes+WM nature of the foreground defect makes source contracts the right automatable guard; the convention matches the existing frontend style.

**Multi-platform neutrality.** No `cfg(windows)` additions anywhere in the diff. `set_focus`, `maximize`, `isMaximized`, and the marker file read are all cross-platform Tauri/stdlib APIs; the Windows-specific foreground analysis lives in comments only (sanctioned). The new `switch_restart_focus_contract` module is `#[cfg(test)]` on all platforms (unlike the pre-existing unix-gated `mod tests`), so it runs on Windows too.

**Documentation sync.** No README/PLAN/docs mentions of window-geometry behavior exist to update (searched `docs/**` and root `*.md`); the new pub fn and the new frontend module both carry doc comments. The only doc gap is LOW 1 above.

**File-tools-first.** No signs of shell-based mutation in the diff (clean, style-consistent edits; no formatting anomalies).

**Pre-existing workspace failure.** `agent::factory::tests::tools_array_stays_within_context_budget` is confirmed pre-existing (reproduced with these changes stashed) and does not interact with this diff: nothing here touches agent factory, tool registration, or the tools array — the Rust changes are a marker read + a window focus call + tests.

## Verdict

The change is correct, minimal, and plan-faithful; both findings are LOW (one stale comment, one narrow residual edge outside the fixed path). Fix LOW 1 (comment reword); LOW 2 is optional. Then land.
