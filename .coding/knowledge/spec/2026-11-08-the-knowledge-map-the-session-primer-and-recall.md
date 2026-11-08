+++
title = "the knowledge map, the session primer and recall_count"
created = "2026-11-08"
evidence = "src/memory/knowledge_index.rs tests; the_primer_skips_file_less_stale_and_status_first_rows; note_recalled_does_not_bump_the_store_version; the installed hook printed the 255-byte map on this repo"
+++

src/memory/knowledge_index.rs generates a gitignored .coding/knowledge/INDEX.md after every knowledge write and project open: up to 10 sections by area, 15 newest records each. An area comes from a record's most-cited path, with `src-tauri/src/<x>` shortened to `src-tauri/<x>` and `mnemo2/src/<x>` to `mnemo2/<x>`; the smallest areas fold into `general`. A Claude Code lead's SessionStart gets a one-line map of areas and counts, at most 1.5 KB, only when the hook sends session_start; other hooks stay byte-identical.

The primer (`primer_records`, 8 rows) takes SPEC, DECISION and HOW rows that a file backs, by access_count then recency. It skips BUG records (old fixed bugs are poor standing context and bug searches are already gated), rows stamped moved_paths, and legacy digests whose content starts `MERGED into main`, because format_primer shows the first 200 chars and those lead with landing status. recall_count is a nullable column auto-recall increments on a fresh recall through note_recalled; recall_peek stays read-only, and it does not touch access_count, last_accessed_at or version(). The plan, spawn and steering riders do not count yet.
