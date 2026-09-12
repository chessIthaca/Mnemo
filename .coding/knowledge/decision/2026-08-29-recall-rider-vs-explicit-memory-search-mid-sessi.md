+++
title = "Recall-rider vs explicit memory_search — mid-session lookups are gap-driven (final)"
supersedes = "2026-08-29-recall-rider-vs-explicit-memory-search-measureme"
created = "2026-08-29"
+++

DECISION (2026-09-15, plan 812e1f0a, search-optimization round): FINAL — supersedes the "wait ~2 weeks" plan. BOTH MANDATORY memory_search triggers in TOOL_STRATEGY (prompt.rs) stay verbatim (planning pointer-first bundle + bug record_type:"bug") — cheap indexed lookups, high-value. Mid-session memory_search is now GAP-DRIVEN: search only when auto-recalled hits + plan rider do not cover the question; a lookup returning what auto-recall already carried is a wasted round-trip (evidence: 6/9 redundant mid-session lookups in the 2026-09-15 tally; 2/5 relevance in the steering-follow-ups recall batches). Tool-scoped counters (get_steering_stats: recall-rider fired/switched + the new fired-only literal-tip marker) verify effectiveness post-hoc instead of gating the change; the 14-day rider age gate (c40d6b7) already removed stale-digest noise. File: .coding/knowledge/decision/2026-08-29-recall-rider-vs-explicit-memory-search-mid-sessi.md (supersedes 2026-08-29-recall-rider-vs-explicit-memory-search-measureme.md).
