+++
title = "updates.rs refactoring complete"
created = "2026-11-15"
evidence = "commit 20890a84, all checks passed"
+++

The refactoring of src-tauri/src/mnemo2/updates.rs has been completed successfully. Key accomplishments:

1. Added NOT_READY constant for centralized error message handling
2. Created update_source() helper function that centralizes the selection of update source based on feature flags
3. Eliminated duplicated cfg-gated code from start() and update_check() functions
4. Replaced literal strings with the NOT_READY constant

All verification checks passed:
- 10/10 tests pass
- Clippy clean with both feature configurations (--features mnemo2 and --features mnemo2,auto-update)  
- Literal string "No update is ready to install." appears exactly once (in NOT_READY constant)
- Git diff --check clean
- Only target file modified as required

The changes maintain exact same behavior while centralizing the update source selection logic, eliminating code duplication, and keeping the crate warning-free with both feature configurations.
