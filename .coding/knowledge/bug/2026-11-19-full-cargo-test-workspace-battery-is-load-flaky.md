+++
title = "full `cargo test --workspace` battery is load-flaky on this machine (approval, headless, shell, process_tree, browser tests)"
created = "2026-11-19"
evidence = "Plan 5b85ab61 step 4 (wt/agent-neutralS, commit 9a58c953): baseline run with the diff stashed failed cancel_during_approval_exits_agent at src\\runtime\\agent.rs:5881 (3600 passed, 1 failed); a loaded run failed 6 tests including shell/process_tree/browser; each passed alone."
+++

Under load (the Mnemo2 app + its msedgewebview2 processes running, plus parallel cargo builds), `cargo test --workspace` in C:\Mnemo fails on timing-sensitive tests that pass in isolation:
- `runtime::agent::tests::cancel_during_approval_exits_agent` / `interrupt_during_approval_stops_turn_and_keeps_agent_alive` (src/runtime/agent.rs:5881/5981, 2s / 5s deadlines)
- `headless::approve_via_api_prompts_and_honours_decision` (tests/integration/headless.rs:923, 5s recv timeout)
- `process_tree::tests::kill_on_close_job_ends_the_whole_tree`, `tool::agent::shell::tests::background_child_holding_the_pipes_does_not_hang_the_call` ("call took 23s — expected the few-seconds grace bound"), `tool::agent::shell::tests::timed_out_command_kills_the_background_grandchild`, `tool::browser::tests::click_and_type_roundtrip`.
Proving pre-existing: `git stash` the worker diff, run the same command on the base, see the same failures, `git stash pop`. The mnemo lib battery alone was seen at 35s and at 212s for the same 3600+ tests — the variance is machine load, not code.
Also: the exact check command often needs a warm-up `cargo test --workspace --no-run` first, because the shell's fixed 5-minute limit kills compile+run in one call (a cold no-run alone was 4m20s).
