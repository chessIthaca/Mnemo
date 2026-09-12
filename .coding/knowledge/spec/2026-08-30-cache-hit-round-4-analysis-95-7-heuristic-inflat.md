+++
title = "Cache-hit round 4 analysis — 95.7% heuristic-inflated, proxy cache limit at ~340K, tool-result bloat is the lever"
created = "2026-08-30"
+++

Cache-hit + context optimization analysis round 4 (2026-12-04). Full report: .coding/analysis/cache-hit-4-report.md. Diffs: .coding/analysis/cache-hit-4-diffs.txt.

DATA SOURCES: .coding/logs/traces.jsonl (2 opt-in lines, ids 295-296, glm-5.2 via the LiteLLM proxy, 2026-08-30) + .coding/memory.db request_stats (12,713 rows, always-on) + provider-errors.jsonl (216 lines).

HEADLINE: R1/R5 structure confirmed working — stable head byte-identical across all requests, CONTEXT_FOOTER is the byte-stable last message (50 chars), tools array stable within a state. Within a tool loop, hits are 98-99.9%. Overall (heuristic-inflated): 95.7%, 740 resets / 12,260 (6.0%).

KEY FINDINGS:
1. HEURISTIC MASKS REAL MISSES — when provider reports cached_tokens=0, memory.db records min(prev_prompt, curr_prompt) (turn.rs:1155-1174). Trace id=295: provider cached=0, but memory.db shows cached=340,820 (99.8%). The 95.7% is inflated; real provider-reported hits are lower.
2. LITELLM PROXY CACHE SIZE LIMIT — cache held 99% from 330K→341K tokens, then dropped to 4.4% at 370K. The proxy caches only the system head (~16K) beyond ~340-350K tokens. NOT a prompt-structure issue — R5 footer is byte-stable, head/tools/tail all identical.
3. TOOL-RESULT BLOAT — 245 tool messages in a 486-message conversation. read_files returns 2000-line slices (~16K tokens each); search returns 48K chars (~12K). Top 10 tool results = ~136K tokens (half the context). Total: ~270K tokens estimated.
4. SUMMARIZATION NEVER TRIGGERED — fill_rate=0.5, max_context=1M → summarize_at=500K. The 370K context never reached threshold. Even R11 (0.4→400K) wouldn't have triggered.
5. max_completion_tokens=131072 — R9 cap IS in source (openai.rs:1315-1322, 3 tests present), but with max_context=1M the cap never kicks in (min(131072, 1000000-341619-1024)=131072). Correct but wasteful.

RECOMMENDATIONS (ranked):
- R12: Fix heuristic fallback — report 0 when provider says 0, don't estimate min(prev,curr). turn.rs:1155-1174. HIGH impact (unblocks measurement), LOW effort.
- R13: Lower summarization threshold — fill_rate 0.5→0.3 or absolute cap ~200K. context.rs:69-70. HIGH (prevents exceeding proxy cache limit), TRIVIAL.
- R14: Truncate tool results after consumption — replace full content with summary after model acted on it. ~50 LOC. HIGH (could cut context 40-60%).
- R15: Reduce read_files default max_lines 2000→500. Trivial. MEDIUM.
- R16: Investigate litellm proxy cache limit (~340K). Research. MEDIUM.
- R17: Cap max_output_tokens for large-context endpoints (sane default 16K regardless of context window). LOW effort.

PROVIDER ERRORS (216): 404 (66, model not found), 0 (49, transport), 200 (49, stream aborted), 502 (32, bad gateway), 401 (18, auth), 400 (2, max_completion_tokens pre-R9 fixed). The 49× stream-aborts + 49× transport failures are the dominant reliability issue (cause retries that waste tokens).

BASELINE TREND: 80.7% (pre-R1) → 87.2% (post-R1) → 98.5% within-state (post-R5) → 95.7% overall heuristic-inflated (now). The remaining gap is NOT prompt structure — it's context size exceeding the proxy's cache limit + tool-result bloat.
