# Headless-Browser Debugging Feature — Review

**Date:** 2026-08-12
**Reviewer:** read-only subagent
**Scope:** the chromiumoxide headless-browser feature (commits 3e927ce, a966444, 9bf2793, 8ffc94b, 9e03e84) — `src/browser/mod.rs`, `src/tool/browser/mod.rs`, `src/tool/mod.rs`, `src/agent/factory.rs`, `src-tauri/src/ipc/browser.rs`, `src-tauri/src/ipc/state.rs`, `src-tauri/src/main.rs`, `frontend/` (BrowserView, rightPanelViews, agentState, Sidebar, tauri.ts, types.ts), `src/error.rs`, `Cargo.toml`, `.coding/browser-debugging.md`.

**Working tree:** `git diff HEAD` (stat, full diff, status) is **empty** — there are no uncommitted changes. The entire review surface is the committed feature; there is nothing outside the feature to review. (The feature being committed directly to `main` was an explicit user decision — not reported, per instructions.)

**Verification limits:** this reviewer is read-only and cannot run `cargo test`, `cargo build`, or `git` itself. The warning-free / test-green claims rest on the committed state + the absence of `#[allow]` suppressions in the feature files; the main agent must run `cargo test` before closing.

---

## Executive summary

The feature is well-structured and largely correct: lock-free broadcast streaming, listener registration *before* the forwarder task spawn (a real race that was properly closed), minimal `record_console` lock scope, correct safety-level assignments, correct ToolFilter gating, sandbox-validated screenshot writes, and a clean frontend listener/ref pattern. Two items need real fixes before merge-worthiness: **(1) the documented crash-respawn does not exist** (a stale `Browser` handle is never detected), and **(2) no URL scheme allow-list** — `browser_navigate`/`browser_open` accept `file://` and arbitrary hosts, silently crossing the project-root sandbox boundary and enabling SSRF despite the schema claiming "http(s) and data: URLs". Everything else is medium/minor.

---

## 1. Correctness

### C1 (medium) — The global state Mutex is held across CDP round-trips and process launch
`src/browser/mod.rs`:
- `navigate` (lines 147–193) holds `state.lock()` across `ensure_browser` (which runs `Browser::launch` — a process spawn taking seconds), `new_page`, two `page.event_listener(...)` calls, and `page_info` (which awaits `page.url()` + `page.get_title()` — 2 CDP round-trips at 459–471).
- `list_pages` (196–204) holds it across N `page_info` awaits (N round-trips).
- `close_page` (208–226) holds it across `page.close().await`.
- `screenshot`/`snapshot`/`eval`/`click`/`type_text` (239–328) hold it across all their CDP round-trips.

This is **not a deadlock** (I checked: no re-entrant acquisition — `page_info` takes `&State`, not the lock; `record_console`'s lock block at 430–441 has no awaits inside; chromiumoxide's internals never touch this mutex; the forwarder tasks hold it only in that tiny block; abort-then-close in `close_page`/`close` has no wait cycle). But it is severe head-of-line blocking: all 10 tools + 4 IPC commands serialize on one mutex, and a dead/stalled Chromium makes every CDP call wait out chromiumoxide's full command timeout while holding the lock — freezing the Browser tab UI too. Fix: resolve the id + clone the `Page` handle under the lock, drop the lock, then await the CDP calls.

### C2 (medium, real bug) — "Re-spawn on crash" is documented but not implemented
`src/browser/mod.rs`:
- Module doc (lines 9–13) and `ensure_browser` doc (line 115) promise transparent re-spawn if the process dies.
- `ensure_browser` (116–120) only checks `state.browser.is_some()`. When Chromium exits or the CDP connection drops, the handler task (136–140) ends silently, but `state.browser` stays `Some` (a stale handle) and `state.pages` keeps stale pages. Every subsequent operation fails with CDP "session closed"-style errors, and `navigate` never re-spawns. The failure is permanent until an explicit `close()` or app restart.
- Fix: before returning `Ok(())`, check `state.handler.as_ref().is_some_and(|h| !h.is_finished())` (or an `AtomicBool` set when the handler loop exits) and, on death, clear `browser`/`pages`/`console`/`console_tasks` and `active` before launching fresh.

### C3 (minor bug) — Failed `navigate` clobbers the active page of other pages
`src/browser/mod.rs:172–182`: the listener-subscription failure path does `state.active = None` unconditionally. With other pages already open, id-less operations now fail with "no open page" even though pages exist. Should restore the previous active id (or leave `active` untouched).

### C4 (minor bug/design) — `console()` drain is shared destructively by the agent tool AND the UI
`BrowserManager::console` (251–260) *drains* the per-page ring buffer. It is consumed by both the `browser_console` tool (src/tool/browser/mod.rs:272) and the IPC `browser_console` command (src-tauri/src/ipc/browser.rs:75–85). The Browser tab's refresh (`BrowserView.tsx:63`, `refreshView`) drains the buffer the agent's tool would otherwise read — whichever consumer reads first starves the other, and entries a drain-based tool would have reported are silently consumed by a UI refresh. Recommend: IPC peeks (or returns without draining); agent tool drains; or per-consumer cursors.

