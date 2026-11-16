+++
title = "worker build cost measured 2026-11-16 — fresh checkouts dominate; mnemo lib-test is one 154 s unit"
created = "2026-11-16"
evidence = "cargo --timings reports target/cargo-timings/cargo-timing-20261116T094330184Z, T094538144Z, T094709997Z; probe edits of src/tool/mod.rs and src-tauri/src/mnemo2/team.rs; cargo tree output; ls target/debug/deps."
+++

Measured 2026-11-16 (rustc 1.99.1, 32 threads, shared CARGO_TARGET_DIR=C:\Mnemo\target with warm dependencies) in a freshly created worker worktree:
- Fresh checkout (workspace crates cold): `cargo test --workspace --no-run` 156 s, whose critical path is ONE unit, `mnemo "lib" (test)` 154 s; the test run takes 100 s because cargo runs the test binaries one after another (mnemo unit 29 s, mnemo-app unit 41 s, integration 8 s); `cargo clippy -p mnemo-app --all-targets --features mnemo2` 92 s; the first `cargo clippy --workspace --all-targets` adds mnemo check-test 126 s; `cargo build -p mnemo2-app --bin mnemo2` 118 s (mnemo 61.5 s, mnemo-app 56 s, link 9.6 s). Average CPU 13-25% of 32 threads: the workspace crates compile mostly serially.
- Warm checkout, one doc-comment edit: in src/ the test build takes 47 s (mnemo lib-test 44 s incremental); in src-tauri/src/mnemo2 it takes 21 s (mnemo-app lib-test 19 s).
- Cargo keys workspace crates by source path, so every worktree compiles its own mnemo / mnemo-app / vendored wry copies even in a shared target dir: 44 libmnemo-*.rlib (7.7 GB) in target/debug/deps; target/ was 203 GB with C: 96% full.
- `cargo tree --workspace -e features -i mnemo-app` shows --workspace already enables mnemo-app's mnemo2 feature (through mnemo2-app), so `cargo clippy -p mnemo-app --all-targets --features mnemo2` repeats `cargo clippy --workspace --all-targets`, and `cargo test -p mnemo-app --features mnemo2 --lib` re-runs tests that `cargo test --workspace` already ran.
- Module cycles inside the mnemo crate (crate:: references): tool→agent 28, tool→app 14, memory→provider 16, config→memory 12, provider→memory 6, provider→agent 5, memory→tool 3, memory→app 2; agent and tool (~105k of 244k lines) form one knot that a crate split would have to keep together or untangle.
Newer than the timing tables in .coding/knowledge/link-time-research.md (round 2, measured on a much smaller crate).
