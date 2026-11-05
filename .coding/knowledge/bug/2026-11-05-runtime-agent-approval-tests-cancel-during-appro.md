+++
title = "runtime::agent approval tests cancel_during_approval/interrupt_during_approval are timing-flaky under full-battery load"
created = "2026-11-05"
+++

cargo test --workspace on this repo can fail on exactly two timing-flaky tests: runtime::agent::tests::cancel_during_approval_exits_agent and runtime::agent::tests::interrupt_during_approval_stops_turn_and_keeps_agent_alive (panics "Cancel during approval must emit Exited (agent terminates)" / "Interrupt during approval must end the turn (Finished)", src/runtime/agent.rs:5496 area). Both pass in isolation (cargo test --lib cancel_during_approval, ~0.3s) and on battery re-run — flaky under full-battery parallel load, unrelated to the working diff. When a full battery fails on exactly these two, re-run the battery before chasing the diff.
