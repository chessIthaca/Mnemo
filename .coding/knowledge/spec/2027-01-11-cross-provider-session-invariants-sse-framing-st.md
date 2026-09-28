+++
title = "cross-provider session invariants — SSE framing state, origin-gated raw echo, deterministic-4xx fail-fast"
created = "2027-01-11"
+++

Cross-provider session invariants shipped 2027-01 (plan 428a9f1e, wt/mnemo; commits 5b7f99b / ad9c76f / c19e79b; review PASS round 3):

1. SSE parsing (Anthropic, src/provider/stream-of anthropic.rs): the pending `event:` name lives on StreamState::pending_event and survives network chunk boundaries. Invariant: an `event:`/`data:` split must never drop a frame. New provider parsers must keep ALL per-stream framing state on the stream state, never in parser-fn locals.

2. Raw echo (src/provider/openai/request.rs::raw_origin_allows_echo): a stored Message.raw echoes verbatim ONLY within its own wire family — origin_provider absent (legacy) or ProviderKind-equal to the target client. Foreign raws (e.g. Anthropic content arrays to an OpenAI-compat endpoint) fall through to field construction; the same-family echo preserves unknown vendor keys byte-identical. Invariant: never send a foreign provider's native block types (thinking/tool_use/tool_result) inside another family's body — it is a deterministic 400 (z.ai code 1214).

3. Request ladder (src/agent/dispatch.rs complete_with_retry + Error::is_deterministic_4xx): HTTP 4xx other than 408/429 fails fast — one attempt, no backoff — BEFORE failure triage; 408/429/5xx/network stay retryable, and the turn-level ladder still owns endpoint switching. Invariant: an unmodified payload that a provider already refused is never re-sent by the same-provider ladder.

Mid-session provider switching (e.g. Anthropic → GLM) is now a supported state, not a session-killer. Detail records: BUG files 2027-01-11-anthropic-empty-truncated-tool-args-dropped-text.md, 2027-01-11-glm-z-ai-400-code-1214-content-0-type-type-error.md, 2027-01-11-deterministic-http-400-retried-3-with-backoff-re.md. Follow-up queued: handle_bad_json batch isolation (backlog bec236f6).
