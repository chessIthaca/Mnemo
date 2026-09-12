+++
title = "Gemini is proxy-only — stateless thought_signature is the continuity mechanism (no Interactions API)"
created = "2026-12-20"
+++

SPEC: Gemini reasoning-state continuity — proxy-only, stateless thought_signature path (Rule 3 investigation, 2026-12-21).

FINDING: This app accesses Gemini exclusively via OpenAI-compatible proxies (LiteLLM at `<gateway>/v1/`, OpenRouter, Google's own OpenAI-compat endpoint), NOT the native Gemini Interactions API. Evidence: provider-errors.jsonl shows `litellm.BadRequestError: OpenAIException` for Gemini 400s; the SPEC file `.coding/knowledge/spec/2026-12-20-gemini-thought-signature-wire-contract-round-tri.md` was verified via `ai.google.dev/gemini-api/docs/thinking (+ /docs/openai)`. No `previous_interaction_id` exists anywhere in config or code.

CONSEQUENCE: The stateless `thought_signature` path IS Gemini's reasoning-continuity mechanism. The `thought_signature` is captured verbatim in `Message::raw` (via `LlmEvent::RawAssistantDelta` from the OpenAI-compatible stream, Step 2) and echoed unchanged by the request builder (Step 5). The hard 400 "Function call is missing a thought_signature in functionCall parts" (seen in production logs) is exactly the serialization bug Rule 6 requires failing loudly on.

POLICY: `ProviderPolicy::for_kind_and_model` sets `stateful_key=None` for Gemini (was `Some(PreviousInteractionId)` — the native Interactions API capability, but unused since we're proxy-only). `reasoning_required=true` (hard 400 if missing). `signatures_portable_across_models=true` (within Google models). The native Interactions API (`previous_interaction_id`) would require a separate client path, out of scope until a direct Gemini endpoint is configured.

Plan step 9 of 'Reasoning-state continuity: raw-payload source of truth + ProviderPolicy'.
