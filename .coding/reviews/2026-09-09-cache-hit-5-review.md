## Verdict: FINDINGS (1 high, 3 low)

**Axis: cache hit rate round 5** — fresh aggregates over 28,506 `request_stats` rows (2026-08-10..09-09; 15,713 post-R12 rows carry real provider-reported cached tokens). Post-R12 real hit is 70.5% with 30.0% resets — but on cache-**reporting** models it is ~81% hit / ~17% resets: 15.9% of post-R12 traffic (all five `glm-5.3-flash*` groups, `gemini-vlm-gcp`, `glm-5.2-maas-gcp`, `deepseek-v4-flash` — 2,495 rows, ~245M prompt tokens) reports 0% cached and supplies 53% of all resets. The biggest single lever is provider-side (flash-tier prefix caching / cached-token reporting); the biggest ours-side leak is the ~340K proxy cliff (47 rows >340K at 16.3% hit — Rule-4 open-loop skip + no send-gate + tokenizer drift, all code-verified at HEAD). Ranked suggestions R18-R22 below, each tagged [OURS] vs [PROVIDER].

## 1. What was measured

- Source: `.coding/analysis/cache-hit-5-aggregates.txt` — extraction over `.coding/memory.db` `request_stats` (28,506 rows, 646 sessions, 18 models, 2026-08-10..09-09 UTC epoch clock), plus a `traces.jsonl` summary (single trace, id 255) and a `provider-errors.jsonl` histogram (431 errors, 08-14..09-09).
- Post-R12 subset (15,713 rows after the 2026-08-31 cutoff, when the R12 real-cached-tokens fix + R13 fill 0.3 + R17 output budget landed): **70.5% token hit, 30.0% resets (<50%)**; hit distribution >=90%: 39.7%, 50-90%: 30.3%, <50%: 30.0%.
- Code verified at HEAD c18b5d0 (details in section 6): R9/R12/R13/R14/R15/R17 all live; `PROXY_CACHE_CEILING_TOKENS = 340_000` and `PROXY_CACHE_PRESSURE_MARGIN_TOKENS = 32_768` (src/provider/mod.rs:753-759); fill-rate default 0.3 with `proxy_cache_ceiling_tokens` defaulting to the 340K constant (src/config/general.rs:334/343).

## 2. Headline interpretation — the reframe that should drive round 6

**The 70.5% is not our prompt shape.** Split the post-R12 rows by whether the model group ever reports `prompt_tokens_details.cached_tokens > 0`:

| tier | rows | share | hit | resets |
|---|---|---|---|---|
| Reporting (glm-5.2, glm-5.3, glm-5.3-gcp*, b200/h200 variants) | 13,218 | 84.1% | ~81% (request-weighted) | 2,216 (16.8%) |
| Non-reporting / cache-hostile (glm-5.3-flash* x5, gemini-vlm-gcp, glm-5.2-maas-gcp, deepseek-v4-flash) | 2,495 | 15.9% | 0.0% by construction | 2,495 (100%) |

- The non-reporting tier supplies **53% of all resets (2,495/4,711)** while carrying ~245M of the ~1.79B post-R12 prompt tokens (~14%). Token-weighted, roughly 46% of all uncached prompt tokens sit in that tier.
- Consequence: chasing the "30% resets" with prompt-shape work chases phantom waste. The real ours-side target is the ~17% resets on reporting models (cold starts, model switches, fallback-induced cold caches, the 340K cliff) plus the measurement blind spots (LOW-1).
- The pre-R12 95.7% was R12-heuristic inflation; 70.5% is the honest number, and ~81% is the honest number for the tiers that can cache at all.

## 3. Findings

### HIGH-1 — Requests still cross the ~340K proxy cliff post-R13: compaction can be skipped (Rule 4) or insufficient, and the request is sent anyway

