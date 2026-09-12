+++
title = "reasoning_effort \"off\" → DeepSeek instant 400 — FIXED, MERGED into main (07365fc)"
created = "2026-12-29"
+++

MERGED into main at 07365fc (2026-12-29, merge_to_main skill), branch wt/agenticcoding deleted (pre-merge tip 8f6f7d9). Bug plan a73d4855 (backlog 3f3044b9): DeepSeek rejects reasoning_effort "off" with an instant non-retryable HTTP 400 "unknown variant `off`" — its enum is none|minimal|low|medium|high|xhigh|max. Root cause: openai.rs build_request_json emitted the client config's effort verbatim with no policy guard; the resolvers collapsed "off"→None so DeepSeek never got its explicit thinking-off value. Fix: policy.rs reasoning_effort_off_wire_value(kind, model) → Some("none") iff DeepSeek-family (ProviderPolicy vendor detection); both config resolvers' off-arms consult it; the OpenAI request builder guards effort=="off" → policy value or omit. Regression: build_request_json_maps_off_effort_to_none_for_deepseek (+5 companions across openai.rs/config_io.rs/endpoints.rs). Full record: .coding/knowledge/bug/a73d4855.md · fix commit a73d38f.
