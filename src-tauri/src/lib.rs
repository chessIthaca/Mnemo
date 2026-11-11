// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Mnemo2 desktop host's reusable library surface.
//!
//! The `mnemo2-app` package links this crate (with the `mnemo2` feature) to
//! build the Mnemo2 desktop app.

// Tauri compiles the Windows resource (COMCTL32 v6 manifest + versioninfo +
// icon) into OUT_DIR/resource.lib and — now that this package is a library —
// embed-resource links it with a PROPAGATING `cargo:rustc-link-lib` that
// would double-manifest every downstream binary (mnemo2-app ships its own).
// build.rs therefore moves the real file to resource-tests.lib and leaves an
// empty stub at the propagated path; link the real one here so the
// feature-gated unit tests (which import TaskDialogIndirect) keep their
// manifest.
#[cfg(all(windows, test, feature = "mnemo2"))]
#[link(name = "resource-tests", kind = "static")]
extern "C" {}

/// Per-instance WebView2 user-data-dir arbitration for the Mnemo2 host.
pub mod webview_udf;

/// Same-project instance markers used by the Mnemo2 host and its MCP helper.
pub mod instance_marker;

/// The hang watchdog: freeze reports for a stalled main thread.
pub mod watchdog;

/// The Mnemo2 desktop host: a minimal Tauri shell around the headless
/// engine's lead session. Gated by the `mnemo2` feature; the `mnemo2-app`
/// package enables it.
#[cfg(feature = "mnemo2")]
pub mod mnemo2;
