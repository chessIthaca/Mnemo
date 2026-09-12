# Review — Replace Browser-tab iframe with embedded child WebView2

**Date:** 2026-12
**Reviewer:** read-only subagent
**Scope:** ALL uncommitted changes (`git diff HEAD` + 3 untracked files).
**Files changed:** `src-tauri/Cargo.toml`, `src-tauri/src/ipc/browser_webview.rs` (NEW), `src-tauri/src/ipc/mod.rs`, `src-tauri/src/ipc/state.rs`, `src-tauri/src/main.rs`, `src-tauri/tauri.conf.json`, `src/browser/mod.rs`, `src/tool/browser/mod.rs`, `frontend/src/components/views/BrowserView.tsx`, `frontend/src/hooks/useBrowserRect.ts` (NEW), `frontend/src/hooks/useBrowserOverlay.ts` (NEW), `frontend/src/lib/tauri.ts`, 5 modal components (`AboutDialog`, `MergeToMainDialog`, `SafetyToggleDialog`, `ProjectPicker`, `SettingsDialog`), `.coding/browser-debugging.md`.

**Plan goal:** Replace the Browser tab's `<iframe>` with a native child WebView2 embedded in the "main" Tauri window, so framing-refusing sites (Google/YouTube) load. Human drives it via the URL bar (all builds); agent's `game_*` tools attach via CDP to the child (debug-only).

**Verification limits:** read-only — could not run `cargo test` / `npx tsc`. The main agent must run both before closing. Findings below are from static analysis of the diff + full file reads.

---

## Executive summary

The architecture is sound and the security choke points (`normalize_url`, debug-only CDP, CSP tightening) are correctly preserved. The overlay counter logic is correct (balances under strict-mode, saturating underflow guard works). However there are **two HIGH correctness bugs** that will break the feature in common real-world conditions: a **physical/logical coordinate-system mismatch** that mispositions the child webview on any DPI-scaled display (very common on Windows), and an **`is_app_url` over-match** that breaks CDP target selection when the child navigates to a localhost dev server — a documented primary use case. Both should be fixed before merge.

---

## 1. Correctness

### C1 (HIGH) — Physical-pixel coordinates fed into `LogicalPosition`/`LogicalSize` → child webview mispositioned + mis-sized on any DPI-scaled display

**Files:** `src-tauri/src/ipc/browser_webview.rs:148-149, 160-161, 183-184`; frontend `frontend/src/components/views/BrowserView.tsx` (`openUrl`) + `frontend/src/hooks/useBrowserRect.ts:33-39`.

The frontend correctly computes **physical** pixels (`getBoundingClientRect()` × `devicePixelRatio`) and documents this intent in doc comments ("Coordinates are physical pixels (CSS px × devicePixelRatio)"). But the backend wraps those physical values in **`LogicalPosition` / `LogicalSize`** — which Tauri interprets as logical (CSS) pixels:

```rust
// browser_webview.rs:145-150 (ensure, create path)
let webview = window.add_child(
    builder,
    LogicalPosition::new(x as f64, y as f64),   // x,y are PHYSICAL px
    LogicalSize::new(w as f64, h as f64),          // w,h are PHYSICAL px
)
// browser_webview.rs:160-161 + 183-184 (set_rect / reposition)
let _ = wv.set_position(LogicalPosition::new(x as f64, y as f64));
let _ = wv.set_size(LogicalSize::new(w as f64, h as f64));
```

Tauri converts logical→physical by multiplying by the scale factor `s` (= `devicePixelRatio`). So the webview lands at `physical_x × s = (css_x × s) × s = css_x × s²` physical px — i.e. **offset and size are multiplied by the scale factor a second time**. On a 100% display (`s=1`) it's correct; on 125% (`s=1.25`) the child is 1.25× too far right/down and 1.25× too large; on 150% (`s=1.5`, extremely common on Windows laptops) 1.5× off. The child will overflow the Browser tab area and overlap the URL bar / other panels.

