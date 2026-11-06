// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

// A release build is a GUI app: no console window (mnemo2-mcp stays a
// console binary, it speaks MCP over stdio).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(warnings)]

//! Mnemo2's desktop entry point.
//!
//! Run with `cargo run -p mnemo2-app --bin mnemo2` while the
//! Mnemo2 Vite server is listening on port 5180.

fn main() {
    mnemo_app::mnemo2::run();
}
