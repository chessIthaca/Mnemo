+++
title = "Reviewer prompts must be clean — skip the recalled-context rider for reviewer spawns (backlog 1d0332ca)"
created = "2027-01-11"
+++

Symptom: Every spawned reviewer's first prompt (the task) arrives with an embedded "recalled context" block — memories semantically matched to the task text — polluting the self-contained reviewer protocol task with parent-project memories (noise at best, review bias at worst: the reviewer should judge the diff/plan, not be steered by recalled context) plus needless prompt bloat on every review. Reproduce: spawn any role:"reviewer" agent whose task text matches existing memories (e.g. mentions "review", "diff", a file path, or a plan topic) with the memory store populated — the spawned agent's first prompt carries the recalled-context rider; visible in the reviewer's transcript/task echo (observed on the reviewers spawned during plan 995436c2's closing sequence, user report 2026-09-19). · regression test: reviewer_task_skips_the_recalled_context_rider

Full record for plan 660fdedc (see .coding/plans/660fdedc.md for the plan file).

regression test: reviewer_task_skips_the_recalled_context_rider · path .coding/plans/660fdedc.md · branch wt/mnemo @ cd93eac (unmerged — exists only on this branch)
