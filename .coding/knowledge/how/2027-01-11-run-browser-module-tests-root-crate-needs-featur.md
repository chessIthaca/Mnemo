+++
title = "run browser-module tests — root crate needs --features browser"
created = "2027-01-11"
+++

The root crate mnemo gates src/browser behind the `browser` feature (chromiumoxide dep), OFF by default — plain `cargo test` at the root compiles NO browser tests (silently filtered out; ~2494 vs ~2528 lib tests). Run them with `cargo test --features browser <filter>` (+ `-- --ignored` for the Chromium-spawning integration tests). The app crate src-tauri always selects the feature. Verified 2027-02-05 (plan ec425270 step 1: `cargo test wedge_renderer -- --ignored` matched 0 tests; with the feature it ran).
