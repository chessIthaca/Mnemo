+++
title = "cargo-nextest hangs on this project (even latest v0.9.143 + rustc 1.98) — stick with cargo test"
supersedes = "2026-08-30-cargo-nextest-is-a-regression-on-this-setup-stic"
created = "2026-08-30"
+++

HOW (2026-09-19, updated after toolchain upgrade to rustc 1.98): cargo-nextest is NOT viable on this project, even with the latest version + toolchain. Tested v0.9.143 (latest, installed after upgrading rustc 1.89→1.98) — it runs tests in parallel (confirmed: 15 tests completing within a 0.02s window), reaching test 954/1653 at ~7.7s, but then HANGS — the process ran for 300s without finishing. A test after #954 deadlocks under nextest's process-per-test model on Windows. The same tests run fine under `cargo test --lib` (1653 passed, 0 failed, 12 ignored in 13.97s), so it's nextest's execution model, not the tests. RECOMMENDATION: stick with `cargo test --lib` (13.97s under rustc 1.98). Do not use cargo-nextest on this project until the Windows process-per-test hang is resolved upstream. The toolchain upgrade to rustc 1.98 IS a keeper (clean build, zero warnings, tests pass) — only nextest is the problem.
