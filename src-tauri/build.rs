// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

fn main() {
    // `tauri_build::build()` only tracks the default `tauri.conf.json`.
    // The Mnemo2 host has its own config, so make cargo rebuild when it changes.
    println!("cargo:rerun-if-changed=mnemo2-app/tauri.conf.json");
    tauri_build::build()
}
