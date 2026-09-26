+++
title = "is_adoptable_path accepts whitespace-only — macOS test failure root cause + fix"
created = "2027-01-11"
+++

ROOT CAUSE COMPLETE (2026-09-21, plan 263a9e31): the macOS CI failure is a genuine, real bug — NOT a platform/flake issue.

- Function: `is_adoptable_path` at src-tauri/src/main.rs:118 (`#[cfg(unix)]`):
  `!value.is_empty() && !value.chars().any(|c| c.is_control())`
- Test: `adoptable_path_rejects_empty_and_control_characters` at src-tauri/src/main.rs:1878-1885 (module `#[cfg(all(test, unix))]` at line 1859), asserting `!is_adoptable_path("   ")` at line 1881.

WHY IT FAILS: `"   "` (three spaces) is non-empty and contains no control characters, so the predicate returns `true` — the assertion `!true` fails. The whitespace-only case is the ONLY failing assertion (line 1880 empty-string passes, 1882 newline and 1883 BEL and 1884 tab pass via is_control).

WHY IT WAS INVISIBLE UNTIL NOW: the test module is `#[cfg(all(test, unix))]`, so it never compiles/runs on the Windows dev box — a green `cargo test --workspace` on Windows proves nothing about it. Only the macOS leg exercises it. The five earlier fixes (plan 5cff52cf) were correct; this is the distinct sixth failure.

INTENT (from the doc comment at 113-116 and the sibling test at 1868-1874): reject empty values and control characters (a newline means rc files echoed output into the captured stdout), while LEGALLY ALLOWING spaces inside real PATH entries like `/Applications/Some App/bin`. So the fix must reject WHITESPACE-ONLY (or trimmed-empty) values while still accepting paths containing internal spaces — it must NOT revert to a blanket `!value.contains(' ')` (that was review F5, explicitly guarded by `adoptable_path_allows_spaces`).

FIX: add a trimmed-empty check to the predicate, e.g. `!value.trim().is_empty() && !value.chars().any(|c| c.is_control())`, which keeps `/Applications/Some App/bin` adoptable and rejects `"   "`.

EVIDENCE PATH (the job-logs API is 403): the workflow's own `tee macos-test-output.txt` + `actions/upload-artifact@v4` step; downloaded via `gh run download 35575972400 --name macos-test-output --dir .coding/tmp-macos-diag`. Artifact read at line 4167: `panicked at src-tauri/src/main.rs:1881:9: assertion failed: !is_adoptable_path("   ")`, result `FAILED. 296 passed; 1 failed`. The other two panics in the log (src/workflow/mod.rs:428, src-tauri/src/ipc/memory_maintenance.rs:627) are expected-panic tests that PASSED.

REGRESSION TEST NOTE: the existing test already IS the regression test for this fix (it fails without it on macOS). Because it is unix-gated, it cannot be proven red/green on the Windows dev box — the fix must be verified by the macOS diagnostic dispatch. Consider whether the trimmed-empty guard deserves a platform-neutral unit test too, so Windows CI guards it.
