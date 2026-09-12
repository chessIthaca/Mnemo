# Review: Unified Browser tab (interactive iframe + agent CDP control)

**Scope:** All uncommitted changes (12 modified + 1 deleted + 1 new plan). Goal:
unify Browser + Game tabs into one interactive "Browser" tab (iframe the human
drives), fix the `google.com` URL-normalization bug, and give the agent
navigate/click/type control of that iframe via CDP attach. Keep headless Chromium
as an agent-only surface.

**Verdict:** The URL-normalization fix and the tab unification are correct and
well-tested. However, `webview_click` / `webview_type` have a **high-severity
coordinate-system bug** that makes them miss their targets in the real app
(the `#[ignore]`d tests mask it). Two medium issues (no reliable test for
click/type, behavioral inconsistency in `game_type`) and a few minor items
follow. Security and constitution compliance are largely clean.

---

## HIGH — correctness / bugs

### H1. Coordinate-system bug: `webview_click` / `webview_type` dispatch mouse events at iframe-local coordinates, not viewport coordinates

**Files:** `src/browser/mod.rs:832` (rect_expr), `:858-869` (mousePressed),
`:901` (focus_expr), `:930-941` (mousePressed in webview_type)

`getBoundingClientRect()` called **inside the iframe's execution context**
returns coordinates relative to the **iframe's viewport**, not the browser
(top-level page) viewport. But CDP `Input.dispatchMouseEvent` `x`/`y` are
"relative to the client area of the browser viewport" — i.e. page-level
coordinates. The code passes the iframe-local rect center directly to
`DispatchMouseEventParams::builder().x(x).y(y)` with **no offset added**:

```rust
// webview_click, src/browser/mod.rs:831-832
let rect_expr = format!(
    "(function(){{var el=document.querySelector({sel_json});...var r=el.getBoundingClientRect();return JSON.stringify({{x:r.x+r.width/2,y:r.y+r.height/2}});}})()"
);
// ...
DispatchMouseEventParams::builder().x(x).y(y)  // :859-861 — x,y are iframe-local!
```

In the real app the Browser tab's iframe is offset from (0,0) by the URL bar,
padding, borders, and the right panel's position (100+ px). So the dispatched
click lands at (element_iframe_x, element_iframe_y) in the **page** — which is
NOT on the target element (it's above/left of the iframe). The click hits
whatever main-frame element happens to be at those page coordinates, or empty
space. `webview_type` has the identical bug (it dispatches mousePressed at the
same iframe-local center to focus, then `InsertText`).

**Why the tests don't catch it:** The only tests exercising click/type are
`webview_frame_and_input_cdp_surface` (src/browser/mod.rs:1742) and
`webview_click_and_type_drive_iframe` (src/browser/mod.rs:2099), both
`#[ignore]`d. Both set up the iframe at the top-left of a bare `data:` page
(default body margin ~8px), and the target buttons are large enough that the
~8px offset error still lands inside the button. In the real app the offset is
100+px, so the click misses entirely. The ignored tests passing in isolation is
consistent with the bug being present but masked.

**Fix:** Add the iframe's viewport offset to the element's iframe-local rect.
Get the iframe element's `getBoundingClientRect()` in the **main frame** (a
main-frame eval: `document.querySelector('iframe').getBoundingClientRect()`),
then add `(iframe_rect.x, iframe_rect.y)` to the element's iframe-local center
before dispatching. Alternatively, use CDP `DOM.getBoxModel` on a resolved node
(which returns viewport coordinates directly). After fixing, add a non-ignored
regression test with the iframe at a non-zero offset (see M2).

---

## MEDIUM — bugs / constitution

### M1. `webview_type` clears `el.value=''` unconditionally — behavioral inconsistency with `browser_type` + no-op on non-inputs

**File:** `src/browser/mod.rs:901`

```rust
let focus_expr = format!(
    "(function(){{var el=document.querySelector({sel_json});...el.value='';el.focus();...}})()"
);
```

Two issues:
1. **Inconsistency:** The headless `browser_type` (src/browser/mod.rs:508-521,
   `type_text`) clicks to focus then calls `el.type_str(text)` which **appends**
   at the caret (no clear). `game_type` **replaces** (clears first). The same
   conceptual operation has opposite semantics across the two tool families,
   which can confuse the agent (it can't predict whether typing appends or
   replaces). Pick one semantics; if replace is intended for the live tab,
   document it in the schema.
