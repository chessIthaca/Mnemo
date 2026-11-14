// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

fn main() {
    // `tauri_build::build()` only tracks the default `tauri.conf.json`.
    // The Mnemo2 host has its own config, so make cargo rebuild when it changes.
    println!("cargo:rerun-if-changed=mnemo2-app/tauri.conf.json");
    // This crate is now a library (the Mnemo2 host): src-tauri/tauri.conf.json
    // and src-tauri/capabilities/mnemo2.json exist only so tauri_build can
    // generate the ACL the host's `generate_context!("mnemo2-app/tauri.conf.json")`
    // needs. Mnemo2 itself is bundled from src-tauri/mnemo2-app.
    tauri_build::build();

    // Tauri emits resource.lib for bins, not test targets. Now that this
    // package is a library, embed-resource (inside tauri_build) additionally
    // links that resource with a PROPAGATING
    // `cargo:rustc-link-lib=dylib=resource` — every downstream binary would
    // carry this manifest next to its own (rust-lld: duplicate resource
    // MANIFEST). mnemo2-app ships its own complete resource, so move the
    // compiled file to resource-tests.lib (linked for THIS crate's own test
    // targets: src/lib.rs's #[link] for the feature-gated unit tests, and the
    // rustc-link-arg-tests below for cargo's [[test]] targets) and leave an
    // empty archive at the propagated `resource.lib` path: resolving to
    // nothing is exactly right downstream.
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref()
        == Some(std::ffi::OsStr::new("windows"))
    {
        let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        let resource = out_dir.join("resource.lib");
        let tests_resource = out_dir.join("resource-tests.lib");
        if resource.exists() {
            let _ = std::fs::remove_file(&tests_resource);
            std::fs::rename(&resource, &tests_resource)
                .expect("move resource.lib to resource-tests.lib");
            std::fs::write(&resource, b"!<arch>\n")
                .expect("write the empty propagated resource.lib");
        }
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!(
            "cargo:rustc-link-arg-tests={}",
            tests_resource.display()
        );
    }
}
