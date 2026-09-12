+++
title = "cargo-nextest is a regression on this setup — stick with cargo test (13.55s)"
created = "2026-08-30"
status = "superseded"
+++

HOW (2026-09-19, plan 0899de2b): cargo-nextest is NOT a win on this setup — it's a massive regression. Measured: nextest v0.9.128 (pinned for rustc 1.89 compat; latest needs 1.91) runs tests SERIALLY at ~3.4 tests/second (each test in a separate process, ~0.3s process-spawn overhead on Windows). 1653 tests × ~0.3s ≈ 486s (~8 min) projected full run — vs `cargo test --lib` at 13.55s (in-process parallel runner, 122 tests/sec). That's ~36x SLOWER. Even `-j 32` didn't help (still timed out at 300s) — the per-process model dominates. RECOMMENDATION: stick with `cargo test --lib` (13.55s). Revisit nextest only after upgrading to rustc 1.91+ (which allows the latest nextest that may have better Windows behavior). The nextest binary is installed (v0.9.128) but should NOT be used for the default test command.
