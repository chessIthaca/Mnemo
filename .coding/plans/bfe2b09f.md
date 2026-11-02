# Plan: Worker shell commands never hang on a background process

## Goal
The shell tool always returns within its timeout, even when the command leaves a child process running, and no process a shell command started outlives that command.

## Kind
bug_fixing

## Context
Why: on 2026-11-01 two Mnemo2 workers (rowColumn71, layaPage76) hung for 50-90 minutes after starting a vite dev server, and every worker that started vite left node.exe/cmd.exe processes running that held its `.worktrees/agent-*` folder open, so team_merge could not remove the worktree ("The process cannot access the file because it is being used by another process").

Diagnosis (verified by reading `src/tool/agent/shell.rs`): `ShellTool::execute` spawns the shell (`program`/`flag` around line 320-330; PowerShell on Windows) with piped stdout/stderr and `kill_on_drop(true)`, and bounds only `child.wait()` with `self.timeout` (`DEFAULT_TIMEOUT` = 5 minutes, line ~57). After the shell itself exits, the success path joins the two pipe readers with NO bound: `let stdout_bytes = (&mut readers.out).await` (around line 600). A grandchild that inherited the pipe handles (a backgrounded vite/node, or `npx` → `cmd` → `node`) keeps the pipes open, so `pump_stream` never sees EOF and the call hangs forever. On the timeout path `kill_on_drop` kills only the shell process, not its descendants, so the descendants survive either way.

Required behaviour:
1. Bound the post-exit reader join: once the shell has exited, wait a short grace period (a few seconds) for EOF, then abort the readers and return the output collected so far with a clear note that a background process kept the output open and was stopped.
2. Kill the whole process tree a command started, both on completion and on timeout/interrupt. On Windows put the spawned shell in a Job Object created with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE (add the needed `windows-sys` features in `Cargo.toml`, e.g. `Win32_System_JobObjects`), assign the child right after spawn, and close/terminate the job when the call ends. On Unix spawn in its own process group and kill the group. A shell command therefore cannot leave a long-running server behind; that is intended: workers must not run dev servers (the lead renders previews).
3. Keep the existing streaming, filtering, interrupt and timeout semantics and messages otherwise unchanged.

Regression test: `background_child_holding_the_pipes_does_not_hang_the_call` in the `#[cfg(test)]` module of `src/tool/agent/shell.rs`. It runs a command that starts a long-lived grandchild inheriting stdout (Windows: e.g. `Start-Process -NoNewWindow ping -ArgumentList '-n','60','127.0.0.1'`; Unix: `sleep 60 &`) and then exits, with the tool's normal timeout. It asserts the call returns within ~15 s and the grandchild is no longer running afterwards. Without the fix it hangs until the test's outer `tokio::time::timeout` fails it. Wrap the call in an outer timeout so a regression fails rather than hangs.

Verification: `cargo test --lib tool::agent::shell` then `cargo test --lib`, and `cargo clippy --lib --tests -- -D warnings` (root crate, run from the repo root). Never start a dev server or any command that does not exit on its own while working on this.

## Acceptance scenarios
- A shell call whose command starts a background process that inherits stdout and then exits returns within ~15 s instead of hanging, and its output notes the background process was stopped.
- After any shell call returns (success, timeout, or interrupt), no process it started is still running.
- Ordinary commands (cargo test, npm run check, git) behave and stream exactly as before.

## Steps
- [ ] 1. **Bound the reader join and kill the process tree** — `src/tool/agent/shell.rs` (and `Cargo.toml` for the `windows-sys` JobObjects feature): bounded post-exit reader join with a note, Job Object (Windows) / process group (Unix) so the whole tree dies when the call ends or times out; regression test `background_child_holding_the_pipes_does_not_hang_the_call` plus a test that a timed-out command's grandchild is killed.

## Bug
A worker shell command that leaves a process running (e.g. `npx vite` started with `&`/Start-Process, or any dev server) never returns: the worker hangs for 50-90+ minutes, and the server outlives the worker and blocks its worktree from being removed.
