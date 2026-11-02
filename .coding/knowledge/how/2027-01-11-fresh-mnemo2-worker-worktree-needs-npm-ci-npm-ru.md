+++
title = "fresh Mnemo2 worker worktree needs npm ci + npm run build before cargo test"
created = "2027-01-11"
+++

Fresh Mnemo2 worker worktrees (C:\Mnemo\.worktrees\agent-*) have no target/ and no node_modules, so a first `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2` fails in tauri's generate_context! unless the embedded frontend dist exists. Required setup: `npm ci --no-audit --no-fund` then `npm run build` at the worktree root (frontend vite outDir = ../target/frontend-dist, which tauri.conf.json's frontendDist points at). Also: the shell tool kills commands at 300 s regardless of timeout_ms — run long builds detached via Start-Process with RedirectStandardOutput/RedirectStandardError into .coding/tmp/*.log and poll. The worktree has its own target dir (no shared CARGO_TARGET_DIR), so the first mnemo2 test build takes ~10+ min after deps are cached in registry.
