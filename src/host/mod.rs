// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Host-side wiring shared by every frontend (the Tauri GUI, the console, and
//! the headless API): the brain builder + its host-notifier seam, the shared
//! agent-spawn path + lifecycle bookkeeping, startup helpers, the maintenance
//! busy-flag, and the startup code-graph index progress. Nothing in this
//! module may depend on Tauri or on the app crate.

// 3a.1 (D1): the brain module compiles on EVERY feature combination — the
// feature-gated pieces (the bundled ONNX embedder, the shared headless
// browser) are gated at their own sites inside the module, with runtime-
// optional wiring (the hash embedder / no browser) on the bare build.
pub mod brain;
pub mod index_progress;
pub mod maintenance;
pub mod notifier;
pub mod rewire;
pub mod runtime;
pub mod startup;
