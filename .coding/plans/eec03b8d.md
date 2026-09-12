# Plan: Research: cut dev/test link time (post-lld round 2)

## Goal
Produce a ranked, measured set of recommendations to cut link time in the dev/test loop on Windows (MSVC), building on the rust-lld + line-tables-only changes already in main.

## Kind
research

## Context
Prior work (merged 2026-09-19): .cargo/config.toml sets linker=rust-lld for x86_64-pc-windows-msvc (~60s→10-15s for the Tauri app link), and [profile.dev]/[profile.test] use debug="line-tables-only". Baseline cargo test --lib was 13.55s. User reports linking still takes too much time. Workspace = mnemo lib (src/) + mnemo-app (src-tauri/), Windows 11, toolchain ~1.98. Remaining candidate levers to evaluate: debug=false for dependency packages ([profile.dev.package."*"]), codegen-units tuning, number of integration-test binaries (each links the full dep tree statically), Windows Defender real-time scanning of target/, newer lld in toolchain, dylib/prefer-dynamic dev strategy, cargo build --timings breakdown.

## Steps
- [x] 1. **Measure where link time goes** — Baseline the current loop on Windows (PowerShell, read $LASTEXITCODE, time via Measure-Command): (1) touch src/lib.rs minor change → cargo build, note link phase; (2) cargo build --timings and capture the link-heavy crates from the HTML/report; (3) time cargo test --lib warm and note per-binary link; (4) time cargo build -p mnemo-app. Record numbers per target (lib test bin, app bin, integration test bins) in a scratch note under .coding/knowledge/.
- [x] 2. **Inventory link targets + settings** — Count test binaries: list tests/ dirs in root crate and src-tauri (each integration test = a separate full static link of the dep tree). Check cargo config for incremental/codegen-units overrides (none currently in .cargo/config.toml beyond rust-lld). Confirm rust-lld is actually invoked for test binaries too (cargo build -v 2>&1 | grep rust-lld). Note dep-tree mass: cargo tree --depth 1 for the app crate.
- [x] 3. **Check environment factors** — Windows-side link killers: check Windows Defender real-time protection status and whether C:\AgenticCoder\AgenticCoder\target is excluded (Get-MpPreference | select -ExpandProperty ExclusionPath). Check target/ disk (SSD?), and count/size of PDBs in target/debug (Get-ChildItem target/debug -Filter *.pdb | sort Length -desc | select -First 10). MsMpEng scanning linker writes is a classic hidden link-tax.
- [x] 4. **Survey remaining levers (web + docs)** — Web-research current best practice for Rust link time on Windows MSVC with rust-lld: [profile.dev.package."*"] debug=false impact; codegen-units tradeoff for link; PDB /DEBUG:FASTLINK equivalents under lld-link; Rust 1.9x parallel frontend / -Zthreads status (compile-side but affects perceived cycle); newer rust-lld improvements in 1.98+; any wild-linker Windows status (expected: none). Rank levers by expected seconds saved vs risk/behavior change for THIS repo (big dep mass: tauri, fastembed, chromiumoxide).
- [x] 5. **Write findings + ranked recommendations** — Write .coding/knowledge/link-time-research.md with: measured baseline table, which link dominates, ranked recommendations (config-only / environment / code-structure changes), and expected win per lever. memory_write a SPEC/HOW pointer. Do not change any build config — research only; implementation follows as its own plan if the user wants it.
