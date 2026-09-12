## Verdict: FINDINGS (0 high, 2 low)

The build-perf changes (lld linker + line-tables-only debuginfo) are correct, MSVC-gated, multi-platform-neutral, and verified by the green test run. The git-test removal correctly converted 4 validation tests to pure-logic (validation fires before git spawn in each, verified by reading each execute path) and left no dead code. One pure-logic validator lost all coverage, and one stated metric overstates the result.

### Verified correct

**lld config (`.cargo/config.toml`):** `[target.x86_64-pc-windows-msvc] linker = "rust-lld"` is the correct modern form — `rust-lld` is the toolchain-shipped driver that auto-selects the `lld-link` (COFF) flavor for the MSVC target. Gated to the MSVC target, so macOS (`*-apple-darwin`) is unaffected → multi-platform neutral. lld still links against the MSVC CRT/SDK (drop-in for `link.exe`), so no toolchain change is needed. The green `cargo test` run proves the binary links and runs identically. Thorough inline comment. ✓

**debuginfo (`Cargo.toml`):** `[profile.dev]` + `[profile.test] debug = "line-tables-only"` (= debug 1) keeps line-number info for panic backtraces while cutting link time/binary size. Defined in the workspace-root Cargo.toml (correct location — profiles must live at the workspace root), so it applies to all members incl. `src-tauri`. I checked for tests that assert on debuginfo/backtrace presence: the only `backtrace`/`debuginfo` hits are (a) `panic::set_hook`/`catch_unwind` panic-safety tests (`app/mod.rs`, `codegraph/mod.rs`, `thread_util.rs` — don't inspect debuginfo), and (b) string fixtures in `shell_filter.rs` matching cargo's literal output text (`"Finished `dev` profile [unoptimized + debuginfo]"`) — cargo prints that string for any debug>0 incl. line-tables-only, so these are unaffected. No test depends on full debuginfo. ✓

**4 converted validation tests — validation fires before git spawn (verified by reading each execute path):**
- `merge_rejects_flag_injection`: `valid_branch_name("--no-commit")` rejects at git.rs:678, before `run_git`. ✓
- `checkout_rejects_flag_injection`: `valid_branch_name("-b")` rejects at git.rs:699, before `run_git`. ✓
- `git_show_rejects_option_injection_and_metacharacters`: `validate_commitish("--output=x")` rejects at git_read.rs:260, before `run_git_read`. ✓
- `git_log_limit_bounds_and_validates`: limit-range check at git_read.rs:178 and `validate_repo_path` at :185 both fire before `run_git_read`; the removed `limit:1` success assertion (which spawned git) was correctly dropped, keeping only the rejection assertions. ✓

**No dead code / no unused imports:** the removed helpers (`init_repo`, `current_branch` in git.rs/git_read.rs; `repo_with_change` in git_diff.rs) have no remaining references — the other `current_branch` hits are unrelated symbols in `git_ops.rs` (a repo method), `plan.rs` (its own test helper), and `files.rs` (a struct field). `tempfile::tempdir` is still imported/used by the kept validation tests; `tokio::process::Command` still used by `run_git_impl`/`run_git_read`. `git_diff.rs` test module is clean (`name_and_category` uses `"/tmp"`, no tempdir). ✓

**Warning-free:** the green `cargo test` under `#![deny(warnings)]` (both crate roots) proves zero warnings. ✓

### LOW-1 — `validate_branch_list_args` / `branch_list_argv` lost ALL test coverage

The branch-list allowlist (`validate_branch_list_args` / `branch_list_argv`, git.rs:270-307) is a **pure-logic, security-relevant validator**: it refuses positional args (which would create a branch via `git branch <positional>`) and any flag not on the read-only allowlist (`--set-upstream-to=`, `-d`, `-m`, …). It fires before any git spawn (execute path git.rs:792-797 → `branch_list_argv` → `validate_branch_list_args` → `Err` before `run_git`).

The four tests that covered it — `branch_list_rejects_positional_arg`, `branch_list_rejects_write_flags` (reject path) and `branch_list_accepts_verbose_args`, `branch_list_accepts_all_flag` (accept path) — were **removed entirely, not converted to pure-logic**. This is inconsistent with how the merge/checkout/git_show/git_log validation tests were handled (correctly converted by dropping `init_repo` and keeping the rejection assertions). No remaining test exercises `branch list` with args: `args_rejected_on_write_subcommands` tests the `is_read_sub` gate for `branch create`/`delete` (a different code path, git.rs:611), and `read_args_reject_write_exec_flags` tests the status/diff/log denylist (`validate_read_args`), not the branch allowlist.

Per the plan's own principle ("keep only pure-logic tests … validation rejections"), this validator should retain coverage. A regression that accidentally allows a positional (e.g., dropping the `!a.starts_with('-')` check at git.rs:278) would now go undetected.

**Fix:** add a pure-logic test — either call `validate_branch_list_args`/`branch_list_argv` directly (mirroring how `resolve_subcommand_from_either_field` tests the resolvers), or `tool.execute(json!({"subcommand":"branch","action":"list","args":["newbranch"]}))` asserting `!success` + output contains `"positional"`, plus `args:["--set-upstream-to=origin/main"]`/`["-d"]` asserting `"not allowed"`. No `init_repo` needed (validation fires before git). Optionally also assert the accept path (`["-vv"]`, `["-a"]` → `Ok`).

### LOW-2 — "Zero git subprocesses in the lib test suite" is inaccurate

The plan goal and measured results both state "Zero git subprocesses in the lib test suite." That is not literally true: `src/project/git_ops.rs` (test helper at :622 `repo.git(&["init"])`, :633 commit, :1291 `tempfile::tempdir()`) and `src/tool/workflow/plan.rs` (`current_branch` helper at :1908 spawning `git rev-parse`) still spawn real git subprocesses during `cargo test --lib`. These were correctly left in place — they test **our own** git-orchestration / auto-fork code (where git is the mechanism, not the thing under test), so removing them would untest our own logic. But the stated metric overstates the result.

**Fix:** qualify the claim in the commit message and any memory record — e.g., "the git *tool* tests no longer spawn git; `git_ops`/`plan` orchestration tests still do, by design." Do not claim "zero git subprocesses in the lib test suite."

### Additional observations (informational, non-blocking)

- The converted pure-validation tests (e.g., `merge_rejects_flag_injection`, `git_log_limit_bounds_and_validates`) still create a `tempdir()` used only as the tool's `project_root`, even though git is never spawned against it. Harmless and consistent with the other tests, but the tempdir is now strictly unnecessary for these — a future cleanup could pass any path. Not worth changing now.
- Consider updating the HOW memory `2026-08-30-build-test-perf-baseline-bottlenecks` (or its 2026-09-19 follow-up) to record the lld + line-tables-only improvements and the new measured baseline (7.2s rebuild / 21.2s test), so the next perf investigation starts from the post-optimization numbers.
