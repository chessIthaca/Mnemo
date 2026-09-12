+++
title = "429 → auto cross-provider model fallback (shipped b11ed94)"
created = "2026-08-30"
status = "superseded"
+++

SPEC: 429 → automatic cross-provider model fallback — SHIPPED on wt/agenticcoding at b11ed94 (2026-12-04, plan 6d221559, awaiting merge_to_main).

Behavior: on a 429 (rate limit / out of quota), the agent automatically switches to the same model id on a DIFFERENT endpoint and retries once, instead of burning the 3× same-provider retry stack or failing.

1. `Error::is_rate_limited()` (src/error.rs) — classifies 429s ("http 429", "too many requests", "rate limit", "limit exhausted") distinctly from `is_non_retryable()`. A 429 is rate-limited but NOT non-retryable — it gets the fallback path, not the immediate-fail path.
2. `complete_with_retry` (src/agent/dispatch.rs) returns immediately on `is_rate_limited()` (alongside `is_non_retryable()`) — no 3× same-provider backoff on a 429 ("stop after a single 429").
3. `ModelResolver::find_alternate_endpoint(model_id, exclude_endpoint)` (src/model_resolver.rs) — finds the first OTHER endpoint (config order) serving the same model id, excluding the one that just 429'd. Returns None when no alternate exists.
4. `AgentLoop::try_429_fallback()` (src/agent/loop_impl.rs) — reads `resolved_model()` (falling back to `provider().model()` when no override resolved — the default-provider path, the most common config) + `effective_provider_name()`, calls `find_alternate_endpoint`, builds via `build_turn_provider`, pins via `set_explicit_provider` (beats `forced_model` in `resolve_turn_provider` priority → works for the reviewer subagent too).
5. `run_turn_attempt` (src/runtime/agent.rs) — on `is_rate_limited()` && `!tried_fallback` → `try_429_fallback`; if Some → emit switching note (Error{retrying:true}) + continue (retry run_turn with fallback); if None or already tried (`tried_fallback` guard) → `attempt = MAX` (final failure, no cascade through endpoints). Final-failure qualifier: "rate limited (429), no alternate provider found" + actionable hint ("Switch models via the status-bar picker, or configure another endpoint").

"Ask the user" mechanism: main agent → terminal Error{retrying:false} with actionable text. Reviewer → same terminal failure (return None); the EXISTING failed-reviewer protocol (parent latches reviewer_failure_pending → parent asks_user "retry on another model / abandon review") handles the ask. No new ask_user-from-turn-layer.

Scope limits: at most ONE automatic provider switch per turn (tried_fallback flag). Same model id only (no fuzzy "comparable model" matching). Key files: src/error.rs, src/agent/dispatch.rs, src/model_resolver.rs, src/agent/loop_impl.rs, src/runtime/agent.rs.
