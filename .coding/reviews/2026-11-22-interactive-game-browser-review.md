# Review — Interactive game browser (CDP attach infra + Game panel + agent tools)

**Scope:** all uncommitted changes in the working tree (8 files, +481/-5).
**Plan goal:** infra for a human+agent to build/play/debug a game together — the human
plays a game in an iframe embedded in the Tauri WebView2; the agent attaches to that
SAME WebView2 via its CDP debug port and inspects live game state. Infra only; no game
wired in.

The diff is well-structured and follows the existing `BrowserManager` / browser-tool
patterns closely. The connect-mode lifecycle (lazy connect, dead-handler respawn,
`close()`/`close_webview()` cleanup) is sound, and the test mirrors the proven spike.
Three findings below, ordered by severity.

---

## Security

### S1 — `game_eval` is `AutoRun` but executes arbitrary JS against the LIVE app WebView2 (should be `NeedsApproval`)

**File:** `src/tool/browser/mod.rs:766-811` (`GameEvalTool`), registered in
`src/agent/factory.rs:524`.

`GameEvalTool` is `SafetyLevel::AutoRun` with the rationale "read-only inspection of an
already-open page." But `game_eval` runs **arbitrary JavaScript** (`evaluate_expression`
with a caller-supplied expression) against the app's **own** WebView2 — the live app UI
the human is using, not a throwaway headless browser. Arbitrary eval is not read-only: the
agent can `window.location.href = 'https://…'` (navigate the app's main frame away from
the app, breaking the UI), mutate the DOM, call into app JS, or `fetch(...)`/exfiltrate
app state — all without approval.

This is **inconsistent with the existing `EvalTool`** (`browser_eval`,
`src/tool/browser/mod.rs:500-547`), which is `SafetyLevel::NeedsApproval` for the *same*
capability — and `browser_eval` targets a sandboxed headless browser, so `game_eval` is
strictly the more sensitive of the two (live app vs. throwaway process). The tool
description's "Read-only inspection (does not navigate)" claim is not enforced by the
implementation; nothing prevents navigation or mutation.

`game_screenshot` and `game_snapshot` ARE genuinely read-only (capture pixels / serialize
DOM) and `AutoRun` is correct for them. Only `game_eval` is mis-classified.

**Recommendation:** set `GameEvalTool::safety()` to `SafetyLevel::NeedsApproval`, matching
`EvalTool`. If the intent is truly read-only game-state reads, that should be a separate,
restricted tool — but arbitrary eval must be approval-gated.

---

## Bugs / Concurrency

### B1 — `ensure_webview` holds the state lock across the full 30s connect+retry loop, blocking all browser operations

**File:** `src/browser/mod.rs:564-611` (`ensure_webview`), called from
`webview_page` at `src/browser/mod.rs:617-627`.

