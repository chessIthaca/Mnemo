+++
title = "multi-provider prompt caching + waiting reduction — MERGED into main (a0ad7d8)"
created = "2026-12-21"
+++

MERGED into main at a0ad7d8 on 2026-12-21 (merge_to_main skill); pre-merge tip was on wt/backlog-title-chip (deleted post-merge). Prompt caching + latency optimizations now live on main:
1. Provider waiting reduction:
   - CONNECT_TIMEOUT shortened from 30s to 10s (OpenAiClient + AnthropicClient) so dead connections fail fast rather than stalling for 93s over retries.
   - Error::is_non_retryable() extended with proxy/gateway context overflow patterns (LiteLLM/Vertex 502/400 errors) so doomed requests fail fast without burning 3x 8-10s retry loops.
   - Remaining context-budgeted max_completion_tokens (OpenAI) and max_tokens (Anthropic) quantized to 2048-token buckets, eliminating per-turn parameter jitter that busts proxy-level cache keys.
2. Prompt caching:
   - Anthropic system block splitting: system messages serialized as an array of content blocks with cache_control: {"type": "ephemeral"} on the stable head (system[0]), leaving volatile tail uncached so state updates do not invalidate the head.
   - Anthropic tool caching: cache_control: {"type": "ephemeral"} on the last tool definition in tools_json.
   - Message-history breakpoints deliberately NOT set: volatile tail mutates every turn inside system; history breakpoints would miss every turn plus incur write-cost penalties.
   - 3-tier tool schema ordering: priority_class extended to Class 0 (semantic tools: graph/memory/search), Class 1 (universal base tools: read_files, git_read, ask_user, backlog_list, etc.), Class 2 (state-dependent mutation tools: file_edit, file_write, shell, git, create_plan, complete_step). Transitions Planning -> Executing -> Reviewing only append/mutate the tail, keeping the Class 0 + 1 prefix 100% byte-stable.
   - Documented in PLAN.md. All unit tests passed (1850 passed, 0 warnings). Review: PASS. Backlog #8814f81c done.

Amended 2027-01-07: Amendment (2027-01-10): the "Message-history breakpoints deliberately NOT set: volatile tail mutates every turn inside system" line is superseded by plan ee33d615 (commit d5cfd52 on wt/agenticcoding): the volatile tail no longer sits in `system` — later system messages are relocated into the final message's content as trailing text blocks (the uncached varying suffix), and breakpoint 3 (cache_control ephemeral) now lands on the final message's last real block, caching the conversation history. The old rationale was correct for the old shape (a per-turn-mutating block in `system` invalidates every messages breakpoint, since the cache-prefix order is tools -> system -> messages); the relocation removes that blocker. See SPEC e7ce0347 (.coding/knowledge/spec/2027-01-07-anthropic-conversation-history-caching-3rd-break.md) and DECISION be871f44 (Anthropic automatic caching rejected — manual breakpoint before the varying tail).
