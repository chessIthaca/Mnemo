## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of plan 47735e3c (backlog 3f838ea1, `wt/mnemo`). **Both round-1 findings are verified fixed** — the `browser://url-changed` payload mismatch is closed at every seam, and the setBrowserUrl test is re-homed into its own describe — and the diff is exactly the round-1 change set plus the two fixes. One new LOW: the new Rust regression test `browser_url_payload_is_url_keyed` was appended **after** the closing brace of `#[cfg(test)] mod tests` — it sits at file scope, not inside the tests module as the fix description states. Harmless on this toolchain (rustc strips `#[test]` items from non-test builds — verified in the rustc source), but a convention break and a fix/code mismatch.

## Round-1 HIGH-1 (payload mismatch) — FIXED, verified

- Shared helper `browser_url_payload(url: impl Into<String>) -> serde_json::Value` (src-tauri/src/ipc/browser_webview.rs:375-383) constructs `serde_json::json!({ "url": url.into() })` — an object, never a bare string.
- **Both** emits route through it: the page-load hook in `child_webview_builder` (:409 — `app_for_loads.emit(BROWSER_URL_CHANNEL, browser_url_payload(url.to_string()))`) and the reveal emit in `ensure_for_agent_impl` (:486 — `app.emit(BROWSER_REVEAL_CHANNEL, browser_url_payload(normalized.clone()))`).
- These are the **only** emit sites of either channel in src-tauri (identifier search across `**/*.rs`: every other hit is a const declaration, doc comment, or the new test's doc comment) — no bare-string emit remains on either browser channel.
- TS interfaces match the emitted objects: `BrowserUrlPayload { url: string }` (frontend/src/lib/tauri.ts:1850-1853) and `BrowserRevealPayload { url: string }` (:1836-1839). `useAgentEvents`' `ensureBrowserUrlListenerStarted` reads `payload.url` — defined at runtime now; round-1's failure trace (bare string → `payload.url` undefined → store guard no-ops → box never syncs) is closed at the constructing seam.
- Reveal channel: `handleBrowserReveal(): void` (frontend/src/hooks/browserReveal.ts:23) takes no payload — the object emit is shape alignment only, zero behavior change, exactly as described.
- Channel doc synced: `BROWSER_URL_CHANNEL`'s doc (:367-373) now reads "Payload: [`browser_url_payload`] — an object with a `url` field (never a bare string…)".
- Regression test `browser_url_payload_is_url_keyed` (:1041-1047) asserts the helper serializes to `{"url":"https://example.com/page"}` — pinning the shape at the single constructing seam both emits share, exactly the regression test round-1 asked for ("a unit test on a payload-constructing helper"). Its placement is the new LOW-1 below.

## Round-1 LOW-1 (test placement) — FIXED, verified

- The setBrowserUrl test now lives in its own `describe("browserUrl (the child webview's current URL)")` (frontend/src/hooks/useAgentStore.test.ts:2632-2641) with a self-contained `useAgentStore.setState({ browserUrl: "" })` reset.
- Single occurrence in the file (search: one match at :2633) — nothing remains inside the `revealRightPanelTab + requestFileOpen` describe.


## NEW LOW-1: `browser_url_payload_is_url_keyed` sits at file scope, OUTSIDE `mod tests`

**Evidence** (src-tauri/src/ipc/browser_webview.rs):
- `#[cfg(test)] mod tests` spans :725-1033 (closing brace at :1033).
- The new test (doc comment + `#[test] fn browser_url_payload_is_url_keyed`) is at :1035-1047 — **after** the module's closing brace, i.e. an ungated file-scope item of `ipc::browser_webview`. Its 4-space indentation makes it *look* inside.
- The fix description says "appended to the tests module" — the code shows it was appended *after* the module (the edit anchored one brace too low; same genre as round-1's LOW-1).

**Why LOW, not HIGH (verified, not assumed):** no build break and no binary bloat. rustc removes `#[test]`-annotated items entirely when not compiling with `--test` — `expand_test_or_bench` in rustc_builtin_macros/src/test.rs: "If we're not in test configuration, remove the annotated item" → `return vec![]`. In normal builds the fn is stripped before dead-code analysis, so there is no `never used` warning under `#![deny(warnings)]` and nothing ships in release binaries. This is consistent with the reported green run: src-tauri has four integration tests (tests/tao_backport.rs, webview_udf.rs, wry_hard_reload_patch.rs, wry_sso_patch.rs), and per the cargo book `cargo test` then also builds the bin normally — the stripped fn raised no warning there. Under `--test` the fn is kept, collected, and runs (included in the reported 16).

**Why it is still a finding:** every other test in this 1047-line file — and across the repo (100 `mod tests` declarations) — lives inside a `mod tests` module (inline or a separate `*/tests.rs` file included as one); this is the only test sitting outside one. Anyone scanning `mod tests` misses it, and a future non-`#[test]` helper placed next to it outside the module would NOT be stripped and would break the warning-free build.

**Fix:** move the block (:1035-1047) to just above the module's closing `}` (:1033) — the indentation already matches — and re-run `cargo test` (the test must still run; nothing else changes).

## Containment — nothing else changed since round 1

- The uncommitted diff is exactly: the round-1 change set (MarkdownImpl default `a: MarkdownLink` override + source-contract test, BrowserView sync effect, useAgentEvents module-scope listener, useAgentStore `browserUrl` state/action + tests, openChatLink.test contract, tauri.ts channel + `BrowserUrlPayload`/`onBrowserUrlChanged`, vitest.config registration, browserUrl.ts/.test.ts, the `child_webview_builder` refactor) **plus** the round-2 browser_webview.rs delta (payload helper, both emits, channel doc, the new test) **plus** the useAgentStore.test.ts describe move **plus** the backlog.jsonl status flip (expected bookkeeping).
- Cross-checked against round-1's quoted evidence: the bare-string emit it quoted at :394-397 is now the object emit at :409; the `normalized.clone()` reveal emit at :473 is now :486; the "Payload: the URL string." doc at :370 is now the object doc at :367-373. The line shifts match the round-2 insertions — nothing else moved.

## Round-1 "Verified correct" — stands

Spot-checked, unchanged since round 1: MarkdownImpl `components={{ a: MarkdownLink, ...components }}` (caller spread still wins); BrowserView's sync effect before the `!supported` early return, deps `[browserUrl]` only (the box stays an input); both creation sites (`browser_webview_ensure`, `ensure_for_agent_impl`) share `child_webview_builder`; emits stay best-effort (`let _ =`); platform neutrality (browser_webview is the sanctioned Windows-only WebView2 area; the frontend changes are platform-neutral); doc comments match the landed behavior.

## Tests

Reported green by the parent (read-only reviewer — not re-run): `cargo test` 2485 + 16 passed, 0 failed, warning-free, exit=0 (the new test included); vitest 88 files / 1226 tests, exit=0. Consistent with the code-level verification above.

**Commit note:** include the round-1 report, this round-2 report, and `.coding/plans/47735e3c.md` in the commit (round-1 already flagged the plan file).