## Verdict: PASS

Review of plan 541b6381 ("Allow file:// URLs in the debug browsers") — full uncommitted diff on `wt/agenticcoder` (10 files + `.coding/backlog.jsonl` bookkeeping). The relaxation is correct, well-contained, honestly documented, and tested in both directions. No blocking findings; two informational notes at the end.

### 1. Correctness — ✓

- **The guard is exactly as claimed** (`src/browser/mod.rs:1391-1432`): `candidate` is lowercased before matching; the `"file" if rest.starts_with("//")` arm returns the original `trimmed` unchanged (so `FILE:///C:/…` case is preserved — valid per RFC 3986 scheme case-insensitivity); `file:`, `file:foo`, `file:/C:/page.html` fall to the precise "must use the file:///path form" error; the generic scheme-rejection error auto-lists the new scheme via `ALLOWED_SCHEMES.join(", ")` (`:1428`), so no stale message text. Drive paths (`C:\foo`) still die in `is_scheme_like` (candidate `c`, non-digit rest) and `javascript:`/`about:` keep the generic rejection. The http/https/data arms and omnibox autodetection are untouched (`file:` inputs return before autodetection can misfire).
- **Single choke point holds.** The code graph lists exactly these callers of `normalize_url`, and I read each: `browser_normalize_url` (UI URL bar, `ipc/browser.rs:126`), `browser_webview_ensure` (`:310`), `ensure_for_agent_impl` (`:374`), `browser_webview_navigate` (`:546`), `BrowserManager::navigate` (`:355`), `webview_navigate` (`:1070`). Both `WebviewUrl::External` construction sites parse the *normalized* string. No path bypasses the check.
- **`url_host` fallback** (`browser_webview.rs:75-83`) is correct and platform-neutral: `file:///C:/x.html` → authority split yields `""` → falls back to the whole URL, so watchdog notes stay informative; `http(s)://host/…` and `data:` behavior unchanged.
- **Tests assert the new behavior and would fail at HEAD**: `file://` rows moved into the pass-through list with four shapes (Windows drive, POSIX, UNC host, uppercase scheme — `:1680-1691`); three malformed `file:` forms added to the reject list with a message-content assert (`:1714-1720`, `:1731-1735`); `navigate_rejects_disallowed_schemes` keeps `javascript:`/`about:` and still proves nothing spawns (`:1641-1660`); the new `#[ignore]d` integration test builds the canonical file URL correctly for both platforms (leading-`/` check → `file://{p}` vs `file:///{p}`, `:1615-1622`) and proves a real headless render via `document.title`. `tempfile` is already a dev-dependency (`Cargo.toml:97`) — no manifest change needed, and none was made.

### 2. Security — ✓ acceptable, containment verified

- The relaxation is the user's explicit request; the mitigation is unchanged and real: `offscreen_browser_navigate` and `browser_navigate` are both `SafetyLevel::NeedsApproval` (verified in both tool impls), so every `file://` navigation shows the user the exact URL before it loads.
- **The old sandbox rationale stays as intact as the feature allows**: the headless Chromium launch args contain **no** `--allow-file-access-from-files` (searched `src/**/*.rs` — no matches), so a loaded `file://` page cannot XHR/fetch neighboring local files; one approved navigation renders exactly the file the URL names. `browser_eval` remains NeedsApproval. This is the same accepted posture as the pre-existing `data:` entry (2026-08-13 review M3).
- **No SSRF widening**: explicit `http://` to any host was already allowed, so network reach is unchanged; Chromium blocks top-frame `file://` navigation originating from web content, so there is no `http→file` redirect chain; **`web_fetch.rs` is untouched** (absent from the changed-file set) and stays http(s)-only.
- **Child WebView2 creation** (`WebviewUrl::External(normalized)` at `:323`/`:404`) now accepting `file://` is the feature itself; the child is built with a plain `WebviewBuilder` (no added IPC capabilities), and the whole surface stays behind `child_webview_supported()` → `cfg!(windows)` — the sanctioned Windows-only gate. No scheme-confusion path found: the guard is anchored on the lowercased scheme token before any parsing, and backslash/authority-less forms are rejected.
- Docs state the residual honestly rather than pretending it away (`.coding/browser-debugging.md:97-105`, module doc `:20-28`, `ALLOWED_SCHEMES` doc `:96-101`).

### 3. Project rules — ✓

