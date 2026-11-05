+++
title = "Mnemo2 cargo checks need --features mnemo2, and clippy must pass with and without it"
created = "2026-11-05"
+++

The src-tauri `mnemo2` module is behind the `mnemo2` cargo feature (src-tauri/Cargo.toml). `cargo test -p mnemo-app --lib mnemo2::<module>` WITHOUT `--features mnemo2` compiles the lib without the module and matches 0 tests — a vacuous green. Briefs and checks for Mnemo2 code must use `cargo test -p mnemo-app --features mnemo2 --lib mnemo2::<module>` and report the test count. Clippy must be clean in both builds: `cargo clippy -p mnemo-app --all-targets -- -D warnings` (Mnemo1 bin, deny(warnings) in src-tauri/src/main.rs) and the same with `--features mnemo2`. Imports used only by mnemo2-gated code (e.g. in src-tauri/src/ipc/finetune.rs) need the same #[cfg(feature = "mnemo2")] gate, or the no-feature build fails on unused imports.
