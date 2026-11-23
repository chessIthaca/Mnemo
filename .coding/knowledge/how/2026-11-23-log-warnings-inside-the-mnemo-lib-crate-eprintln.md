+++
title = "log warnings inside the mnemo lib crate — eprintln!, not tracing"
created = "2026-11-23"
evidence = "grep for `tracing` over src/** = 0 matches; Cargo.toml [dependencies] has no tracing; commit 65eaf078 (wt/agent-shotExcl) uses eprintln! for ensure_screenshot_exclude failures and cargo test --workspace + clippy pass."
+++

The `mnemo` lib crate (src/**) has NO `tracing` dependency: 0 hits in src/**, absent from Cargo.toml. Its warning convention is `eprintln!("mnemo: …")` (src/app/worktrees.rs:538, src/app/tool/workflow/plan.rs:1395). A plan/step that says "log with tracing::warn" for code under src/ must fall back to `eprintln!("mnemo: …")` rather than adding a dep; only the Tauri app crate (src-tauri) uses tracing.
