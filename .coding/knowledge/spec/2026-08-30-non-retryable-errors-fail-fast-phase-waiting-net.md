+++
title = "non-retryable errors fail-fast; Phase::Waiting = network-bound (shipped 0212676)"
created = "2026-08-30"
status = "superseded"
+++

SPEC: Non-retryable error fail-fast + Phase::Waiting honesty — MERGED into wt/agenticcoding at 0212676 (2026-12-04, plan 09579efe, awaiting merge_to_main).

Shipped behavior:
1. `Phase::Waiting` now emits BEFORE the provider request (turn.rs, before `complete_with_retry`), not after `stream_result?`. So connect timeouts + retry backoff sleeps show as "waiting" not "sending". Sending = local prep only (token counting, recall, prompt build).
2. `Error::is_non_retryable()` (src/error.rs) classifies context-overflow ("maximum context length", "context length is", "max_completion_tokens", "max_model_len", "prompt is too long"), auth (401/403 "unauthorized", "auth_error", "virtual key expected"), and model-not-found ("notfounderror", "model ... not found") as non-retryable. These skip the 3×3 retry stack at BOTH the provider level (dispatch.rs `complete_with_retry`) and the turn level (agent.rs `run_turn_attempt` via `attempt = MAX_PROVIDER_TURN_ATTEMPTS`).
3. **429 / 500 / 502 / 503 / 504 / connect failures / stream stalls remain RETRYABLE** — they are NOT in `is_non_retryable()`. This is the gap the 429-fallback feature (backlog 931ebe06) addresses: a 429 currently burns the retry stack instead of switching providers.
4. Error records now stamp `ttft_ms`/`generation_ms` (trace.rs `set_ttft_ms`/`set_generation_ms`) so connect timeouts and mid-stream stalls are attributed in the trace, not lost.

Key files: src/error.rs (is_non_retryable), src/agent/dispatch.rs (complete_with_retry), src/runtime/agent.rs (run_turn_attempt), src/agent/turn.rs (Phase::Waiting), src/provider/{openai,anthropic,trace}.rs.
