+++
title = "A shrinking .coding/backlog.jsonl is usually the 30-day tombstone purge, not lost work"
created = "2026-11-02"
+++

BacklogStore::open (src/app/backlog.rs) purges soft-deleted items (deleted_at) older than 30 days and collapses duplicate-id lines (first wins), then persists. Mnemo2's Backlog section opens the store on every command, reads included, so the first read after a long gap can rewrite the tracked file and drop many lines; on 2026-11-02 104 lines became 26 that way with no live item lost. Before restoring, check the removed lines for a deleted_at field. Since then every save goes through a guard (persist_with_permitted_removals) that refuses, with an error log, any write dropping a live Pending/InFlight item that was not explicitly removed. Side effect: the purge leaves .coding/backlog.jsonl modified, which blocks team_merge's clean-tree rule until it is committed.
