+++
title = "The lead reviews every worker deliverable hands-on before merging"
created = "2027-01-11"
+++

The user's standing rule: all worker work gets a hands-on review by the lead (Claude Code or the built-in lead) before team_review and team_merge. Reading the diff and counting passing tests is not enough. The lead exercises the change itself: runs the command or app path it changes, renders UI work in the worktree's dev server across real states (long/short content, statuses, narrow widths), clicks through every interaction, and judges the result against each point of the user's request: professional, functional, genuinely useful. Findings go back to the worker (team_route_fix / review_fix) before merge. Record what was exercised in team_check entries and in team_review evidence (execution, integration). See the mnemo-review skill for the review checklist.
