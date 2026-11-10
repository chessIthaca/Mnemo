+++
title = "read_files has no re-read cache; only big files are outlined, and the quality grade is fill-only"
created = "2026-11-09"
evidence = "Ledger query on .coding/memory.db savings_events (2026-11-09): skeleton 139 events / 132,619 tokens, delta_read 3 events / 3,096 tokens. The user said \"Yes, remove both\"."
+++

Decided 2026-11-09 by the user. read_files no longer serves a signature skeleton for an unchanged whole re-read or a diff for a changed one: every whole re-read serves the file again. Only a source file over 500 lines (OUTLINE_MIN_LINES in src/tool/agent/read_files.rs) is served as its signature outline, on every whole read; outline_followup charges later ranged reads against what the outline saved.

Why: the savings ledger showed both levers were worthless. Over six weeks they saved about $0.31 in total, roughly 0.03% of traffic.

Consequences:
- ReadCache (src/tool/agent/read_cache.rs) holds only the outline debt.
- No tool resets anything on compaction or trims.
- QualityReport (src/agent/optimizer.rs) has only grade, fill_pct and decision_density, with score = 100 - fill_penalty. The waste and stale-read signals were fed only by skeletons.
- The config key [general.optimizer] delta_reads keeps its name so configs load. It gates only outlines and is labelled "Outline reads".
- Old delta_read and skeleton ledger rows are history; the Cost tab labels them "Retired."

Don't re-add a re-read cache without ledger evidence that re-reads cost real money.
