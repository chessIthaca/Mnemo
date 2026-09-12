+++
title = "one wt/* branch per agent directory (auto-fork, no auto-accumulation)"
created = "2026-08-26"
+++

DECISION (user, 2026-09-08): branch creation policy revised to "one wt/* branch per agent directory."

(1) create_plan AUTO-FORKS: when called with no `branch` arg while on main, it forks a stable per-directory wt/* branch from main (reusing an existing one if present — the resume flow); when already on a non-main branch, it reuses it (stays). Sub-plans (mid-executing) still reuse the parent's branch — unchanged. This closes G1 (branch creation was opt-in; plans silently stayed on main).

(2) The `branch` arg is reserved for EXPLICIT user requests only — the agent never auto-passes a branch name. run_all's RUN_ALL_STEER no longer tells agents to pass `branch: "fix/<slug>"` (that caused one unmerged branch per backlog item → accumulation mid-batch). This closes G3.

(3) Additional branches are created ONLY on explicit user request, never automatically.

(4) merge_to_main already deletes the merged branch (skill step 6: `git branch -d`), so no accumulation across sessions once merged.

REFINES DECISION c8fd9d60 (wt/* → main, develop tier removed — that stands); revises its "each plan works on its own wt/* branch" sub-claim to "one wt/* branch per agent directory, reused across plans." Truth: .coding/knowledge/decision/2026-09-08-one-wt-branch-per-agent-directory.md
