## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes on `wt/agenticcoder` (bug_fixing plan 5ae26d22: browser_navigate self-bootstraps the Browser tab's child webview + readable browser tool-card summaries). The fix design is implemented faithfully and correctly: no correctness, security, or constitution defects. 3 low findings (dead `game_*` branches with a misleading doc comment, a corner-case error message, two missing trailing newlines).

## Findings

### Low 1 — dead `game_*` tool-name branches + doc comment describes tools that don't exist
- **Where:** `frontend/src/lib/toolCardPaths.ts:401-406` (`browserArgLabel` browserish check: `game_navigate` / `game_click` / `game_type`) and `frontend/src/lib/toolCardPaths.ts:474-479` (`browserResultInfo` browserish check: `game_navigate` / `game_screenshot` / `game_snapshot`); doc comments at `toolCardPaths.ts:385-387` ("the live-tab `browser_*`/`game_*` tools") and in `Message.tsx:577-578`.
- **What:** No `game_*` tools exist anywhere in the Rust codebase — a repo-wide search of all 243 `.rs` files finds zero `game_` matches; the actual tool names are `browser_*` and `offscreen_browser_*` (verified in `src/tool/browser/mod.rs:74-1065`). The `game_*` comparisons are inert dead code, and the doc comments tell a future maintainer that a `game_*` family exists (it doesn't — the naming is legacy from before the Mnemo rename; the project constitution's "sanctioned `game_*` tooling" clause refers to that legacy).
- **Impact:** None at runtime (the branches simply never match). Risk is documentation drift only.
- **Fix:** Either drop the five `toolName === "game_*"` comparisons and reword the comments to "the live-tab `browser_*` tools and the headless `offscreen_browser_*` tools", or — if kept deliberately as rename-history tolerance — add one sentence to each doc comment stating the `game_*` names no longer exist and are kept only for old transcripts. Dropping is cleaner.

