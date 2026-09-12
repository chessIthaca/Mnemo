## Verdict: FINDINGS (0 high, 1 low)

One documentation-sync gap: README.md documents the first vendored crate (tao, line 35) and the Browser tab (lines 66, 118) but never mentions the second vendored crate (vendor/wry) or the OS-account SSO feature it delivers. Everything else verified correct — patch placement, patch wiring (Cargo.lock resolves wry to the vendored path source), regression guard, vendored-tree delta vs pristine 0.55.1 (exactly the three documented changes, no drift), multi-platform neutrality, and vendoring hygiene.

### Finding

**F1 (LOW) — README.md not updated for the vendored wry / Browser-tab SSO feature (documentation sync).**
- Where: README.md:35 (tao precedent), README.md:66 and README.md:118 (Browser tab description).
- README.md:35 documents the first vendored dependency — "fixed by a vendored tao 0.35.4 backport of upstream PR #1215 (`vendor/tao/PATCHES.md`)" — and README.md:66/:118 describe the Windows-only embedded-WebView2 Browser tab, but the README has zero mention of vendor/wry or that the Browser tab now signs in to AAD/MSA sites silently with the Windows primary account. Per the project constitution's documentation-sync review expectation ("A feature that ships with its docs not updated is an incomplete change"), the second vendored crate and its user-visible feature need a parallel mention.
- Fix (one sentence): extend the Browser-tab bullet at README.md:66, e.g. "…the agent can click, type, and screenshot — and, via a vendored wry 0.55.2 patch (`vendor/wry/PATCHES.md`), the Browser tab signs in to AAD/MSA sites silently with the Windows primary account (Edge-equivalent OS SSO)" — or add a standalone bullet mirroring the tao mention at line 35.

### Correctness — verified

1. **Patch placement** (vendor/wry/src/webview2/mod.rs:328-330): `options.set_allow_single_sign_on_using_os_primary_account(true);` sits inside the existing `unsafe {` block of `create_environment` (lines 326-364), immediately after `options.set_additional_browser_arguments(...)` (line 327) and before `CreateCoreWebView2EnvironmentWithOptions(` (line 346). Compared against the pristine 0.55.1 source (docs.rs crate source view): the surrounding function is byte-for-byte upstream plus exactly this one call and its two-line comment. No new unsafe block; the call is the same COM-setter pattern as the adjacent `set_are_browser_extensions_enabled` / `set_language` / `set_scroll_bar_style` calls on the same wrapper object.
2. **The setter exists and is wired to the COM property**: webview2-com 0.38.0 `src/options.rs` (docs.rs source view) defines `pub unsafe fn set_allow_single_sign_on_using_os_primary_account(&self, value: bool)` on `CoreWebView2EnvironmentOptions`, backed by the `allow_single_sign_on_using_os_primary_account: UnsafeCell<bool>` field (default `false`) and surfaced through the `ICoreWebView2EnvironmentOptions_Impl::SetAllowSingleSignOnUsingOSPrimaryAccount` COM method — so the flag genuinely reaches `put_AllowSingleSignOnUsingOSPrimaryAccount` when `CreateCoreWebView2EnvironmentWithOptions` receives `&ICoreWebView2EnvironmentOptions::from(options)`. Default `false` also confirms pristine wry 0.55.1 ships with SSO off.
3. **[patch.crates-io] wiring applies**: root Cargo.toml:173 adds `wry = { path = "vendor/wry" }` under the existing section with an accurate extended comment block (lines 164-170). Cargo.lock:7359-7361 now has `name = "wry"` / `version = "0.55.2"` with **no `source` and no `checksum`** — i.e. a path dependency, not a registry one. `tauri-runtime-wry 2.11.4` (the only wry dependent in the graph, Cargo.lock:5708) lists `"wry"` and resolves to this single entry; there is no second (registry) wry package. The renumber satisfies the `^0.55` pin, so no "patch was not used" situation exists — the shipped binary carries the patched wry.
4. **Regression guard genuinely guards the invariant** (src-tauri/tests/wry_sso_patch.rs): `create_environment_sets_os_sso` locates all three markers by byte offset — `CoreWebView2EnvironmentOptions::default()` (file line 325), `set_allow_single_sign_on_using_os_primary_account(true)` (line 330), `CreateCoreWebView2EnvironmentWithOptions(` (line 346) — and asserts `default < sso < create`. Each marker string occurs exactly once in the file (verified against the pristine source), so first-`find()` semantics are sound, and the ordering window is entirely inside the unsafe block. On pristine 0.55.1 the SSO marker is absent → the test panics with the explanatory message (FAILS, the login-wall bug); with the patch it PASSES. `vendored_wry_is_renumbered_0_55_2` asserts `version = "0.55.2"` (vendor/wry/Cargo.toml:16 — present). Style mirrors src-tauri/tests/tao_backport.rs (same header, helpers, doc-comment register). The test is pure file I/O off `CARGO_MANIFEST_DIR/../vendor/wry` — hermetic and cross-platform.

### Bugs / security — verified

