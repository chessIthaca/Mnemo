+++
title = "Mnemo2's Memory panel tab reads knowledge records and memories"
supersedes = "2026-11-02-mnemo2-has-a-top-level-memory-tab-for-knowledge"
created = "2026-11-09"
evidence = "App.navigation.test.ts 'shows Memory, Graph and Trace in the panel from any main tab'; store.paths.test.ts; the earlier record's own content for the view's behaviour."
+++

Memory is a tab of Mnemo2's side panel (MemoryView.svelte + lib/memoryView.ts), not a main tab. It lists knowledge records and memories newest first with SPEC/DECISION/BUG/HOW/other and tier filters and paging, searches them, and reads a record as Markdown, preferring the knowledge file .coding/knowledge/<rel_path> (body, created date, status, links) over the stored digest, with clickable backlinks; a collapsed Diagnostics section shows embedder model/fingerprints, per-tier counts, index status and a recall test. Path links go through actions.openRepoPath: files open in the panel's Files tab at their line (which replaces Memory in the panel), plan documents open the Plans main tab with the panel left open. Backend: src-tauri/src/mnemo2/memory_view.rs over the open engine's MemoryApi/store. Read-only: no editing or deleting.
