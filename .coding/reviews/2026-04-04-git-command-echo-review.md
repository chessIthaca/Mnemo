# Review: Display executed git command in git tool output

**Date:** 2026-04-04
**Plan:** Display executed git command in git tool output
**Files changed:** `src/tool/agent/git.rs`, `.coding/backlog.json`, `.coding/plans/*`, `.coding/plans/stack.json`

## Summary

`run_git` now builds a `command_display` string (`"git"` + args joined by spaces) and prepends `$ {command_display}\n` to `ToolResult.output`, ahead of the existing stdout/stderr + `[exit code: N]` block. The `data` JSON field is unchanged. A new test `output_echoes_executed_command` asserts the echo line for `status` and `log`.

## Findings

### Correctness
- **No findings.** The echo faithfully reflects the argv passed to `Command::args` (same `args: &[&str]` slice), so the displayed command matches what git receives. The `commit` flow calls `run_git` twice; the returned result echoes the final `commit` command, and the discarded `add` result's echo is never surfaced — correct.

### Bugs
- **Low — error path omits the echo** (`src/tool/agent/git.rs:116`). When `cmd.output()` returns `Err` (spawn failure), the `ToolResult::error(format!("failed to run git: {e}"))` branch does not include the `$ git ...` line. This is a minor inconsistency: a failed-to-spawn command shows no echo while a spawned-but-failed command does. Not a functional bug (the command never ran), and arguably acceptable since there's no exit code either. Flagging for awareness only; no fix required for this plan's scope.

### Security
- **No findings.** The join is display-only; args are still passed to git as discrete argv with no shell, so there is no injection surface. Existing `valid_branch_name` flag-injection guards are untouched and still apply before any user-supplied value reaches `run_git`.

### Constitution compliance
- **No findings.**
  - `run_git` is private → no doc comment required. No new public items were added.
  - Change is platform-agnostic string formatting; the Windows `CREATE_NO_WINDOW` handling is untouched.
  - No commit to `main` in this diff.
  - Tests added; `cargo test` to be run by the implementing agent before step completion.

### Bookkeeping files
- `.coding/backlog.json`, `.coding/plans/stack.json`, plan markdowns: well-formed, no corruption.

## Verdict

Diff is clean. One low-severity observation (error-path echo omission) noted for awareness; no blocking findings.