### Low 2 — deadline error message is misleading when the ensurer ran but no child target ever registered
- **Where:** `src/browser/mod.rs:853-859` (`webview_page_auto_ensure`'s deadline arm).
- **What:** When the IPC-side handle already exists (`bw.webview.is_some()`, so `ensure_for_agent_impl` skips creation and emits no reveal) but no non-app CDP target appears — e.g. a stale handle after a wry crash, or the child staying at `about:blank` (which `is_app_url`, `src/browser/mod.rs:971-982`, classifies as an app URL) — the 10s deadline returns "…call browser_navigate first — it creates the child webview automatically". That is exactly the tool that just failed, and the auto-create was a no-op; a retry loops forever in that degraded state.
- **Impact:** Rare corner (pre-existing staleness class, not a regression); diagnosis quality only.
- **Fix:** Differentiate the deadline message by whether the ensurer ran and reported success, e.g. when `ensured == true`: "the Browser tab's child webview did not register a CDP page target within 10s — open the Browser tab and navigate once manually, then retry".

### Low 3 — missing trailing newline in two new test blocks
- **Where:** `frontend/src/components/chat/messageArgLabel.test.ts:226` and `frontend/src/lib/toolCardPaths.test.ts:532` (git reports "\ No newline at end of file" for both).
- **Fix:** Add the trailing newline (repo convention: every other changed file ends with one).

## Verified correct (requested checklist)

**Rust core (`src/browser/mod.rs`)**
- Ensurer invoked **at most once per navigate attempt** (`ensured` flag, `src/browser/mod.rs:837-852`); the 10s deadline + 100ms sleeps bound the loop (~100 iterations max) — no infinite loop when the child never appears; an ensurer `Err` returns immediately ("failed to create the Browser tab's child webview: …").
- **No BrowserManager lock held across the ensurer await:** `ensure_webview()` releases its state lock before returning (fast path scoped block `src/browser/mod.rs:705-716`; slow-path store block drops the guard at return, `:741-758`); the poll loop calls only `browser.pages()` (chromiumoxide, no manager locks); the `child_ensurer` read guard is cloned out and dropped at the `let` semicolon (`:839-843`) before `(ensurer)(url).await`. The ensurer takes only the **IPC** state mutex (`browser_webview.rs:371`) and never calls back into BrowserManager — no cycle. Rect reports use `try_lock` (Busy/deferred) so nothing queues behind the ensurer's mutex hold, consistent with the R3 design.
- **Error-text change is compatible:** both new error strings (`src/browser/mod.rs:801-806`, `:854-859`) keep the "is the Browser tab open?" substring asserted by the pre-existing test at `:2431`.
- **click/type/eval never auto-create:** they still route through plain `webview_page()` (`:1014`, `:1071`, type likewise); only `webview_navigate` uses `webview_page_auto_ensure` (`:1055-1062`).
- **Regression tests are real:** `webview_click_without_child_says_to_navigate_first` (`:2625`) asserts the changed guidance text (genuinely fails without the fix); `webview_navigate_auto_ensures_missing_child` (`:2666`) end-to-end exercises the bootstrap (launched-Chromium stand-in, second CDP connection creating the page, ensurer-ran flag, landed URL, follow-up eval reads the bootstrapped child's DOM). Explicit kill + profile-removal retry (B1 pattern) matches the file's existing test hygiene.

**App layer (`src-tauri/src/ipc/browser_webview.rs`)**
- Platform gate first (`:369`, runtime — no new `cfg(windows)`); `normalize_url` choke point intact (`:370`) and **idempotent** (verified `src/browser/mod.rs:1376-1405`: an already-schemed http/https/data URL is returned as-is), so the double normalization (tool layer + impl) is a no-op; allow-list includes `data:` (the test URL is valid).
- `heal_deferred` on mutex acquisition (`:374`) matches `browser_webview_navigate`/`set_tab_visible`; the trailing `supersede_deferred()` (`:436`) mirrors the human ensure and is a no-op in every path (heal already consumed the slot).
- **Rect fallback math is safe on tiny/degenerate windows:** `(size.width / 5).max(1) * 2`, `saturating_sub`, `w.max(1)`, `height.max(1)` (`:389-391`) — no panic at width/height 0; and any wrong guess self-corrects because the reveal event mounts BrowserView → `useBrowserRect` re-reports → `set_rect` applies. The normal case (tab toggled on once) has a stored rect because `try_apply_rect`/`apply_rect` record `bw.rect` even with `webview: None`.
- **`tab_visible` preservation is correct in both cases:** the fresh-bootstrap-with-tab-mounted case has `tab_visible == true` (BrowserView's mount effect `BrowserView.tsx:75-81` fires before any navigation), so `apply_visibility` (`:428`) shows the new child immediately and the reveal event is a harmless re-select; the inactive-tab case stays hidden until reveal → `rightPanelTab = "browser"` → BrowserView mounts → `set_tab_visible(true)` → shown. No case forces a wrong visibility; the human ensure's `tab_visible = true` (`:334`) is correctly NOT copied.
- Mutex serializes a racing human ensure vs agent ensure (second caller sees `webview.is_some()` and takes the move/no-op path) — no double-create.
- `browser_webview_ensure_for_agent` command registered in `main.rs` invoke_handler; `IpcError` has `.message` (no `Display`) — `main.rs` maps via `.message` correctly.

**Wiring (`src-tauri/src/main.rs`)**
- `attach_child_ensurer` installed at **all three** `BrowserManager::new()` sites (verified by search: `:538`, `:611`, `:1446`), incl. the `app: None` guard in `build_brain_inner` (headless → ensurer absent → auto-ensure inert, documented in `set_child_ensurer`'s doc comment).

**Frontend**
- `revealRightPanelTab` (verified `useAgentStore.ts:717-724`) filters `disabledTabs` (never disables), selects, reveals the panel — the 3 new store tests match the actual semantics.
- Module-scope listener flag set synchronously before the async `listen()` (`useAgentEvents.ts:187-197`) — StrictMode double-mount safe, mirrors the backlog listener exactly; `useAgentEvents.dispatch.test.ts` mock correctly gained `onBrowserReveal` (the real module now imports it).
- Tool-card wiring: `browserArgLabel` sits after `graphCallLabel` and before the generic path extraction (browser tools carry no `path` arg — no shadowing); the result chip is pushed only on successful completed calls with a per-call key (`${c.id}:browser`) — no chip collisions.
- **Frontend regexes verified against the real Rust output strings:** "navigated the Browser tab to {url}" (`src/tool/browser/mod.rs:979`), offscreen "opened page {id} ({title}): {url}" (`:110` — degrades to null if a title contains ")", acceptable), snapshot returns the page HTML (`<title>` regex works), screenshot paths use the **forward-slash** constant `SCREENSHOT_DIR` (`src/tool/browser/mod.rs:47`, `:808`) so the regex matches on Windows too. Failed navigate output matches neither regex → null (tested).

**Security**
- The agent URL reaches the webview only via `normalize_url` (twice, idempotent); scheme allow-list (http/https/data) enforced; no raw string reaches `WebviewUrl::External`. CDP stays debug/opt-in gated (the ensurer is reachable only after `ensure_webview()` passes `webview_enabled`). The reveal event carries only the normalized URL and flips pure UI state. `offscreen_browser_*` tools use the launch-mode page API — untouched by the `webview_*` changes; the frontend additions affect their card labels only.

**Constitution**
- Doc comments present on every new pub item: `ChildEnsurer`, `set_child_ensurer`, `BROWSER_REVEAL_CHANNEL`, `browser_webview_ensure_for_agent`, `BrowserRevealPayload`, `onBrowserReveal`, `browserArgLabel`, `browserResultInfo`, `handleBrowserReveal` (plus the pub(crate)/private helpers).
- Warning-free: crate roots `deny(warnings)` and the plan records green `cargo test` (1566 passed) / src-tauri build / 656 vitest / tsc — a green build under `deny(warnings)` proves zero warnings.
- Docs: `.coding/browser-debugging.md` navigate row updated. README (`README.md:64`, `:110`) describes the browser tools at feature granularity — the bootstrap detail is below that granularity and lives correctly in .coding/browser-debugging.md; no further doc change needed.
- Multi-platform: no new `cfg(windows)` anywhere in the diff; the platform gate is the existing runtime `ensure_platform_gate(child_webview_supported())`; the fallback-rect code uses the cross-platform `window.inner_size()`; the new Rust tests launch cross-platform Chromium (same as the file's existing tests).