- **Severity:** HIGH (BUG-class: symptom -> root cause -> where, below)
- **Location:** src/agent/turn.rs:1743-1750 (the "still over -> next iteration re-compacts" fall-through — no send-gate); src/agent/context.rs:437-485 (`summary_cut_index` + `retreat_past_open_tool_loop`, Rule 4); src/agent/context.rs:151-163 (trigger math).
- **Symptom (data):** post-R12, 47 rows >340K prompt tokens at 16.3% hit with 40 resets; the 340K-400K bucket is 17.7% hit / 83.7% resets (n=43) and >400K is 3.5% / 100% (n=4). Daily maxP reaches 408,956 (09-07) and 378,792 (09-09). R13 (fill 0.3 + 340K ceiling minus 32,768 margin -> effective trigger 300K on the 1M-window GLM endpoints) did NOT prevent over-cliff prompts.
- **Root cause (three stacked gaps, all code-verified):**
  1. **Rule 4 skip:** when the conversation ends in an open tool loop that spans back to `messages[1]` (a single-turn session in a long tool run — exactly where context grows fastest), `summary_cut_index` retreats the cut to the loop start and `summarize` returns the messages **unchanged** (`cut <= 1` -> no-op; src/agent/context.rs:262-264 and 336-338). `maybe_compact` then falls through and the request is sent at full size.
  2. **No send-gate:** when compaction runs but cannot shrink below the threshold (kept-verbatim tail dominates), the code explicitly continues — "Still over: the kept-verbatim tail dominates and the next iteration re-compacts (the counter climbs toward the abort above)" (src/agent/turn.rs:1743-1750). The 5-attempt budget aborts only on the **5th** attempt, so up to 4 over-cliff requests go out per turn, and the next turn resets the budget.
  3. **Trigger-to-cliff margin is thinner than it looks:** the trigger counts cl100k BPE tokens; the provider counts with the GLM tokenizer (~10% higher on code-heavy content). A 300K-cl100k conversation can be 330-345K provider tokens, and one iteration's tool results plus GLM reasoning echo (policy: GLM 4.7+ is `never strip`; reasoning routinely outnumbers completion 3:1-10:1 — src/provider/policy.rs) can jump the 300K trigger straight past 340K between checks.
- **Which models/sessions:** the 1M-window GLM endpoints — glm-5.3 (maxP 408,956; n=5,810 post-R12) and glm-5.2 (all-time p90P 333K) — in long single-turn tool-loop sessions (the A5 worst-session shape).
- **Suggested fix:** R20a/R20b/R20c in section 4.

### LOW-1 — request_stats blind spots: error requests, summarizer calls, and TTFT are invisible

