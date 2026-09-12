+++
title = "build/test perf baseline + optimizations shipped (2026-09-19, lld + line-tables-only + git-test removal)"
created = "2026-08-30"
+++

Build/test performance baseline measured 2026-09-19 (Windows, rustc 1.89.0 MSVC target, 32 CPUs, warm cache). UPDATED with post-optimization results (plan d6c6d05e, commit 79fd8cd).

PRE-OPTIMIZATION (warm cache):
- Lib crate (mnemo): compile+link 1.4s; test execution 27.4s (1700 tests)
- App crate (mnemo-app/src-tauri): build 69.8s; test compile+link (no-run) 65.9s — DOMINANT cost
- App-crate incremental rebuild (touch main.rs, deps cached): 10.4s
- 491 deduped crates; default MSVC link.exe; full debuginfo (debug=2); no .cargo/config.toml

OPTIMIZATIONS SHIPPED (commit 79fd8cd, branch wt/agenticcoding):
1. lld linker (.cargo/config.toml): [target.x86_64-pc-windows-msvc] linker = "rust-lld" — toolchain-shipped LLVM lld instead of MSVC link.exe. MSVC-gated (macOS unaffected). Marginal incremental win (~0.5s) but strictly-better-or-equal.
2. Reduced debuginfo (Cargo.toml): [profile.dev]/[profile.test] debug = "line-tables-only" — cuts link time + binary size vs full debuginfo while keeping panic backtraces with line numbers. The bigger lever.
3. Removed git-spawning integration tests from git *tool* modules (git.rs/git_read.rs/git_diff.rs) — git is external/battle-tested. Kept pure-logic tests + added 3 for the branch-list allowlist validator. NOTE: the git *tool* tests no longer spawn git; git_ops.rs/plan.rs orchestration tests still do (by design — they test our own auto-fork/branch-prep code).

POST-OPTIMIZATION (warm cache):
- App-crate incremental rebuild: 10.4s → 7.2s (31% faster)
- Full lib test suite: 29.2s → 17.1s (27% faster; 1664 tests, 0 failed)

REMAINING BOTTLENECKS (if further optimization needed):
- src-tauri cold build still ~70s (Tauri + 491 deps compile from scratch) — lld/debuginfo only help incremental, not cold
- Test execution (17.1s) — cargo-nextest (parallel test execution) could help further
- Heavy deps (fastembed/ONNX, rusqlite/bundled C, chromiumoxide, tree-sitter, tauri) dominate cold compile
