+++
title = "updates.rs refactor completed"
created = "2026-11-15"
evidence = "commit 20890a84, all checks passed"
+++

The updates.rs file has been successfully refactored as specified in plan step 1. Key changes made:
1. Added NOT_READY constant with the message "No update is ready to install." 
2. Replaced literal string occurrences in request_install() and update_restart() with the NOT_READY constant
3. Created update_source() helper function that selects the appropriate source based on the auto-update feature
4. Removed duplicated cfg-gated source selection code from start() and update_check()
5. All 10 existing tests pass
6. Clippy runs clean with both features enabled and disabled
7. The literal string now appears exactly once in the file (in NOT_READY)
8. Only src-tauri/src/mnemo2/updates.rs was modified as required
