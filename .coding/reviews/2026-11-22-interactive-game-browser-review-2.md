# Review 2 — Interactive game browser (verify S1/B1/R1 fixes + new-issue sweep)

**Scope:** the committed feature on `feat/interactive-game-browser` (commit 3bae3ae).
Reviewed by reading the final file state directly (changes are committed, so
`git_diff` shows only bookkeeping). Files reviewed:
`src-tauri/src/main.rs`, `src-tauri/tauri.conf.json`, `src/browser/mod.rs`,
`src/tool/browser/mod.rs`, `src/agent/factory.rs`, `frontend/src/hooks/agentState.ts`,
`frontend/src/hooks/rightPanelViews.tsx`, `frontend/src/components/views/GameView.tsx`.

## Verdict

All three prior findings (S1, B1, R1) are **fixed correctly and completely**.
One new **low-severity** documentation issue was introduced/left by the S1 fix
(a stale module comment that re-asserts the very claim S1 corrected). No
correctness, concurrency, security, or constitution-compliance defects found.

---

## Prior-fix verification

### S1 — `game_eval` safety level  →  FIXED ✓

`src/tool/browser/mod.rs:797-799` — `GameEvalTool::safety()` now returns
`SafetyLevel::NeedsApproval`, matching the existing `EvalTool` (`browser_eval`,
line 528-530). The tool's own doc comment (lines 763-767) and schema description
(lines 781-787) were rewritten to say "Arbitrary script execution against the
live app — approval required" and no longer claim "read-only." Confirmed
`GameScreenshotTool` (lines 730-732) and `GameSnapshotTool` (lines 837-839)
remain `SafetyLevel::AutoRun` — correct, since they only capture pixels /
serialize DOM and cannot navigate or mutate.

### B1 — `ensure_webview` lock duration  →  FIXED ✓

`src/browser/mod.rs:566-639`. Verified:
- **Fast path** (lines 570-581): acquires the lock, and if `webview` is present
  + its handler alive, clones the `Arc<Browser>` and returns immediately under
  a short lock. ✓
- **`cfg!` gate** (lines 584-588): outside the lock. ✓
- **Slow path** (lines 591-638): `Browser::connect` (line 594) and
  `tokio::time::sleep` (line 624) run **outside** the lock — the lock is
  re-acquired only at line 604 to store the result. ✓
- **`webview_page`** (line 646) calls `self.ensure_webview().await?` — it no
  longer passes `&mut state` or hold the state lock across the connect. ✓

**Concurrent-winner logic** (lines 604-621) is sound:
- On re-acquire, it recomputes `dead` from the *current* `webview_handler`.
  If a concurrent winner already stored a live connection (`webview` is `Some`
  and `!dead`), the loser returns `Arc::clone(existing)` (line 612) — the
  winner's connection is used, the loser's is discarded. **No double handler
  task is stored** (the loser returns before reaching line 620). ✓
- If the stored connection died between the fast-path check and re-acquire
  (`dead == true`), the code falls through and replaces it (lines 616-620),
  aborting the stale handler first. **No lost connection.** ✓
- The loser's own freshly-spawned handler task (local `task`, line 597) is
  detached rather than explicitly aborted when the loser takes the
  concurrent-winner branch. This is **not a leak**: the loser's `connected`
  `Arc<Browser>` is also dropped on return, which tears down the CDP WebSocket,
  causing the detached handler's `handler.next().await` to return `None` and
  the task to self-terminate. Noted but acceptable (see below).

### R1 — release-build localhost:9222 probing  →  FIXED ✓

`src/browser/mod.rs:584-588`. The `if !cfg!(debug_assertions)` short-circuit is
positioned correctly: **after** the fast path (so an already-connected webview
is returned regardless) and **before** the 30s retry loop. In a release build
`cfg!(debug_assertions)` is `false`, so `!false` triggers and the call fails
immediately with a clear message (`"game inspection requires a debug build
(the CDP port is dev-only)"`) instead of probing `localhost:9222` for 30s. ✓

The test `webview_connect_screenshot_eval_snapshot` (lines 1290-1385) still
passes: tests compile with `debug_assertions` enabled, so `cfg!(debug_assertions)`
is `true`, the gate is skipped, and the slow path connects to the launched
stand-in Chromium via `new_with_webview_url`. ✓

---

## New findings

### N1 (low — documentation correctness, security-relevant) — stale module + registration comments still claim all three `game_*` tools are "Read-only (AutoRun)" and "cannot bypass the URL scheme allow-list"

**Files:** `src/tool/browser/mod.rs:696-703` (module section comment) and
`src/agent/factory.rs:524-525` (registration comment).

The S1 fix correctly rewrote `GameEvalTool`'s own doc comment + schema to say
"arbitrary script execution … approval required" and dropped the "read-only"
claim from the tool-level text. However, the **module-level section comment**
above the three tool structs was not updated:

```rust
// src/tool/browser/mod.rs:696-703
// ---- Live WebView2 (game) inspection tools ----
//
// These attach to the app's OWN WebView2 ... They let the agent see and read
// the live game state — screenshot the game as the human sees it, eval JS
// against the live page, or dump its DOM. Read-only (AutoRun): they inspect
// an already-open page and open no new navigation surface, so they cannot
// bypass the URL scheme allow-list.
```

and the factory registration comment:

```rust
// src/agent/factory.rs:524-525
// Live WebView2 (game) inspection tools — attach to the app's own
// WebView2 via CDP to inspect the game the human plays. Read-only.
```

