# Review: Window geometry persistence + resizable tools panel + reset-all appearance

**Date:** 2026-04-04
**Scope:** All uncommitted changes (`git diff HEAD`) — `frontend/src/App.tsx`,
`frontend/src/hooks/useAgentStore.ts`, `frontend/src/components/layout/RightPanel.tsx`,
`frontend/src/components/layout/ConfigDialog.tsx`, `src-tauri/capabilities/default.json`.
(Plan/safety.toml/stack.json bookkeeping excluded per instructions.)

**Verdict:** No blocking correctness or security defects. Several low-to-medium
robustness bugs in the ResizeHandle pointer logic and the beforeunload save path.
Constitution compliance: clean (all new public functions have doc comments; no
Rust changes so `cargo test` is N/A; no commits to main).

---

## Bugs

### B1. `beforeunload` save is unreliable — final geometry can be lost
**File:** `frontend/src/App.tsx:289-292`

```ts
const onBeforeUnload = () => {
  // Fire-and-forget; the page is unloading so we can't await.
  void save();
};
```

`save()` is async and immediately hits `await win.isMaximized()` — a Tauri IPC
round-trip. The browser does not wait for unresolved promises during
`beforeunload`, so the IPC calls are abandoned mid-flight and the geometry is
**not** written. The debounced resize/move save (400 ms) is the real
persistence mechanism; `beforeunload` is effectively dead code for its stated
purpose.

**Concrete data-loss window:** if the user moves/resizes the window and closes
within 400 ms, the debounce timer is cleared by the effect cleanup
(`App.tsx:296`) and the `beforeunload` fallback doesn't complete — the final
bounds are lost.

**Suggested fix:** use Tauri's `win.onCloseRequested()` (an async handler that
*can* await) instead of the DOM `beforeunload` event, or save synchronously by
caching the last-known logical bounds from the `onMoved`/`onResized` callbacks
and writing them directly to localStorage in `beforeunload` (no IPC).

---

### B2. ResizeHandle has no `pointercancel` handler — listener leak on touch/OS cancel
**File:** `frontend/src/App.tsx:392-393`

The drag registers `pointermove` + `pointerup` on `window` but not
`pointercancel`. When the OS cancels the pointer (touch interruption, system
gesture, `pointercancel` dispatched instead of `pointerup`), `onUp` never runs,
so both window listeners leak permanently for the lifetime of the page.

**Suggested fix:** also listen for `pointercancel` and route it to the same
cleanup as `onUp`.

---

### B3. `releasePointerCapture` is called before `removeEventListener` — leak if it throws
**File:** `frontend/src/App.tsx:382-385`

```ts
const onUp = (ev: PointerEvent) => {
  handle.releasePointerCapture(ev.pointerId);   // can throw
  window.removeEventListener("pointermove", onMove);
  window.removeEventListener("pointerup", onUp);
  ...
};
```

