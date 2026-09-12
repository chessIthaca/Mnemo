+++
title = "CGU=1 measured no-win for dev loop — not added to config"
created = "2026-09-01"
+++

HOW (plan 1c712c30 step 7, 2026-12-19): CGU keep-or-revert — DECIDED: NOT ADDED (reverted/no-op). Measured warm incremental probes on Windows/MSVC/toolchain 1.95, real mtime-bump-then-rebuild method, CARGO_PROFILE_DEV_CODEGEN_UNITS=1 vs default: cargo check -p mnemo 2.34s vs 2.33s (no delta — check emits metadata only, CGU never enters); cargo build -p mnemo 3.2s vs 3.21s (0.3% = noise). Threshold was ≥15% faster → keep; measured ~0% → .cargo/config.toml left with rust-lld linker only. Why no win: dev incremental compilation dominates the warm loop and deps (where CGU partitioning matters) compile once from clean, never in the warm loop. Don't re-run this experiment — result is stable absent a toolchain overhaul.
