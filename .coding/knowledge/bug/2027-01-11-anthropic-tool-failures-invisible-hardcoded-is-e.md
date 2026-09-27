+++
title = "Anthropic tool failures invisible — hardcoded is_error:false + Stop fallback on missing stop_reason"
created = "2027-01-11"
status = "superseded"
+++

Symptom: on the native Anthropic path the model re-issued the identical failed tool call in a loop (tool failures and tool-turn endings invisible to it). Root cause: Message (src/provider/mod.rs:189-254) had no failure bit, so tool_result_block (src/provider/anthropic.rs:580-585) hardcoded "is_error": false even though the tool loop knows result.success (src/agent/turn.rs:1987-2041, builds "[tool error] ..." text at 2000); a stream whose message_delta never arrived also fell back to FinishReason::Stop despite emitted tool_use blocks (anthropic.rs:778-784). Fix: Message.tool_is_error (serde default false, skipped) set at turn.rs:2040 (= !result.success) and at the not-run paths (interrupt + malformed-args) -> is_error; StreamState.saw_tool_use infers ToolCalls. Regression tests: failed_tool_result_is_serialized_for_the_provider_as_an_error (src/agent/tests.rs), missing_message_delta_still_reports_a_tool_turn (src/provider/anthropic.rs).
