+++
title = "three-strikes error caps + tool-card error display"
created = "2026-08-25"
+++

Four "three-strikes"-style caps exist; do not conflate:
1. `MAX_RETRIES=3` (src/agent/mod.rs:42, checked in src/agent/turn.rs) — consecutive TOOL-EXECUTION errors. Incremented in ONE place: turn.rs:~1405 tool-execution failure (`result.success==false`, unless is_user_denial_tool_output). Aborts at turn.rs:~1679 with "Aborting turn: 3 consecutive tool errors (the model may be stuck)."
2. `MAX_BAD_JSON_RETRIES=8` (src/agent/mod.rs:58) — consecutive LLM-produced malformed/truncated tool-call arguments (bad JSON). Incremented in the `has_bad_json` block (turn.rs:~1157); resets to 0 when valid JSON is produced (turn.rs:~1230). Aborts at turn.rs:~1158. The model CAN recover by emitting valid JSON, so this cap is deliberately higher than MAX_RETRIES — three strikes does not make sense for an error the model can self-correct.
3. `MAX_PROVIDER_TURN_ATTEMPTS=3` (src/runtime/agent.rs:25) — provider/network/stream errors, retried w/ exponential backoff (1s/2s/4s) in run_turn_attempt. Model CANNOT continue (provider down) — NOT what "llm error for tools" means.
4. `DOOM_ERROR_STREAK=3` (frontend agentEventReducer.ts:112) — doom sound trigger; counts ALL error events + failed tool results (cosmetic, fires only on a FINAL error w/ streak>=3).

User request (2026-12): (1) bad-JSON (LLM) errors should NOT count toward the 3-strike abort — model can continue; (2) ToolCard merge (reduceToolCallStart canMerge, agentEventReducer.ts:~439) must NOT merge a new call into a card whose last call completed with an error; (3) errored tool calls need a simple one-line error description in the ToolCard (Message.tsx). The Output right-panel tab was REMOVED (plan c9b8580f) — "output window" = the chat transcript ToolCard.
