# Cache-Hit + Context Optimization Analysis Round 3

**Date:** 2026-08-25
**Source:** `.coding/logs/traces.jsonl` (7 lines, ids 190-196, model glm-5.2 via the LiteLLM proxy) + `.coding/logs/provider-errors.jsonl` (180 lines, multiple models)
**Prior analyses:** `.coding/analysis/cache-hit-analysis.md` (baseline, 80.7%), `.coding/analysis/cache-hit-2-report.md` (post-R1-R3, 87.2%)

---

## 1. Headline: R1/R5 structure confirmed working; one critical bug found

The R1 (stable head / volatile tail split) and R5 (byte-stable context footer as final message) fixes are **confirmed in production** -- the context footer is present as the last message in every traced request. Within a single workflow state, cache-hit is **98.5%** -- excellent.

But a **critical bug** in `max_completion_tokens` caused **14 request failures** (12 x HTTP 502, 2 x HTTP 400), and a workflow state transition still resets the cache (R6, accepted as unavoidable).

### Trace summary (4 requests with usage)

| id | hit% | prompt | cached | nMsgs | nTools | headLen | tailLen |
|----|------|--------|--------|-------|--------|---------|---------|
| 190 | 98.5% | 176,622 | 173,888 | 256 | 25 | 16,057 | 1,823 |
| 192 | 98.5% | 178,202 | 175,488 | 260 | 25 | 16,057 | 1,823 |
| 193 | **0.8%** | 180,391 | 1,472 | 262 | **29** | **16,227** | 2,157 |
| 194 | 0.8% | 180,430 | 1,408 | 264 | 29 | 16,227 | 2,141 |
| 195 | 93.3% | - | - | 266 | 29 | 16,227 | 2,141 |
| 196 | 92.2% | - | - | 268 | 29 | 16,227 | 2,141 |

---

## 2. CRITICAL BUG: max_completion_tokens not dynamically capped

### Root cause

`src/provider/openai.rs:1203`:
```rust
let max_completion = self.caps.max_output_tokens.max(4096);
```

This sends the configured `max_output_tokens` (here: 131,072 = 128K) as `max_completion_tokens` in every request, **without capping it to the model context window or the remaining token budget**.

### Failure evidence (14 failures in provider-errors.jsonl)

| Pattern | Count | Model | Status | Error |
|---------|-------|-------|--------|-------|
| output + input > max_context | 12 | deepseek-v4-flash-gcp | 502 | "maximum context length is 262144 tokens. However, you requested 131072 output tokens and your prompt contains at least 131073 input tokens" |
| output > max_model_len | 2 | qwen-3.6 | 400 | "max_completion_tokens=131072 cannot be greater than max_model_len=max_total_tokens=65536" |

### Fix proposal (R9)

Cap `max_completion_tokens` dynamically. The token count is already computed by `ContextManager::count_tokens` in the turn loop (`token_accounting.update(messages)` at turn.rs:359). Thread it as a parameter to `build_request_json` and cap to `min(max_output_tokens, max_context - prompt_tokens - 1024)`. This fully eliminates both failure patterns.

---

## 3. Cache-hit analysis

### Within a workflow state: 98.5% -- excellent

Ids 190 and 192 (both Planning state, 25 tools, headLen=16057) hit 98.5%. The R1/R5 structure works: the stable head is byte-identical, the context footer is the last message, and the conversation history grows append-only.

### State transition reset: 0.8% -- the R6 cache-breaker (accepted)

Between id=192 and id=193, a workflow state transition occurred (Planning -> Executing):
- **Tools array changed:** 25 -> 29 (write tools added). This breaks the cache prefix because the tools array is part of the request body before the messages.
- **Stable head changed slightly:** 16057 -> 16227 (+170 chars). The PROJECT MEMORY primer was re-fetched.

This is the R6 finding from the round-2 analysis: workflow state transitions swap the tool set, which is a **security gate** (Planning must not expose write tools). Confirmed: still the right call to accept these resets.

### Post-transition warmup: 0.8% -> 93.3% -> 92.2%

