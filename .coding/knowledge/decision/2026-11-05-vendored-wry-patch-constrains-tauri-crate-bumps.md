+++
title = "vendored wry patch constrains tauri crate bumps to their wry ^0.57 pin"
created = "2026-11-05"
+++

Mnemo's Cargo workspace vendors `wry` via `[patch.crates-io] wry = { path = "vendor/wry" }` (vendor/wry, version 0.57.2). Consequence for dependency bumps: any `tauri`/`tauri-runtime-wry` update that moves their `wry ^0.57` pin out of range makes the vendored patch unsatisfiable — cargo then records `[[patch.unused]]` in Cargo.lock (or errors), which the guard test tests/integration/ci_workflow.rs::cargo_lock_keeps_the_vendored_patches fails on. Safe practice: bump the tauri-* plugin/build crates (tauri-plugin-dialog, tauri-plugin-notification, tauri-plugin-shell, tauri-build) with targeted `cargo update -p <crate> --precise <ver>` calls while leaving tauri / tauri-runtime-wry at a version whose wry pin the vendored 0.57.2 satisfies; verify with `cargo tree --workspace -i wry` (must print the vendor/wry path) and a zero count of `patch.unused` in Cargo.lock. Cargo.lock may legitimately hold two sha2 entries, so a sha2 bump must be targeted as `cargo update -p sha2@<from-ver> --precise <to-ver>`.
