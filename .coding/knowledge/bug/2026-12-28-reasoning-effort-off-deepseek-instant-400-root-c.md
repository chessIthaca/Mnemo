+++
title = "reasoning_effort \"off\" → DeepSeek instant 400 — root cause + fix"
created = "2026-12-28"
+++

BUG: DeepSeek instant-400 on reasoning_effort "off" (backlog 3f3044b9). Symptom: HTTP 400 "unknown variant `off`" — DeepSeek's enum is none|minimal|low|medium|high|xhigh|max. Root cause: openai.rs build_request_json emitted the client config's effort verbatim with no policy guard; the resolvers collapsed "off"→None so DeepSeek never got its explicit thinking-off value. Fix: policy.rs reasoning_effort_off_wire_value(kind,model) → Some("none") iff DeepSeek-family (ProviderPolicy vendor detection); both resolvers' off-arms consult it; builder guards effort=="off" → policy value or omit. Regression: build_request_json_maps_off_effort_to_none_for_deepseek (+5 companions across openai.rs/config_io.rs/endpoints.rs).