This diverged from the plan, which specified `PhysicalPosition::new(x,y)` / `PhysicalSize::new(w,h)`.

**Fix:** import `tauri::{PhysicalPosition, PhysicalSize}` and use them in `browser_webview_ensure` (create path) and `browser_webview_set_rect` (reposition path). The frontend already sends physical px, so no frontend change is needed. (Alternatively, stop multiplying by `dpr` in the frontend and keep `Logical*` in the backend — but the physical-px contract is already documented and used, so switching the backend to `Physical*` is the smaller change.)

### C2 (HIGH) — `is_app_url` over-matches `http://localhost:<any-port>` → CDP target selection breaks when the child navigates to a localhost dev server

**File:** `src/browser/mod.rs:784-785`

```rust
|| (url.starts_with("http://localhost:")
    && !url.starts_with("http://localhost:9222"))
```

This treats **every** `http://localhost:<port>` (except the CDP port 9222) as the app's own page. The inline comment justifies it with "the child navigates to external sites, never to the dev server" — but that assumption is wrong. The architecture (per the project memory "Interactive browser architecture" + the prior plan 81587c61) explicitly supports the human loading **a game from a separate project's dev server** — e.g. `http://localhost:3000`. The dev devUrl itself is `http://localhost:5179` (`tauri.conf.json:8`).

When the child navigates to `http://localhost:3000`:
1. `select_child_target` (`mod.rs:758-762`) iterates pages. The child's URL `http://localhost:3000` matches `is_app_url` → **skipped** (misclassified as the app page).
2. The real app page (`http://localhost:5179` / `tauri://localhost`) is also an app URL → skipped.
3. The loop finds no non-app page → falls back to `pages.first()` (see C3). `webview_page()` non-deterministically returns the app page or the child depending on enumeration order.

Result: the agent's `game_*` tools either drive the app's own UI or fail to find the child — for exactly the localhost-dev-server use case the feature exists to support.

