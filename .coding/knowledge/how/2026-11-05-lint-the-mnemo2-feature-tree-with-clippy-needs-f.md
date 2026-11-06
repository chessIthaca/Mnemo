+++
title = "lint the mnemo2 feature tree with clippy (needs --features mnemo2)"
created = "2026-11-05"
+++

Procedural (mnemo-app, durable): `cargo clippy --workspace --all-targets -- -D warnings` does NOT lint the mnemo2 module tree — `mnemo2 = []` in src-tauri/Cargo.toml is not a default feature, so plain workspace clippy (and plain `cargo test`) never compiles `src-tauri/src/mnemo2/**`. Lint it with `cargo clippy -p mnemo-app --features mnemo2 --all-targets -- -D warnings`, and trust a clippy result only after a real compile: a sub-second "Finished" can be a warm cache hit (after `cargo clean -p mnemo-app` the fresh feature-tree lint takes ~50s). The constitution's feature-gated test clause applies to clippy the same way. Also: `IndexingTracker::new()` in src-tauri/src/mnemo2/indexing.rs returns `Arc<Self>` — do not wrap it in another `Arc` (a double-Arc cost one build cycle on laya_check.rs tests).
