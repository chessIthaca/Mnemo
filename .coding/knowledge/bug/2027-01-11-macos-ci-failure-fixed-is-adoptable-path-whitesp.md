+++
title = "macOS CI failure FIXED — is_adoptable_path whitespace-only (verified green)"
created = "2027-01-11"
+++

FIXED + VERIFIED 2026-09-21 (plan 263a9e31). The sixth macOS CI failure was a real bug: `is_adoptable_path` (src-tauri/src/main.rs:118, #[cfg(unix)]) only checked `!value.is_empty()`, so a whitespace-only captured PATH value (`"   "`) was adopted. The test `adoptable_path_rejects_empty_and_control_characters` (src-tauri/src/main.rs:1878, module `#[cfg(all(test, unix))]`) asserted it must be rejected — and that module NEVER COMPILES ON WINDOWS, which is exactly why the failure was invisible on the dev box and only the macOS leg caught it.

FIX: `!value.trim().is_empty() && !value.chars().any(|c| c.is_control())` — rejects whitespace-only while keeping real paths with internal spaces (`/Applications/Some App/bin`) adoptable (that internal-space case is review F5, guarded by `adoptable_path_allows_spaces`). Added `adoptable_path_keeps_internal_spaces` and extended the reject test with `"\t"`/`"\n"` cases.

VERIFICATION: diagnostic dispatch run 35578123470 on wt/macos-fix → `completed / success`, **macos: completed success** (was failing on runs 35521221353, 35531680467), windows success. The macOS job is now RESTORED to the normal path (no gate, `needs: [windows, macos]`, macos artifact download + dist/macos globs back, `run_macos` dispatch input removed).

EVIDENCE METHOD THAT WORKED (the job-logs API is 403/admin-only): a workflow step doing `cargo test --workspace --no-fail-fast 2>&1 | tee macos-test-output.txt` + `actions/upload-artifact@v4` (if: always()), then `gh run download <run-id> --name macos-test-output`. `gh run view --log-failed` also works once a run completes. This is the pattern for any future macOS-only failure.

LANDING NOTE: `main` is now protected by the active repository ruleset "Protect from direct pushing." (id 23755694, rules: deletion, non_fast_forward, update, pull_request) — direct pushes to main are REJECTED; changes must go through a pull request. The merge_to_main skill's direct-merge+push flow no longer works on this repo.
