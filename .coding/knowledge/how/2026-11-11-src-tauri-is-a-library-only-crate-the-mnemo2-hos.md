+++
title = "src-tauri is a library-only crate (the Mnemo2 host); Mnemo1's binary and ipc/ are gone"
supersedes = "2026-11-02-src-tauri-crate-split-ipc-is-mnemo1-binary-only"
created = "2026-11-11"
evidence = "Merge 6748f8c4 (commit 674702df) on wt/mnemo, 2026-11-11; src-tauri/Cargo.toml has no [[bin]]."
+++

Since Mnemo1 retired on 2026-11-11, the `src-tauri` package `mnemo-app` has no binary.

- Library contents: src/lib.rs with webview_udf, instance_marker, watchdog, and `mnemo2` behind the `mnemo2` feature (the Mnemo2 host).
- The Mnemo2 binaries (mnemo2, mnemo2-mcp) live in src-tauri/mnemo2-app, which depends on mnemo-app with the mnemo2 feature.
- These still exist only for the library's tauri_build: src-tauri/tauri.conf.json (bundle.active false, frontendDist ../target/mnemo2-dist), src-tauri/capabilities/mnemo2.json and src-tauri/build.rs. They feed the ACL that the host's generate_context!("mnemo2-app/tauri.conf.json") needs.
- frontend/, src-tauri/src/main.rs, console.rs, startup.rs and ipc/ no longer exist. Nothing reads Mnemo1's IPC layer.
- Host tests: cargo test -p mnemo-app --features mnemo2 --lib.
