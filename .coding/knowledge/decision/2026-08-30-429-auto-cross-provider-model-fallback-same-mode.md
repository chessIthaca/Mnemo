+++
title = "429 → auto cross-provider model fallback (same model id, different endpoint)"
created = "2026-08-30"
+++

DECISION: 429 → automatic cross-provider model fallback (backlog 931ebe06, design 2026-12-04).

User request: on a 429 (rate limit / out of tokens), instead of retrying the same provider (pointless — still out of quota) or failing, automatically find the SAME model id on a DIFFERENT endpoint and switch to it. Stop after a single 429 (no same-provider retry). If no alternate endpoint serves the model, ask the user. Particularly important for the reviewer subagent.

Design (chosen):
1. `Error::is_rate_limited()` (src/error.rs) — string match on Provider errors: "http 429", "too many requests", "rate limit", "limit exhausted" (case-insensitive). 429 is NOT in is_non_retryable() (it stays out of that set — it's handled separately).
2. `complete_with_retry` (src/agent/dispatch.rs): add `e.is_rate_limited()` to the early-return condition alongside `is_non_retryable()` → a 429 returns immediately (1 provider call, no 3× same-provider backoff). Implements "stop after a single 429."
3. `ModelResolver::find_alternate_endpoint(model_id, exclude_endpoint)` (src/model_resolver.rs) — new trait method (default None); ConfigModelResolver impl iterates endpoints, finds first (after exclude) that has_model(model_id). Returns ModelRef{endpoint, model: model_id}.
4. `AgentLoop::try_429_fallback(&self) -> Option<FallbackInfo>` (src/agent/loop_impl.rs) — reads resolved_model()+effective_provider_name(), calls resolver.find_alternate_endpoint, builds via build_turn_provider, pins via set_explicit_provider (beats forced_model in resolve_turn_provider priority → works for reviewers). Returns from/to endpoint + model for the switching note.
5. `run_turn_attempt` (src/runtime/agent.rs) Err arm: on is_rate_limited() && !tried_fallback → try_429_fallback; if Some → emit switching note (Error{retrying:true}) + continue (retry run_turn with fallback); if None or already tried → attempt=MAX (final failure). Final-failure message branches: "rate limited (429), no alternate provider found" vs "non-retryable" vs "retries exhausted".

"Ask the user" mechanism: main agent → terminal Error{retrying:false} with actionable text (switch via status-bar picker / configure another endpoint). Reviewer → same terminal failure; the EXISTING failed-reviewer protocol (parent latches reviewer_failure_pending → parent asks_user "retry on another model / abandon review") handles the ask. No new ask_user-from-turn-layer needed.

Scope limits: at most ONE automatic provider switch per turn (tried_fallback flag). Same model id only (no fuzzy "comparable model" matching — if no exact match on another endpoint, ask). Request-time 429s (HTTP 429 before streaming) are the primary target; mid-stream 429s that surface as Err are also caught.

Key files: src/error.rs, src/agent/dispatch.rs, src/model_resolver.rs, src/agent/loop_impl.rs, src/runtime/agent.rs.
