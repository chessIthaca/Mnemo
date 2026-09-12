+++
title = "R9 max_completion_tokens cap + R10 repetition detector + model-switch summarization"
created = "2026-08-25"
status = "superseded"
+++

SPEC: Three context-optimization fixes shipped on feat/max-tokens-cap-repetition-model-switch (commit b6c70af, unmerged):

1. **R9 — Dynamic max_completion_tokens cap** (src/provider/openai.rs:1203, src/provider/anthropic.rs:240): `max_completion_tokens`/`max_tokens` is now capped to `min(max_output_tokens, max_context - prompt_est - 1024).max(1024)` using a conservative byte-based `estimate_prompt_tokens()` (src/provider/mod.rs:268). Default `max_output_tokens` lowered 64K→32K. Eliminates the 14 request failures (12× HTTP 502 deepseek, 2× HTTP 400 qwen-3.6) caused by uncapped output exceeding the context window.

2. **R10 — Repetition detector** (src/provider/openai.rs detect_repetition + stream loop): when the accumulated response text ends with the same 200-byte window repeated 3× consecutively, the stream is aborted with an LlmEvent::Error to prevent token waste from stuck loops. Multi-byte UTF-8 safe (boundary checks before slicing). Constants: REPETITION_WINDOW=200, REPETITION_THRESHOLD=3.

3. **Model-switch summarization** (src/agent/loop_impl.rs PendingSwap + set_explicit_provider, src/agent/turn.rs run_turn): when set_explicit_provider is called with a smaller-context model, the swap is deferred (stored in pending_swap) so run_turn can summarize using the OLD provider first if the conversation exceeds 80% of the new window. IPC emits a user-facing note instead of ModelChanged when deferred.
