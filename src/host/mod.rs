// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Host-side wiring shared by every frontend (the Tauri GUI, the console, and
//! the headless API): startup helpers, the maintenance busy-flag, and the
//! startup code-graph index progress. Nothing in this module may depend on
//! Tauri or on the app crate.

pub mod index_progress;
pub mod maintenance;
pub mod startup;
