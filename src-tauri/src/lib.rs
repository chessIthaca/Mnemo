// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Mnemo desktop shells' reusable library surface.
//!
//! The Mnemo1 Tauri shell remains a standalone binary (`main.rs`); this
//! library exposes the intentionally separate Mnemo2 host without pulling
//! Mnemo1's larger IPC surface into the Mnemo2 binary.

// Tauri supplies resource.lib to binaries but not to a library's unit-test
// target. Link the manifest here too so TaskDialogIndirect resolves to COMCTL32 v6.
#[cfg(all(windows, test, feature = "mnemo2"))]
#[link(name = "resource", kind = "static")]
extern "C" {}

/// Per-instance WebView2 user-data-dir arbitration shared by both shells.
pub mod webview_udf;

/// Same-project instance markers shared by both shells.
pub mod instance_marker;

/// The hang watchdog both shells use: freeze reports for a stalled main thread.
pub mod watchdog;

/// The Mnemo2 desktop host: a minimal Tauri shell around the headless
/// engine's lead session. Gated by the `mnemo2` feature so the Mnemo1 main
/// binary does not link its Tauri context.
#[cfg(feature = "mnemo2")]
pub mod mnemo2;
