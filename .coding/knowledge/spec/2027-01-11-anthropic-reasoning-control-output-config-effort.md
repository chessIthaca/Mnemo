+++
title = "Anthropic reasoning control — output_config.effort (NOT thinking/budget_tokens; never send a thinking field)"
created = "2027-01-11"
+++

Durable API facts (verified 2027-01-11 against platform.claude.com docs) deciding how Mnemo controls Anthropic reasoning depth:

1) `thinking: {type:"enabled", budget_tokens:N}` (manual extended thinking) is DEPRECATED on Claude 4.6 (still succeeds) and REJECTED with a 400 by Claude 4.7+ ("thinking.type.enabled is not supported"). Never hardcode a budget table.
2) The modern control is the top-level `output_config.effort` (low|medium|high|max|xhigh), NO beta header required; docs: "Where adaptive thinking is available, effort is the recommended way to control thinking depth", and effort "works whether or not thinking is enabled". Wire: {"output_config": {"effort": "medium"}}.
3) Mnemo must NEVER send a `thinking` field: {"type":"disabled"} is a 400 on models whose thinking is always on (Opus 5.5 at every level; Opus 5 at xhigh/max), {"type":"enabled"} is a 400 on 4.7+. OMISSION = the model's own default (thinking already on by default on Opus 5.x / Sonnet 5), so `off` = "send no reasoning control" = pre-effort behavior.
4) Model discovery (/v1/models): entries carry `max_input_tokens` (context window), `max_tokens` (max output), `capabilities.image_input.supported`, and `capabilities.effort.{low,medium,high,max,xhigh}.supported`. Existing Mnemo parser keys (context_length/max_model_len/max_completion_tokens) all miss → caps were always None for anthropic kind.
5) The display ladder is off|minimal|low|medium|high|max → mapped minimal→low (no lower API level); the per-model `reasoning_efforts` allow-list is what gates levels, and endpoint.effective_reasoning_effort_for already resolves override→allow-list-clamp→"max" default, with `off` → None for anthropic kind.

Implemented in plan b3188918 (src/provider/anthropic.rs build_request_json + AnthropicClientConfig.reasoning_effort; src/provider/client_factory.rs threading; src/provider/models.rs caps/effort parsing).