### C5 (minor) — Pages that self-close are never pruned; stream half-close ends forwarding
`src/browser/mod.rs`: if a page closes itself (`window.close()` in JS), its forwarder task ends (streams close) but the dead `Page` stays in `pages` and the finished `JoinHandle` stays in `console_tasks` (only `close_page`/`close` remove them) — `list_pages` then shows a zombie page with empty url/title. Also `forward_console_events` (375–397) breaks when *either* stream ends; if only one domain closes, the other stops feeding the buffer. Low impact; prune on `page_info` error, or handle the two stream ends independently.

### Checked and OK
- Console listener registration race: listeners are subscribed synchronously in `navigate` (167–183) *before* the forwarder task spawn — correct. Load-time logs (before registration) are lost, but that is documented and tested (`console_events_stream_to_buffer_and_broadcast`, lines 534–535).
- `record_console` lock scope (424–445): minimal block, no awaits inside — correct.
- Broadcast back-pressure (444): non-blocking `send`, laggers re-read via `browser_console` — correct.
- Close-page vs forwarder-abort: abort-then-close ordering (216–221) plus the `console.get_mut(page_id)` None-guard (432) is race-safe — correct.
- No `unsafe` in any feature file (tree-wide `unsafe` is only pre-existing Windows ACL code in `src/config/keys.rs`).

---

## 2. Bugs

### B1 (medium) — Per-launch temp profile directories leak forever
`src/browser/mod.rs:120–126`: `ensure_browser` creates `%TEMP%\myharness-browser-<id>` on every launch and never removes it. The comment ("cleaned up by the OS on reboot") is wrong in practice: Windows does not reliably clean `%TEMP%` (Storage Sense is off by default). Every app start and every crash-respawn leaks a full Chromium profile (tens of MB). Fix: hold a `tempfile::TempDir` in `State` (dropped on `close()`), plus best-effort `remove_dir_all` of stale `myharness-browser-*` dirs at startup.

### B2 (info) — chromiumoxide API usage: verified correct, one doc note
- `HeadlessMode::New` (129) is right for Chrome/Chromium ≥ 109 (older binaries fail with a clear launch error).
- Explicit `Runtime::EnableParams::default()` + `Log::EnableParams::default()` (365–370) covers the case where `event_listener` doesn't auto-enable — harmless double-enable.
- Per-launch `user_data_dir` (130) correctly avoids the profile-lock failure mode for concurrent managers/tests.
- `ScreenshotParams::default()` → PNG (test asserts magic bytes at src/tool/browser/mod.rs:383). `page.evaluate` (285) uses default params, i.e. `awaitPromise: false` — promise-returning expressions yield a promise descriptor, not the resolved value; document this in the `browser_eval` schema or switch to `EvaluateParams` with `await_promise: true`.

### B3 (minor) — Frontend console list grows unbounded
`frontend/src/components/views/BrowserView.tsx:89`: the live-push appends with no cap; a noisy page (render-loop logging) grows the array without limit until the next pull refresh. Trim to the last ~500 entries to mirror the Rust `CONSOLE_CAP`.

### B4 (nit) — Console level strings mismatch the UI's color map
CDP serializes `ConsoleAPICalledType` as `"warning"`/`"info"`/`"debug"`, but `BrowserView.tsx:206–210` colors only `"warn"`/`"error"`/`"exception"`. Warnings render in the default color. Cosmetic.

---

## 3. Security

### S1 (high) — No URL scheme allow-list; `file://` crosses the sandbox boundary + SSRF
- `browser_navigate` (src/tool/browser/mod.rs:61–93) and the IPC `browser_open` (src-tauri/src/ipc/browser.rs:87–97) pass any URL straight to `BrowserManager::navigate`. The tool's schema/doc says "Supports http(s) and data: URLs" but **nothing enforces it**.
- `file:///C:/Users/<u>/.myharness/keys.toml` (API keys live there) or any file outside the project root can be loaded, then read via `browser_snapshot`/`browser_eval` — bypassing the file tools, which are sandboxed to the project root (`Sandbox`). A page can also fetch `file://` content and exfiltrate it over the network.
- SSRF: navigation to localhost/internal endpoints (local dev DB admin panels, Docker socket via other schemes, `169.254.169.254` metadata) from a tool the agent chooses autonomously in `SafetyMode::Autonomous`.
- Mitigating context (stated for balance): the `shell` tool already grants arbitrary local execution, so this isn't new *privilege* versus shell — but it *is* a silent crossing of the file-tool sandbox boundary by a tool advertised as browser debugging, and it applies to both agent calls and the UI URL bar.
- Fix: enforce a scheme allow-list (`http`, `https`, `data`) in `BrowserManager::navigate` (one choke point covers both tool + IPC), and consider denying/flagging localhost + private ranges behind a user-visible setting.

