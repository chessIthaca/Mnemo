// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Mnemo desktop shells' reusable library surface.
//!
//! The Mnemo1 Tauri shell remains a standalone binary (`main.rs`); this
//! library exposes the intentionally separate Mnemo2 host without pulling
//! Mnemo1's larger IPC surface into the Mnemo2 binary.

/// The Mnemo2 desktop host: a minimal Tauri shell around the headless
/// engine's lead session. Gated by the `mnemo2` feature so the Mnemo1 main
/// binary does not link its Tauri context.
#[cfg(feature = "mnemo2")]
pub mod mnemo2;