`webview_page` acquires `self.state.lock().await` and then awaits `ensure_webview` **while
still holding the guard**. `ensure_webview` does `Browser::connect(url).await` (network
I/O: HTTP `/json/version` + WebSocket) and, on failure, `tokio::time::sleep(500ms).await`
in a loop bounded by a **30-second deadline**. So when the WebView2 is not yet ready (or
the debug port isn't exposed — e.g. a release build, see R1), a single `game_*` call holds
the **shared** state `Mutex` for up to 30 seconds.

Because the launch-mode browser (`browser`/`handler`/`pages`/`console`) lives behind the
**same** `state` mutex, a 30s webview connect blocks **every** headless-browser operation
too (`browser_navigate`, `browser_screenshot`, `browser_eval`, `browser_list_pages`, …)
for the whole retry window — not just the game tools.

This mirrors the existing `ensure_browser` pattern (`navigate` holds the lock across
`Browser::launch().await`), so it is consistent with the codebase — but `ensure_browser`'s
lock-hold is a one-shot launch (seconds), whereas `ensure_webview`'s is a 30s retry loop,
which is materially worse. There is no deadlock (single lock, no nested acquisition) and no
data race; the issue is lock-duration / head-of-line blocking.

**Recommendation:** perform the connect + retry **outside** the lock — drop the guard,
run the `Browser::connect` retry loop lock-free, then re-acquire the lock only to store
`webview` + `webview_handler` (re-checking for a concurrent winner). This keeps the
fast-path (`is_some() && !dead`) under a short lock and stops a missing WebView2 from
stalling the headless browser.

---

## Robustness (low severity)

### R1 — `game_*` tools are registered unconditionally; in release builds they connect to `localhost:9222` where nothing (or something unrelated) listens

**Files:** `src/agent/factory.rs:524-526` (registration, no `cfg` gate);
`src/browser/mod.rs:147,863` (`webview_url` defaults to `http://localhost:9222` in both
`new()` and `Default`); `src-tauri/src/main.rs:41-45` (env var is correctly
`#[cfg(debug_assertions)]`-gated).

The CDP port exposure is correctly debug-gated in `main()` — release builds do NOT set
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`, so the WebView2 does not expose CDP. Good.
However, the `game_*` tools are registered for **every** build, and `webview_url` always
defaults to `http://localhost:9222`. So in a release build, a `game_*` call enters
`ensure_webview` and retries `Browser::connect("http://localhost:9222")` for 30s, then
errors. Worse, if some *other* local process happens to expose a CDP endpoint on 9222
(e.g. a Chrome launched with `--remote-debugging-port=9222`), the agent would silently
attach to **that** process and inspect/eval against it — combined with S1 (`game_eval` is
AutoRun), that is an unapproved arbitrary-eval path against an unintended target.

The tool descriptions say "Requires a debug build," but nothing enforces it.

**Recommendation:** either `#[cfg(debug_assertions)]`-gate the `game_*` tool registration
in `register_browser_tools`, or have `ensure_webview` short-circuit with a clear error
when `cfg!(not(debug_assertions))` (e.g. `"game inspection requires a debug build"`), so
release builds fail fast instead of probing localhost:9222 for 30s.

---

## Noted but acceptable (not findings)

- **`frame-src … https://*`** (`src-tauri/tauri.conf.json:27-28`): broad, but the iframe
  is user-loaded (the human explicitly types a game URL), and `https://*` is necessary for
  the feature to load arbitrary hosted games. The CSP is the enforcement layer for
  injection; user-directed loads are the intended use. Acceptable for a dev tool.
- **iframe `sandbox="allow-scripts allow-same-origin allow-forms allow-popups"`**
  (`frontend/src/components/views/GameView.tsx:58`): `allow-scripts allow-same-origin` is a
  known risk combination, but the game is cross-origin to the app (e.g. `localhost:3000`
  vs the Tauri origin), so the same-origin policy protects the parent app's DOM. The game
  can only affect its own origin. Acceptable for user-loaded games.
- **`webview_page()` returns the first page target** (`src/browser/mod.rs:631-637`):
  correct for production, where the WebView2 has exactly one page (the app UI). The test
  mirrors this. If multiple page targets ever exist, `into_iter().next()` is arbitrary, but
  that is out of scope for the single-page design.
- **`close()` webview cleanup** (`src/browser/mod.rs:543-546`): correctly aborts
  `webview_handler` and drops the connected `Browser`. The comment is accurate — dropping a
  *connected* `Browser` closes the CDP WebSocket without killing the app's WebView2
  process. No leaked handler tasks or browsers on reconnect/close.
- **CDP port debug-gate** (`src-tauri/src/main.rs:41-45`): correctly
  `#[cfg(debug_assertions)]`; release builds do not expose CDP. (The release-side gap is
  R1 above, not the gate itself.)
- **`game_*` tools cannot navigate** (review prompt (c)): confirmed —
  `webview_screenshot`/`webview_snapshot` capture pixels / serialize DOM;
  `webview_eval`/`game_eval` is the only one that *can* navigate (via `window.location`),
  which is exactly why S1 applies to it and not to the other two.

## Constitution compliance

- **Warning-free under `#![deny(warnings)]`** (`src/lib.rs:1`, `src-tauri/src/main.rs:7`):
  no `#[allow(...)]` suppressions added anywhere in the diff. `std::env::set_var` is safe
  in edition 2021 (confirmed `edition = "2021"` in `Cargo.toml:4`) — the
  `unsafe`-in-2024 lint is `allow`-by-default, so no warning. `new_with_webview_url` is
  `#[cfg(test)]` + `pub(crate)` and is used by the test, so no dead-code warning. Per the
  task, `cargo test --workspace` passes (74 tests, 0 failed) — consistent with a
  warning-free build.
- **Public functions have doc comments:** `webview_screenshot`/`webview_eval`/
  `webview_snapshot`/`close_webview`/`new_with_webview_url` and the three tool structs all
  carry doc comments; private `ensure_webview`/`webview_page` do too. ✓
- **Code style** matches the repo (Ctx struct, `parse_args!` macro, `ToolSchema`,
  `SafetyLevel`, `ToolCategory`, `Error::Browser(format!(…))`, verbose comments). ✓
- **Factory test** (`src/agent/factory.rs:961-963`) updated to include the 3 new tool
  names, consistent with the registration in `register_browser_tools`. ✓
