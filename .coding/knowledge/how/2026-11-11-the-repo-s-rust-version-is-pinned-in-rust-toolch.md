+++
title = "the repo's Rust version is pinned in rust-toolchain.toml; how to bump it"
supersedes = "2026-11-11-reproduce-pr-gate-clippy-lints-from-newer-stable"
created = "2026-11-11"
+++

Since 2026-11-11, rust-toolchain.toml at the repo root pins the Rust version (channel "1.99.1", components clippy + rustfmt, profile minimal, no targets). Every cargo command in the repo, local or in worker worktrees, resolves to it. Every CI job installs it with `rustup toolchain install` (no arguments, reads the file); the macOS job also runs `rustup target add aarch64-apple-darwin`. No workflow uses dtolnay/rust-toolchain any more, so every `uses:` is SHA-pinned with no exception. Guards: tests/integration/ci_workflow.rs `rust_toolchain_is_pinned_for_local_and_ci` and `workflows_pin_actions_to_full_shas`. Why: CI floating on stable turned the PR gate red three times on new clippy lints under #![deny(warnings)] (the fetch_update deprecation, the 1.95 lint storm, the 1.99.1 lints on PR #118). To bump: change `channel`, then run `cargo clippy --workspace --all-targets`, `cargo clippy -p mnemo-app --all-targets --features mnemo2` and `cargo test --workspace`, and fix new lints in the same PR. Dependabot does not update the file. To test a candidate version without bumping, use `cargo +<ver> clippy ...` after `rustup toolchain install <ver> --profile minimal -c clippy`.
