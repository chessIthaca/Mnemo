+++
title = "Mnemo2 batch tool timers included earlier calls — FIXED"
created = "2027-01-11"
+++

Symptom: later tool-card suffixes looked cumulative. Root cause: ToolCallStart announced a model batch before src/agent/turn.rs executed calls sequentially, and Mnemo2 started timers at announcement. Fix: AgentEvent::ToolExecutionStart fires immediately before each logical call's first execute_tool_call (not retries/not-run calls); pending Mnemo2 lead/worker cards re-anchor on tool_execution_start, with announcement as fallback. Regressions: sequential_tool_calls_time_from_execution_not_announcement in src/agent/tests.rs plus independent lead/worker reducer and LeadChat suffix tests. Verified: full cargo test --workspace, 223 Mnemo2 tests, zero Svelte warnings.
