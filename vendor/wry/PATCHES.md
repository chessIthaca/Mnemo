# Vendored wry 0.55.3 — 0.55.1 + OS-account SSO patch + hard-reload patch

This directory is a pristine copy of `wry 0.55.1` (from the crates.io
registry cache) with **two** local patches and the version renumbered to
`0.55.3`.

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
supported"). wry 0.55.1 builds the environment options itself and never
sets the flag, and Tauri 2's `WebviewBuilder` exposes no environment
hook — so app code cannot set it without patching wry.

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
below; MS Learn's own example for the API is `Runtime.evaluate`). Tauri
2.11.5's `Webview::reload` exposes no hook for it, so the patch lives
here.

`src/webview2/mod.rs`, `reload()`: issues the CDP call (fire-and-forget
— the completion handler discards both the error code and the JSON
result) and falls back to plain `Reload()` when the call itself fails. Every in-process
webview2 reload goes through this path, and only the Browser tab's
Reload button ever calls it (the main + agent-chat webviews are never
reloaded), so the blast radius is exactly that button.

## Keeping it honest

`src-tauri/tests/wry_sso_patch.rs` asserts the SSO invariant: the SSO
call sits in `create_environment` between options construction and
`CreateCoreWebView2EnvironmentWithOptions`, and the vendored version is
0.55.3. `src-tauri/tests/wry_hard_reload_patch.rs` asserts the
hard-reload invariant: `reload()`'s primary path is the CDP
`Page.reload` + `ignoreCache` call with `Reload()` only as the error
fallback, and the vendored version is 0.55.3. Both fail on pristine
0.55.1, so a vendor refresh can never silently drop either patch.

## Renumbering

`tauri-runtime-wry 2.11.4` pins `wry = "0.55.0"` (`^0.55`); the renumber
to 0.55.3 satisfies the pin and makes the patched copy distinguishable
from the registry's 0.55.1 and the previous vendored 0.55.2 in
`Cargo.lock` (crates.io never published a 0.55.2 or 0.55.3).

## Upstream

- wry: https://github.com/tauri-apps/wry (tag `wry-v0.55.1`)
- MS Learn — ICoreWebView2EnvironmentOptions:
  https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2environmentoptions
- MS Learn — ICoreWebView2::CallDevToolsProtocolMethod:
  https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2#calldevtoolsprotocolmethod
- CDP — Page.reload:
  https://chromedevtools.github.io/devtools-protocol/tot/Page/#method-reload

Registry packaging artifacts (`.cargo-ok`, `.cargo_vcs_info.json`,
`Cargo.lock`, `CHANGELOG.md`, `MOBILE.md`, `renovate.json`,
`rustfmt.toml`, `SECURITY.md`, `.gitignore`, `.license_template`,
`.cargo/`) were pruned to mirror `vendor/tao`.
