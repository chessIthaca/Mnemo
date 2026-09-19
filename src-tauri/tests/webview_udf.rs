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
//! one WebView2 session, and a differently-configured second environment for
//! an in-use UDF fails to create WebView2 objects (MS Learn "Process model");
//! the window is created before the webview, so the second instance shows a
//! live-but-blank window.
//!
//! The fix: the first instance keeps the default (persistent) profile; every
//! additional instance gets its own per-pid WebView2 user data folder (see
//! `src/webview_args.rs` helpers + `src-tauri/src/webview_udf.rs`). This test
//! is the deterministic stand-in for a GUI check: it asserts the source-level
//! invariants — all three webview builder sites (the agent-chat webview in
//! `main.rs`, and both child-webview creation sites in
//! `browser_webview.rs`) must run their builder through the data-dir apply
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

/// The agent-chat webview (the one that fills the whole window and white-
/// screened on the second instance) must be built through the per-instance
/// data-dir helper — a plain `WebviewBuilder::new` leaves WebView2 on the
/// default UDF, which the first instance's session already holds.
#[test]
fn agent_chat_builder_uses_per_instance_data_dir() {
    let main_rs = read_app("src/main.rs");
    let builder_site = main_rs
        .find("webview_udf::apply(")
        .unwrap_or_else(|| {
            panic!(
                "src/main.rs must wrap the agent-chat WebviewBuilder with \
                 webview_udf::apply(...) — the plain builder leaves WebView2 \
                 on the shared default UDF, which is the blank-white second \
                 instance (2027-01-13); missing in src/main.rs"
            )
        });
    // The wrap must sit at the builder site (before the add_child), not
    // anywhere incidental later in the file.
    let add_child = main_rs
        .find(".add_child(")
        .unwrap_or_else(|| panic!("main.rs lost its agent-chat add_child call"));
    assert!(
        builder_site < add_child,
        "webview_udf::apply must wrap the agent-chat builder before add_child"
    );
}

/// Both child-webview creation sites (human ensure + agent ensure) must run
/// their builders through the same helper — otherwise a second instance that
/// opens the Browser tab gets a webview on the shared default UDF. Since the
/// 2027-01-24 consolidation (backlog 3f838ea1) both sites share ONE choke
/// point: the `child_webview_builder` helper (which also attaches the
/// page-load hook emitting `browser://url-changed`), so the UDF wrap lives
/// inside it exactly once and each site must call the helper.
#[test]
fn child_webview_builders_use_per_instance_data_dir() {
    let bw = read_app("src/ipc/browser_webview.rs");
    // The UDF wrap lives inside the shared helper — exactly once.
    let (Some(apply), Some(helper)) = (
        bw.find("webview_udf::apply("),
        bw.find("fn child_webview_builder("),
    ) else {
        panic!(
            "browser_webview.rs must wrap the child WebviewBuilder with \
             webview_udf::apply(...) inside the shared child_webview_builder \
             helper — the plain builder leaves WebView2 on the shared default \
             UDF, which is the blank-white second instance (2027-01-13)"
        );
    };
    assert!(
        apply > helper,
        "the webview_udf::apply wrap must live inside the child_webview_builder helper"
    );
    // BOTH creation sites must route through the helper.
    let first = bw.find("child_webview_builder(&normalized");
    let second = first
        .and_then(|i| bw[i + 1..].find("child_webview_builder(&normalized").map(|j| i + 1 + j));
    let (Some(first), Some(second)) = (first, second) else {
        let found = match (first, second) {
            (None, _) => 0,
            (Some(_), None) => 1,
            _ => 2,
        };
        panic!(
            "browser_webview.rs must route BOTH child-webview creation sites \
             (browser_webview_ensure + ensure_for_agent_impl) through the \
             shared child_webview_builder helper — two call sites; found \
             {found} occurrence(s)"
        );
    };
    assert!(
        first < second,
        "both creation sites must call the helper (calls in order)"
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