Both still characterize **all three** tools as "Read-only (AutoRun)." This is
now wrong for `game_eval`, which is `NeedsApproval` and **not** read-only:
`webview_eval` runs caller-supplied JS against the app's *own* live WebView2
main frame, where `window.location.href = 'file://…'` would navigate the app's
main frame — bypassing the `navigate()` scheme allow-list (which is enforced
only on the headless-browser path, not on `webview_eval`). The module comment's
claim "they cannot bypass the URL scheme allow-list" is therefore **false** for
`game_eval` — which is precisely the reason S1 required approval-gating it.

This is the same inaccuracy S1 corrected at the tool level; it survives at the
module/registration level. It is a comment, not code, so it does not affect
runtime safety (the `NeedsApproval` gate is the actual enforcement), but it
undermines the fix's clarity and could mislead a future reader into treating
`game_eval` as read-only.

**Recommendation:** update the two comments to distinguish `game_eval`
(approval-gated, arbitrary JS, can navigate the app frame) from
`game_screenshot`/`game_snapshot` (AutoRun, genuinely read-only). For example,
replace "Read-only (AutoRun): … cannot bypass the URL scheme allow-list" with
"Read-only for screenshot/snapshot (AutoRun); `game_eval` runs arbitrary JS
against the live app frame and is approval-gated (it can navigate the app's
main frame, so it is NOT confined to the URL scheme allow-list)."

---

## Noted but acceptable (not findings)

- **Concurrent-loser detached handler task** (`src/browser/mod.rs:597-612`):
  when a loser of the concurrent-connect race takes the "use theirs" branch,
  its own handler task is detached (the local `task` `JoinHandle` is dropped,
  not aborted) and its `Arc<Browser>` is dropped. Dropping the connected
  `Browser` closes the CDP WebSocket, so the detached handler's
  `handler.next().await` returns `None` and the task self-terminates. No
  permanent leak, no double-stored handler, no lost connection. Cleaner would
  be to `task.abort()` explicitly on the loser path, but the current behavior
  is correct. (The prompt's "handler task aborted on reconnect/close" is
  satisfied: reconnect aborts the stale handler at lines 616-618; `close()`
  at 543-545 and `close_webview()` at 713-715 both abort.)
- **CSP `frame-src` breadth** (`src-tauri/tauri.conf.json:27-28`):
  `frame-src 'self' http://localhost:* http://127.0.0.1:* https://*` in both
  `csp` and `devCsp`. Broad, but the iframe is user-directed (the human
  explicitly types a game URL, typically a local dev server like
  `http://localhost:3000`), and the wildcards are necessary for the feature
  to load arbitrary hosted/local games. Consistent with the prior review's
  acceptance. Acceptable for a dev tool.
- **iframe `sandbox="allow-scripts allow-same-origin allow-forms allow-popups"`**
  (`frontend/src/components/views/GameView.tsx:58`): `allow-scripts
  allow-same-origin` is a known risk combination, but the loaded game is
  cross-origin to the app (e.g. `localhost:3000` vs the Tauri origin), so the
  same-origin policy protects the parent app's DOM — the game can only affect
  its own origin. Acceptable for user-loaded games, consistent with prior
  review.
- **`webview_page()` returns the first page target** (`src/browser/mod.rs:656`):
  correct for production, where the WebView2 has exactly one page (the app UI).
  The test mirrors this. Acceptable for the single-page design.
- **`close()` / `close_webview()` webview cleanup** (`src/browser/mod.rs:539-546`,
  `711-718`): correctly abort `webview_handler` and drop the connected
  `Browser`. Dropping a *connected* `Browser` closes the CDP WebSocket without
  killing the app's WebView2 process (the comment at 540-542 is accurate). No
  leaked handler tasks or browsers.
- **CDP port debug-gate** (`src-tauri/src/main.rs:41-45`): correctly
  `#[cfg(debug_assertions)]`; release builds do not expose CDP. (The
  release-side gap was R1, now fixed via the `cfg!` short-circuit.)

## Constitution compliance

- **Warning-free under `#![deny(warnings)]`** (`src-tauri/src/main.rs:7`,
  `src/lib.rs`): **no `#[allow(...)]` suppressions were added by this feature.**
  The two `#[allow(clippy::too_many_arguments)]` occurrences in the repo are in
  `src/agent/loop_impl.rs:256,295` — pre-existing and unrelated to this feature.
  `std::env::set_var` in `main.rs:42` is safe under edition 2021 (confirmed
  `edition = "2021"` in `Cargo.toml:4`); no `unsafe` and no warning.
  `new_with_webview_url` is `#[cfg(test)]` + `pub(crate)` and is used by the
  test, so no dead-code warning. `is_none_or` (used at lines 576, 608) is
  already used pre-existing at line 189, so the toolchain supports it.
- **Public functions have doc comments:** `webview_screenshot`, `webview_eval`,
  `webview_snapshot`, `close_webview`, `new_with_webview_url`, and the three
  tool structs (`GameScreenshotTool`, `GameEvalTool`, `GameSnapshotTool`) all
  carry doc comments. Private `ensure_webview`/`webview_page` do too. ✓
- **Code style** matches the repo (Ctx struct, `parse_args!` macro, `ToolSchema`,
  `SafetyLevel`, `ToolCategory`, `Error::Browser(format!(…))`, verbose
  comments). ✓
- **Factory test** (`src/agent/factory.rs:938-966`) includes the 3 new tool
  names (`game_screenshot`, `game_eval`, `game_snapshot`) and asserts the total
  count matches exactly. ✓
