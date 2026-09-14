+++
title = "prompt-cache resets in a session — in-place compaction batch and plan-boundary request-shape switch"
created = "2027-01-11"
+++

SPEC (diagnosis 2027-01-11, research plan 83fb316a): a session's provider prompt-cache "big reset" has exactly two structural causes here — neither is a defect.

(1) IN-PLACE COMPACTION BATCH. `compact_old_tool_results` (src/agent/context.rs:950) fires once at its hysteresis high-water crossing and rewrites a batch of already-sent tool results in place (observed: 11 results; the in-place marker count jumped 11 → 22 messages; localized break = the OLDEST rewritten message, index 23, a tool result going 30,262 → 628 chars). The provider caches the byte prefix, so the boundary snaps back to that index: cached 20,048 / prompt 124,025 = 16.2%, TTFT 9,378 ms vs ~1.0-1.7 s on cached rows — one full re-bill (~104k tokens). The NEXT call recovers (98.2%) because the mutated history is now the cached prefix. Note: chars/4 token estimates under-count ~2-2.4x in this data; use ratios, not absolutes.

(2) PLAN-BOUNDARY REQUEST-SHAPE SWITCH. When a plan ends the request changes wholesale: model+endpoint (per-slot mapping in ~/.mnemo/config.toml — [models.bug_fixing] deepseek-v4.1-flash @ Ollama Cloud, [models.executing] deepseek-v4-flash @ deepseek, [models.complete] glm-5.3-flash), head bytes (24,403 → 24,232, sha cc1403365345 → ce08d83da304) and advertised tools (34 → 26, the plan-frozen array popping to the per-state surface). A different model+endpoint is a different cache namespace → cold (13,184 / 105,398 = 12.5%). Reducible only by keeping the slot models on one model/endpoint.

REFUTED for this window: the fold-vendor tail-in-head bug (fixed in main at d53546e) — messages[0] carries no volatile-tail markers, its sha is byte-identical across records 57-61, and the last message is the byte-stable CONTEXT_FOOTER (src/agent/prompt.rs:470) with a constant sha, i.e. the post-fix shape; no `purpose='summarize'` rows in the window (no compaction head swap); all rows share one session.

Evidence: `.coding/analysis/cache-reset-why.md` (verdict), `.coding/analysis/cache-reset-tail.txt` (extractor output), `.coding/analysis/cache-reset-extract.py` (read-only extractor, re-runnable). Caveat: the trace ring stopped at 10:07:43 (mirror mtime), so a later 3.2% dip at 10:11:07 visible in request_stats has no byte record — the same boundary pattern is the leading, unverified candidate.
