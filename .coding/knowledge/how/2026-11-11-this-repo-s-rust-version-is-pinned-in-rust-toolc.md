+++
title = "this repo's Rust version is pinned in rust-toolchain.toml and CI installs it with a bare `rustup toolchain install`"
created = "2026-11-11"
evidence = "tests/integration/ci_workflow.rs::rust_toolchain_is_pinned_for_local_and_ci; no-arg `rustup toolchain install` output in a worker worktree"
+++

This repo pins one Rust version in rust-toolchain.toml at the repo root (currently 1.99.1, components clippy + rustfmt, profile minimal, deliberately NO `targets` key so Windows machines don't download Apple std libraries). Local cargo commands and every CI job take the version from it: the workflows use `run: rustup toolchain install` with no arguments — rustup >= 1.28 reads the file and installs the channel plus components (verified on rustup 1.29.3; output: "it's active because: overridden by '<worktree>/rust-toolchain.toml'"). Bumping is a one-line `channel` edit plus the lint fixes in the same PR: run `cargo clippy --workspace --all-targets`, `cargo clippy -p mnemo-app --all-targets --features mnemo2`, `cargo test --workspace`; Dependabot cannot update the file.
