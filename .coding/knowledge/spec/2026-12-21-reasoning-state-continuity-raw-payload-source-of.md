+++
title = "Reasoning-state continuity — raw-payload source of truth (Rules 1-6, Rule 7 UI waived)"
created = "2026-12-21"
+++

SPEC: Reasoning-state continuity — raw-payload source of truth (implemented 2026-12-21, commits 949fed2 + aec3d39 on wt/agenticcoding, plan 70910b7d).

The harness now follows the multi-provider reasoning-state-continuity spec (Rules 1-6; Rule 7 UI-removal waived — thinking panel + Trace stay):

RULE 1 (raw source of truth): Each provider's assistant turn is stored VERBATIM in `Message::raw` (reassembled from streamed deltas via `LlmEvent::RawAssistantDelta`). Both request builders (openai.rs `build_request_json`, anthropic.rs `message_to_json`) echo `raw` unchanged for assistant turns instead of reconstructing from view fields — so every key the provider sent (reasoning_content, thought_signature, thinking-block signature, encrypted_content, unknown keys) round-trips byte-identical. `raw_is_usable()` guard (openai.rs:1797) falls through to field construction when raw is empty (null turn) or has malformed tool-call args (sanitized by turn loop). `TurnView`/`Message::view()` is the read-only derived view for UI/planning — never sent back.

RULE 2 (ProviderPolicy): `src/provider/policy.rs` — `ProviderPolicy { name, reasoning_required, stateful_key, reasoning_field, include_params, template_kwargs, signatures_portable_across_models, vendor }` + `for_kind_and_model(kind, model)` registry (Gemini/Claude/OpenAI Responses/DeepSeek/Qwen/Kimi/GLM/vLLM) + `is_cross_vendor()`. Drives cross-vendor detection; full builder consultation of include_params/template_kwargs/reasoning_field is a follow-up (values currently hardcoded correctly in builders).

RULE 3 (stateful/stateless): OpenAI Responses API (`use_responses_api` flag on OpenAiClientConfig) — `build_responses_request_json` sends `previous_response_id` when a prior turn has `response_id` (captured via `LlmEvent::ResponseId` from `response.created`/`response.completed`); `parse_responses_sse_chunk`/`parse_responses_sse_buffer` parse the Responses API SSE protocol. Stateless raw-echo path always available; both pass the same tests. Gemini is proxy-only (stateful_key=None — the stateless thought_signature path IS its continuity mechanism; no native Interactions API).

RULE 4 (no trim in open loop): `summary_cut_index` (context.rs) + `retreat_past_open_tool_loop` — if the cut lands inside an open (incomplete) tool-use loop, retreat to the loop's start (last user message). `cut <= 1` skip guard in both `summarize` and `summarize_with_interrupt` (loop spans whole conversation → compaction does not run).

RULE 5 (model switching): `strip_cross_vendor_reasoning(messages, current_kind, current_model)` (mod.rs) — pre-build normalization pass called in turn.rs before `complete_with_retry`. Cross-vendor turns: strip reasoning from raw + set `reasoning_stripped=true`. Same-vendor different-model: unchanged. One-way (irreversible). `strip_reasoning_from_raw` handles both OpenAI flat keys + Anthropic content-array blocks.

RULE 6 (fail loudly): `Error::is_serialization_bug()` (error.rs) detects: missing thought_signature, reasoning_content must be passed back, Expected thinking…found text, modified prior content. `is_non_retryable()` calls it (so ALL retry layers skip). `complete_with_retry` (dispatch.rs) emits "SERIALIZATION BUG (not retried)" error. No strip-and-resend retry exists anywhere.

LEGACY BRIDGE: `provider_meta`/`reasoning_content` fields on Message/ToolCall retained for serialized-conversation backward compatibility (no builder reads them; removal is a follow-up requiring a load migration). `PROVIDER_PASSTHROUGH_KEYS`/`capture_provider_meta`/`apply_provider_meta` removed (superseded by raw echo).

Tests: 1837 + 16 pass, 0 warnings. 5 acceptance tests per provider (sequential tool call, byte equality, compaction safety, cross-vendor switch, stateful/stateless parity).

Amended 2027-01-11: AMENDED 2027-01-11 (plan d4b951be, backlog be85a2a2, provider-comms review finding 4 / executive summary BUG 10): RULE 2's ProviderPolicy struct listing above is superseded — the `template_kwargs` field (GLM {"clear_thinking": false}, Qwen/Kimi {"preserve_thinking": true}) was DELETED: no request builder ever read it (the params never left the process), neither name appears in current public vendor docs, and the wiring path is gated on the vendor-policy config direction the user rolled back 2027-01-09 (main reset to e0ac5dd). The struct is now { name, reasoning_required, stateful_key, reasoning_field, include_params, signatures_portable_across_models, vendor } (+ retention/fold fields); the follow-up mention reduces to include_params/reasoning_field. include_params is NOT dead — the Responses builder hardcodes its include value. Regression guard: policy_source_carries_no_never_sent_template_kwargs (src/provider/policy.rs). Detail: .coding/knowledge/bug/2027-01-11-policy-rs-template-kwargs-must-send-claims-never.md.
