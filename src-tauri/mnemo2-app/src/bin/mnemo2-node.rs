// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `mnemo2-node`: the headless worker node of a remote Mnemo2 hub (plan
//! a040713f, phase 2 step 6).
//!
//! A console binary (no `windows_subsystem` attribute), so pairing over SSH
//! prints its result and `run` logs to the terminal. It is a separate binary
//! from the GUI `mnemo2`: a node has no window, no tray and no Tauri.

// `#[path]` keeps cargo from auto-discovering the helper as a binary.
#[path = "../node/mod.rs"]
mod node;

fn main() -> std::process::ExitCode {
    node::main()
}
