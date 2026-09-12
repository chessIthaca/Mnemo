+++
title = "GLM-5.3-Flash stop tokens, sampling parameters, and SSE stream guard implementation"
created = "2026-12-24"
+++

SPEC: GLM-5.3-Flash stop tokens, sampling parameters, and SSE stream guard implementation (commit b3b18a3, plan 5ce79c00). Implemented .coding/knowledge/spec/2026-12-21-glm-5-3-flash-stop-token-boundaries-sampling-par.md:
1. Endpoint & ModelSpec in src/config/endpoints.rs support optional temperature, top_p, stop, stop_token_ids, and extra_body (merged last into request body); resolution helpers on Endpoint cascade model-level overrides over endpoint defaults.
2. Settings save in src/config/patch.rs:apply_endpoints preserves unexposed sampling/stop/extra_body fields.
3. OpenAiClientConfig in src/provider/openai.rs carries resolved sampling fields and propagates them to build_request_json. GLM-5.3 built-in stop defaults expanded to include <|observation|> and \n\n\n\n alongside <|endoftext|>, <|user|>, <|assistant|>, and \n\n\n; stop_token_ids [151329, 151330, 151336].
4. Client-side SSE stream guard in OpenAiClient terminates turns early with FinishReason::Stop when raw boundary tokens appear in text deltas.
