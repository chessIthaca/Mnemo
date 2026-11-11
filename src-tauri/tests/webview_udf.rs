// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Regression guard for the second-instance blank-white-window bug
//! (2027-01-13, user report: a second production mnemo instance on the same
//! Windows machine comes up as an empty white frame with no UI).
//!
//! Root cause: every webview in the app is built as a plain
//! `WebviewBuilder::new(...)` with NO `data_directory`, so wry passes an
//! EMPTY user data folder to `CreateCoreWebView2EnvironmentWithOptions`
//! (vendor/wry/src/webview2/mod.rs:348 `&data_directory.unwrap_or_default()`).
//! Both instances therefore resolve the SAME Win32 default UDF — the exe path
//! + `.WebView2` (MS Learn "Manage user data folders"). A UDF holds at most
//!   one WebView2 session, and a differently-configured second environment for
//!   an in-use UDF fails to create WebView2 objects (MS Learn "Process model");
//!   the window is created before the webview, so the second instance shows a
//!   live-but-blank window.
//!
//! The fix: the first instance keeps the default (persistent) profile; every
//! additional instance gets its own per-pid WebView2 user data folder (see
//! `src/webview_args.rs` helpers + `src-tauri/src/webview_udf.rs`). This test
//! is the deterministic stand-in for a GUI check: it asserts the source-level
//! invariants — both webview builder sites of the Mnemo2 host (the shell
//! webview in `mnemo2/mod.rs` and the Browser tab's child webview in
//! `mnemo2/browser.rs`) must run their builder through the data-dir apply
//! helper, and the helper contract must exist. It FAILS on the pre-fix tree
//! (no per-instance data directory anywhere) and PASSES once the fix lands,
//! so a regression can never silently reappear.

use std::path::PathBuf;

/// `src-tauri` root, resolved via the package manifest dir so the test is
/// hermetic and repo-relative.
fn src_tauri_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_app(rel: &str) -> String {
    let path = src_tauri_dir().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The shell webview (the one that fills the whole window and white-screened
/// on the second instance) must be built through the per-instance data-dir
/// helper — a plain `WebviewBuilder::new` leaves WebView2 on the default UDF,
/// which the first instance's session already holds.
#[test]
fn shell_builder_uses_per_instance_data_dir() {
    let host = read_app("src/mnemo2/mod.rs");
    // The primary-vs-secondary profile arbitration runs before the first
    // webview is created.
    let install = host
        .find("crate::webview_udf::install_default_data_dir(")
        .unwrap_or_else(|| {
            panic!(
                "src/mnemo2/mod.rs must call crate::webview_udf::install_default_data_dir \
                 before creating its first webview — without it the \
                 per-instance profile split never happens"
            )
        });
    let builder_site = host
        .find("crate::webview_udf::apply(")
        .unwrap_or_else(|| {
            panic!(
                "src/mnemo2/mod.rs must wrap the shell WebviewBuilder with \
                 crate::webview_udf::apply(...) — the plain builder leaves \
                 WebView2 on the shared default UDF, which is the \
                 blank-white second instance (2027-01-13)"
            )
        });
    assert!(
        install < builder_site,
        "install_default_data_dir must run before the shell webview is built"
    );
}

/// The Browser tab's child webview must run its builder through the same
/// helper — otherwise a second instance that opens the Browser tab gets a
/// webview on the shared default UDF.
#[test]
fn child_webview_builder_uses_per_instance_data_dir() {
    let browser = read_app("src/mnemo2/browser.rs");
    // The UDF wrap lives at the single builder site, before the webview is
    // added.
    let (Some(apply), Some(add_child)) = (
        browser.find("crate::webview_udf::apply("),
        browser.find(".add_child("),
    ) else {
        panic!(
            "mnemo2/browser.rs must wrap the child WebviewBuilder with \
             crate::webview_udf::apply(...) and create it via add_child — \
             the plain builder leaves WebView2 on the shared default UDF, \
             which is the blank-white second instance (2027-01-13)"
        );
    };
    assert!(
        apply < add_child,
        "the webview_udf::apply wrap must happen before the child webview is \
         added"
    );
}

/// The `src-tauri` helper module must exist with its two entry points: the
/// builder-wrapping `apply` (called unconditionally, a no-op on non-Windows)
/// and the startup `install_default_data_dir` that decides primary vs
/// secondary profile via the named mutex.
#[test]
fn webview_udf_module_declares_the_contract() {
    let udf = read_app("src/webview_udf.rs");
    for needle in ["pub fn apply(", "pub fn install_default_data_dir("] {
        assert!(
            udf.contains(needle),
            "src-tauri/src/webview_udf.rs must declare {needle} — the missing \
             helper means the per-instance data directory is not wired"
        );
    }
}

/// The lib-side pure helpers (the OnceLock accessors + the per-pid path +
/// the mutex name) must exist — they are what makes the data directory
/// per-instance and deterministic.
#[test]
fn lib_helpers_declared_in_webview_args() {
    let wa = read_app("../src/webview_args.rs");
    for needle in [
        "pub fn set_webview_data_dir(",
        "pub fn webview_data_dir(",
        "pub fn secondary_webview_data_dir(",
        "pub fn mutex_name_for(",
    ] {
        assert!(
            wa.contains(needle),
            "src/webview_args.rs must declare {needle} — the per-instance \
             WebView2 data-dir plumbing (fails without the fix by design)"
        );
    }
}
