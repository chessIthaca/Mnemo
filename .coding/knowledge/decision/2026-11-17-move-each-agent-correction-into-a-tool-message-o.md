+++
title = "move each agent correction into a tool message or a failing test, not agent.md"
created = "2026-11-17"
evidence = "tests/integration/policy.rs (cfg_windows_stays_in_known_files, allow_attributes_do_not_grow); shell.rs tests uncounted_check_run_gets_a_note, shell_file_write_gets_a_note"
+++

When an agent repeats a mistake, the fix goes to the strongest level it can reach, and agent.md keeps only judgement calls and short pointers. The levels, strongest first:
1. Code that makes the mistake impossible to write.
2. A test or lint that fails the build.
3. A tool message at the moment of the mistake.
4. Prose.

Rationale: a rule in agent.md is a request nobody is told they broke, and it costs context in every prompt.

In place:
- The shell tool notes an uncounted check run (a check command run chained, piped, redirected or outside the root), a check run that changed tracked files, and shell file writes (src/tool/agent/shell.rs: check_not_counted_note, check_tree_changed_note, SHELL_FILE_WRITE_NOTE).
- tests/integration/policy.rs ratchets Windows-only cfg code to a listed set of files with reasons, and #[allow] counts per file. Both directions fail: a new occurrence, or a stale entry.

When adding a rule, first ask which of these levels can hold it.