After the state transition, the cache warms up over 2-3 requests. The 93.3% and 92.2% hits show the cache is recovering.

---

## 4. Context waste + compaction opportunities

### 4a. Stuck-loop repetitive output

The first trace line (id 181, from the prior session) shows the assistant got stuck in a loop, producing thousands of tokens repeating "Let me commit the uncommitted work..." dozens of times. The `max_completion_tokens=131072` cap allowed the loop to run for a very long time.

**Recommendation (R10):** Consider a repetition detector in the stream parser that aborts generation when the same N-char window repeats more than K times.

### 4b. max_completion_tokens = 131072 is too high as a default

Even without the capping bug, 128K output tokens is unreasonable. Most coding tasks need 2K-8K output tokens.

**Recommendation (R9b):** Lower the default `max_output_tokens` in `Capabilities::openai()` from 64,000 to 32,000 (or 16,000).

### 4c. Volatile tail compaction

The volatile tail is ~2,100 chars (~600 tokens) -- already compact. It is at the end (cache-immune per R1). **No further compaction needed.**

### 4d. Conversation summarization

The summarization system is well-designed (structured 8-section handoff format, running-summary update, fill-rate threshold). **No further compaction needed**, but the fill-rate threshold could be lowered (0.5 -> 0.4) to trigger summarization earlier.

### 4e. Provider reliability (not a cache issue, but wastes retries)

180 error entries in provider-errors.jsonl, including 401 auth errors, 404 model not found, 502/400 (max_completion_tokens bug), 0 transport failures, 200 stream aborted/stalled. The R9 fix would eliminate 14 of these failures.

---

## 5. Recommendations (ranked by impact/effort)

| # | Recommendation | Impact | Effort | Type |
|---|---------------|--------|--------|------|
| **R9** | **Dynamic max_completion_tokens cap** -- thread prompt token count to build_request_json, cap to min(max_output_tokens, max_context - prompt_tokens - 1024) | **High** -- eliminates 14 failures/session | Medium (~30 LOC) | Bug fix |
| **R9b** | **Lower default max_output_tokens** -- Capabilities::openai() 64K -> 32K | Medium -- limits stuck-loop blast radius | Trivial (1 line) | Config |
| **R10** | **Repetition detector in stream parser** -- abort generation when same N-char window repeats K times | Medium -- caps stuck-loop waste | Medium (~50 LOC) | Context saving |
| **R11** | **Lower summarize_at_fill_rate** -- 0.5 -> 0.4 | Low-medium -- keeps context leaner | Trivial (config) | Compaction |
| - | **Accept R6 state-transition resets** -- tools array change is a security gate | n/a | n/a | Accepted |
| - | **Collect more traces** -- 10+ requests within a single state to confirm post-warmup hit rate | n/a | n/a | Measurement |

---

## 6. Code references

| Location | Role |
|---|---|
| `src/provider/openai.rs:1203` | `let max_completion = self.caps.max_output_tokens.max(4096)` -- the uncapped max_completion_tokens (R9 fix site) |
| `src/provider/mod.rs:60` | `max_output_tokens: 64_000` -- Capabilities::openai() default (R9b fix site) |
| `src/agent/turn.rs:359` | `token_accounting.update(messages)` -- prompt token count available here (thread to build_request_json for R9) |
| `src/agent/prompt.rs:277-299` | `build_stable_head` -- byte-stable head |
| `src/agent/prompt.rs:319-339` | `build_volatile_tail` -- RECALLED MEMORIES + WORKFLOW STATE + CAPABILITIES |
| `src/agent/prompt.rs:416` | `format_primer` -- PROJECT MEMORY, cached per session |
| `src/agent/context.rs:69-74` | `ContextManager::new` -- fill_rate -> summarize_at threshold (R11) |

---

## 7. Caveats

- Only 7 trace lines (4 with usage) -- small sample. More traces needed to confirm post-warmup hit rates.
- The provider (glm-5.2 via litellm) may have different cache semantics than DeepSeek.
- The 14 max_completion_tokens failures are from 2 specific models (deepseek-v4-flash-gcp and qwen-3.6), not from glm-5.2.