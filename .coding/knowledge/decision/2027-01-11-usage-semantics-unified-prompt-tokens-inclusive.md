+++
title = "usage semantics unified — prompt_tokens inclusive, cached_tokens hit-only"
created = "2027-01-11"
+++

Usage semantics are UNIFIED across provider paths (plan 2972d681 / commit 6cd0ef9, backlog 648051bf):

1. **prompt_tokens = ALL billed prompt tokens on every path.** OpenAI chat/Responses `prompt_tokens`/`input_tokens` already include cached tokens; the Anthropic path now emits `prompt_tokens = input_tokens + cache_read_input_tokens + cache_creation_input_tokens` (Anthropic's `input_tokens` EXCLUDES the cached prefix). Consumers (request_stats, AgentEvent::Usage, ContextUsage re-anchor, StatsView cost, the frontend reducer) see one meaning.
2. **cached_tokens = tokens served FROM cache (the hit) on every path** — OpenAI `prompt_tokens_details`/`input_tokens_details` `cached_tokens`; Anthropic `cache_read_input_tokens`. Deliberately NOT folded with cache-creation: creation tokens are writes, not hits — folding them in would corrupt `LlmUsage::cache_hit_ratio` and the R19 cache-hit analysis pipeline.
3. **No dedicated cache-creation field** (deliberate): creation tokens are folded into prompt_tokens. Anthropic bills them at 1.25× input rate, but the pricing model has no creation rate — the same simplification as the OpenAI path. If cost analysis ever needs the distinction, add a field to LlmEvent::Usage + LlmUsage + the wire format (the parse already tracks it in `StreamState.cache_creation_tokens`, src/provider/anthropic.rs).
4. The Responses API hit lives at `usage.input_tokens_details.cached_tokens` (src/provider/openai/sse.rs) — the counterpart of the chat path's `prompt_tokens_details.cached_tokens`.

Regression tests: `usage_includes_anthropic_cache_tokens` (src/provider/anthropic.rs) + `parse_responses_sse_chunk_parses_cached_tokens` (src/provider/openai/tests.rs). Related: DECISION "CDP port is per-instance" (same session's sibling item); reviews .coding/reviews/2026-09-12-anthropic-cache-usage-fields-review.md + -round2.md.
