+++
title = "no rebase-before-merge in merge_to_main (pointer stability, review fidelity, git-tool surface)"
created = "2027-01-11"
+++

User asked (2026-09-14) why a long wt/* branch isn't rebased onto main before the merge. Rationale for keeping the current `git merge --no-ff` (skill .coding/skills/merge_to_main.toml; sync-first step 2 = fetch + pull --no-rebase):
1. Pointer stability is load-bearing. Memory records and review reports cite exact branch SHAs ("MERGED into main at <sha>", "pre-merge tip <sha>"). A rebase rewrites every branch hash and silently dangles them; the --no-ff merge keeps the reviewed commits reachable verbatim as the merge's second parent, even after `git branch -d`.
2. Review fidelity. The reviewer signs off on the branch tip; rebase-after-review lands different hashes, or unreviewed conflict resolutions if the rebase was unclean. The only sound ordering would be rebase BEFORE the review.
3. Tooling/approval surface. The `git` tool's subcommands (status/diff/log/commit/merge/checkout/stash/branch/push/restore) include no rebase, so it would run via `shell`, bypassing the structured, subcommand-checked core-operation path with its always-on approval gate; an interrupted rebase also leaves rebase-in-progress / detached-HEAD state the crash-resumption model doesn't handle (a conflicted merge has `git merge --abort`).
Rebase is genuinely safe for a strictly private branch (one author, one worktree) — safety turns on shared history, not branch length; a force-push is unsafe once another contributor has fetched/based work on the branch. Both orders land identical content on main; the merge resolves divergence once against the synced tree that will actually be pushed, while a rebase replays commit-by-commit (same hunk can conflict per commit).
