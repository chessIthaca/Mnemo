+++
title = "test_default() config factories + test-support feature (commit 7c7598b, wt/agenticcoding, unmerged)"
created = "2026-12-24"
status = "superseded"
+++

DECISION: test configs are built via test_default() factories, never raw struct literals. Endpoint::test_default() (name "test", base_url http://localhost:9999/v1/, models ["m"], kind OpenAI), ModelSpec::test_default() (id "m"), OpenAiClientConfig::test_default() (base_url http://localhost/v1/, api_key "dummy", model "test-model", kind OpenAI, provider "test") — each returns Default for everything else; tests override only non-baseline fields via struct-update syntax (`..X::test_default()`). Adding a config field now touches only the struct + Default impl + factory. Factories are gated #[cfg(any(test, feature = "test-support"))]; the `test-support` cargo feature (root Cargo.toml) is enabled by src-tauri's dev-dependency so its test modules see them cross-crate; never compiled into release builds. Shipped in commit 7c7598b on branch wt/agenticcoding — NOT yet merged to main (pending merge_to_main). Out of scope by design: tests/integration/provider_integration.rs (env-driven literal, integration tests can't see feature-gated items) and anthropic.rs AnthropicClientConfig literals. Review: .coding/reviews/2026-12-24-test-factory-helpers-review.md (PASS). Plan: 7383a4d8.
