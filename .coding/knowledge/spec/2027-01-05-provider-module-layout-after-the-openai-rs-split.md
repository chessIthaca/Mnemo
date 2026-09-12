+++
title = "provider module layout after the openai.rs split (backlog 76efacba)"
created = "2027-01-05"
status = "superseded"
+++

SPEC (backlog 76efacba, plan 5ee62832, commits 523b99d/10884e1/d1e4897/f60fcc8/b9d7b8f on wt/agenticcoding): the provider layer layout after splitting the 6,589-line openai.rs size hotspot (zero behavior change; test counts identical throughout: root 1992/0/4, integration 16/3, src-tauri 186+4/0):

- src/provider/openai.rs (750 lines) — OpenAiClientConfig, OpenAiClient, impl LlmClient, and the complete() pipeline stages: prepare_request (validate + build body + open trace record → PreparedRequest{body,url,trace,rec_id,record_created}) → open_stream (POST + status check + reasoning_effort 400 fallback retry, takes &mut PreparedRequest) → spawn_stream (channel(128) + header capture + stop-boundary list → tokio::spawn). PreparedRequest lives here.
- src/provider/openai/request.rs — build_request_json + build_responses_request_json (pub(super) methods; params + GLM/DeepSeek special-casing), message_to_responses_input, raw_is_usable, sanitize_local_messages, validate_request_messages (pub(super)).
- src/provider/openai/sse.rs — parse_sse_chunk / parse_sse_buffer / parse_responses_sse_chunk / parse_responses_sse_buffer (pub(super)) + THINK tags / ThinkState / ThinkTagFilter (pub(super) + methods).
- src/provider/openai/stream.rs — StreamTask (pub(super), 11 fields) + pump_sse_stream (pub(super) async fn — the parse loop: read timeout, trace mirroring, think filter, stream guard, repetition guard, fallback Finish).
- src/provider/openai/guard.rs — find_boundary_cutoff + apply_stream_guard (pub; re-exported from openai.rs via `pub use` for path stability).
- src/provider/openai/tests.rs — the whole test module (3,363 lines, single file; the 4-way subdivision descoped — rationale recorded in plan 5ee62832).
- src/provider/models.rs — the /models discovery family: ModelWithVision, fetch_models_anthropic (x-api-key shape), fetch_models_with_vision (Bearer shape), parse_models_with_vision, model_supports_vision, ModelCaps/model_reported_caps + 16 tests. Callers: src-tauri/ipc/models.rs via mnemo::provider::models.
- src/provider/sse_util.rs — shared SSE + HTTP-response error plumbing: truncate_raw_stream, header_str, error_chain, finish_reason_label, SseOutcome, parse_data_url + provider_error / check_response / truncate_for_display (moved from openai.rs; used by openai, anthropic, vision, models).

Cross-module visibility is pub(super) within the openai subtree. complete() is a 3-line pipeline; every provider change now lands in a named stage. Detail: the plan file .coding/plans/5ee62832.md.
