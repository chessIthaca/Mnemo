+++
title = "session_search searches past worker and lead conversations"
created = "2026-11-08"
evidence = "src/memory/transcript_index.rs tests; session_search_is_a_lead_tool_and_answers; called through the real MCP server after the rebuild: 8.3 MB index, 0.9 s first search on the release build"
+++

`session_search` is a lead host tool (in-app and external leads; workers do not get it), in src-tauri/src/mnemo2/team/lead_tools.rs over src/memory/transcript_index.rs. With `query` (plus optional `worker`) it returns matching messages as `<source> #<seq> <role>: <snippet>`; with `worker` and `seq` and no query it returns one message, seq being the zero-based index in the file. Sources are `<state dir>/transcripts/<id>.json` and lead.json as `lead`.

The index is its own rebuildable SQLite file, `<state dir>/transcript_index.db`, not memory.db: it is a per-machine cache that must not touch the memory schema, its write lock or the recall eval. It syncs lazily on each call by file length and mtime, replaces a changed source in one transaction, keeps old rows for a damaged file and reports it, and rebuilds a corrupt or wrong-version file. Secrets are redacted before storing, system messages and reasoning are skipped, tool calls are indexed as `-> name args`, and each message is capped at 8,000 chars. Messages carry no timestamps, so hits rank by relevance only.
