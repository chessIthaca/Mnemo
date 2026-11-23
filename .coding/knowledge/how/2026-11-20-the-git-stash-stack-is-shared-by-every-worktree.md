+++
title = "the git stash stack is shared by every worktree — never stash in a worker checkout"
created = "2026-11-20"
evidence = "git stash list is shared across C:\\Mnemo\\.worktrees\\* ; the incident was recovered from stash commit 78b29bbd per the lead's message."
+++

`git stash` is repository-global, not per-worktree: every agent checkout of this repository pops from the same stack. A worker that runs `git stash push` and later `git stash pop` can consume another worker's stash, and its own entry can be popped into a different checkout (observed 2026-11-20: `git stash pop` applied an ancient stash with ~59 conflicted paths, while the worker's own entry had already been popped elsewhere). Rule: never use stash inside a worker checkout. Park work as a WIP commit, or restore by SHA (`git stash apply <sha>` — a recovered stash commit stays addressable even after its entry left the stack). To prove a red check, edit the file, run, then restore it from the commit.