**Fix:** match the **specific** dev-server port, not all localhost ports. The dev devUrl is `http://localhost:5179`; in release the app is `tauri://localhost` / `http://tauri.localhost`. So:
```rust
|| url.starts_with("http://localhost:5179")  // dev devUrl only
```
(Hardcoding 5179 mirrors the hardcoded `9222` CDP port already in the file. A cleaner option is to read the configured devUrl, but that's a larger change; matching 5179 specifically is the minimal correct fix.) Add a regression test that navigates the child to `http://localhost:<ephemeral>` and asserts `webview_page()` selects it (the existing `webview_page_selects_child_target` test uses `data:` URLs and would not catch this).

### C3 (MEDIUM) — `select_child_target` fallback returns the app page when no child exists → `game_*` can silently operate on the app's own UI

**File:** `src/browser/mod.rs:764-766`

```rust
// Fallback: no non-app page found — return the first (test stand-in
// with a single page that isn't the app URL).
pages.first().cloned()
```

If the human hasn't opened the Browser tab yet (no child webview created), only the app page exists on the CDP port. `select_child_target` finds no non-app page and falls back to `pages.first()` — **the app's own page**. `webview_page()` then returns the app page instead of erroring, so `game_screenshot` / `game_snapshot` / `game_eval` (the AutoRun reads) silently inspect the **app's own UI** rather than a game. `game_eval` running arbitrary JS against the app page is a mild security concern (agent eval'ing the app rather than a game); the bigger issue is correctness confusion (the agent believes it's seeing a game).

The fallback exists only so a single-page test stand-in resolves. But the existing tests (`webview_connect_screenshot_eval_snapshot`, `webview_page_selects_child_target`) navigate to `data:` URLs, which are NOT app URLs, so they're returned directly by the loop — the fallback is never exercised by any test. It's dead weight that opens a safety hole.

**Fix:** remove the fallback — return `None` when no non-app page is found, so `webview_page()` errors with its existing "the child WebView2 has no page target to attach to — is the Browser tab open?" message. The tests still pass (they have a real non-app page).

### C4 (LOW/INFO) — Non-`Dialog` overlays are not wired with `useBrowserOverlay`

The z-order fix wires the 5 `<Dialog>`-based modals (Settings, About, ProjectPicker, MergeToMain, SafetyToggle). But the app has other overlay-type UI (dropdown menus, popovers, tooltips, and notably the **approval/question popups** the agent uses to ask the user). None of these call `useBrowserOverlay`. If any of them renders over the browser rect while the child webview is visible, the native HWND will punch through it (the modal/overlay renders *behind* the child). The docs note only "full-viewport modals" hide the child, but a small approval popover that happens to overlap the browser area would be unreadable.

**Action:** verify these overlays never overlap the Browser tab area (e.g. approval popups render in a fixed corner away from the right panel), or wire them with `useBrowserOverlay` too. Not necessarily blocking if they're positioned outside the browser rect, but needs a conscious check.

---

## 2. Bugs

### B1 (INFO, no leak) — `take_webview` on exit is correct
`src-tauri/src/main.rs:486-497` clones the `browser_webview` Arc, `block_on`s the lock, and calls `take_webview()` which sets `webview = None` → drops the `Webview` handle → wry tears down the child HWND. Mirrors the existing `browser.close()` block_on above it. No handle leak. The `block_on` runs on the main thread (Tauri event handler), not the tokio runtime, so no deadlock. **No finding** — noted only because the review prompt asked.

### B2 (INFO) — Overlay counter balances under React strict-mode; underflow guard is correct
`src-tauri/src/ipc/browser_webview.rs:233-258` + `frontend/src/hooks/useBrowserOverlay.ts`. Traced enter→exit→enter→exit (strict-mode double-invoke): depth 0→1→0→1→0, balanced. The saturating guard (`if prev == 0 { store(0) }`) correctly clamps a stray exit (0 → fetch_sub wraps to `u32::MAX`, then store(0) restores). All `overlay_depth` access is serialized by the async mutex, so the brief `u32::MAX` window between `fetch_sub` and `store` is never observed. `should_show` correctly ANDs `tab_visible` with `overlay_depth == 0`. **No finding.**

### B3 (INFO) — `browser_webview_ensure` redundantly sets `tab_visible = true` on creation
`browser_webview.rs:154`. Harmless (the frontend's mount effect already set it true), and correct in practice since `ensure` is only called from `openUrl` (Browser tab active). Not a bug.

---

## 3. Security

### S1 (PASS) — `normalize_url` is the single choke point for child-webview navigation
Both `browser_webview_ensure` (`browser_webview.rs:129`) and `browser_webview_navigate` (`:200`) call `normalize_url(&url)` before any navigation. `normalize_url` (`src/browser/mod.rs:1151`) enforces the `http`/`https`/`data` allow-list and rejects `file://`/`javascript:`/`about:`/etc. `file://` cannot cross the sandbox boundary; no SSRF via the child webview. The agent's `webview_navigate` (BrowserManager) also calls `normalize_url` (`mod.rs:836`). **No finding.**

### S2 (PASS) — CDP port stays debug-only
`ensure_webview` (`src/browser/mod.rs:660-664`) short-circuits with an error when `!cfg!(debug_assertions)`. The `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` env var that exposes port 9222 is set under `#[cfg(debug_assertions)]` in `main.rs` (pre-existing, confirmed via memory + plan 81587c61). Release builds never expose the CDP endpoint. **No finding.**

### S3 (PASS) — CSP `frame-src` removal is correct + tightens security
`tauri.conf.json:27-28`. The iframe is gone, so `frame-src` is no longer needed. Without it, the CSP falls back to `default-src 'self'` (prod) / `default-src 'self' http://localhost:5179` (dev) — i.e. the app can no longer frame external content at all. The child webview is a native HWND, not subject to the app's CSP. Removing the previously-broad `frame-src ... https://*` is a security improvement. **No finding.**

### S4 (INFO) — `unstable` Tauri feature is acceptable
`src-tauri/Cargo.toml:13`: `tauri = { version = "2", features = ["unstable"] }`. `unstable` gates `Window::add_child` + `WebviewBuilder` — the only API for embedding a child webview. `unstable` is an API-stability flag (the API may change between minor Tauri versions), not a security flag. Acceptable. Recommend pinning the Tauri version (currently `version = "2"` resolves to 2.11.5 per the plan) so a future `cargo update` doesn't break the `add_child` call surface.

---

## 4. Constitution compliance

### Doc comments — PASS
Every public item in `browser_webview.rs` has a doc comment: `CHILD_WEBVIEW_LABEL`, `BrowserWebviewState` (+ fields), `new`, `take_webview`, and all 7 `#[tauri::command]` fns. The public `webview_*` methods in `src/browser/mod.rs` (`webview_eval`/`snapshot`/`navigate`/`click`/`type`) have updated doc comments. The private `select_child_target`/`is_app_url`/`element_center` have doc comments. Frontend `useBrowserRect`/`useBrowserOverlay` + the 6 IPC wrappers have JSDoc. **No finding.**

### Warning-free / no `#[allow]` — PASS (static)
Scanned the diff + new files: no `#[allow(...)]` added anywhere. `#![deny(warnings)]` is at both crate roots (pre-existing). The main agent must run `cargo test` to confirm zero warnings dynamically.

### Regression tests — PARTIAL
- `webview_page_selects_child_target` — target selection (data: URLs). Present. ✓
- `webview_click_and_type_drive_child_webview` — click/type against the child. Present (non-ignored). ✓
- 3 overlay-counter unit tests — balance, underflow, tab-hidden. Present. ✓
- **Gap:** no regression test for C2 (localhost dev-server navigation) — the existing test uses `data:` URLs and would not catch the `is_app_url` over-match. Add one.
- **Gap:** the old `webview_navigate_sets_iframe_src` test included a `file://`-rejection assertion (integration-level); the replacement test dropped it. `normalize_url`'s own unit tests (`normalize_url_rejects_non_urls`) cover scheme rejection at the choke point, so this is INFO, not a gap — but consider keeping an integration assertion for defense-in-depth.

### Line endings — PASS
Git's `LF will be replaced by CRLF` warnings are for `.coding/plans/*` (bookkeeping) and `Cargo.toml` — standard Windows autocrlf normalization, not a source line-ending violation. The 3 new files triggered no such warning.

---

## 5. Style / maintainability (INFO, not constitution-enforced)

### ST1 — Rambling stream-of-consciousness comment in the target-selection test
`src/browser/mod.rs:1969-1977` — the comment block in `webview_page_selects_child_target` reads as discarded design notes ("Actually: navigate the default page to a data: URL titled 'app'... no. The cleanest stand-in..."). It's confusing to future readers and should be replaced with a 2-line explanation of the stand-in setup (about:blank = app stand-in because `is_app_url` returns true for it; second data: page = child stand-in).

---

## Summary

| Severity | Count | Blocking? |
|---|---|---|
| Correctness HIGH (C1 coords, C2 localhost) | 2 | **Yes** — fix before merge |
| Correctness MEDIUM (C3 fallback) | 1 | Yes — small fix, removes a safety hole |
| Correctness LOW (C4 non-Dialog overlays) | 1 | Verify positioning |
| Bugs (all INFO / no-action) | 3 | No |
| Security | 0 (all PASS) | — |
| Constitution | 0 (doc comments ✓, no #[allow], tests partial) | — |
| Style INFO | 1 | No |

**Bottom line:** Fix **C1** (use `PhysicalPosition`/`PhysicalSize` — the frontend already sends physical px), **C2** (match the specific dev port 5179, not all localhost ports, + add a localhost regression test), and **C3** (drop the `pages.first()` fallback so `game_*` errors instead of hitting the app page). Then run `cargo test` + `npx tsc --noEmit` + `npx vitest run` before committing. The security posture (normalize_url choke point, debug-only CDP, tightened CSP) is clean.
