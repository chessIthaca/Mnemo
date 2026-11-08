+++
title = "the startup sweep retires duplicate plan and review digests"
created = "2026-11-08"
+++

The memory startup integrity sweep retires duplicate plan and review digests. `quarantine_duplicate_digests` (src/memory/integrity.rs, called from src/host/brain.rs beside the git-state and synthetic-summary sweeps) lists semantic rows, keeps only PLAN and REVIEW record types, and groups them by (record type, normalized prefix-stripped title) via health.rs's `bare_title`/`normalize_title` (both `pub(crate)`).

Resolution rule: a group of two or more always keeps a DERIVED row (the file is the truth, re-derived by the indexer); when one is present every AUTHORED twin is quarantined. A group with no derived row keeps the newest authored row (largest `created_at`, ties by largest id) and quarantines the rest. Quarantine only, never delete: reason `duplicate digest of the same plan or review`, still reachable through `include_quarantined`, and a second run returns nothing. `access_count` protects nothing — the twin holds the same plan or review. SPEC/DECISION/BUG/HOW pairs are deliberately untouched; they belong to the health report's duplicate-title check.

Effect measured on the live store: 142 duplicate rows retired (141 authored PLAN + 1 authored REVIEW) of 5,137 semantic rows, lifting the recall eval's paraphrased hit@1 from 0.150 to 0.175 and hit@5 from 0.400 to 0.425 with the title control steady at hit@5 1.000.