- **Severity:** LOW
- **Location:** src/agent/turn.rs:~2440 (request_stats row written only in the `LlmEvent::Usage` arm — successful streams only); src/agent/context.rs:355-392 (`summarize_with_interrupt` consumes its own stream and ignores the `Usage` event); src/provider/trace.rs:517/568 (`usage: None` until `set_usage` fires on the stream's final chunk).
- **Symptom:** `ttft_ms=0` on 28,267/28,506 rows (99.2% — the value never flows into request_stats even on success); error->retry cycles (each re-sends the full prompt; a LiteLLM fallback switch to another deployment guarantees a cold next request) leave no row; compaction's own mega-prompt (up to ~300K tokens, guaranteed 0% cache — a fresh single-message prefix) leaves no row. Trace id 255's `usage: None` is exactly this path: the request died at BadGateway before any SSE chunk, so `set_usage` never fired and no request_stats row exists.
- **Root cause:** all recording hooks fire only on the success path; the trace layer has the data (ttft/generation/usage) but request_stats never receives it.
- **Suggested fix:** R21.

### LOW-2 — qwen-3.6 endpoint sends max_completion_tokens=131072, bypassing the R9 cap; every request 400s

- **Severity:** LOW
- **Location:** src/provider/openai/request.rs:300-310 (R9 cap: `min(max_output_tokens.max(4096), quantized_context_budget, 32_000).max(1024)` — can never emit 131,072); src/config/endpoints.rs (`extra_body` merged last, documented as able to override any request field).
- **Symptom:** 11 all-time / 9 post-R12 status-400 errors "max_completion_tokens=131072 cannot be greater than max_model_len"; qwen-3.6 has zero request_stats rows (every request failed) — a dead endpoint entry.
- **Root cause (hypothesis — user config not in repo):** the qwen-3.6 endpoint's `extra_body` in endpoints.toml sets `max_completion_tokens=131072` (an OpenAI o-series-style value), overriding the capped field after the merge. The R9 cap itself is verified live (trace 255 shows 32000 on a 503-message request).
- **Suggested fix:** R22.

### LOW-3 — The reset metric conflates "cache not reported" with "cache missed"

- **Severity:** LOW
- **Location:** src/provider/openai/sse.rs:190-210 (absent `prompt_tokens_details.cached_tokens` -> 0 — correct per R12) + the aggregation methodology in `.coding/analysis/cache-hit-5-aggregates.txt`.
- **Symptom:** the headline "30% of requests are resets" overstates real waste: 2,495 of the 4,711 resets are rows from model groups that report 0 cached tokens on every request (all flash tiers, gemini-vlm-gcp, maas, ds-flash). The 0-50K bucket's 50.6% hit is likewise contaminated (gemini-vlm-gcp avgP 52.9K — roughly half its 761 rows sit in that bucket at 0%).
- **Root cause:** R12 correctly trusts the provider-reported value, but "field absent" (Gemini's OpenAI-compat layer doesn't populate it; possibly the flash deployments too) and "nothing cached" are indistinguishable in the current aggregates.
- **Suggested fix:** R19.

## 4. Ranked suggestions (R18+)

Ranked by token-weighted impact on the post-R12 window. [OURS] = prompt-shape/measurement fix in our code; [PROVIDER] = proxy/model-group config issue for the LiteLLM operator.

### R18 — [PROVIDER] Get prefix caching enabled — or cached-token reporting fixed — on the flash tier and gemini; route long-context away from flash until then

- **Impact: very high.** All five `glm-5.3-flash*` groups: 0.0% cached on **every** request (1,699 post-R12 rows, avgP 105-218K, maxP 367K) while `glm-5.3-gcp*` hits 76-92% through the same proxy with the same request shape. Non-flash sglang (b200-sglang-01: 89%) reports cached tokens while flash sglang (0%) doesn't — so the discriminator is the model group, not the serving engine or our shape. gemini-vlm-gcp (761 rows, 0.0%) is almost certainly a pure reporting gap (Gemini's OpenAI-compat layer doesn't populate `prompt_tokens_details.cached_tokens`). glm-5.2-maas-gcp (17.5%, n=28) looks cache-hostile too. Together: ~245M prompt tokens at 100% reported miss, roughly 46% of all uncached tokens. If flash actually caches once reporting is fixed, the headline recovers most of the way toward ~81%; if it truly doesn't cache, a 200K-prompt session on flash re-bills the full 200K every request — the flash tier's speed (gen50 4.2-5.7s) doesn't compensate at these prompt sizes.
- **Effort:** provider ticket (zero code). Optional app-side (small): a "cache-hostile model" hint in the model picker derived from request_stats, and/or a settings advisory when a flash model is the default for long-context work.
- **Disambiguation (needs R21):** real misses at 150-280K prompts would show multi-second TTFTs; a pure reporting gap would show fast TTFTs. Current data can't tell (ttft unrecorded).

### R19 — [OURS, measurement] Split every aggregate by cache-reporting vs non-reporting tier

- **Impact: high (targets the right work).** On reporting models the post-R12 hit is ~81% and resets ~17% (2,216/13,218) — that, not 70.5%/30%, is the prompt-shape baseline. The daily trend also becomes interpretable: 09-09's 37.6% (614 resets) is flash-tier traffic (trace 255 at 22:21 is flash; the A5 worst sessions are flash/gemini-heavy) plus fallback-induced cold caches after the day's 22 provider errors plus over-cliff requests (maxP 378,792) — not a prompt-shape regression. Same for 09-04 (43.5%) and 09-01 (26.3%).
- **Effort: small.** Bucket models by "ever reported cached_tokens > 0 in the window" (or a static list: gemini-vlm-gcp, glm-5.3-flash*, glm-5.2-maas-gcp, deepseek-v4-flash) in the extraction; optionally persist a `reports_cached_tokens` flag in the stats schema.
- **Location:** the aggregates extraction over memory.db (ad-hoc script); optionally the request_stats writer.

### R20 — [OURS, prompt-shape] Close the >340K cliff leak (fixes HIGH-1; three parts, ranked)

- **R20a — send-gate at the cliff.** Impact: medium (the 340K-400K bucket's 83.7% resets is ~14M wasted prompt tokens over 9 days, plus the worst per-request latency/billing spikes). Effort: medium. When the pre-request count is over the proxy ceiling after compaction attempts, don't send the full prompt: either (i) allow under-pressure historical-reasoning stripping for GLM as a cliff-pressure exception (the `under_pressure` machinery already exists at src/provider/openai/request.rs:54 and DeepSeek uses it; GLM's `never strip` policy currently blocks it — src/provider/policy.rs), or (ii) fail the turn fast with an actionable error instead of burning up to 4 over-cliff requests. Location: src/agent/turn.rs:1660-1760 (maybe_compact) + policy.rs.
- **R20b — trigger margin for 1M-window endpoints.** Impact: prevents the jump-past-the-trigger class (gap 3 of HIGH-1). Effort: small (one constant). The effective trigger is 300K cl100k — only ~12% below the cliff, which tokenizer drift plus one tool batch can consume. Lower it to ~280K (fill 0.28 for 1M windows, or ceiling 320K). Location: src/config/general.rs:334/343 (defaults) / src/agent/context.rs:151-163.
- **R20c — Rule-4-stuck fast-fail signal.** Impact: converts a silent cliff violation into an actionable error. Effort: small-medium. When `summarize` no-ops because the open tool loop spans the conversation AND the count is over the ceiling, the current code silently counts it as an attempt and re-sends the full prompt. Return a "skipped: open loop" signal from `summarize`/`summarize_with_interrupt` (src/agent/context.rs:262-264, 336-338) so maybe_compact can surface "context over the proxy cache ceiling inside an open tool loop — finish or interrupt the tool run" immediately.

### R21 — [OURS, measurement] Record request_stats for error requests and summarizer calls; fix the TTFT plumbing

- **Impact: medium.** Makes error-retry waste and compaction cost visible, and unlocks the R18 disambiguation (TTFT cross-check). Effort: small-medium.
- Error requests: write a row with our estimated prompt tokens, `cached = NULL` (not 0), outcome = error — so reset analysis can separate real misses from error-retry cycles (each retry re-sends the full prompt; a LiteLLM fallback switch to another deployment guarantees a cold next request — the sticky-endpoint map at src/agent/loop_impl.rs:1714-1779 already reduces flapping).
- Summarizer calls: forward the `Usage` event from `summarize_with_interrupt` (src/agent/context.rs:355-392) to request_stats with a tag — compaction's own ~300K single-message prompt is currently invisible.
- TTFT: 99.2% of rows carry ttft_ms=0 although the trace layer records it — plumb it through (src/agent/turn.rs:~2440 recording site, src/provider/trace.rs:568).

### R22 — [config hygiene] Clamp or warn when extra_body sets max_completion_tokens above the sane cap

- **Impact: low (one dead endpoint). Effort: small.** Remove the `max_completion_tokens=131072` key from the qwen-3.6 endpoint's extra_body (endpoints.toml, user config); optionally clamp extra_body's `max_completion_tokens` to the same 32K sane cap — or emit a config-load warning — at the merge site (src/provider/openai/request.rs extra_body merge; src/config/endpoints.rs).

**No action needed (verified healthy):** the 0-50K bucket's genuine cold starts — the cache-stable head (20,301-char system head + 25 tool schemas, byte-stable; trace 255 confirms the sentinel design), the hysteresis tool-result compaction (R14/R15: src/agent/turn.rs:1447, keep 10/20 @ 500 chars), and the R9 output cap are all live and doing their jobs. Cross-session prefix hits at small sizes depend on the provider's cache TTL/eviction, not our shape.

## 5. Answers to the round's specific questions

1. **Post-R12 70.5% vs pre-R12 95.7%:** the 95.7% was heuristic inflation (the R12 fix landed 08-31); 70.5% is real. On cache-reporting models it is ~81% — the drop is mostly the non-reporting tier entering honest measurement, not a shape regression.
2. **Flash 0% vs gcp 76-92%:** provider-side model-group difference (R18) — same proxy, same request shape; non-flash sglang reports (89%) while flash sglang doesn't (0%), so it's the model group, not the engine or our shape. gemini's 0% is likely a pure reporting gap.
3. **Why R13 didn't prevent all >340K prompts:** three code-verified gaps — Rule-4 open-loop skip (compaction no-ops), no send-gate (request sent anyway, up to 4 over-cliff requests before the 5th attempt aborts), and cl100k->GLM tokenizer drift plus one-batch growth jumping the 300K trigger past 340K (HIGH-1). Affected: glm-5.3 (maxP 408,956) and glm-5.2 on the 1M-window endpoints, in long single-turn tool-loop sessions.
4. **0-50K at 50.6%:** contaminated by non-reporting models (gemini ~half its rows sit here at 0%); the genuine remainder is cold starts (new sessions, subagent spawns, model switches) — inherent, and the head is already byte-stable. See R19.
5. **09-09 at 37.6%:** flash-tier traffic + 22 provider errors with fallback-induced cold caches + over-cliff requests (maxP 378,792). Not a prompt-shape regression; needs the R19 split to read.
6. **Trace 255 `usage: None`:** error path — `set_usage` fires only on the stream's final chunk (src/provider/trace.rs:568); the request died at BadGateway ("No fallback model group found") before any chunk, so usage/ttft/generation stay None and no request_stats row is written (LOW-1, R21).

## 6. Verified live at HEAD c18b5d0

- **R9** dynamic max_completion_tokens cap — src/provider/openai/request.rs:300-310 (`min(max_output_tokens.max(4096), 2048-quantized context budget, 32_000).max(1024)`); trace 255 shows 32000 on a 503-message request.
- **R12** real provider-reported cached tokens — src/provider/openai/sse.rs:190-210 (`usage.prompt_tokens_details.cached_tokens`, absent -> 0), trusted as-is in turn.rs.
- **R13** fill 0.3 + ceiling guard — src/config/general.rs:334/343 (`proxy_cache_ceiling_tokens` default `PROXY_CACHE_CEILING_TOKENS`), src/provider/mod.rs:753-759 (340,000 / 32,768), src/agent/context.rs:151-163 (`effective_summarize_threshold` = min(fill product, ceiling minus margin)).
- **R14/R15** hysteresis tool-result compaction — src/agent/turn.rs:1447 (`compact_old_tool_results(messages, 10, 20, 500)` after every tool batch).
- **R17** output budget — the 32K sane cap above.
- Cache-stable head + volatile tail + footer sentinel (trace 255: system[0] 20,301 chars; last message = sentinel).
- 429-fallback sticky-endpoint with live-count viability — src/agent/loop_impl.rs:1714-1779.

## 7. Bug list (for the executive summary)

1. **HIGH — over-cliff requests still sent post-R13:** compaction is skipped (Rule 4 open tool loop) or insufficient, and the request goes out anyway — up to 4 over-cliff requests per turn before the 5th attempt aborts; 47 post-R12 rows >340K at 16.3% hit. (src/agent/turn.rs:1743-1750, src/agent/context.rs:437-485.) Fix: R20a/b/c.
2. **LOW — request_stats success-path-only recording:** error retries and summarizer calls invisible; TTFT not flowing (0 on 99.2% of rows). (src/agent/turn.rs:~2440, src/agent/context.rs:355-392, src/provider/trace.rs:568.) Fix: R21.
3. **LOW — qwen-3.6 sends max_completion_tokens=131072 (R9 cap bypassed, likely extra_body override); every request 400s.** (src/provider/openai/request.rs:300-310 + endpoints.toml extra_body.) Fix: R22.
4. **LOW — reset metric conflates non-reporting models with real misses:** 30% resets is really ~17% on reporting models. (src/provider/openai/sse.rs:190-210 + aggregation.) Fix: R19.
