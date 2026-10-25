// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

fn main() {
    // `tauri_build::build()` only tracks the default `tauri.conf.json`.
    // The Mnemo2 host has its own config, so make cargo rebuild when it changes.
    println!("cargo:rerun-if-changed=mnemo2-app/tauri.conf.json");
    tauri_build::build();

    // Tauri emits resource.lib for bins, not test targets. Library tests also
    // need its COMCTL32 v6 manifest before importing TaskDialogIndirect.
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref()
        == Some(std::ffi::OsStr::new("windows"))
    {
        let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!(
            "cargo:rustc-link-arg-tests={}",
            out_dir.join("resource.lib").display()
        );
    }
}
