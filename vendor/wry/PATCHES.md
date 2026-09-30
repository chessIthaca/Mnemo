# Vendored wry 0.57.2 — 0.57.0 + OS-account SSO patch + hard-reload patch

This directory is a pristine copy of `wry 0.57.0` (from the crates.io
registry cache) with **two** local patches and the version renumbered to
`0.57.2`. It succeeds the 0.55.3 copy — see "What changed vs the 0.55
port" below.

## Why this exists

The app's Browser tab is a native child WebView2 (see
`src-tauri/src/ipc/browser_webview.rs`). Signing in to Microsoft/AAD
properties (e.g. `forms.cloud.microsoft`) from that webview demanded an
interactive login that the org's Conditional Access then rejected,
because the embedded WebView2 — unlike Edge — does not use the Windows
primary account for silent single sign-on.

WebView2 exposes exactly this switch:
`ICoreWebView2EnvironmentOptions::AllowSingleSignOnUsingOSPrimaryAccount`
(MS Learn: "used to enable single sign on with AAD and personal MSA
resources inside WebView; all AAD accounts connected to Windows are
supported"). wry builds the environment options itself and never sets
the flag, and Tauri 2's `WebviewBuilder` exposes no environment-options
hook — so app code cannot set it without patching wry. (Re-check on each
Tauri bump: if upstream ever adds such a hook, this patch becomes
droppable.)

## Patch 1: OS-account SSO

`src/webview2/mod.rs`, `create_environment`: one added call right after
`set_additional_browser_arguments`:

```rust
options.set_allow_single_sign_on_using_os_primary_account(true);
```

This applies to every WebView2 environment wry creates in-process (main
app webview, agent-chat child, Browser tab child). The first two only
ever load local app content, so SSO there is inert; the Browser tab is
the beneficiary. The app's CDP remote-debugging port is injected via the
`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` env var (`src/webview_args.rs`,
set in `src-tauri/src/main.rs` before the Tauri builder), which the
WebView2 loader applies independently of the options object — unaffected
by this patch.

## Patch 2: hard reload

The Browser tab's Reload button performed a normal WebView2 reload —
`ICoreWebView2::Reload()` serves cached subresources, so pages came
back out of sync with their server state (stale JS/CSS after a
redeploy; only devtools could force a real refresh). WebView2 has no
native hard-reload API and Chromium ignores `location.reload(true)`,
but the DevTools protocol has exactly this: CDP `Page.reload` with
`{"ignoreCache":true}` via `ICoreWebView2::CallDevToolsProtocolMethod`
(Page.reload + ignoreCache is documented in the CDP spec — see Upstream
below; MS Learn's own example for the API is `Runtime.evaluate`). Tauri's
`Webview::reload` exposes no ignore-cache hook, so the patch lives here.

`src/webview2/mod.rs`, `reload()`: issues the CDP call (fire-and-forget
— the completion handler discards both the error code and the JSON
result) and falls back to plain `Reload()` when the call itself fails.
Every in-process webview2 reload goes through this path, and only the
Browser tab's Reload button ever calls it (the main + agent-chat
webviews are never reloaded), so the blast radius is exactly that
button.

## What changed vs the 0.55 port

- Base moved `0.55.1` -> `0.57.0` (published 2026-09-08); renumbered
  `0.57.2`. Both patches carried over **verbatim** — the anchors are
  unchanged: `set_additional_browser_arguments` is still immediately
  followed by `set_are_browser_extensions_enabled` in
  `create_environment`, and `reload()` was still the stock
  `unsafe { self.webview.Reload() }` body.
- Dependency shifts in 0.57.0: `webview2-com` 0.39, `windows` /
  `windows-core` 0.62 (Windows 7 support dropped), `dirs` 6 -> 7;
  MSRV 1.77 -> 1.85 (the repo's toolchain already builds the 1.85-MSRV
  fastembed 7 tree, so this costs nothing).
- The guard tests' version pins move with it: `wry_sso_patch.rs` and
  `wry_hard_reload_patch.rs` assert `0.57.2` (they asserted `0.55.3`).
- The `HSTRING` import and the `webview2_com::{...Win32::*, *}` glob
  already provision the CDP handler type, so neither patch needed an
  import change.

## Keeping it honest

`src-tauri/tests/wry_sso_patch.rs` asserts the SSO invariant: the SSO
call sits in `create_environment` between options construction and
`CreateCoreWebView2EnvironmentWithOptions`, and the vendored version is
0.57.2. `src-tauri/tests/wry_hard_reload_patch.rs` asserts the
hard-reload invariant: `reload()`'s primary path is the CDP
`Page.reload` + `ignoreCache` call with `Reload()` only as the error
fallback, and the vendored version is 0.57.2. Both fail on pristine
0.57.0, so a vendor refresh can never silently drop either patch.

## Renumbering

`tauri-runtime-wry` (via tauri 2.12) pins `wry = "0.57"` (`^0.57`); the
renumber to 0.57.2 satisfies the pin and makes the patched copy
distinguishable from the registry's 0.57.0 in `Cargo.lock` (crates.io
has published neither 0.57.1 nor 0.57.2 — checked 2026-09-30).

## Upstream

- wry: https://github.com/tauri-apps/wry (tag `wry-v0.57.0`)
- MS Learn — ICoreWebView2EnvironmentOptions:
  https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environmentoptions
- MS Learn — ICoreWebView2::CallDevToolsProtocolMethod:
  https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2#calldevtoolsprotocolmethod
- CDP — Page.reload:
  https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-reload

Registry packaging artifacts (`.cargo-ok`, `.cargo_vcs_info.json`,
`Cargo.lock`, `Cargo.toml.orig`, `CHANGELOG.md`, `MOBILE.md`,
`renovate.json`, `rustfmt.toml`, `SECURITY.md`, `.gitignore`,
`.license_template`, `.cargo/`) were pruned, as in the previous vendored
copies, to keep the tree to source + licenses + README.
