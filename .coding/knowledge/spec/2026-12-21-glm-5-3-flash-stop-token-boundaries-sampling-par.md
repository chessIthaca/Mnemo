+++
title = "GLM-5.3-Flash stop-token boundaries, sampling parameters, and harness stability"
created = "2026-12-21"
+++

# GLM-5.3-Flash Stability: Stop Tokens, Sampling Parameters, and Harness Architecture

## 1. Problem & Symptoms
When using GLM-5.3-Flash (and similar reasoning/agentic models like Nous Hermes 3) in agent coding loops:
- After concluding reasoning (`</think>`) or completing an assistant turn, the model frequently emits garbage tokens: repeated closing backticks, hallucinated user/assistant dialogue, recursive JSON/XML tags, or thousands of empty lines.
- The generation often runs until the remaining `max_completion_tokens` / `max_tokens` budget is fully exhausted.

## 2. Root Cause
1. **Tokenizer Boundary Tokens:** GLM-5.3-Flash introduced dedicated special tokens and boundaries for its `<think>` reasoning block and agent conversation loops.
2. **Missing Stop Parameters in API Requests:** Most agent harnesses send only standard fields (`model`, `messages`, `max_tokens`). Without explicit `stop` strings or `stop_token_ids`, the inference engine relies on default chat template EOS matching.
3. **Proxy Stripping / Server Template Gaps:** 
   - When routed through proxies (LiteLLM, OpenRouter, vLLM, SGLang, Ollama), the proxy often validates payloads against rigid OpenAI schemas and strips non-standard keys like `stop_token_ids`.
   - If the backend server's Jinja template does not map GLM-5.3 EOS tokens to generation stop conditions, generation never halts naturally.
4. **Sampling Penalty Pitfall:** Setting positive `frequency_penalty` or `presence_penalty` (> 0) makes this drastically worse. Coding and reasoning require repeating syntax tokens (braces, quotes, closing XML tags). Penalizing repetition forces the model to invent garbage unicode tokens instead of emitting closing tags.

## 3. Requirements & Recommendations for GLM-5.3-Flash
- **`stop` (Strings):** Must include `["<|endoftext|>", "<|user|>", "<|assistant|>", "<|observation|>", "\n\n\n\n"]`. Supported across all OpenAI-compatible servers.
- **`stop_token_ids` (Integers):** GLM-5.3 token IDs `[151329, 151330, 151336]`. Required by engines like vLLM/SGLang for hardware-level token cutoff before decoding.
- **`temperature`:** `0.0` - `0.2` for deterministic tool use and code editing (or provider baseline `0.6` for reasoning). Avoid values `> 0.7`.
- **`top_p`:** `0.7` - `0.9` (default `0.8`).
- **`frequency_penalty` & `presence_penalty`:** Must remain `0.0`.
- **`repetition_penalty` (vLLM/Ollama):** If used, strictly between `1.0` and `1.05`.

## 4. Cross-Harness Comparison
- **Claude Code:** No support for custom `stop` or `stop_token_ids`. Hardwired to Anthropic Messages API.
- **Cursor:** Indirect / no custom stop parameter knobs. Relies on server-side chat template EOS.
- **Hermes Client:** Native support. Explicitly injects `stop: ["</tool_call>", "<|im_end|>"]` and integer token IDs.
- **Pi (`@earendil-works/pi-mono`):** Native support via `samplingParams: Record<string, unknown>` and `onPayload` lifecycle hooks, passed directly to OpenAI completions endpoints.

## 5. Mnemo Implementation Strategy
1. **Config Layer (`src/config/endpoints.rs`):** Add optional `temperature`, `top_p`, `stop`, `stop_token_ids`, and `extra_body` to `Endpoint` and `ModelSpec`.
2. **Client Factory (`src/provider/client_factory.rs`):** Pass these configurations into `OpenAiClientConfig`.
3. **Payload Builder (`src/provider/openai.rs`):** Inject configured sampling parameters, `stop`, `stop_token_ids`, and arbitrary `extra_body` keys into `build_request_json`.
4. **Client-side Stream Guard:** In the SSE stream reader (`openai.rs`), terminate turns early if raw boundary strings (`<|user|>`, `<|assistant|>`) appear in stream deltas.
