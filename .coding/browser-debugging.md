# Browser debugging

Mnemo has **two** browser surfaces, both driven over the Chrome DevTools
Protocol (CDP):

1. **The Browser tab** (right panel) — a **native child WebView2** embedded in
   the app's "main" window (replaces the old iframe). The human drives it
   directly (clicks, types, plays) via the URL bar; the agent inspects and
   controls this *same* child webview via the `browser_*` tools, which attach
   to it over the shared CDP debug port. A child WebView2 is a separate
   OS-level instance — it is never "framed," so sites that refuse framing via
   `X-Frame-Options` / CSP `frame-ancestors` (Google, YouTube) load normally.
   This is the surface for building a game together (human plays) + debugging
   it (agent inspects live state). Works in **debug builds only** for the
   agent's `browser_*` tools (the CDP port is dev-only); the human's URL bar
   works in **all builds**.

2. **The headless Chromium** (agent-only, no UI tab) — a separate headless
   browser the agent drives autonomously via the `offscreen_browser_*` tools.
   Used for overnight/automated browsing where no human is watching. Works in
   **all builds** (it spawns its own Chromium process).

## The Browser tab (native child WebView2)

Enter a URL in the address bar (e.g. `google.com` — `https://` is added
automatically) and the child webview loads it directly. The child webview is a
separate HWND composited above the app's HTML, positioned over the Browser
tab's area; it shares the WebView2's GPU/WebGPU, so games render natively. The
human can click, type, and play; the agent inspects and controls the *same*
child webview via the `browser_*` tools.

> **Z-order note:** because the child webview is a native HWND rendered above
> the app's HTML, full-viewport modals (Settings, About, the project picker,
> the merge confirm, the safety toggle) hide the child webview while they're
> open so the modal renders on top. The child reappears when the modal closes.

### The `browser_*` tools (drive the live Browser tab)

These attach to the child webview via CDP (selecting the child's page target,
not the app's main page) and operate on its main frame. Reads are auto-run;
mutations need approval.

| Tool | Safety | What it does |
|------|--------|--------------|
| `browser_screenshot` | auto | PNG of the live child webview → `.coding/browser/screenshots/browser-<ts>.png`, returns the path. |
| `browser_snapshot` | auto | Text dump of the live child webview's DOM (cheap "see the page"). |
| `browser_eval` | approval | Run arbitrary JavaScript against the live child webview, return the JSON value. |
| `browser_navigate` | approval | Navigate the child webview to a URL (normalized: `google.com` → `https://google.com`; scheme allow-list enforced — `http`, `https`, `data`, `file`). When no child webview exists yet (tab toggled on but never navigated), it creates the child webview + reveals the Browser tab itself — the one tool that can open the tab. |
| `browser_click` | approval | Click an element by CSS selector in the child webview (real CDP mousePressed + mouseReleased at its center — no iframe offset). |
| `browser_type` | approval | Focus an element by CSS selector in the child webview and type text into it (CDP `Input.insertText`). |

### The vision loop (see the page)

```
browser_navigate  { "url": "http://localhost:3000" }
browser_screenshot {}                          # → .coding/browser/screenshots/browser-<ts>.png
describe_image { "path": ".coding/browser/screenshots/browser-<ts>.png",
                 "question": "What is the score?" }
```

### Example: debug a web game

```
browser_navigate { "url": "http://localhost:3000" }
browser_snapshot {}                            # find the UI selectors
browser_click   { "selector": "#start-button" }
browser_eval     { "expression": "window.__score" }   # read live game state
browser_screenshot {}                          # what does it look like now?
```

## The headless browser (agent-only)

A separate headless Chromium the agent drives autonomously via the
`offscreen_browser_*` tools. No UI tab — the agent is the only driver. Use it
for automated browsing where no human is watching.

### The `offscreen_browser_*` tools (drive the headless browser)

All take an optional `page_id` (default: the **active** page). Reads are
auto-run; mutations need approval.

| Tool | Safety | What it does |
|------|--------|--------------|
| `offscreen_browser_navigate` | approval | Open a URL in a new page; returns `page_id` + title. |
| `offscreen_browser_list_pages` | auto | List open pages (`id`, `url`, `title`, active). |
| `offscreen_browser_switch_page` | approval | Make a page the active page. |
| `offscreen_browser_close_page` | approval | Close a page. |
| `offscreen_browser_screenshot` | auto | PNG → `.coding/browser/screenshots/<page>-<ts>.png`, returns the path. |
| `offscreen_browser_snapshot` | auto | Text dump of the page DOM (cheap "see the page"). |
| `offscreen_browser_console` | auto | Drain buffered console messages + JS exceptions. |
| `offscreen_browser_eval` | approval | Run arbitrary JavaScript, return the JSON value. |
| `offscreen_browser_click` | approval | Click an element by CSS selector. |
| `offscreen_browser_type` | approval | Focus an element and type text into it. |

## Notes & limits

- **URL scheme allow-list.** Both `offscreen_browser_navigate` and
  `browser_navigate` (and the Browser tab's URL bar) accept `http`, `https`,
  `data`, and `file` URLs — `file://` is on the list deliberately so local
  HTML files can be debugged in either browser (approval-gated navigation
  renders the page the URL names). `javascript:`, `about:`, and other schemes
  are rejected at the single choke point (`normalize_url` in
  `BrowserManager`), so exotic schemes can't drive SSRF through the browser.
  Scheme-less hostnames (`google.com`) get an omnibox-style scheme
  (`https://`; `http://` for localhost/IPs).
- **The live `browser_*` tools require a debug build OR the opt-in setting** —
  the CDP port that lets the agent attach to the child webview is dev-only by
  default (set in `main.rs` under `cfg(debug_assertions)`). Release builds
  expose it only when you enable **Settings → Advanced → "Agent browser
  inspection"** and restart (the env var is read at WebView2 creation
  time). The setting exposes an **unauthenticated** localhost port (9222 by
  default — each instance takes the next free port when several run side by
  side) — any local process can inspect the browser tab / run arbitrary
  JS in the app's webview — so it is off by default; enable it only on a
  machine you fully control. The human's URL bar works in all builds (the
  child webview is created via the Tauri Webview API, not CDP). The
  `offscreen_browser_*` (headless) tools work in all builds.
- **`browser_eval` / `offscreen_browser_eval` await promises** (`awaitPromise`
  + `return_by_value`), so `Promise.resolve(42)` yields `42`, not a promise
  handle.
- **No same-origin limit on the live `browser_*` tools:** the child webview IS
  the top-level page (not an iframe in the app's DOM), so `browser_eval` /
  `browser_click` / `browser_type` operate on the child's main frame directly —
  no iframe offset, no same-origin restriction.
- **Throwaway profile.** Each headless browser spawn uses a fresh temp
  Chromium profile (no cookies/history persist), removed on shutdown; stale
  profiles left by a crash are swept at the next launch.
- **The CDP debug endpoint** is an unauthenticated localhost WebSocket. Any
  local process could attach to the debug browser; the throwaway profile keeps
  the exposure to what that page can see.