- **Doc sync**: every shipped text that named the old list was updated — module doc, `navigate` doc, `ALLOWED_SCHEMES` doc, `normalize_url` doc + error text, both tool schemas (`src/tool/browser/mod.rs:87-90`, `:957-962`), `ipc/browser.rs:117-123`, `browser_webview.rs:536-540` + `url_host` doc, `tauri.ts:1595-1603`, `BrowserView.tsx:25-28` + placeholder/hint, `.coding/browser-debugging.md:48` + Notes. README.md and PLAN.md make no scheme claims (searched — no matches); historical `.coding/` records are correctly left as history. No stale "file:// rejected" claim remains in shipped docs or code.
- **Warning-free**: implementer ran `cargo test --features browser` green under `#![deny(warnings)]`; the diff is strings/arms/docs with no new imports in lib code and no `#[allow]`. (As a read-only reviewer I cannot execute the suite myself — consistent with prior reviews, this is static verification plus the implementer's run.)
- **Multi-platform**: the integration test handles both POSIX and Windows file-URL forms; `url_host` is pure string ops; the `file:///C:` placeholder examples render only where the Browser tab exists at all (non-Windows gets the red panel instead of the URL bar), so they are correct by construction; error text shows both `file:///path` and `file:///C:/` forms.

### 4. factory.rs ceiling raise — ✓ documented, justified, not silent drift

- Each of the three entries carries a dated (2026-12-08) comment naming the cause (the unified Browser-tab tool set post-dating the 2026-12-04 raise), the measured feature-gated values (23_948 / 20_761 / 21_109 chars vs. ceilings 23_600 / 20_400 / 20_700 → ~350/360/410 over), matching the "~350 over"/"~410 over" notes. The test's own doc (`factory.rs:1401-1409`) explicitly sanctions "a deliberate edit to this test, with the new tool justified" — which this is.
- Arithmetic closes: ~+90 chars of new schema text across the two browser navigate tools → ~24_038 / 20_851 / 21_199 ≤ 24_200 / 21_100 / 21_300. The test is un-gated at the fn level with a `#[cfg(feature = "browser")]` inner block, so CI's default-features `cargo test --workspace` passes either way, while the feature-gated suite — which was *already failing at HEAD* (verified by the implementer via stash) — is repaired rather than newly broken. The raise was required, not convenient.

### Informational notes (no action required)

- **N1**: bare `file://` and `file:///` pass the guard (`"//"` with an empty path) and reach Chromium as a no-op/empty page rather than a clear error. Cosmetic; mirrors the already-permissive `data:` edge; zero security impact.
- **N2**: the UNC form `file://server/share` (test-covered as valid) can trigger SMB resolution on Windows when approved. Inherent to the file scheme, behind the same approval prompt, and adds no network reachability that an explicit `http://` navigation didn't already have.

**Limitation**: read-only reviewer — did not execute `cargo test`/the ignored integration test; verified statically (graph + reads + searches) against the implementer's reported green runs (`cargo test`, `--features browser`, ignored test passed when run).


### Addendum — full-diff re-verification (diff was truncated on first pass)

Re-ran `git diff HEAD` untruncated after writing the verdict above: **every hunk matches what was reviewed; PASS stands.** Three precision notes for the committing agent:

1. **`.coding/backlog.jsonl` (the hunk previously seen only as a stat line)** removes both entries — `24e1c98e` (status *pending*: "update plan callable in reviewing, main agent only") and `8b8f40d2` (status *done*: graph-tool discipline) — leaving a blank line. `backlog_list` confirms the **live queue is empty too**, so store and file agree (this is NOT the PowerShell JSON round-trip corruption pattern, cf. memory 7c692858). Both items' substance is delivered by later merged work: `update_plan` in Reviewing landed 2026-08-31 with the main-agent-only / regression_test-only restriction, and the strict `ToolFilter::Reviewer` allow-list excludes plan tools entirely. Characterization: **deliberate queue drain, not data loss** — but confirm it's intended before committing, since the commit makes the empty queue permanent in history.
2. **Untracked files must join the commit**: `git status` shows `?? .coding/plans/541b6381.md` and `?? .coding/reviews/2026-12-08-file-url-browsers-review.md`. The closing sequence requires the review report in the commit; `git add` both (`.coding/` travels with git per the branch policy).
3. **Optional pre-existing nit, not part of the diff**: the `normalize_url` doc comment still says the choke point is shared by "…the UI URL bar, and the **iframe** navigation" — the child is a separate top-level webview now, not an iframe (its own newer docs say so). One-word drive-by fix if desired; it predates this change and does not affect the verdict.

Non-blocking style observation on the factory.rs hunk: the `ExecutingResearch` explanatory comment sits *after* its `21_100` entry (and before the Reviewing comment) — content complete and dated, purely ordering cosmetics.

Reviewer limitation (unchanged): read-only — tests were not executed by this reviewer; static verification against the implementer's reported green runs (`cargo test`, `cargo test --features browser`, ignored file:// integration test passed when run).
