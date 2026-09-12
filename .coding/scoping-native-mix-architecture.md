# Scoping memo: MIX architecture — native core + webview subcomponents

**Date:** 2026-12  
**Status:** Research finding (no source changes). Gates the next implementation plan.

## 1. The reframe (the key insight)

Prior scoping assumed "the game browser must be the *sole* WebView2, so all app UI must be native." **That was over-constrained.** Only the **game browser** needs CDP; the **agent chat** does not. The agent chat can be a *second* WebView2 that renders React perfectly without CDP.

This is not a guess — it is the **confirmed behavior of the shipped child webview**: a second WebView2 environment renders fine (google.com loads), it just gets no CDP target (its 9222 bind fails silently because the first env already holds the port). "Renders, no CDP" is exactly acceptable for the agent chat.

So the architecture is a **MIX**: a native core (window + layout + webview hosting + creation-order control) with two webview subcomponents:
- **Game browser** — WebView2 created *first* → grabs 9222 → CDP for agent `game_*` tools.
- **Agent chat** — WebView2 created *second* → renders React, no CDP (acceptable).

This is **none of the exhausted paths**:
- Not browser-in-browser (two sibling child webviews in one window, not nested).
- Not env-share (doesn't compile — ICoreWebView2Environment is !Send across add_child's closure).
- Not a second debug port (Spike B: crashes the UI).
- Not a separate OS window (user-rejected: sync/overlay problems).

## 2. The creation-order constraint (confirmed from source)

- `src-tauri/src/main.rs:60-65` sets `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` (port 9222) **process-global**, before the Tauri builder.
- The **first** WebView2 environment created in the process binds 9222 → gets CDP.
- A **second** env's 9222 bind fails silently, but the env still creates + the webview renders (proven by the shipped child).
- `Window::add_child` (tauri-2.x `src/window/mod.rs:1129`) calls `webview_builder.build()`, which creates a **fresh** env per child (wry reuses the parent env only when `pl_attrs.environment` is `Some`, which `add_child` does not set).

**Conclusion:** the native core's only real job is **creation order** — create the game-browser webview first, the agent-chat webview second.

## 3. The make-or-break unknown (two paths)

### Path (a) — WITHIN Tauri (near-zero port) — UNPROVEN, test first
A bare Tauri `Window` (created via `WindowBuilder`, which carries no webview of its own) hosts two `add_child` webviews in order:
1. `add_child(game-browser)` → first env → binds 9222 → CDP.
2. `add_child(agent-chat, WebviewUrl::App("index.html"))` → second env → renders React, no CDP.

- **What stays:** the entire React frontend, the entire IPC layer (Tauri invoke/listen works per-webview, including child webviews), the entire `myharness` brain (lib crate), the `browser_webview.rs` lifecycle module (already does add_child + rect + overlay-depth).
- **What changes:** `main.rs` window model — swap `WebviewWindowBuilder` (window+webview combo) for `WindowBuilder` (bare window) + two ordered `add_child` calls; the agent-chat webview loads the existing React bundle; the game-browser webview is the existing child.
- **Effort:** S (reconfigure + one spike to prove creation order).
- **Risk:** unproven that a bare Tauri Window (a) lets the first add_child grab 9222 with no env created for the window itself, and (b) supports IPC inside an add_child webview. Both are plausible (add_child webviews are full webviews) but never run.

### Path (b) — NON-Tauri native shell (bigger) — fallback if (a) is NO-GO
A native shell (egui / iced / raw tao+wry) with explicit WebView2 creation order.
- `iced_webview` is **NO-GO** (Ultralight/WebKit, no CDP — different engine, not a native HWND host). But iced/egui hosting a native child HWND via wry directly is untested.
- **raw tao+wry** is the proven-composable base (Tauri is built on it) — full control over creation order, but the shell must reimplement window management, event loop, and layout.
- **What changes:** the entire `src-tauri` shell is replaced; IPC (invoke/listen) is replaced by direct Rust API calls into the brain (a simplification — eliminates the 83-command serialization layer); layout/positioning of two child webviews is native.
- **Effort:** M/L.

## 4. UI inventory — what stays React vs goes native

