+++
title = "Mnemo2's Memory panel tab reads knowledge records and memories"
supersedes = "2026-11-02-mnemo2-has-a-top-level-memory-tab-for-knowledge"
created = "2026-11-09"
evidence = "App.navigation.test.ts 'shows Memory, Graph and Trace in the panel from any main tab'; store.paths.test.ts; the earlier record's own content for the view's behaviour."
+++

Memory is a tab of Mnemo2's side panel (MemoryView.svelte + lib/memoryView.ts), not a main tab. It lists knowledge records and memories newest first with SPEC/DECISION/BUG/HOW/other and tier filters and paging, searches them, and reads a record as Markdown, preferring the knowledge file .coding/knowledge/<rel_path> (body, created date, status, links) over the stored digest, with clickable backlinks; a collapsed Diagnostics section shows embedder model/fingerprints, per-tier counts, index status and a recall test. Path links go through actions.openRepoPath: files open in the panel's Files tab at their line (which replaces Memory in the panel), plan documents open the Plans main tab with the panel left open. Backend: src-tauri/src/mnemo2/memory_view.rs over the open engine's MemoryApi/store. Read-only: no editing or deleting.

Amended 2026-11-11: Records stay read-only, but the Diagnostics section now also holds Memory upkeep, the one deliberate exception (user request): Clean up memory, Rebuild search and Rebuild index. They are backed by src-tauri/src/mnemo2/memory_maintenance.rs (commands memory_cleanup, memory_rebuild_search, memory_rebuild_index over MemoryApi::cleanup/rebuild_search/rebuild_index, progress events on memory://maintenance in Mnemo1's wire shape, one run at a time via mnemo::maintenance's busy flag) and lib/memoryMaintenance.ts. Cleanup skips the live sessions of the lead and every worker and summarises with AgentLoopFactory::summarize_client (the [models.summarize] model, else the default provider).
