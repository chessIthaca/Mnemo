+++
title = "the external lead's search/search_read repaired stale index entries without writing index-staleness.jsonl"
created = "2026-11-14"
evidence = "Regression test code_search_repairs_are_recorded_in_the_staleness_log: red with the wiring disabled (note without the suffix), green with it; 59 mcp_server tests pass."
+++

Symptom: with Claude Code leading, `search` and `search_read` reported "reindexed N stale file(s) ... serving fresh index results" without the "; full list: index-staleness.jsonl" suffix, and `~/.mnemo/index-staleness.jsonl` stopped changing, so the cause of a stale file (edit vs touch) was unknowable.
Root cause: `code_search_tools` in src-tauri/src/mnemo2/mcp_server.rs built `SearchTool`/`SearchReadTool` without `.with_staleness_log(..)`, so `staleness_log` stayed None and `log_stale` (src/tool/agent/search.rs) recorded nothing. The in-app lead's tools are wired (engine factory: `StalenessLog::global()`; runtime.rs graph tool: `StalenessLog::at(config_dir)`).
Fix: `code_search_tools` takes `staleness_dir: Option<&Path>`; `call_tool` passes `self.host.config_dir()`, `tool_list` passes None (schemas only).
Regression test: `code_search_repairs_are_recorded_in_the_staleness_log` (mcp_server.rs tests) — indexed in-memory graph, file edited with mtime +5 s, asserts the note's suffix and a JSONL record per tool.
Lesson: any new construction site of SearchTool/SearchReadTool/GraphSearchTool must wire a staleness log, or repairs go unrecorded silently.
