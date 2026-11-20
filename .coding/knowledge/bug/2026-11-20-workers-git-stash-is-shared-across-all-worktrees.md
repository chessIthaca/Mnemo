+++
title = "workers' git stash is shared across all worktrees, so parallel workers pop each other's work"
created = "2026-11-20"
evidence = "The reflog of worktrees/pool-24/HEAD shows \"reset: moving to HEAD\" at 05:06:47; dangling commits 78b29bbd (On wt/agent-hostToke: step14 wip) and d80cd2f5 (WIP on wt/agent-symVec01). pool-20 briefly held the models_host.rs and shared_service.rs edits."
+++

Every worker checkout (.worktrees/pool-N) is a git worktree of one repo, so `refs/stash` is ONE stack shared by all of them and by the lead checkout. On 2026-11-20, two parallel workers each stashed (to prove a test red) within 30 s. symVec01's `git stash pop` applied hostToken01's "step14 wip" stash in the wrong checkout, then got its own back. hostToken01's work survived only as the dangling commit 78b29bbd, and its next pop would have applied an unrelated Oct 31 stash.

Recovery: `git fsck --no-reflogs | grep "dangling commit"`, find the "On <branch>: ..." or "WIP on <branch>" commit, then `git stash apply <sha>` in the right checkout.

Prevention: tell workers never to use git stash (prove red by editing the fix out, or with a WIP commit). Mnemo's worker git and shell gates should refuse `git stash` push/pop/apply/drop without an explicit SHA, or the worker prompt should forbid it. This is not yet enforced in code.
