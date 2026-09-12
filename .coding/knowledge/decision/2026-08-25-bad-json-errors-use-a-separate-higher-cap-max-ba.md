+++
title = "bad-JSON errors use a separate higher cap (MAX_BAD_JSON_RETRIES=8), not MAX_RETRIES"
created = "2026-08-25"
status = "superseded"
+++

DECISION: Bad-JSON (LLM-produced malformed/truncated tool-call arguments) no longer counts toward MAX_RETRIES=3. It uses a separate, higher cap MAX_BAD_JSON_RETRIES=8 (src/agent/mod.rs:59). Rationale: the model can self-correct by emitting valid JSON, so three-strikes doesn't make sense for an error it can recover from. Tool-execution failures (result.success==false) still abort at MAX_RETRIES=3. The bad_json_count resets to 0 whenever valid JSON is produced. Implemented in src/agent/turn.rs (bad_json_count declared ~line 115, incremented in has_bad_json block ~1157, reset ~1230). branch feat/llm-tool-error-handling @ 65fc772 (unmerged). Also: ToolCard no longer merges errored calls with successful ones (agentEventReducer.ts canMerge), and failed cards show a one-line error summary (Message.tsx + toolErrorSummary in toolCardPaths.ts).
