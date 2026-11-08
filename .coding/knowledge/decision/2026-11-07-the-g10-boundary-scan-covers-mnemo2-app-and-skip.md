+++
title = "the G10 boundary scan covers mnemo2-app and skips test-only files named tests.rs, *_tests.rs, testutil.rs"
created = "2026-11-07"
+++

tests/boundary.rs `tauri_uses_only_public_api` scans three roots (src-tauri/src, src-tauri/mnemo2-app/src, src-tauri/mnemo2-app/examples) with one BANNED list. Files whose stem is `tests`, `testutil` or ends in `_tests` are skipped (test code may name kernel internals, like the inline #[cfg(test)] stripper), and `test_only_files_are_declared_under_cfg_test` proves each is declared `#[cfg(test)]` + `#[path = "...<name>"]` (or `mod <stem>;`) by a file in its directory or the one above, so production code cannot hide in a file called tests.rs. Moving a module's tests into a sibling file (`#[cfg(test)] #[path = "x/tests.rs"] mod tests;`) is therefore boundary-safe; use exactly that shape. Rejected idea (ARCH-6): moving `lead_instructions` out of the app crate - mnemo2-app's helper bin already imports mnemo_app for project::list and instance_marker::pid_alive and shares one Cargo package, so one constant would change nothing. Pointer: tests/boundary.rs.
