+++
title = "tool::agent::shell timing tests flake under parallel-agent load — retry, don't chase"
created = "2026-11-20"
evidence = "Plan e90e3047 step 1: run_checks FAIL (3 shell tests, 3739 passed) → isolated run 4/4 passed in 17.7s → run_checks re-run all four PASS, no code change between."
+++

`cargo test --workspace` on this multi-agent machine can flake three timing-margin tests in src/tool/agent/shell.rs — light_command_never_waits (light command < 5s), slot_wait_is_not_counted_as_silence (4s silence window), background_child_holding_the_pipes_does_not_hang_the_call (< 15s grace bound) — when parallel agents hold the machine-wide build slots / peg the CPU. Seen 2026-11-20 (13.0s/4.0s/16.8s observed); the same three passed in isolation in 17.7s, and a full re-run of run_checks went green with no code change. Recipe: confirm in isolation (`cargo test -p mnemo --lib -- <the three names> killed_command_leaves_no_process`), then simply re-run run_checks — do not chase a code cause. shell.rs also has its own local test-only pid_alive copy, so process_tree::pid_alive changes cannot affect these tests.
