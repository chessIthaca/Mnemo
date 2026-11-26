+++
title = "a finished remote worker releases its node checkout; only live records pin one"
created = "2026-11-26"
evidence = "reconcile_releases_a_merged_remote_workers_checkout in team/tests.rs; remote_agent_e2e.rs assert_released"
+++

The hub's host reconciler counts a node checkout as referenced only when a worker record with a non-empty local worktree names it in remote.checkout. A working remote worker keeps its hub shadow checkout as worktree. Merge and deletion clear worktree but keep record.remote for display, so the next reconcile pass (about every 60 s) detaches that node checkout. Never decide that a node checkout is in use from record.remote alone.
