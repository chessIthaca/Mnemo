// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Regression guard for the Browser tab's cache-serving reload.
//!
//! The symptom (2027-01-11): the Browser tab's Reload button performed a
//! normal WebView2 reload — `ICoreWebView2::Reload()` serves cached
//! subresources, so pages came back out of sync with their server state
//! (stale JS/CSS after a redeploy; only devtools could force a real
//! refresh).
//!
//! The fix: WebView2 has no native hard-reload API and Chromium ignores
//! `location.reload(true)`, but the DevTools protocol has exactly this —
//! CDP `Page.reload` with `{"ignoreCache":true}` via
//! `ICoreWebView2::CallDevToolsProtocolMethod` (Page.reload + ignoreCache
//! is documented in the CDP spec). Tauri 2.11.5's
//! `Webview::reload` exposes no hook for it, so the vendored wry 0.55.3
//! (`vendor/wry`, see PATCHES.md) patches the webview2 `reload()` to
//! issue the CDP call, falling back to `Reload()` when the call itself
//! fails.
//!
//! This test is the deterministic stand-in: it asserts the patch's
//! source-level invariants against the vendored tree (`vendor/wry`,
//! workspace root). It FAILS on the pristine 0.55.1 source (cache-serving
//! reload — the out-of-sync bug) and PASSES once the patch is applied,
//! so the hard reload can never silently disappear via a vendor refresh
//! or a botched merge.

use std::path::PathBuf;

/// The vendored wry crate root, resolved relative to this package
/// (`src-tauri`) so the test is hermetic and repo-relative.
fn vendor_wry_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vendor")
        .join("wry")
}

fn read_vendored(rel: &str) -> String {
    let path = vendor_wry_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read vendored wry file {}: {e}", path.display()))
}

/// The patch's core: webview2 `reload()` must issue CDP `Page.reload`
/// with `ignoreCache` via `CallDevToolsProtocolMethod` as its
/// PRIMARY path, with plain `Reload()` only as the error fallback —
/// asserted by byte offset so the CDP call cannot be dropped or demoted
/// behind the cache-serving path.
#[test]
fn reload_issues_cdp_page_reload_with_ignore_cache() {
    let webview2 = read_vendored("src/webview2/mod.rs");
    let reload_fn = webview2.find("pub fn reload(&self)").unwrap_or_else(|| {
        panic!(
            "webview2 reload() not found in {}",
            vendor_wry_dir().join("src/webview2/mod.rs").display()
        )
    });
    let page_reload = webview2.find("\"Page.reload\"").unwrap_or_else(|| {
        panic!(
            "reload() must target CDP Page.reload — the only hard-reload mechanism WebView2 \
             offers; missing in {}",
            vendor_wry_dir().join("src/webview2/mod.rs").display()
        )
    });
    let ignore_cache = webview2.find("\"ignoreCache\"").unwrap_or_else(|| {
        panic!(
            "reload() must pass ignoreCache:true — without it Page.reload is a normal \
             reload again; missing in {}",
            vendor_wry_dir().join("src/webview2/mod.rs").display()
        )
    });
    let cdp_call = webview2
        .find("CallDevToolsProtocolMethod(")
        .unwrap_or_else(|| {
            panic!(
                "reload() must call CallDevToolsProtocolMethod — without it the Browser \
                 tab's Reload serves cached subresources and pages come back out of sync \
                 (2027-01-11); missing in {}",
                vendor_wry_dir().join("src/webview2/mod.rs").display()
            )
        });
    let fallback = webview2.find("self.webview.Reload()").unwrap_or_else(|| {
        panic!(
            "reload() must keep the Reload() fallback for CDP call failures; missing in {}",
            vendor_wry_dir().join("src/webview2/mod.rs").display()
        )
    });
    assert!(
        reload_fn < page_reload
            && page_reload < ignore_cache
            && ignore_cache < cdp_call
            && cdp_call < fallback,
        "reload()'s primary path must be the CDP Page.reload(ignoreCache) call, with \
         Reload() only as the error fallback — a reload() that calls Reload() first (or \
         drops the CDP call) serves cached subresources and the Browser tab goes stale."
    );
}

/// The renumber: the vendored crate must present itself as 0.55.3 so
/// `[patch.crates-io]` satisfies tauri-runtime-wry's `wry = "^0.55"` pin
/// while staying distinguishable from the registry's 0.55.1 and the
/// previous vendored 0.55.2 in Cargo.lock (crates.io never published a
/// 0.55.2 or 0.55.3).
#[test]
fn vendored_wry_is_renumbered_0_55_3() {
    let manifest = read_vendored("Cargo.toml");
    assert!(
        manifest.contains("version = \"0.55.3\""),
        "vendored wry must be renumbered 0.55.3 (0.55.1 + SSO patch + hard-reload patch) so \
         the [patch.crates-io] entry satisfies tauri-runtime-wry's ^0.55 requirement and the \
         patched copy is distinguishable from the registry's versions"
    );
}
