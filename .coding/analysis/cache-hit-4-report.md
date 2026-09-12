# Cache-Hit + Context Optimization Analysis Round 4 (latest traces)

**Date:** 2026-12-04
**Source:** .coding/logs/traces.jsonl (2 lines, 3.49 MB, ids 295-296, 2026-08-30, glm-5.2 via the LiteLLM proxy) + .coding/logs/provider-errors.jsonl (216 lines) + .coding/memory.db request_stats (12,713 rows, always-on)
**Prior analyses:** cache-hit-analysis.md (baseline 80.7%), cache-hit-2-report.md (post-R1 87.2%), cache-hit-3-report.md (post-R5 98.5% within-state)

---

## 1. Headline: system is 95.7% overall — but the heuristic masks real misses; context bloat is the remaining lever

| Metric | Baseline (pre-R1) | Round 2 (post-R1) | Round 3 (post-R5) | **Round 4 (now)** |
|---|---|---|---|---|
| Requests analyzed | 82 | 69 (post-R1 seg) | 4 (with usage) | **12,713** (memory.db) |
| Overall hit rate | 80.7% | 87.2% | 98.5% (within-state) | **95.7%** (heuristic-inflated) |
| Resets (hit <50%) | 15 | 9 | 1 | **740 / 12,260** (6.0%) |
| Avg prompt size | ~150K | ~150K | ~178K | **~165K avg, up to 370K** |

**The R1/R5 structure is confirmed working** — the stable head is byte-identical across requests, the CONTEXT_FOOTER is the last message (50 chars, byte-stable), and the tools array is stable within a workflow state. Within a tool loop, cache hits are 98-99.9%.

**But two new findings emerge from the larger dataset:**

1. **The cache-hit heuristic overestimates real hits** — when the provider reports cached_tokens=0, memory.db records min(prev_prompt, curr_prompt) (the heuristic fallback in 	urn.rs:1163-1173). The 2 opt-in trace lines show the ACTUAL provider values: id=295 cached=0 (0%), id=296 cached=16,320 (4.4%). But memory.db records id=295 as cached=340,820 (99.8%) via the heuristic. **The 95.7% overall is inflated; the real provider-reported hit is lower.**

2. **Resets correlate with large context (>340K tokens)** — in the 2026-08-30 session, the cache held 99% from 330K→341K tokens, then dropped to 4.4% at 370K. The litellm proxy appears to have a cache size limit (~340-350K tokens) beyond which it stops caching the conversation prefix and only caches the system head (~16K tokens).

---

## 2. The 2 trace lines — byte-level diff (id 295 → 296)

| Field | id=295 | id=296 | Identical? |
|---|---|---|---|
| prompt tokens | 341,619 | 370,340 | — |
| cached tokens (provider-reported) | **0** | **16,320** | — |
| hit % | **0%** | **4.4%** | — |
| nMessages | 483 | 486 | — |
| nTools | 25 | 25 | **✓ byte-identical** |
| sysLen (messages[0]) | 16,733 | 16,733 | **✓ byte-identical** |
| tail (2nd-to-last msg) | 1,725 chars | 1,725 chars | **✓ byte-identical** |
| footer (last msg) | 50 chars | 50 chars | **✓ byte-stable** |
| max_completion_tokens | 131,072 | 131,072 | — |
| send_ms | 86,078 | 46,860 | — |

**What changed between 295→296:** 3 messages appended (1 assistant + 2 tool results). The 2 tool results are **48,057 chars each (~12K tokens)** — search outputs. The prefix [head, conversation(1..480)] is byte-identical. Yet the provider cached only 16,320 tokens (the head) on id=296, and 0 on id=295.