### S2 (medium) — `browser_eval` power (correctly gated, but amplifies S1)
`browser_eval` is arbitrary JS in the page context and is correctly `NeedsApproval` (src/tool/browser/mod.rs:524–526). The per-launch throwaway profile is a genuine mitigation — no persistent cookies, so no credential-theft-on-a-real-origin risk. But combined with S1's `file://` navigation, eval becomes a full local-file-read primitive. Closing S1 shrinks eval's blast radius to the navigated origin.

### S3 (verified OK) — Screenshot path sandbox validation
`src/tool/browser/mod.rs:222–233`: the page_id is interpolated into `.coding/browser/screenshots/<page>-<ts>.png` and written through `Sandbox::validate_for_creation`, which lexically normalizes `.`/`..` via `Path::components()` (`src/tool/agent/sandbox.rs:121–134, 185–203` — handles Windows `\` separators) and rejects anything escaping the root. A crafted page_id either stays inside the root or fails the write (Windows-invalid filename chars). The returned `rel` is independently re-validated by `describe_image`. **No finding.**

### S4 (verified OK) — Safety levels, ToolFilter, CSP
- All six mutations are `NeedsApproval`; reads are `AutoRun` — enforced by the test `safety_levels_are_set` (src/tool/browser/mod.rs:413–429).
- `ToolFilter` gating: Browser reads visible in Planning/Complete (src/tool/mod.rs:230, 290); mutations visible only in Executing/Reviewing (232–279). Matches the plan exactly.
- CSP already allows `img-src 'self' data: blob:` in both `csp` and `devCsp` (src-tauri/tauri.conf.json:26–27) — the base64 screenshot `<img>` works with no CSP change. **No finding.**

### S5 (low/info) — Unauthenticated localhost CDP endpoint
chromiumoxide's `--remote-debugging-port` WebSocket is unauthenticated on localhost; any local process could attach. Standard for this class of tool; the throwaway profile limits the exposure. Note only.

### S6 (low) — `browser_console` is AutoRun but destructive
See C4: a tool classified as a read (`AutoRun`) drains shared state. Not exploitable on its own; fold the fix into C4.

---

## 4. Constitution compliance

- **Doc comments on public items:** verified present on every public item in the feature: `PageInfo`/`ConsoleEntry`/`ConsoleEvent` (structs + every field), `BrowserManager` + all 11 `pub fn`s, `ToolCategory::Browser` (src/tool/mod.rs:35–36), all 10 tool structs + `pub(crate) Ctx` + `SCREENSHOT_DIR`, the 4 IPC commands, `with_browser`/`browser_slot`, `AgentRuntimeContext.browser`. **No finding.**
- **Warning-free build / no `#[allow]`:** no `#[allow(...)]` was added in any feature file. The only `#[allow]` hits tree-wide are pre-existing `#[allow(clippy::too_many_arguments)]` in `src/agent/loop_impl.rs` (clippy-only, pre-existing, not part of this feature). I could not execute `cargo test` (read-only, no shell) — the main agent must run it to confirm warning-freeness under `#![deny(warnings)]`.
- **No `unsafe`:** confirmed — none in the feature files.
- **Line endings:** no mixed-endings evidence visible in any file read; git-side CRLF/LF handling cannot be verified without shell access.
- **Closing sequence:** the working tree is clean (`git diff HEAD` = empty), so there are no non-feature changes to review; the review report file itself should be included in the commit per the sequence.

---

## 5. Frontend

### F1 (verified OK) — `selectedIdRef` pattern + listener cleanup
`BrowserView.tsx:84–98`: the listener is registered once (empty deps) and reads `selectedIdRef.current`; the ref is updated in all three selection paths (`refreshPages:46`, `openUrl:109`, `select onChange:147`). Cleanup handles the unlisten-before-resolve race correctly via the `cancelled` flag (86–97). **No finding.**

### F2 (minor) — Pull refresh races the live push
`BrowserView.tsx:61–66`: `refreshView` replaces `consoleEntries` with the drained Rust buffer; events pushed between the drain completing and `setConsoleEntries` are lost. Cosmetic (next push re-syncs); a merge-on-replace would fix it.

### F3 (verified OK) — TS ↔ Rust type parity + invoke arg casing
`BrowserPage` ↔ `PageInfo` (id/url/title/active), `BrowserConsoleEntry` ↔ `ConsoleEntry`, `BrowserConsoleEventPayload.page_id` ↔ serde snake_case `page_id` (Rust `ConsoleEvent` serializes as `page_id`, matching `event.page_id` in the listener). `invoke("browser_screenshot_latest", { pageId })` relies on Tauri 2's camelCase→snake_case argument conversion — consistent with the file's existing convention (`get_workflow_state { agentId }`, tauri.ts:169). **No finding.**

### F4 (nit) — Redundant set after `openUrl`
`BrowserView.tsx:105–109`: `await refreshPages()` already selects the new page (it's active), then `setSelectedId(page.id)` re-sets it. Harmless.

---

## Bottom line

Fix **C2** (stale-browser detection/respawn), **S1** (URL scheme allow-list in `BrowserManager::navigate`), and **C1** (drop the lock before CDP awaits) before merging; the remaining items (B1 temp-dir leak, C3/C4, C5, B3, S5/S6, F2) are smaller follow-ups worth doing in the same pass where cheap.
