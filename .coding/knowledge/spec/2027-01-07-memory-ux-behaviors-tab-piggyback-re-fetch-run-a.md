+++
title = "Memory UX behaviors — tab piggyback re-fetch, run-all knowledge note, auto-recall version invalidation"
created = "2027-01-07"
+++

Three memory-UX behaviors shipped in commit 6eac1a5 (wt/agenticcoding; round-2 review PASS at .coding/reviews/2026-09-09-memory-ux-polish-review-round2.md):

1. Auto-recall cache invalidation via store version: MemoryStoreTrait::version() (default 0 — test mocks inert); MemoryStore bumps store_version (AtomicU64) at the END of write/update_memory/supersede_memory/delete_memory — after the row is committed/visible, and update/delete only when a row actually changed. The agent loop (src/agent/turn.rs) reads the version BEFORE the recall and stamps that value (bump-after-visibility + pre-recall stamp ⇒ any write invisible to a recall snapshot necessarily bumps above the stamped version → next same-query recall is forced fresh). Summarize still resets the cache. Regression test: same_turn_memory_write_invalidates_auto_recall_cache (src/agent/tests.rs).

2. Run-all completion note: pub static KNOWLEDGE_WRITES (process-global AtomicU64, mnemo::tool::memory) counts successful knowledge-file writes (bumped in MemoryWriteTool's knowledge-backed Ok arm); RunAllState.knowledge_writes_at_start snapshots it at arm time; end_run diffs (saturating_sub) and sets IpcState.run_completion_note = "wrote N knowledge record(s) during this run — uncommitted in the main tree" — attribution-neutral wording because the counter includes main-agent writes (sequential runs write on the main agent). Cleared when the next run arms; surfaced via RunAllProgress.note → BacklogView amber line next to the run controls (persists after the run ends).

3. Memory tab staleness: the overview poll (POLL_MS) piggybacks a list re-fetch when JSON.stringify(ov.counts) changes (first poll sets the baseline only); the fetch lives in one shared loadList useCallback (isCancelled param keeps the effect's cancelled-flag cleanup) reused by the tier-filter effect, the refresh button, and the piggyback.
