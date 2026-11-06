+++
title = "mnemo2-app test harness facts — no lib target, quit::tests paths, fetch_update vs try_update"
created = "2026-11-05"
+++

Durable facts about the mnemo2-app test harness and Rust toolchain divergence (no branch/plan pointer — the shipped code is the truth):

1. `AtomicUsize::fetch_update` is deprecated on CI's `dtolnay/rust-toolchain@stable` (renamed to `try_update`), and `try_update` does not exist on rustc 1.95. Under `#![deny(warnings)]` (set at both crate roots) that makes the deprecated call a hard build failure on CI only — never on the local toolchain. Version-neutral equivalent: `let left = a.load(Ordering::SeqCst); left > 0 && a.compare_exchange(left, left - 1, Ordering::SeqCst, Ordering::SeqCst).is_ok()`.

2. The `mnemo2-app` package has NO lib target. `cargo test -p mnemo2-app --lib` fails with "no library targets found". Its test target is the binary `mnemo2_mcp` from `src/mnemo2_mcp.rs`, so test module paths are `quit::tests::…`, `serve::tests::…`, etc. — NOT `mnemo2_mcp::quit::…`. A filter with the wrong prefix prints "0 filtered out / 0 passed" and exits 0, i.e. a silently green no-op.

3. `src-tauri/mnemo2-app/src/mnemo2_mcp/quit.rs` is not rustfmt-clean at HEAD (`rustfmt --check` reports several pre-existing diffs unrelated to any given change). Never run rustfmt on that file for a one-line change — it reformats unrelated lines and inflates the diff.

4. Several mnemo2_mcp tests deliberately exercise failure paths, e.g. a test invokes the binary with `--no-such-flag`, so an `error: Unrecognized option` line in `cargo test -p mnemo2-app` output is expected test output, not a test failure; trust the `test result:` lines over any grep for "error".

5. Piped PowerShell exit codes are the pipe's, not the command's — for `cargo test`/`cargo clippy` verdicts run unpiped and read `$LASTEXITCODE`, or rely on the `test result:` / `Finished` lines.
