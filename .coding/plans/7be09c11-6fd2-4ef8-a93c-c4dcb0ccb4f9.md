# Plan: Clear directory-path error for file tools (os error 5 misdirection)

## Goal
Make reading/editing a directory path return a clear "is a directory, not a file — use search with glob <dir>/*" error instead of the misleading Windows "Access is denied (os error 5)", pinned by regression tests on both platforms.

## Kind
bug_fixing

## Context
Diagnosis (verified by reading code): the exact error string `failed to read '{}': {e}` is produced ONLY by file_read.rs:128 and file_edit.rs:596. context_pack (src/tool/memory/retrieval.rs) is fs-free — the transcript card merged an adjacent context_pack auto-run (both are AutoRun → 🧠 badge) with a failing file_read call. On Windows, std::fs::read_to_string on a directory returns "Access is denied. (os error 5)" (ERROR_ACCESS_DENIED); on Unix "Is a directory (os error 21)". No is_dir() pre-check exists in any of the four file-reading tools (file_read.rs:126, file_edit.rs:~590, read_files.rs:~215, convert_line_endings.rs:~145), and no list-directory tool exists, so agents naturally try file_read on a dir and get a misleading permission error. Fix: is_dir() pre-check with an actionable hint pointing at the search tool's glob listing (multi-platform, no Windows-only APIs).

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
context_pack-adjacent tool card shows: failed to read '.coding/plans': Access is denied. (os error 5) — actually file_read on a directory path; Windows read_to_string(dir) = ERROR_ACCESS_DENIED, no is_dir pre-check, misleading permission error (backlog #89)

## Regression test
reading_a_directory_path_returns_directory_hint
