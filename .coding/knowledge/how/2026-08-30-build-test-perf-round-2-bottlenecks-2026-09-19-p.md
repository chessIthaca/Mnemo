+++
title = "build/test perf round-2 bottlenecks (2026-09-19, post lld+line-tables-only)"
created = "2026-08-30"
+++

Build/test perf round-2 investigation (2026-09-19, research plan b72f3095, post lld+line-tables-only+git-test-removal). Windows, rustc 1.89.0 MSVC, 32 CPUs.

COLD BUILD (from clean):
- Lib: 82.2s; src-tauri: 100.6s
- Top crates by compile time (src-tauri cold): chromiumoxide_cdp 39.6s (auto-generated CDP bindings, #1), windows 33.2s (Win API bindings from tauri/wry/tao, #2), mnemo 23s, mnemo-app 18.8s, windows-sys ~33s total (5 builds), tokenizers 6.1s (fastembed), rav1e 5s (image crate AV1 decoder), tokio 5.6s, syn 5.3s
- chromiumoxide_cdp + windows alone = ~73s of the 100.6s cold build

WARM INCREMENTAL REBUILD:
- Lib: 5.3s; src-tauri: 11.6s (includes lib recompile; app-crate-only ~6s)

TEST EXECUTION:
- Parallel (32 threads): 17.1s (1664 tests); Serial (1 thread): 155.9s → 9x from parallelism, but only ~29% efficiency on 32 cores (theoretical 4.9s)
- Slowest modules: browser 17.3s/33 tests (~524ms/test, ALSO FAILING in headless — WebView2 "receiver is gone" timeouts), git_ops 9s/24 tests (~375ms/test, git subprocesses), tool::workflow 5.5s/97 tests, agent::tests 5.2s/115 tests; everything else ~2-2.5s/module (overhead)
- nextest NOT installed (cargo nextest → "no such command")

RANKED RECOMMENDATIONS (next round, by impact/effort):
1. Fix or skip browser tests — 17.3s AND failing. Mark #[ignore] with a separate slow-test runner, or fix the WebView2 headless setup. Biggest test-suite win, low effort.
2. Feature-gate chromiumoxide — 39.6s cold compile saved for builds that don't need the headless browser. Biggest cold-build win, medium effort (architectural — src/browser/mod.rs is always compiled).
3. cargo-nextest — install + use for test execution. Lower per-test overhead + better scheduling could cut 17.1s → ~10-12s. Easy install (cargo install cargo-nextest or binstall), no code change.
4. git_ops test optimization — 9s/24 tests. These test our own orchestration (correctly kept), but could use a shared repo fixture or mock git calls.
5. Disable rav1e (AV1 decoder, 5s) from the image crate if AV1 isn't needed — image = { default-features = false, features = ["png","jpeg","gif","webp","bmp"] } already excludes avif but rav1e may come from another path.
