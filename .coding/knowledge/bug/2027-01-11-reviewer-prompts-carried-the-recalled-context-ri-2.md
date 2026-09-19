+++
title = "reviewer prompts carried the recalled-context rider (backlog 1d0332ca) — MERGED into main (5a86e9d)"
supersedes = "2027-01-11-reviewer-prompts-carried-the-recalled-context-ri"
created = "2027-01-11"
+++

MERGED into main at 5a86e9d (5a86e9d1157a497a8f6fdc567c29d584dda774eb) on 2027-01-24 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 33ad6ec) — supersedes this record's earlier unmerged marker. WHAT: every spawned reviewer's first prompt carried an embedded RECALLED CONTEXT block (root cause: the rider block in spawn_agent's execute ran for every spawn with no role check); fixed by the role-gated skip (plan 660fdedc, commit cd93eac). Regression test: reviewer_task_skips_the_recalled_context_rider.