If `releasePointerCapture` throws (e.g. capture was already implicitly
released, or `pointerId` doesn't match an active pointer), the two
`removeEventListener` calls below it are skipped, permanently leaking the
listeners. In normal mouse usage capture is still active during `pointerup`,
so this is uncommon — but it is a defensive-coding defect.

**Suggested fix:** wrap `releasePointerCapture` in `try/catch`, or move the
`removeEventListener` calls above it.

---

### B4. No unmount cleanup for an in-flight drag
**File:** `frontend/src/App.tsx:364-394`

`ResizeHandle` adds window-level listeners inside `onPointerDown` and removes
them only inside `onUp`. There is no `useEffect` tracking the in-flight drag
state. If the component unmounts while a drag is active (e.g. the right panel is
hidden via `toggleTabAndReveal` disabling the last tool, or `rightPanelVisible`
flips to false), `onUp` never fires and the listeners leak.

**Suggested fix:** track drag state in a ref + `useEffect` cleanup that removes
the listeners on unmount.

---

### B5. Async listener-registration race (dev / fast unmount)
**File:** `frontend/src/App.tsx:274-285`

```ts
let unlistenResize: (() => void) | null = null;
win.onResized(() => debouncedSave())
  .then((u) => { unlistenResize = u; })
  .catch(() => {});
```

`unlistenResize` is assigned inside the `.then()` callback. If the effect
cleanup runs before the registration promise resolves (React StrictMode dev
double-invoke, or a fast unmount), `unlistenResize` is still `null` at cleanup
time, so the listener is never unlistened → leak. The registration promise then
resolves against a dead closure.

**Suggested fix:** use a `cancelled` flag:
```ts
let cancelled = false;
win.onResized(cb).then((u) => { if (cancelled) u(); else unlistenResize = u; });
return () => { cancelled = true; if (unlistenResize) unlistenResize(); ... };
```

---

## Performance

### P1. `setRightPanelWidth` writes localStorage synchronously on every `pointermove`
**Files:** `frontend/src/hooks/useAgentStore.ts:1201-1208`, `frontend/src/App.tsx:380`

`onMove` calls `onWidthChange` (→ `setRightPanelWidth`) on every pointermove
event (60+ Hz). `setRightPanelWidth` calls `writeLs` (synchronous
`localStorage.setItem`) + a Zustand `set` on every move. Synchronous localStorage
writes block the main thread and can cause visible jank during drag. The
codebase already uses `requestAnimationFrame` batching for streaming text
(`appendStreamingText`), so this is inconsistent with the established perf
pattern.

**Suggested fix:** debounce/throttle the localStorage write (e.g. rAF-batch the
store update, or only persist in `onUp`).

---

## Security / least-privilege

### S1. Unused permission `core:window:allow-maximize`
**File:** `src-tauri/capabilities/default.json:13`

The code calls `win.isMaximized()` (covered by `core:window:default`, which
includes `allow-is-maximized`) but never calls `win.maximize()`. The
`core:window:allow-maximize` grant is therefore unnecessary. `allow-set-position`
and `allow-set-size` (lines 11-12) are correctly required for the restore path.

**Suggested fix:** remove `core:window:allow-maximize` unless a future feature
needs it.

---

## UX

### U1. Persisted panel width is not re-clamped on restore
**Files:** `frontend/src/hooks/useAgentStore.ts:1151`, `frontend/src/components/layout/RightPanel.tsx:42`

The width is clamped to `[300, innerWidth * 0.8]` during drag
(`App.tsx:378-379`), but `readLsNumberOrNull` returns the raw stored value on
load with no re-clamp. If the window is now smaller (monitor change, smaller
restore), a stored width of e.g. 1000 px can exceed the viewport, pushing the
`flex-1` main column toward zero width or causing horizontal overflow.

**Suggested fix:** clamp `rightPanelWidth` against `window.innerWidth` on first
read (or in `RightPanel`'s render).

---

## Correctness (verified OK — no action needed)

- **`readLsNumberOrNull` handles the empty-string case correctly**
  (`useAgentStore.ts:166-176`): `setRightPanelWidth(null)` writes `""`, and the
  helper checks `raw === ""` *before* `Number(raw)` (which would yield `0`), so
  it correctly returns `null`. ✓
- **Restore-on-mount race is guarded** (`App.tsx:244-245`): the `restoreDone`
  flag + 400 ms debounce prevents a resize/move event fired by the restore's own
  `setPosition`/`setSize` from overwriting saved geometry with stale values. ✓
- **Tauri API errors are handled in dev/web context** (`App.tsx:236-237`,
  `262-264`, `279`, `285`): all IPC calls are wrapped in try/catch or
  `.catch(() => {})`, so a non-Tauri environment logs and degrades gracefully. ✓
- **DPI round-trip is correct** (`App.tsx:250-260`): `outerPosition`/`outerSize`
  are physical px; dividing by `scaleFactor()` stores logical px, and
  `LogicalPosition`/`LogicalSize` re-apply them as logical. ✓
- **`resetAppearance` is a correct superset of `resetColors`**
  (`useAgentStore.ts:1369-1420`): resets theme + font + all 11 colors, writes
  localStorage, re-applies CSS vars, and updates the store in one `set()`. ✓

---

## Constitution compliance

- **Public functions have doc comments:** `readWindowGeometry`, `writeWindowGeometry`
  (`App.tsx:33,60`), `readLsNumberOrNull` (`useAgentStore.ts:160-165`),
  `setRightPanelWidth` (`useAgentStore.ts:377`), `resetAppearance`
  (`useAgentStore.ts:413-417`), and `ResizeHandle` (`App.tsx:351-356`) all have
  JSDoc. ✓
- **`cargo test`:** no Rust changes in this diff (frontend TS/React only). N/A. ✓
- **Never commit to main:** no commits in this review. ✓
