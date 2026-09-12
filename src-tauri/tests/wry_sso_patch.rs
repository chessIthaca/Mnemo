// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Regression guard for the Browser tab's missing OS-account single sign-on.
//!
//! The symptom (2026-09-07): signing in to Microsoft/AAD properties (e.g.
//! `forms.cloud.microsoft`) from the Browser tab's native child WebView2
//! demanded an interactive login that the org's Conditional Access then
//! rejected — the embedded WebView2, unlike Edge, does not use the Windows
//! primary account for silent single sign-on.
//!
//! The fix: WebView2 exposes exactly this switch —
//! `ICoreWebView2EnvironmentOptions::AllowSingleSignOnUsingOSPrimaryAccount`
//! (MS Learn: "used to enable single sign on with AAD and personal MSA
//! resources inside WebView; all AAD accounts connected to Windows are
//! supported"). wry 0.55.1 builds the environment options itself and never
//! sets the flag, and Tauri 2's `WebviewBuilder` exposes no environment
//! hook — so the flag cannot be set from app code. The vendored wry 0.55.3
//! (`vendor/wry`, see PATCHES.md) patches `create_environment` to call
//! `set_allow_single_sign_on_using_os_primary_account(true)`.
//!
//! This test is the deterministic stand-in: it asserts the patch's
//! source-level invariants against the vendored tree (`vendor/wry`,
//! workspace root). It FAILS on the pristine 0.55.1 source (SSO off — the
//! login-wall bug) and PASSES once the patch is applied, so OS SSO can
//! never silently disappear via a vendor refresh or a botched merge.

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

/// The patch's core: `create_environment` must set
/// `AllowSingleSignOnUsingOSPrimaryAccount(true)` on the options object,
/// between constructing it and handing it to
/// `CreateCoreWebView2EnvironmentWithOptions`. Asserted by byte offset so
/// the call cannot be dropped or moved out of the environment-creation
/// path (e.g. behind a flag that is never set).
#[test]
fn create_environment_sets_os_sso() {
    let webview2 = read_vendored("src/webview2/mod.rs");
    let options_default = webview2
        .find("CoreWebView2EnvironmentOptions::default()")
        .unwrap_or_else(|| {
            panic!(
                "create_environment must construct CoreWebView2EnvironmentOptions; missing in {}",
                vendor_wry_dir().join("src/webview2/mod.rs").display()
            )
        });
    let sso_call = webview2
        .find("set_allow_single_sign_on_using_os_primary_account(true)")
        .unwrap_or_else(|| {
            panic!(
                "create_environment must call set_allow_single_sign_on_using_os_primary_account(true) \
                 — without it the Browser tab's WebView2 demands an interactive login that \
                 Conditional Access rejects (2026-09-07); missing in {}",
                vendor_wry_dir().join("src/webview2/mod.rs").display()
            )
        });
    let create_env = webview2
        .find("CreateCoreWebView2EnvironmentWithOptions(")
        .unwrap_or_else(|| {
            panic!(
                "CreateCoreWebView2EnvironmentWithOptions not found in {}",
                vendor_wry_dir().join("src/webview2/mod.rs").display()
            )
        });
    assert!(
        options_default < sso_call && sso_call < create_env,
        "the OS-account SSO flag must be set on the options object between its construction \
         and the CreateCoreWebView2EnvironmentWithOptions call — a call outside that window \
         (or a dropped call) leaves the Browser tab without Edge-equivalent silent SSO."
    );
}

/// The renumber: the vendored crate must present itself as 0.55.3 so
/// `[patch.crates-io]` satisfies tauri-runtime-wry's `wry = "^0.55"` pin
/// while staying distinguishable from the registry's 0.55.1 in Cargo.lock.
/// (0.55.3 = 0.55.1 + the SSO patch + the hard-reload patch — see
/// tests/wry_hard_reload_patch.rs.)
#[test]
fn vendored_wry_is_renumbered_0_55_3() {
    let manifest = read_vendored("Cargo.toml");
    assert!(
        manifest.contains("version = \"0.55.3\""),
        "vendored wry must be renumbered 0.55.3 (0.55.1 + OS-account SSO patch + hard-reload \
         patch) so the [patch.crates-io] entry satisfies tauri-runtime-wry's ^0.55 requirement \
         and the patched copy is distinguishable from the registry's 0.55.1"
    );
}
