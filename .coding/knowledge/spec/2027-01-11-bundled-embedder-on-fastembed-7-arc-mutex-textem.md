+++
title = "bundled embedder on fastembed 7 — Arc<Mutex<TextEmbedding>> invariants and the feature-gated-bump rule"
created = "2027-01-11"
+++

fastembed 7 port (plan 4c7cacc1, commits 7d60f41 + bb418f2 on wt/mnemo; supersedes dependabot PR #67). New invariants in src/memory/embedder.rs:

- BundledEmbedder's model is `Arc<std::sync::Mutex<fastembed::TextEmbedding>>` because fastembed 7.0.0 gave `TextEmbedding::embed` a `&mut self` receiver. The lock guard is created and dropped entirely inside the sync `spawn_blocking` closure — it must NEVER cross an `.await` (std::sync::Mutex, not tokio's, is correct for a sync closure). Concurrent recalls serialize on the mutex by design: one ONNX instance = one CPU-bound inference at a time (review round 1 verified lock safety + no simultaneous status-RwLock hold; a panic inside the closure surfaces as JoinError → the `_` arm → zero vector + status Fallback, contract unchanged).
- Construction uses `fastembed::TextInitOptions::new(model).with_cache_dir(...)` — `InitOptions` is a deprecated alias since 7.0.0 and the crate-root `#![deny(warnings)]` makes using it a hard error. `with_cache_dir` still means the explicit cache location; 7.x caveats: FASTEMBED_CACHE_DIR overrides only the unset default, but a user-set HF_HOME takes precedence over `with_cache_dir` (environment-dependent download-redirect risk, same class as pre-7 hf-hub).
- Process rule (agent.md, Code style): a feature-gated dependency bump requires the feature-enabled run (`cargo test --features embeddings` / `--features browser`). No PR-triggered CI job compiles the feature trees: codeql.yml Rust is no-build-only; build.yml compiles them via `cargo test --workspace` (src-tauri selects both features) but triggers only on manual dispatch + v* tags.

Detail: plan .coding/plans/4c7cacc1.md; reviews .coding/reviews/2026-09-30-fastembed7-embedder-review.md (round 1, FINDINGS 0 high 1 low) and 2026-09-30-4c7cacc1-round2-review.md (PASS).