In the MIX model, **the entire current frontend stays React** (it IS the agent-chat webview). Only the window shell + webview hosting changes.

| Component | LOC | Stays React? | Notes |
|---|---|---|---|
| Sidebar (tool toggles) | 105 | ✅ | |
| MainPanel (agent tabs + Conversation) | 213 | ✅ | |
| RightPanel (10 tool tabs) | 129 | ✅ | |
| StatusBar (model picker, safety, merge) | 830 | ✅ | |
| InputBar (slash, history, images, steer) | 776 | ✅ | |
| Conversation + Message + Markdown | 487+126 | ✅ | The hard-to-port-natively stuff STAYS |
| InflightBar (reasoning, tokens) | 365 | ✅ | |
| DiffViewer / DiffView | 288+ | ✅ | Code highlighting stays React |
| LlmTraceView | 820 | ✅ | |
| BacklogView | 734 | ✅ | |
| PlanProgress | 280 | ✅ | |
| StatsView | 463 | ✅ | |
| FileBrowser | 167 | ✅ | |
| MemoryDebugView | 457 | ✅ | |
| MdViewer | 166 | ✅ | |
| Settings dialog (multi-section) | large | ✅ | |
| ProjectPicker | 276 | ✅ | |
| **BrowserView** | 124 | ⚠️ minimal | Becomes a placeholder signaling the native core to show/position the game-browser sibling (already what `browser_webview_set_tab_visible` + `set_rect` do) |

**Goes native (path a):** window model reconfigure only.  
**Goes native (path b):** window mgmt, event loop, two-webview layout/positioning, Z-order/overlay between siblings.

The porting risk that dominated the all-native plan (Markdown, code highlighting, streaming perf, diff viewer) **vanishes** — all of it stays React.

## 5. IPC + brain (unchanged in path a)

- **83 Tauri commands** across 15 modules; **79 typed wrappers** in `tauri.ts` (1170 lines); 929-line Zustand store + `agentEventReducer`; ~20 event types.
- Path (a): IPC layer **stays** (Tauri invoke/listen works in child webviews).
- Path (b): IPC is **replaced** by direct Rust API calls (eliminates the serialization layer — a simplification).
- The `myharness` lib crate (brain: config, provider, tools, workflow, agent loop, memory, safety, sandbox) is **untouched either way**.

## 6. Effort estimate

| Path | Effort | What it buys |
|---|---|---|
| (a) Tauri bare-window ordered-children | **S** (reconfigure + 1 spike) | Near-zero port; keeps React + IPC + brain |
| (b) Non-Tauri native shell | **M/L** | Full control; eliminates IPC; bigger build |

## 7. Phased plan

1. **Spike path (a) first** — it is NOT browser-in-browser (it's creation-order within one window, distinct from all exhausted paths) and it's the smallest possible change. Throwaway spike: bare `WindowBuilder` + two ordered `add_child` (game browser first with 9222, agent chat second). Probe: does 9222 show the game-browser target? Does the agent-chat webview render React + does IPC work inside it?
2. **If (a) GO** → minimal reconfigure of `main.rs` window model; keep everything else. Ship.
3. **If (a) NO-GO** → path (b): native shell (tao+wry recommended as the proven-composable base). Bigger effort but the brain + React frontend are reused as-is (React becomes the agent-chat webview's content).

## 8. Risks + open questions

- **The creation-order spike is the gate.** Path (a) is architecturally sound (add_child creates a fresh env per child; first env binds 9222) but never run. If a bare Tauri Window creates its own env implicitly, the ordering breaks — must verify.
- **Two-webview Z-order/layout:** the overlay-depth counter pattern in `browser_webview.rs` already solves one-child+HTML; two-children+tiled needs the same show/hide/position logic generalized.
- **IPC in add_child webviews:** Tauri invoke/listen must work inside the agent-chat child webview (it should — child webviews are full webviews with the Tauri runtime injected — but unproven in this config).
- **Security win (either path):** today CDP attaches to 9222 = the *app's* webview (agent can read the whole React DOM: API keys, conversation state). Scoping CDP to just the game browser is a real improvement.
