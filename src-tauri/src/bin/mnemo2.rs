// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's desktop entry point.
//!
//! Run with `cargo run -p mnemo-app --features mnemo2 --bin mnemo2` while the
//! Mnemo2 Vite server is listening on port 5180.

fn main() {
    mnemo_app::mnemo2::run();
}
