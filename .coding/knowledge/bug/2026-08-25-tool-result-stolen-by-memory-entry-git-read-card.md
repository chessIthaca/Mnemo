+++
title = "tool_result stolen by memory entry — git_read card stuck \"running\""
created = "2026-08-25"
+++

BUG: git_read tool card stuck "running" forever (user report 2026-08-25, backlog 12d7ecb8).
Symptom: in a parallel tool block [git_read, memory_search, search], the git_read card never gets its ✓; memory_search and search cards complete fine.
Root cause: frontend/src/hooks/agentEventReducer.ts reduceToolResult (~520-551): the memory-entry finalization loop matches ANY running `memory` transcript entry on ANY tool_result — never correlates event.tool_call_id (memory entries store no call id). git_read's result arrives first → finalizes the memory_search entry (misparsing git output as tier/title) → memoryFinalized=true skips the tool-card loop → git_read's call.result stays null forever.
Fix: stamp id+index+args on memory entries at tool_call_start; accumulate arg deltas on running memory entries by index; finalize ONLY on entry.id === event.tool_call_id; fall through to the tool loop when unmatched. UX: git_read header chip (op + commit/path/limit) + memory_search query chip (args-parsed query, matched count from "N memories matched" header).
Regression test (RED confirmed pre-fix 2026-08-25, frontend/src/hooks/useAgentStore.test.ts): `regression: a foreign tool's result must not be stolen by a running memory entry (git_read stuck "running")`.
