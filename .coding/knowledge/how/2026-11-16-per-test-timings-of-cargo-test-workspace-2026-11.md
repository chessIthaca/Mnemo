+++
title = "per-test timings of cargo test --workspace (2026-11-16) and how to measure them on stable"
created = "2026-11-16"
evidence = "RUSTC_BOOTSTRAP=1 cargo test --workspace --no-fail-fast -- -Z unstable-options --format json --report-time in C:\\Mnemo at 5a9c60d3 on 2026-11-16; run wall 77 s, suite sum 66 s."
+++

Measure: build with `cargo test --workspace --no-run`, then `cargo test --workspace --no-fail-fast -- -Z unstable-options --format json --report-time` with RUSTC_BOOTSTRAP=1 set (libtest honors RUSTC_BOOTSTRAP at runtime, so stable works); every test event carries exec_time, every suite event its wall time; cargo prints the binary order ("Running ...") on stderr.
Result 2026-11-16 (rustc 1.99.1, 32 threads, two workers compiling at the same time, so absolute times run high): 4623 tests, suites 66 s run ONE AFTER ANOTHER: mnemo lib unit 24.0 s (3529 tests), mnemo-app lib unit 32.8 s (803), mnemo2_mcp unit 4.9 s (176), integration 4.1 s (102), the rest ~0.
- mnemo lib wall is set by one test: agent::tests::run_turn_records_summarize_request_stats 16.2 s; then tool::agent::shell::tests::timed_out_command_kills_the_background_grandchild 10.1 s, runtime::agent::tests::provider_site_disabled_triage_keeps_the_ladder_unchanged 9.8 s and provider_site_transient_classification_keeps_the_ladder 9.7 s.
- mnemo-app wall is volume, not one tail: mnemo2::team tests sum 438 s and mnemo2::runtime 363 s (real git repos and git subprocesses per test; 15 single tests take 7-15 s, e.g. runtime::tests::a_step_with_two_tasks_ticks_only_after_both_close 15.1 s); mcp_server 93 s.
- 399 tests take >= 1 s (sum 1264 s), 58 take >= 5 s, 9 take >= 10 s.
cargo-nextest is not an option here (hangs on Windows, see the 2026-08-30 records).
