+++
title = "Fix the macOS test failures from CI round 4: platform tool counts and the full-command-queue test"
created = "2026-11-05"
+++

Symptom: build.yml run 42037069902 macOS `cargo test --workspace`: agent::factory::tests::free_default_excludes_the_reviewer_channel (left 36, right 42) and agent::factory::tests::tool_selection_exclude_carves_named_tools (left 35, right 41) fail; tests/integration/headless.rs close_and_wait_rejects_a_full_command_queue fails "the pending turn leaves its command queue full" (line 1448). · regression test: free_default_excludes_the_reviewer_channel

Full record for plan aa7e9ea2 (see .coding/plans/aa7e9ea2.md for the plan file).

regression test: free_default_excludes_the_reviewer_channel · path .coding/plans/aa7e9ea2.md · branch wt/mnemo @ 72e21743 (unmerged — exists only on this branch)
