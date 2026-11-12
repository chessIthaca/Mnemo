+++
title = "cargo test takes one positional filter — multiple names go after --"
created = "2026-11-11"
evidence = ".coding/tmp/step1-battery.log leg \"test laya_settings model_settings\" exit=1 with the cargo usage error; corrected form re-run in session: \"running 25 tests … 25 passed; 0 failed; 740 filtered out\", exit=0"
+++

`cargo test` accepts exactly ONE positional TESTNAME — a second one aborts before running anything: `cargo test -p mnemo-app --features mnemo2 --lib laya_settings model_settings` fails with "error: unexpected argument 'model_settings' found" (exit 1, exit=1, zero tests run — looks like a red battery but is a CLI usage error). libtest itself accepts multiple filter substrings, so put them after `--`: `cargo test -p mnemo-app --features mnemo2 --lib -- laya_settings model_settings` (verified 2027-01-27, worktree agent-m1Settin: 25 passed, 740 filtered out). Plan texts that list several test-name filters in one cargo command need that `--` or one run per filter.