2. **Non-input elements:** `el.value=''` is a no-op on `contenteditable` /
   `<button>` / `<div>` (sets a meaningless property). The schema says "input to
   type into," so this is low-impact, but `InsertText` into a `contenteditable`
   after `.focus()` has browser-dependent caret behavior. Consider guarding
   `el.value=''` behind an `el.tagName` check, or dropping it (let the agent
   clear explicitly via `game_eval` if needed).

### M2. `webview_click` / `webview_type` have no reliable (non-ignored) regression test

**Files:** `src/browser/mod.rs:1742`, `:2099` (both `#[ignore]`d)

The constitution requires regression tests for new functionality. The only
tests for click/type are the two `#[ignore]`d integration tests. The
non-ignored `webview_navigate_sets_iframe_src` (src/browser/mod.rs:1975) covers
navigate + scheme rejection, but **nothing in CI exercises click or type**.
Combined with H1, this means the coordinate bug could ship undetected. Add a
non-ignored test — even a unit-level one — that would fail with the current
coordinate bug (e.g., place the iframe at a known non-zero offset and assert the
dispatched coordinates equal iframe_offset + element_center, or assert the
click lands on the right element when the iframe is offset).

### M3. Orphaned headless-browser console UI infrastructure (dead code)

**Files:** `src-tauri/src/main.rs:184,267,330` (`spawn_console_forwarder` calls),
`:433-436` (registered IPC commands), `frontend/src/lib/tauri.ts:1020-1035`
(TS wrappers `browserOpen`, `browserPages`, `browserScreenshotLatest`,
`browserConsole`, `onBrowserConsole`)

The new BrowserView (frontend/src/components/views/BrowserView.tsx) only uses
`browserNormalizeUrl`. The old headless-browser UI surface — the console
forwarder (pushes `browser://console` events that nothing listens to anymore),
the `browser_pages` / `browser_screenshot_latest` / `browser_console` / `browser_open`
IPC commands, and their TS wrappers — is now UI-orphaned. The agent's
`browser_*` tools still drive the headless browser (correct, per the plan), but
the UI no longer surfaces its pages/screenshots/console. This is acceptable per
the plan ("headless is agent-only now"), but the orphaned forwarder + IPC
commands + TS wrappers are dead UI code. Either remove them (the agent tools
call `BrowserManager` directly, not via IPC) or add a brief comment that they're
agent-only / pending a future headless debug panel. Not a build warning (all are
still `pub`/registered/used-by-agent-tools), just technical debt.

---

## LOW — minor

### L1. Duplicate doc comment on `normalize_url`

**File:** `src/browser/mod.rs:1166-1181`

