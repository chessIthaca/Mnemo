+++
title = "reviewer prompts carried the recalled-context rider (backlog 1d0332ca)"
created = "2027-01-11"
status = "superseded"
+++

BUG: every spawned reviewer's first prompt (the task) carried an embedded RECALLED CONTEXT block — memories semantically matched to the task text — polluting the self-contained reviewer protocol task with parent-project memories (noise at best, review bias at worst: the reviewer must judge the diff/plan, not be steered by recalled context) plus prompt bloat on every review (observed on plan 995436c2's reviewers, user report 2026-09-19). Root cause: the rider block in src/tool/agent/spawn_agent.rs execute() ran for EVERY spawn with no role check. Fix (backlog 1d0332ca, plan 660fdedc, wt/mnemo): role-gated skip — role.as_deref() == Some("reviewer") passes args.task through byte-identical and skips the store round-trip entirely; unrestricted sub-agents keep the rider (the skip is role-based only, not a spawn option — documented in the rider's comment). Regression test: reviewer_task_skips_the_recalled_context_rider (byte-identical assert_eq! + normal-spawn rider assertion on the same store; confirmed failing pre-fix).
