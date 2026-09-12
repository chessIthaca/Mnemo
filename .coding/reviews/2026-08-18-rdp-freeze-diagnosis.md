# RDP desktop-switch freeze — diagnosis — 2026-08-18

**Symptoms (user-reported, 2026-08-18):**
1. The app froze again. New context: this is a **Remote Desktop** setup and the user switches back and forth between desktops frequently.
2. When switching between apps, the app UI visibly **"catches up real fast"** — as if too many events were buffered while away.

**Method:** static audit at HEAD — a thread map of every WebView2 controller call, a frontend poll/event-buffer inventory — plus upstream issue research (MicrosoftEdge/WebView2Feedback). The 2026-04-19 freeze findings (F1/F3) and the 2026-06-14 N-items were re-verified **fixed at HEAD**; this is a NEW mechanism, not a regression of the old ones.

---

## Mechanism primer: what an RDP desktop switch does to a WebView2 app

- **Switching away** (minimizing the RDP client, covering the window, or switching to another client-local desktop): Chromium's native occlusion tracking treats the window as occluded → the compositor stops producing frames → `requestAnimationFrame` stops firing and timers throttle. WebView2 does **not** fire `visibilitychange` when minimized/covered (upstream #4879, open), so the page cannot even detect this state.
- **Controller calls block on a stalled compositor:** `set_size`/`set_position`/`show`/`hide` map to synchronous WebView2 controller calls (`put_Bounds`/`put_IsVisible`) that RPC into the browser process. When the compositor is wedged, these can block the calling thread (upstream #3581: RDP + foreground WebView2 → GUI freeze; killing `msedgewebview2.exe` unfreezes the app).
- **Switching back:** a resize/DPI storm hits the window (session geometry changes), and everything that buffered while occluded (queued IPC, rAF work) flushes in one burst.

---

## Ranked hypotheses

### R1 — HIGH — Main-thread `set_size` in the window-resize handler blocks on a stalled compositor → hard freeze

**Where:** `src-tauri/src/main.rs:95-103` — `window.on_window_event` matches `Resized` and calls `wv.set_size(*size)` **synchronously on the main event loop** (the agent-chat webview).

**Mechanism:** an RDP session/desktop switch delivers a WM_SIZE storm (remote geometry/DPI changes). If any `put_Bounds` blocks on the wedged compositor (the #3581 mechanism), the **entire event loop stops** — no input, no repaint, "Not Responding". No lock is involved; the block is the synchronous COM/RPC call itself. The `Webview` handle is documented `Send + Sync` wrapping an OS HWND (`src-tauri/src/ipc/browser_webview.rs:22-24`) — controller calls execute synchronously on whatever thread makes them.

**Why this fits best:** it is the **only** WebView2 controller call on the event loop, it fires exactly on the trigger the user described (desktop switching), and it produces a *hard* freeze rather than jank.

### R2 — HIGH — Occluded-renderer event buffering → one giant burst flush on switch-back ("catching up real fast")

**Where:** `frontend/src/hooks/useAgentEvents.ts:136-181` — `text_delta` / `reasoning_delta` / `tool_call_arg_delta` events buffer into per-agent `Map`s (unbounded string append) and flush via `requestAnimationFrame` (`scheduleFlush`, :178-181).

**Mechanism:** while the window is occluded, rAF never fires (and no `visibilitychange` fires to compensate — #4879), but Tauri agent events keep arriving because the agent keeps running. Deltas accumulate; structural events (`tool_call`, `step_completed`, …) additionally flush + dispatch to the store **synchronously while hidden** (:224-228, :278), so React keeps re-rendering an invisible page. On switch-back, the one pending rAF fires and pushes the **entire accumulated backlog** into the store at once, and the backlog of structural store updates paints in the same burst → the observed "UI catching up real fast". Transcript caps (1000 entries/300 activity, F3) keep this bounded — it is jank, not death — but it compounds R1's freeze window.

### R3 — MEDIUM — Unthrottled `ResizeObserver` → `set_rect` burst; synchronous controller calls while holding the browser_webview mutex

**Where:** `frontend/src/hooks/useBrowserRect.ts:25-57` — every `ResizeObserver` tick and every window `resize` fires a `browserWebviewSetRect` IPC, **no throttle/debounce**. Backend `src-tauri/src/ipc/browser_webview.rs:173-189` (`set_rect`) and `:156-165` (`ensure`) call `set_position`/`set_size` synchronously on a tokio worker **while holding the async `browser_webview` mutex**; `apply_visibility` (:79-89) does the same for `show`/`hide`.

**Mechanism:** an RDP switch changes DPR/geometry → rect-report burst → a stalled controller call parks a tokio worker **and holds the mutex**, so all Browser-tab IPC (ensure/navigate/show/hide) queues behind it. Only in play once the Browser tab child webview has been created in the session.

### R4 — MEDIUM — Poll fleet keeps firing while hidden (throttled), adding switch-back catch-up work

**Inventory (all cleaned up on unmount; none focus-gated except the git poll):** trace list 1.5 s (`LlmTraceView.tsx:619`); trace detail 1.5 s, **gated on `is_complete`** — N1 fix verified at HEAD (`:626-634`); plan 2 s (`PlanProgress.tsx:86`); embedder status 10 s (`App.tsx:297`); git branch 60 s + window-focus refresh (`App.tsx:325-326`) → backend `spawn_blocking` (`files.rs:228-234`, the F1 pattern — cheap); GraphView poll only while indexing; MemoryDebugView poll only while its tab is mounted. **No `visibilitychange`/`document.hidden` handlers exist anywhere in the frontend.** Hidden-page timer throttling (≈1/s) keeps these as background load, not a freeze vector — listed for completeness.

### R5 — LOW — Exit-path `block_on` teardown can hang shutdown when the compositor is wedged

**Where:** `src-tauri/src/main.rs:538-582` — `RunEvent::Exit` runs `block_on(browser.close())` and `block_on(browser_webview.lock() → take_webview())` (drops the controller). A wedged WebView2 at exit = an app that won't close. Shutdown-only; low priority.

---

## Upstream evidence (all MicrosoftEdge/WebView2Feedback)

| Issue | Status | Relevance |
|-------|--------|-----------|
| #3581 "Windows GUI freeze" | closed 2023-07 | **Direct match:** accessing the host via Remote Desktop freezes the app UI with WebView2 in the foreground; killing `msedgewebview2.exe` or removing WebView2 eliminates the freeze. |
| #4879 "visibilitychange doesn't fire when parent window is minimized or covered" | open | The page **cannot detect occlusion** — rAF throttles silently. This is R2's buffering mechanism. |
| #3820 "WinUI3 with WebView2 on a secondary window crash in remote desktop session" | open | RDP-session instability, unfixed. |
| #1115 "WebView2 rendering issues/artifacts over remote desktop/HP iLO" | open since 2021, priority-low | RDP rendering problems are a long-standing won't-fix-soon class. |
| #413 "When WebView2 App is minimized, it is getting disconnected… or suspended?" | closed 2021 | Minimized-app throttling/suspension behavior. |

---

## User repro checklist (to confirm the mechanism)

1. **Freeze timing:** does it wedge when switching *away*, or when switching *back*? (away → R1 stall; back → R1 resize storm + R2 burst)
2. Does the freeze still happen in a session where the **Browser tab was never opened**? (if it freezes anyway → R1 alone suffices, since the agent-chat webview takes the `set_size` regardless; if it only freezes after the Browser tab existed → R3 implicated too)
3. Does it **unfreeze by itself** after ~10–60 s, or stay wedged? (compositor resume vs. true deadlock)
4. Minimizing the RDP client window vs. switching to another client-local app — same result?
5. During a freeze: Task Manager → kill **one** `msedgewebview2.exe` process (per #3581) → does the app unfreeze? (confirms compositor wedging)

---

## Fix plan (cheapest / highest-leverage first; independently shippable)

| # | Hypothesis | Fix | Size | Regression test |
|---|-----------|-----|------|-----------------|
| 1 | **R1** | Move the agent-chat resize off the event loop: replace the direct `set_size` in `on_window_event` with a **latest-wins coalescing queue** drained by a dedicated `std::thread` (the handle is `Send + Sync`; `browser_webview.rs` already calls `set_size` from non-UI threads). A stalled `put_Bounds` parks that thread, never the event loop; intermediate resizes coalesce. | ~40 LOC | Unit-test the coalescing queue (latest-wins, final size never lost) — pure logic, no window needed. |
| 2 | **R2** | **Bound the rAF delta buffers** in `useAgentEvents.ts`: when a per-agent buffer exceeds ~64 KiB, flush synchronously instead of waiting for the frame. Converts the switch-back mega-flush into bounded incremental flushes; also fixes non-RDP occlusion (covered window). | ~20 LOC | Dispatch-level test of the threshold flush (extract the threshold decision into a pure helper if the frontend lacks test infra). |
| 3 | **R3** | Throttle `reportRect` (rAF or ~100 ms trailing debounce) in `useBrowserRect.ts`; make backend `set_rect` use `try_lock` and **drop the update when busy** (rect updates are latest-wins — the next tick corrects it). | ~15 LOC | `try_lock` skip-path unit test (busy mutex → update dropped, rect unchanged). |
| 4 | **R2** (opt-in) | `--disable-features=CalculateNativeWinOcclusion` via `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` (existing pattern, `main.rs:64-68`): the renderer stays unthrottled while occluded — no buffering, no catch-up — at the cost of background CPU/GPU. Try **after** 1+2; gate behind an env flag, not default. | ~5 LOC | Manual A/B via the repro checklist. |
| 5 | **R5** | Bound the `RunEvent::Exit` teardown (`block_on` → spawn + timeout) so a wedged compositor can't hang shutdown. | ~15 LOC | Manual: exit during a wedged session. |

**Do #1 (R1) first** — it is the only controller call on the event loop and the most credible hard-freeze vector. #2 kills the "catching up" burst the user observed.

---

## Verified not involved (checked in-session — do not re-chase)

- **F1 / F1b / F2 / F3 / F6** (2026-04-19 diagnosis) and **N1** (2026-06-14 review) are fixed at HEAD: `get_git_branch` clones root → drops lock → `spawn_blocking` (`files.rs:228-234`); trace-detail poll stops on `is_complete` (`LlmTraceView.tsx:626-634`); transcript/activity caps + memoized rows + rAF-batched deltas (F3); 60 s + focus + turn-end git poll (`App.tsx:304-341`).
- No `visibilitychange`/`document.hidden` handlers in the frontend; `main.rs:95` `Resized` is the only window-event handler.
- The headless-Chromium browser manager (`src-tauri/src/ipc/browser.rs:107`) is a separate process driven over CDP — not a WebView2 controller call, out of this failure class.