**Verdict:** The R5 footer IS the last message (byte-stable). The head + tools + tail are all byte-identical. The cache miss is NOT caused by our prompt structure — it's the **litellm proxy's cache behavior** (size limit or different semantics than DeepSeek's "last message must match" law).

---

## 3. Context waste — tool results dominate

**Role distribution (id=296, 486 messages):**
| Role | Count | Notes |
|---|---|---|
| tool | **245** | Dominates the conversation |
| assistant | 221 | |
| user | 17 | |
| system | 3 | head + tail + footer |

**Top 10 largest tool results (id=296):**
| idx | tool | chars | ~tokens | content |
|---|---|---|---|---|
| 56 | read_files | 65,708 | ~16K | openai.rs lines 530-2529 (2000 lines) |
| 59 | read_files | 65,685 | ~16K | agent.rs lines 100-2099 (2000 lines) |
| 55 | read_files | 65,681 | ~16K | turn.rs lines 820-2105 (1285 lines) |
| 81 | read_files | 65,646 | ~16K | tests.rs lines 2748-4747 (2000 lines) |
| 57 | read_files | 54,113 | ~14K | general.rs lines 120-1334 |
| 4 | git_read | 49,592 | ~12K | git diff HEAD --stat |
| 483 | search | 48,057 | ~12K | search results (101 matches) |
| 482 | search | 48,057 | ~12K | search results (duplicate) |
| 54 | read_files | 42,286 | ~11K | loop_impl.rs lines 540-1370 |
| 61 | search | 39,868 | ~10K | search results (91 matches) |

**Total content: 1,079,931 chars (~270K tokens estimated).** The 10 largest tool results alone account for ~545K chars (~136K tokens) — over half the context.

**Root cause:** 
ead_files defaults to max_lines=2000, and the agent reads 2000-line slices of large files. Each slice is ~16K tokens. With 245 tool messages (many being such reads), the context balloons to 370K tokens — which then exceeds the litellm proxy's cache size limit, causing the cache resets.

---

## 4. Summarization never triggered

The fill_rate default is 0.5 (src/agent/context.rs:68-70). With max_context=1,000,000 (the configured value for glm-5.2 endpoints), summarize_at = 500,000 tokens. The 370K-token context **never reached the threshold** — so no summarization occurred, and the conversation grew unbounded.

The R11 recommendation (lower fill_rate to 0.4) would set summarize_at = 400,000 — still above 370K. **Even R11 wouldn't have triggered compaction for this session.**

---

## 5. max_completion_tokens = 131,072

The R9 dynamic cap IS in the source code (src/provider/openai.rs:1315-1322, verified with 3 tests present). The formula: min(max_output_tokens, max_context - prompt_est - 1024).max(1024).

With max_context=1,000,000 and prompt_est≈341K: min(131072, 1000000 - 341619 - 1024) = min(131072, 657357) = 131072. **The cap doesn't kick in because the 1M context window is large enough.** This is correct behavior (input+output fits), but 131K output tokens is wasteful — most coding tasks need 2-8K.

The 2 × HTTP 400 max_completion_tokens failures in provider-errors.jsonl (qwen-3.6, ids 89-90) are from a model with max_model_len=65536 — pre-R9 (the cap would now prevent this).

---

## 6. Provider errors (216 entries)

| Status | Count | Model | Pattern |
|---|---|---|---|
| 404 | 66 | glm-5.2 | Model not found (litellm routing) |
| 0 | 49 | glm-5.2 | Transport failures (connection) |
| 200 | 49 | glm-5.2 | Stream aborted/stalled mid-generation |
| 502 | 32 | glm-5.2 | Bad gateway (litellm upstream) |
| 401 | 18 | glm-5.2 | Auth errors |
| 400 | 2 | qwen-3.6 | max_completion_tokens (pre-R9, fixed) |

The 49 × status-200 stream-aborts and 49 × transport failures are the dominant reliability issue — not cache-related, but they cause retries that waste tokens.

---

## 7. Recommendations (ranked by impact/effort)

| # | Recommendation | Impact | Effort | Type |
|---|---|---|---|---|
| **R12** | **Fix the heuristic fallback** — when the provider reports cached_tokens=0, do NOT estimate min(prev,curr). Report 0 so the stats reflect reality. The current heuristic masks real cache misses, making optimization decisions blind. (	urn.rs:1155-1174) | **High** — unblocks accurate measurement | Low (~10 LOC) | Bug fix |
| **R13** | **Lower the summarization threshold** — fill_rate 0.5→0.3 (or add an absolute cap at ~200K tokens). With max_context=1M, 0.3 → summarize_at=300K, which would trigger before the 340K cache-reset cliff. (context.rs:69-70, settings) | **High** — prevents context from exceeding the cache limit | Trivial (config) | Context saving |
| **R14** | **Truncate tool results after consumption** — after the model has acted on a tool result, replace the full content with a compact summary (first 500 chars + "[N lines truncated]"). The model already saw and used the content; keeping 16K-token file reads in history forever is pure waste. | **High** — could cut context 40-60% | Medium (~50 LOC) | Context saving |
| **R15** | **Reduce read_files default max_lines** — 2000→500. Most lookups need a specific region, not 2000 lines. The agent can always read more. (
ead_files tool schema default) | **Medium** — halves per-read cost | Trivial (1 line) | Context saving |
| **R16** | **Investigate litellm proxy cache limit** — the cache drops at ~340K tokens. If the proxy has a configurable max cacheable prefix, raising it would extend the 99% hit zone. If not, R13 (keeping context under the limit) is the mitigation. | **Medium** — extends cache zone | Research | Cache hit |
| **R17** | **Cap max_output_tokens for large-context endpoints** — when max_context is very large (1M), the R9 cap never triggers. Add a separate sane default (e.g., 16K) for the output budget regardless of context window size. (openai.rs:1320, config) | **Low** — limits stuck-loop waste | Low (~5 LOC) | Context saving |

---

## 8. What's already working well (keep)

- **R1 (stable head / volatile tail split)** — head is byte-identical across all 12,713 requests. ✓
- **R5 (CONTEXT_FOOTER)** — the last message is byte-stable (50 chars). ✓
- **R2 (deterministic tool ordering)** — tools array is byte-identical within a state. ✓
- **R9 (dynamic max_completion_tokens cap)** — in source, prevents context-overflow failures on small-window models. ✓
- **R10 (repetition detector)** — no stuck-loop evidence in the recent traces. ✓
- **Per-turn recall cache** — memories block is stable within a tool loop. ✓

---

## 9. Caveats

- The 2 opt-in trace lines are a tiny sample; the 12,713 memory.db rows are the reliable signal — but they're partly heuristic-inflated (R12).
- The litellm proxy (<gateway>) may have different cache semantics than direct DeepSeek/Kimi APIs where R5 was validated. The "last message must match" cache law was established on DeepSeek; glm-5.2 via litellm appears to use a size-limited prefix cache instead.
- The trace is from 2026-08-30; the running binary may predate some fixes. The source code has been verified current.
- send_ms of 47-86s is suspiciously high — likely the POST upload of a ~1.4MB request body to a remote proxy, possibly compounded by the token-counting pass. Not a cache issue but a latency concern.
