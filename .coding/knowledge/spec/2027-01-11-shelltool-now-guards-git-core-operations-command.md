+++
title = "ShellTool now guards git core operations (command-aware never_auto_for)"
created = "2027-01-11"
+++

The core-operation approval gate (git merge/push; `[git] core_operations` in Settings → Git) is now enforced on BOTH tool paths, closing the residual agent.md documented since 2026-08-24.

WHAT WAS WRONG: `ShellTool` overrode neither `never_auto_for` nor held the ops list, so `shell git push` / `shell git merge` ran UNPROMPTED even in Autonomous mode while the `git` tool forced the prompt (`GitTool::never_auto_for`, src/tool/agent/git.rs, consumed at src/agent/dispatch.rs to bypass auto-run). The skill/prompt text was changed in plan d826b9ad to route merge/push through the git tool; this is the code-level backstop.

FIX (plan f69317d4 follow-up A): `src/tool/agent/shell.rs` gains a `core_operations: Arc<RwLock<Vec<String>>>` field, `with_core_operations(...)` (mirroring GitTool), a `never_auto_for(&self, args)` override reading `args["command"]`, and the free fn `command_is_core_git_op(command, ops)`: lowercase, split on `; | & LF CR` into segments, require a `git`/`git.exe` executable at the START of a segment, skip git's global options (-c/-C/--git-dir/--work-tree/--namespace/--exec-path consume a value) to find the subcommand, then case-insensitively match it against the runtime list. `src/agent/factory.rs` passes the SAME `Arc` handle it gives GitTool, so a Settings → Git save takes effect on the next shell call without a registry rebuild.

TESTS (both verified failing when detection was neutralized with `false &&`): `tool::agent::shell::tests::core_git_op_detection_is_conservative_and_list_driven` (gated: `git push`, `git merge --no-ff x`, `git.exe push`, `git -C repo push`, `git --no-pager merge`, `cd repo && git push`, `foo; git push`, piped push; NOT gated: `git status`, `git log`, `git pull --no-rebase`, `cargo test`, `echo git-push-note`, `rg 'git push' src/`, `git commit -m "mention git push"`; plus a custom-list check) and the extended `agent::factory::tests::factory_set_core_operations_observed_by_built_git_tool` (asserts the built shell tool observes the live setter). Also `never_auto_for_reads_the_command_argument` for the malformed-args path.

KNOWN RESIDUAL (documented on the helper): a quoted string, an alias, a wrapper script, or `sh -c "git push"` still evades. False positives are what the conservative leading-token rule buys.
