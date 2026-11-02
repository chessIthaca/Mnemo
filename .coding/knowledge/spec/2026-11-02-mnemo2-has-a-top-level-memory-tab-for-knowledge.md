+++
title = "Mnemo2 has a top-level Memory tab for knowledge records and memories"
created = "2026-11-02"
+++

Mnemo2's top bar has Memory after Plans (route "memory", actions.openMemory, MemoryView.svelte + lib/memoryView.ts). It lists knowledge records and memories newest first with SPEC/DECISION/BUG/HOW/other and tier filters and paging, searches them, and reads a record as Markdown — preferring the knowledge file .coding/knowledge/<rel_path> (body, created date, status, links) over the stored digest — with clickable backlinks; a collapsed Diagnostics section shows embedder model/fingerprints, per-tier counts, index status and a recall test. Backend: src-tauri/src/mnemo2/memory_view.rs over the open engine's MemoryApi/store. Read-only: no editing or deleting.
