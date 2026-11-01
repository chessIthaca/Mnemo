+++
title = "resolve a Mnemo2 worker merge conflict when the worker cannot merge"
created = "2027-01-11"
+++

Mnemo2 workers cannot integrate: the worker git gate refuses merge, rebase, switch, push and branch deletion ("TeamManager owns branches and integration"), so telling a conflicted worker to `git merge <lead branch>` fails. Working route: dispatch a fresh worker on the same plan step. Its worktree forks from the current lead branch tip. It replays the reviewed commit with `git show <commit> --binary | git apply --3way` (or `git cherry-pick --no-commit`), resolves the conflicting hunk keeping both sides, re-runs the checks and commits. Then review and merge the replay worker, and delete the conflicted one. Avoid it upfront: when two in-flight steps edit nearby lines of the same function, merge one first and dispatch the second afterwards.
