+++
title = "index-staleness log — which files went stale, why (plan 1d36f28d)"
created = "2027-01-11"
+++

Landed in commits e0dde65 (feature) + ede1341 (round-1 fixes, 0 high/3 low) on branch wt/mnemo; reviews: .coding/reviews/2026-09-27-1d36f28d-index-staleness-log-review.md (FINDINGS 0/3) and .coding/reviews/2026-09-27-1d36f28d-round2-fix-verify.md (PASS).

Behavior: every stale-index repair appends one JSON line per stale file to <global_config_dir>/index-staleness.jsonl — fields ts, tool (search | search_read | graph_search), path, action (inline-reindex | surfaced-only), cause (content-edit | touch | line-ending-flip | unindexed | vanished-or-unreadable | mtime-drift), mtime_index / mtime_disk / size_disk. 512 KB cap with rotate-before-append to `.1` (one backup) — rotation MUST precede the append or the tripping record is moved into the backup.

Where: src/index_staleness.rs (StalenessLog::global/at/record, classify, STALENESS_LOG_FILE/ROTATED_SUFFIX/STALENESS_LOG_CAP_BYTES); src/tool/agent/search.rs (log_stale + stale_detail; the disk-hash probe runs only when a log is wired AND stale.len() <= STALE_REINDEX_CAP, reusing codegraph's content_hash, never reimplementing it); search_read.rs; codegraph.rs (all three sweep arms); factory wires StalenessLog::global() in production, tempdir logs in tests (no test touches the real ~/.mnemo). Notes keep their existing prefixes and append ` (≤3 paths, +k more); full list: index-staleness.jsonl` (pointer only when a log is wired). FtsOutcome::Stale carries a `repaired` set so a reindex budget spent mid-set never drops those files' records. Docs: docs/CONFIGURATION.md final paragraph (+ FEATURES.md / PLAN.md).

Pending follow-up: the display half (Settings → diagnostics view) = backlog 7e674f06.
