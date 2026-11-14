+++
title = "Claude Code lead's search: record stale-index repairs in the staleness log"
created = "2026-11-14"
+++

Symptom: When Claude Code leads, `search` and `search_read` repair stale index entries but never write ~/.mnemo/index-staleness.jsonl, and the note lacks the "full list: index-staleness.jsonl" pointer; the log has not changed since 2026-10-31. · regression test: code_search_repairs_are_recorded_in_the_staleness_log

Full record for plan bf9df2de (see .coding/plans/bf9df2de.md for the plan file).

regression test: code_search_repairs_are_recorded_in_the_staleness_log · path .coding/plans/bf9df2de.md · branch wt/mnemo @ bfdbed59 (unmerged — exists only on this branch)