- **SSO flag scope is as reasoned**: the flag applies to every WebView2 environment wry creates in-process. Verified against the app's webview-creation sites: the main window webview is the Tauri app UI (local frontend content), the agent-chat child is `WebviewBuilder::new("agent-chat", WebviewUrl::App("index.html".into()))` (src-tauri/src/main.rs:226 — local app content only), so OS-account SSO never engages there; the Browser tab child (src-tauri/src/ipc/browser_webview.rs:323/:404) is the only webview that navigates to remote content — the beneficiary. The offscreen browser is a separate Chromium process (chromiumoxide), untouched.
- **CDP debug port unaffected**: the remote-debugging port is injected via the `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` env var applied by the WebView2 loader at environment creation, independent of the options object; the patch does not touch the additional-args path.
- **No unsafe misuse**: one setter call inside the pre-existing unsafe block; no new unsafe surface.
- **No secrets** in any changed file.
- Residual risk accepted by design: pages loaded in the Browser tab can now silently authenticate as the OS user on AAD/MSA endpoints — the same risk profile as the user's own Edge browser, which is the feature's intent (user-driven navigation; agent `browser_*` tools are debug-gated per the module docs).

### Multi-platform neutrality — verified

- The patch lives in wry's Windows-only `webview2` module (cfg-gated by upstream design); macOS compiles `wkwebview` instead. The vendored Cargo.toml is identical to pristine 0.55.1 except the version line (verified field-by-field against the docs.rs pristine manifest), so macOS dependency resolution is unchanged.
- The regression test and the browser_webview.rs doc comment are platform-neutral prose/file-I/O; the doc comment sits inside the module that already documents the Windows-only platform gate.
- Cargo.lock churn (windows-sys dep bumps in `dirs-sys` 0.59.0→0.60.2, `os_pipe` 0.48.0→0.52.0, `rustix` 0.59.0→0.52.0, `winapi-util` 0.48.0→0.52.0) is re-resolution unification from changing wry's package identity (registry 0.55.1 → path 0.55.2) — semver-compatible, Windows-target-only deps, build+tests green per plan step 6. Benign; noted, not a finding.

### Vendoring hygiene — verified

- **Delta vs pristine 0.55.1 is exactly the three documented changes**: (a) Cargo.toml `version = "0.55.1"` → `"0.55.2"` (everything else field-identical to the published manifest); (b) the one call + two-line comment in src/webview2/mod.rs create_environment; (c) PATCHES.md added. Corroborated by mtimes (only Cargo.toml, PATCHES.md, and src/webview2/mod.rs carry recent mtimes; every other file retains the registry-cache mtime) and by a full-tree search: the string "Mnemo" occurs exactly once in vendor/wry (the patch comment at src/webview2/mod.rs:328). No target/, no .git/, no .cargo/ config hijack, no stray edits.
- **PATCHES.md is accurate**: why (Browser tab SSO, no Tauri environment hook), what (one call after set_additional_browser_arguments), scope note (all in-process environments; inert on local-content webviews; CDP port unaffected), regression-guard reference, renumber rationale, upstream references (wry repo tag, MS Learn ICoreWebView2EnvironmentOptions), and the pruned-artifacts list — which matches the actual tree (top level holds only Cargo.toml, Cargo.toml.orig, LICENSE-APACHE, LICENSE-MIT, LICENSE.spdx, PATCHES.md, README.md, build.rs, examples/, src/).
- **Version renumber consistent** everywhere: vendored Cargo.toml, Cargo.lock, root Cargo.toml comment, PATCHES.md, PLAN.md row, test assertion — all say 0.55.2.
- **Workspace/git plumbing**: root Cargo.toml `exclude = ["vendor"]` (line 155) covers vendor/wry; .gitignore has no pattern that matches anything under vendor/wry (fully committable); Cargo.lock change is staged for commit.

### Documentation sync — besides F1

- PLAN.md:249 new "wry (WebView2 backend)" row — accurate (dates, mechanism, scope, guard, PATCHES.md pointer). ✓
- src-tauri/src/ipc/browser_webview.rs:15-21 new "## OS-account single sign-on" module-doc section — accurate, well-placed before the platform-gate section. ✓
- Root Cargo.toml:164-170 comment block — accurate. ✓
- README.md — see F1. ✗

### Bookkeeping (benign)

- .coding/plans/0631168e.md — complete plan, 6/6 steps, includes the mid-plan path correction. ✓
- .coding/knowledge/spec/2027-01-07-project-root-is-c-agenticcoding-worktree-constit.md — accurate record of the stale constitution path; useful. ✓
- .coding/backlog.jsonl — one unrelated status flip (item 35b94671, stable-keys, in_flight → done, plan 1f74ebe2) riding along; legitimate session bookkeeping in the mergeable side-car, not a finding.

### Verdict recap

Fix F1 (one README.md sentence), then this change is complete: the SSO patch is correctly placed and wired, the regression guard fails-on-pristine/passes-on-patch, the vendored tree is pristine-plus-exactly-the-documented-delta, and nothing breaks the macOS build or violates the Windows-only gate.
