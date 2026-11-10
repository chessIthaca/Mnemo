+++
title = "the output compressor counts passing tests named after errors as errors"
created = "2026-11-09"
evidence = "Archived rows in `.coding/memory.db` (tool='shell', content like 'compressed cargo-test output%'), queried 2026-11-09; classifier read at output_compactor.rs:165."
+++

Symptom: a fully passing cargo test compresses to "exit 0: 335 error / 3 warning" and stays about 7,300 tokens, big enough to be archived. Root cause: classify_cargo_test (src/tool/agent/output_compactor.rs) calls shell_filter::is_error_line before its passing check, and is_error_line is a substring match on "error", "fail", "failed" and "panicked", so `test failure_triage::x ... ok` is an Error; `... ignored` lines fall through to Keep. classify_cargo_build and classify_npm_test have the same order, so `Compiling thiserror` or a passing vitest line naming an error also counts. Measured 2026-11-09: 314 archived compressed cargo-test rows, median 335 ok/ignored lines kept. Fix direction: test the passing and noise shapes before the error substring. Money is small (about $1-3 so far); the harm is misleading text that buries a real failure. Open at the time of writing.

Amended 2026-11-09: Fixed. Every classifier in src/tool/agent/output_compactor.rs now checks passing and noise shapes before the substring error match, a `test result: ok ... 0 failed` line is kept rather than an Error, and a clean `0 errors` summary is kept too. Vitest's `❯ ... failed` line deliberately stays an Error. Regression tests: a_passing_cargo_test_with_error_words_in_names_is_a_clean_summary, a_failing_test_stays_visible_among_error_named_passes, cargo_build_does_not_count_crate_names_as_errors, a_clean_svelte_check_summary_is_not_an_error.
