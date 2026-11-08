+++
title = "move an inline Rust test module into a sibling file and prove the move is exact"
created = "2026-11-07"
+++

Pattern used for src-tauri/src/mnemo2/runtime.rs -> runtime/tests.rs + runtime/trace_tests.rs and team.rs -> team/tests.rs: replace `#[cfg(test)] mod tests { ... }` with `#[cfg(test)]` / `#[path = "<stem>/tests.rs"]` / `mod tests;` (a #[path] in a non-mod-rs file resolves from that file's directory). The body moves by script, dedented by exactly 4 spaces; never dedent a line that starts inside a multi-line string literal (backslash-continued lines are harmless, real newlines inside a string are not). No visibility or import change is needed, test names stay identical (`cargo test -- --list` diff empty), and the boundary test accepts the file (it skips tests.rs/*_tests.rs/testutil.rs declared under cfg(test)). Review proof: compare every body line of `git show <base>:<file>` with the new file (new == orig[4:] or == orig) and scan the block for lines starting inside string literals. Workers must keep scratch files under an absolute path outside every checkout: a scratch folder created inside C:\Mnemo dirties the lead checkout and blocks team_merge.
