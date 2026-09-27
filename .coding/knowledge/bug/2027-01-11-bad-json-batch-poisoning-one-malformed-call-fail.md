+++
title = "bad-JSON batch poisoning — one malformed call failed its well-formed siblings (plan d3aedfee)"
created = "2027-01-11"
+++

Symptom: when ANY tool call in a batch arrived with empty/malformed JSON arguments, EVERY call in the batch got "arguments malformed or truncated — not run; retrying" and none ran — the well-formed siblings' work was wasted for a round-trip and the visible failure count doubled. (The empty-args CAUSE — Anthropic SSE event-name loss at chunk boundaries — was fixed separately in 5b7f99b; a malformed call can still occur, and batch isolation is the correct containment.)

Root cause (src/agent/turn.rs): has_bad_json at the run_turn call site used .any() over the batch, routing the WHOLE batch to handle_bad_json; its retry arm then errored every call — the guidance loop and the UI-error loop iterated tool_calls, not the malformed subset. Secondary: the first-failure message was a fixed ~85-word generic preamble that never named the failing tool or its missing fields — empirically ineffective in the 2027-01 session.

Fix (commits 499c2aa + 9d98c6b on wt/mnemo): the retry arm PARTITIONS the batch once (arguments parse → valid / else malformed) and executes the well-formed subset through execute_tool_batch, which gained a recorded_calls param (the assistant message records the whole sanitized batch; the execution loop runs only the subset; the pre-existing caller passes the same slice for both, byte-identical). Only malformed calls get the error + guidance; the triage-permanent and cap abort arms stay byte-identical whole-batch (they discard the batch from history — nothing runs). The first-failure text is now bad_json_retry_message(tool_name, required, empty_args): deterministic per (tool, variant), NO call id (repeated_tool_failure matches byte-identical content, and the repeat strategy-switch depends on it); required fields come from the request's advertised schemas via the new tool_schemas param + required_fields helper. src/agent/prompt.rs's MALFORMED-JSON RECOVERY bullet synced.

Regression tests: agent::tests::bad_json_batch_isolation_runs_valid_siblings (RED first: memory_search received the malformed-args error instead of running; GREEN: executed + both calls recorded once, malformed one errored); agent::tests::bad_json_first_failure_names_the_tool_and_its_required_fields (memory_write with empty args → names the tool + tier/title/content, <240 chars); agent::tests::error_recovery_malformed_json updated to the new contract (asserts `file_read` + "required: path" from its schema).

Plan d3aedfee; SPEC c2686f85 amended (the per-(tool,variant)-deterministic invariant supersedes the old byte-identical first-failure text).
