## Verdict: FINDINGS (1 high, 2 low)

Reviewed ALL uncommitted changes on `wt/agenticcoder` (git diff HEAD + the two untracked frontend files). The design is sound and the Rust/TS wire contract is correct and pinned on both sides, but the new frontend test file is not registered in `vitest.config.ts` — its 7 tests silently never run — and the switch-mode double-mount plus a WebView2-overlay consistency gap are worth fixing.

## Findings

### HIGH 1 — `indexingOverlay.test.ts` is never executed: missing entry in `frontend/vitest.config.ts`

`frontend/vitest.config.ts:16-58` enumerates every test file in the `include` array (single files + one `src/components/settings/**` glob). The new `frontend/src/components/projects/indexingOverlay.test.ts` matches NO entry — there is no `src/components/projects/**` glob. Since `include` replaces vitest's default discovery, the 7 `applyIndexProgress` cases never run under `npm test`. The reported "610 passed (45 files)" therefore ran without them, and the plan step "Frontend test for the overlay reducer" (`.coding/plans/fad07509.md:19`) is not actually effective — this is exactly the silent-test-loss failure mode the include-list convention guards against (precedent: `ToolImage.test.ts`, `MainPanel.tabStyle.test.ts` were each added explicitly).

**Fix:** add `"src/components/projects/indexingOverlay.test.ts",` to `include` in `frontend/vitest.config.ts` and re-run `npm test` (expect 617 passed / 46 files). Verify the new suite appears in the run output.

### LOW 1 — Switch-mode double-mount: two stacked overlay instances, failed-card Dismiss needs two clicks

`App.tsx:653` mounts `<IndexingOverlay />` in the main tree (always mounted when a project is open — the `needsProject` early-return at `App.tsx:561` means it is correctly absent at startup). The switch-mode `ProjectPicker` mounts a second instance inside `<Dialog>` (`ProjectPicker.tsx:265`; Radix `Dialog.Root` renders non-portaled children in place, so it really renders). During a switch-mode create flow both instances fold the same event stream and both become visible — identical `fixed inset-0 z-50` layers stacked on top of each other. For `progress` this is invisible duplication (deterministic reducer, identical state). For `failed` it is user-visible: each instance holds its own state, so clicking Dismiss hides only the top (Dialog's) instance and reveals the App-tree instance's identical failed card underneath — the button looks broken on the first click. The component doc comment ("Double-mounting is harmless", `IndexingOverlay.tsx:57`) is inaccurate for the interactive failed state.

**Fix:** remove the `<IndexingOverlay />` from the switch-mode Dialog (`ProjectPicker.tsx:265` — the App-tree instance already covers every switch-mode create flow), or hoist the overlay to a single mount that also wraps the `needsProject` early-return branch and drop both ProjectPicker instances.

### LOW 2 — App-tree overlay doesn't hide the native child WebView2 (Windows), inconsistent with every other full-viewport modal

Every other full-viewport modal in the codebase calls `useBrowserOverlay` to hide the native child WebView2 that is composited above the app's HTML (`AboutDialog.tsx:61`, `MergeToMainDialog.tsx:40`, `SafetyToggleDialog.tsx:30`, `SettingsDialog.tsx:90`, `ProjectPicker.tsx:45`, `ToolImage.tsx:100`). The new `IndexingOverlay` does not. With the Browser tab active (child WebView2 live), a >1s startup indexing pass renders the overlay behind the native web page. The two ProjectPicker-mounted instances are incidentally covered by ProjectPicker's own `useBrowserOverlay(true)`; only the App-tree instance is exposed. (The reconcile dialog at `App.tsx:596` has the same pre-existing gap — the new component copies its styling, but the overlay class of UI is exactly what `useBrowserOverlay` exists for.)

**Fix:** in `IndexingOverlay`, call `useBrowserOverlay(state !== null)` (the hook takes an `open` flag and balances enter/exit via a counter).

## Checklist results

**Correctness/bugs (checked, no findings beyond the above):**
- Event lifecycle: startup pass emits `progress` ticks only after the 1s gate AND the shared throttle, terminal Done/Failed only when `forwarded` is set (`main.rs:1468-1523`) — sub-second passes emit nothing, so no overlay flash; no `Started` on the startup path is handled correctly by the reducer (progress-from-null shows). Create pass emits `Started` before `spawn_blocking`, throttled ticks without the 1s gate, and Done/Failed on all three match arms (`projects.rs:112-158`).
- Gate logic: `t0` is captured inside the spawned task (measures the pass, not the caller's await); `forwarded` is an `Arc<AtomicBool>` correctly cloned into the callback (`cb_forwarded`) and read after `.await`. Borrow/move soundness verified: both closures are defined/moved inside the `spawn_blocking` closure bodies, `Some(&progress)` is a local borrow used synchronously; `g` is moved in two exclusive match arms; the outer `graph` Arc remains for factory + watcher wiring.
- `create_project` AppHandle injection is valid Tauri (injected params may precede `State`); frontend `createProject(path, name)` needs no change.
- `seed_stores` signature change is applied at every call site (search confirms only `projects.rs:129` + the updated tests in `src/project/mod.rs`).
- Channel name exact-match Rust↔TS: `codegraph://index-progress` on both sides; serde `"type"`-tagged kebab-case wire shape matches the TS union and is pinned by `index_progress_event_wire_shape`.
- React disposal: the effect handles StrictMode double-mount / quick unmount via the `disposed` flag + late-resolving unlisten; reducer is exhaustive over all four event kinds; subscription is active from picker mount, well before "Create & open" can be clicked.

**Security:** events carry only counts/summary/error strings — no secrets, no approval bypass, no path/sandbox surface; fire-and-forget on both ends. Clean.

**Constitution:** doc comments on all new pub items (channel const, enum + variants, `emit_index_progress`, `startup_should_forward`, updated `seed_stores`, TS type + `onIndexProgress`, `applyIndexProgress`/`IndexingOverlay`); no `#[allow]` anywhere in the changed code; no dead code/unused imports (all new imports used; `deny(warnings)` green per the reported runs). Regression tests present on the Rust side (wire-shape pin, gate table, `seed_stores_forwards_progress_ticks`) — but the frontend-side regression tests are nullified by HIGH 1.

**Documentation sync:** the README bullet is accurate (1s-gated silence for already-indexed projects matches the implementation). PLAN.md: no row required — this is a UI-layer stream on the already-documented reconcile/reindex event pattern, not a new technical decision; not a finding. The `IndexingOverlay.tsx:57` doc comment is inaccurate per LOW 1 and should be corrected with that fix.

**Multi-platform neutrality:** no OS-specific APIs/paths/shell in the new code; no `cfg(windows)` additions. Tauri events, `Instant`, atomics, and the TS overlay are portable. LOW 2 is a Windows-frontend consistency issue, not gated code.

## Notes (informational, not findings)

- `create_project` emits `Started` even when `codegraph_enabled == false`; the seed then does memory.db only and emits `Done` — a sub-millisecond 0/0 overlay flash at worst, invisible in practice.
- `.coding/backlog.jsonl` marks the worked item `in_flight` and appends two unrelated new items — normal bookkeeping to include in the commit.
- After fixing HIGH 1, re-run the full matrix (root `cargo test`, src-tauri `cargo test`, `npm test`, `npx tsc --noEmit`) since the closing sequence requires it.
