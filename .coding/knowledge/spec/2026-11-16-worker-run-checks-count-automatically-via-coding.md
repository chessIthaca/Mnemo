+++
title = "worker-run checks count automatically via .coding/checks.toml"
created = "2026-11-16"
evidence = "src/tool/agent/shell.rs validated_check_command and tree measurement; src-tauri/src/mnemo2/team/pump.rs counted_check; team/tests.rs a_real_shell_check_run_records_a_check_at_turn_end."
+++

Mnemo2 records a worker's own check run as a passed merge check only when: the command's trim() is byte-equal to an entry of `.coding/checks.toml` (`commands = [...]`) read from the LEAD's project root (never the worker checkout); the command and the entry contain no control character and none of ; | & > < ` $( (validated_check_command in src/tool/agent/shell.rs, also applied when loading the file in src-tauri/src/mnemo2/team/spawn.rs); the shell `cwd` is absent or canonically the checkout root; exit_code is 0; and the shell tool measured equal content trees (temp-index `git add -A` + `write-tree`) immediately before spawn and after exit (data.tree_before / tree_after). At turn end, before the finished notice and under the merge lock, the pump records each such run whose tree equals the committed tip's tree. The lead reruns only what is missing or doubtful. This repo lists cargo test --workspace, cargo clippy --workspace --all-targets, npm test --workspace mnemo2, npm run check --workspace mnemo2.
