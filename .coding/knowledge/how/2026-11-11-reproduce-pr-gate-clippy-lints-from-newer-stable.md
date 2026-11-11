+++
title = "reproduce PR-gate clippy lints from newer stable Rust (cargo +1.99.1)"
created = "2026-11-11"
evidence = "CI run 42986685775 failed in cargo clippy on rustc 1.99.1; reproduced locally with cargo +1.99.1 clippy; fix merged 48064635."
+++

ci.yml job `check` uses dtolnay/rust-toolchain@stable, which tracks current stable (1.99.1 on 2026-11-11). The local default is older (1.95), and each new clippy adds lints that #![deny(warnings)] turns into errors, so a branch clean locally can be red on CI. The gate's first run (PR #118) hit plan_board.rs:1117 and two lints in memory/mod.rs, then summary.rs in mnemo2-app; fixed in 1a61e96d. To reproduce: `rustup toolchain install <ver> --profile minimal -c clippy` (1.99.1 is installed side by side), then `cargo +<ver> clippy --workspace --all-targets` and `cargo +<ver> clippy -p mnemo-app --all-targets --features mnemo2`. Take <ver> from the run log's `rustc X.Y.Z` line. Clippy stops at the first failing crate, so re-run until both exit 0. No rust-toolchain.toml pins the version. A pin must also cover build.yml's macOS job, which adds aarch64-apple-darwin to stable.
