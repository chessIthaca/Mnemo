+++
title = "tool-failure truth on the wire — Message.tool_is_error invariant + stop_reason inference"
created = "2027-01-11"
+++

The tool-failure truth invariant (plan 49f53bf5, landed 302612e on wt/mnemo): every Tool-role Message is constructed via Message::tool_result (the ONLY `role: Role::Tool` literal is inside that constructor, src/provider/mod.rs:375), which defaults `tool_is_error: false`. The four PRODUCTION push sites (src/agent/turn.rs) must keep the flag truthful: the tool loop (~2044) sets `!result.success`; every not-run path sets true — the interrupt orphan synthesis (~4153), the malformed-arguments retry (~4415), and the hard-stop synthesize_not_run_results (~4487). Anthropic's tool_result_block (src/provider/anthropic.rs:589) maps it to the wire's is_error flag; the field serializes only when true, so histories stored before it existed read as success. Stream side: StreamState.saw_tool_use makes a stream whose message_delta never arrived report FinishReason::ToolCalls (an explicit stop_reason always wins). Regression guards (each genuinely red on revert): failed_tool_result_is_serialized_for_the_provider_as_an_error + hard_stop_synthesis_marks_not_run_results_as_errors (src/agent/tests.rs), a_failed_tool_result_maps_to_is_error_true + missing_message_delta_still_reports_a_tool_turn (src/provider/anthropic.rs).

Branch wt/mnemo @ 3ca47f7952fed4d51a32a01dc394766c5d57eb87 (work tip, 2026-09-27) — a PR against main is opened from this branch, awaiting human review; the landing becomes part of main when the human merges.
