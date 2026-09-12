+++
title = "Gemini 3 thought_signature round-trip for OpenAI-compatible endpoints"
created = "2026-12-20"
+++

SPEC: Gemini 3 thought_signature round-trip for OpenAI-compatible endpoints (plan 5c94b497, merged on wt/agenticcoding commits 35b4d98 + 9cf3b74).

Google's stateless-mode requires `thought_signature` attestation bytes to be resent verbatim on subsequent turns. The app captures them generically from any OpenAI-compatible stream into an opaque `provider_meta: Option<serde_json::Map<String, serde_json::Value>>` bag on both `Message` and `ToolCall` (src/provider/mod.rs), persists through stored history serialization unchanged, and echoes back verbatim via `apply_provider_meta` in the request builder.

Pipeline: parse_sse_chunk (openai.rs) → capture_provider_meta extracts passthrough keys (PROVIDER_PASSTHROUGH_KEYS allowlist, currently just `thought_signature`) → emits LlmEvent::ProviderMeta { tool_call_index: Option<u32>, meta } → DeltaAccumulator (stream.rs) merges per-scope (call_metas HashMap + message_meta Option), concatenating repeated string keys → finalize() attaches per-call bags onto ToolCall.provider_meta, take_message_provider_meta() yields message bag → turn.rs 5 packaging sites attach msg_meta to assistant Message pushes → build_request_json (openai.rs) echoes via apply_provider_meta on both assistant messages and tool_calls.

Three invariants: (1) verbatim fidelity — bytes copied untouched, never parsed/normalized; (2) provenance-gated echo — only keys actually received are resent, non-signature endpoints see byte-identical requests (fabrication impossible); (3) conversation-scoped survival — signatures persist across model switches when context fits; summarizing switches (compaction) replace signed turns with unsigned summary, dropping signatures by necessity (fails safe, never fabricates).

Anthropic provider (anthropic.rs) intentionally untouched — native Messages API, not OpenAI-compatible path.

Tests: 9 new (4 accumulator, 2 parse, 3 request-builder including no-fabrication regression). Full suite 1800 tests green, zero warnings under #![deny(warnings)].

Spec file: .coding/knowledge/spec/2026-12-20-gemini-thought-signature-wire-contract-round-tri.md
Review: .coding/reviews/2026-12-20-thought-signature-round-trip-verify.md (PASS)
