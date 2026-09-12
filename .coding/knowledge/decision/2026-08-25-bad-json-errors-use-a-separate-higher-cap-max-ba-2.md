+++
title = "bad-JSON errors use a separate higher cap (MAX_BAD_JSON_RETRIES=8), not MAX_RETRIES"
supersedes = "2026-08-25-bad-json-errors-use-a-separate-higher-cap-max-ba"
created = "2026-08-25"
+++

DECISION: Bad-JSON (LLM-produced malformed/truncated tool-call arguments) no longer counts toward MAX_RETRIES=3. It uses a separate, higher cap MAX_BAD_JSON_RETRIES=8 (src/agent/mod.rs:59). Rationale: the model can self-correct by emitting valid JSON, so three-strikes doesn't make sense for an error it can recover from. Tool-execution failures (result.success==false) still abort at MAX_RETRIES=3. The bad_json_count resets to 0 whenever valid JSON is produced. Implemented in src/agent/turn.rs (bad_json_count declared ~line 115, incremented in has_bad_json block ~1157, reset ~1230). Also: ToolCard no longer merges errored calls with successful ones (agentEventReducer.ts canMerge), and failed cards show a one-line error summary (Message.tsx + toolErrorSummary in toolCardPaths.ts).

MERGED into main at 7378343 (737834357967661b54d2dac037b4e6283f948118) on 2026-08-25. Branch feat/llm-tool-error-handling (pre-merge tip 7beb07d) landed via develop → main merge.