The old doc block (lines 1166-1174, "Normalize a navigation input to a full
URL…") was left in place when the new doc block (lines 1175-1181, "Normalize a
URL input…") was added below it. Two stacked `///` blocks precede the `pub fn`.
Rust allows it (no warning), but it's redundant — merge into one.

### L2. `iframe_context` relies on `frames.get(1)` being the iframe

**File:** `src/browser/mod.rs:974-976`

```rust
let iframe_frame = frames
    .get(1)
    .ok_or_else(|| Error::Browser("the Browser tab has no iframe loaded — call game_navigate first".into()))?;
```

`page.frames()` returns frames in tree/document order, so frames[0] is the main
frame and frames[1] is the first child. This works for the current app (one
iframe — BrowserView's). But if the app ever renders a second iframe (e.g., in
another right-panel view), frames[1] could be the wrong frame, and click/type
would operate on the wrong iframe's DOM. Consider filtering by parent-frame-id
(== main frame) or matching the Browser tab's iframe specifically. Low because
the app currently has exactly one iframe.

### L3. `webview_navigate` doesn't wait for the iframe load

**File:** `src/browser/mod.rs:788-813`

`webview_navigate` sets `f.src = ...` and returns immediately (the eval returns
`f.src`, not a load-completed signal). A subsequent `game_click` / `game_type`
may race the navigation — `iframe_context` would return the old context or
error ("the iframe has no execution context yet"). Low: the agent can poll via
`game_screenshot` / `game_snapshot` before clicking, and the error surfaces
cleanly (no panic). Consider awaiting a load event or documenting that the agent
should poll.

### L4. Loose assertion in `webview_navigate_sets_iframe_src`

**File:** `src/browser/mod.rs:2055-2058`

```rust
assert!(
    src.contains("loaded") || src.contains("data:"),
    "iframe src should reflect the navigation, got: {src}"
);
```

The `|| src.contains("data:")` arm would pass for any `data:` URL regardless of
whether it's the navigated target. Tighten to `src == target` or
`src.contains("marker")` to actually assert the navigation landed.

---

## SECURITY — clean (with notes)

- **`game_navigate` cannot bypass the scheme allow-list.** `webview_navigate`
  (src/browser/mod.rs:789) calls `normalize_url(url)?` **before** any CDP
  interaction, so `file://` / `javascript:` / `about:` are rejected at the same
  choke point as `browser_navigate` and the IPC `browser_normalize_url`. ✓
- **`game_click` / `game_type` resolve selectors in the iframe's context**
  (via `iframe_context` + `context_id`), so a selector can only match elements
  inside the iframe, not the app's main frame. The mouse dispatch is at
  page-level coordinates — the H1 coordinate bug could cause a click to land on
  a main-frame element, but that's a correctness defect, not an intentional
  escape vector. ✓ (modulo H1 fix)
- **CDP port is debug-only (double-gated).** `main.rs:42-46` sets
  `--remote-debugging-port=9222` under `#[cfg(debug_assertions)]` only, and
  `ensure_webview` (src/browser/mod.rs:660-664) fails fast in release builds.
  Release builds never expose the port. ✓
- **iframe sandbox** `allow-scripts allow-same-origin allow-forms allow-popups
  allow-pointer-lock` (BrowserView.tsx:74): `allow-scripts` + `allow-same-origin`
  together is a known CSP caveat, but for the allowed URL schemes (http/https/
  data) the iframe is cross-origin to the Tauri app (`tauri://localhost`), so it
  cannot access `parent.document`. `data:` URLs have an opaque origin (no parent
  access). This is pre-existing (same as the deleted GameView, plus
  `allow-pointer-lock`) and necessary for games (WebGL/storage). ✓ Acceptable.
- **`game_eval` can bypass the allow-list** by setting `iframe.src` directly or
  `window.location.href`, but it is `NeedsApproval` and the existing doc comment
  (src/tool/browser/mod.rs:785-791) already documents this. Not a regression. ✓

---

## CONSTITUTION COMPLIANCE — clean (with notes)

- **Doc comments on all pub fns:** `normalize_url`, `webview_navigate`,
  `webview_click`, `webview_type`, `iframe_context` (private but documented),
  `browser_normalize_url` (IPC), `browserNormalizeUrl` (TS), and the three new
  tool structs all have doc comments. ✓ (minor: L1 duplicate on `normalize_url`)
- **Warning-free build:** No unused imports (Gamepad2/GameView removed cleanly),
  no unused `mut`, no dead-code warnings (orphaned IPC commands are still
  `pub`/registered). The `#[ignore]`d tests are still compiled, and their
  in-body `use` statements (`DispatchMouseEventParams`, `EvaluateParams`, etc.)
  are all used — no hidden warnings. Under `#![deny(warnings)]` a green
  `cargo test` proves zero warnings. ✓
- **Regression tests for the URL fix + navigate:**
  - URL autodetection: `normalize_url_autodetects_bare_hostnames`
    (src/browser/mod.rs:1386) covers `google.com` → `https://google.com` etc. ✓
  - Navigate: `webview_navigate_sets_iframe_src` (src/browser/mod.rs:1975, not
    ignored) covers iframe-src landing + `file://` rejection. ✓
  - **Click/type: NO non-ignored regression test** (see M2). ✗ — must add one.
- **Per-defect regression test rule:** The coordinate bug (H1), once fixed,
  needs a regression test that fails without the fix (see M2).

---

## Summary of required actions before merge

1. **H1 (high):** Fix the coordinate-system bug in `webview_click` /
   `webview_type` — add the iframe's viewport offset to the element's
   iframe-local rect before dispatching mouse events.
2. **M2 (medium):** Add a non-ignored regression test for click/type that would
   catch H1 (iframe at a non-zero offset).
3. **M1 (medium):** Resolve the `game_type` clear-vs-append inconsistency with
   `browser_type` (document or align semantics).
4. **M3 (medium):** Remove or document the orphaned headless-console UI
   infrastructure.
5. **L1-L4 (low):** Merge duplicate doc, harden `iframe_context` frame
   selection, tighten the navigate test assertion, optionally await iframe load.

Security and the core URL-normalization fix are sound; the tab unification is
clean. The blocker is H1 — without it, 2 of the 3 new control tools
(click/type) do not work in the real app.
