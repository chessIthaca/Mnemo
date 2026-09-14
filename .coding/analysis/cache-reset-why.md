# Why the last three trace calls reset the prompt cache (diagnosis, 2027-01-11)

Question: "the last three calls in the logs have a big cache reset, why?"

Data: `.coding/logs/traces.jsonl` (6 records, ids 57-62, 2026-09-14 10:06:17-10:07:35 UTC)
joined to `request_stats` (15 newest rows, session c23b593c-995) by cached-token value.
Extractor: `.coding/analysis/cache-reset-extract.py` -> `.coding/analysis/cache-reset-tail.txt`.

## The last three calls

| call  | started  | model @ endpoint                        | prompt  | cached  | hit   | ttft   |
|-------|----------|-----------------------------------------|---------|---------|-------|--------|
| id 60 | 10:07:10 | deepseek-v4.1-flash @ Ollama Cloud      | 124,025 |  20,048 | 16.2% | 9378ms |
| id 61 | 10:07:30 | deepseek-v4.1-flash @ Ollama Cloud      | 125,495 | 123,188 | 98.2% | 1709ms |
| id 62 | 10:07:35 | deepseek-v4-flash @ deepseek            | 105,398 |  13,184 | 12.5% | 1220ms |

## Call 1 (id 60) - in-place tool-result compaction (mid-history mutation)

* Localized break: message index 23. prev msg[23] = tool result, 30,262 chars;
  cur msg[23] = same slot, 628 chars. The in-place compaction marker count
  jumped 11 -> 22 messages in that single step (11 old tool results rewritten
  at once).
* Mechanism: `compact_old_tool_results` (src/agent/context.rs:950) - the
  hysteresis gate: no-op below the high-water mark, one truncation batch per
  crossing (`compact_below_high_water_mark_mutates_nothing`,
  `compact_fires_at_high_water_mark`).
* Why the cache dies: the provider caches the request byte prefix. Rewriting
  any already-sent message moves the boundary back to just before the OLDEST
  rewritten message, so cached collapses to ~20k (~ the prefix before msg 23).
  (My chars/4 estimate said 8,238 for that prefix - the estimator under-counts
  ~2-2.4x consistently, so treat only the ratios, not the absolutes.)
* Cost: one full re-bill of ~104k tokens - visible as TTFT 9,378 ms vs
  ~1.0-1.7 s on cached rows. The very next call is 98.2% again.
* Verdict: expected behavior of a documented tradeoff (the R14 hysteresis fix;
  the information-loss tradeoff is documented in the round-6 review), NOT a
  defect - but it is the biggest single cost in this window.

## Call 2 (id 61) - the recovery call

* The only "break" is at index 107 = the volatile-tail boundary (a new
  assistant message inserted before the trailing tail user message); everything
  before it is byte-identical.
* 98.2% - proof the compaction reset is a one-shot event, not a standing
  misconfiguration.

## Call 3 (id 62) - plan-completion boundary: model/endpoint + tool surface change (cold)

* Localized: UNRELATED request shape in the SAME conversation. Model
  deepseek-v4.1-flash -> deepseek-v4-flash; endpoint Ollama Cloud -> deepseek;
  head bytes 24,403 -> 24,232 chars (sha cc1403365345 -> ce08d83da304);
  advertised tools 34 -> 26.
* Trigger: the plan-completion boundary. `.coding/plans/12284516.md` (the
  bug_fixing plan that ran this session) was written at 10:07:35 - the same
  minute. Its bug_fixing slot stopped engaging, and the per-slot mapping in
  `~/.mnemo/config.toml` put the session on a different model:
  [models.bug_fixing] = deepseek-v4.1-flash, [models.executing] =
  deepseek-v4-flash, [models.complete] = glm-5.3-flash (served at different
  endpoints). The advertised tool array also changed because the plan frame
  popped (`Workflow::schema_filter` -> PlanFrozen while a plan is active,
  per-state surface after; src/workflow/mod.rs, src/tool/mod.rs).
* Why the cache dies: a different model+endpoint is a different provider cache
  namespace (cold by definition), and the head/tools bytes ride at the front of
  the body, so any change there resets the prefix even on one endpoint.
* Verdict: expected at a plan boundary; reducible only by keeping the slot
  models on one model/endpoint.

## Refuted mechanisms

* Fold-vendor tail-in-head (fixed in main at d53546e): REFUTED. messages[0]
  carries none of the volatile-tail markers (WORKFLOW STATE / RECALLED
  MEMORIES / CONTEXT FOOTER / PROGRESS:), its sha is byte-identical across
  records 57-61, and the last message is the byte-stable CONTEXT_FOOTER
  (50 chars, constant sha 29b55f7bd7de, src/agent/prompt.rs:470) - exactly the
  post-fix shape described at src/agent/prompt.rs:481-495.
* Summarization head swap: REFUTED. All 15 request_stats rows are `main`;
  there is no `purpose='summarize'` row in the window.
* Cold new session / unrelated agent: REFUTED. Every row shares session
  c23b593c-995; records 57-61 share one head sha.

## One further dip, outside the trace ring

* request_stats row @10:11:07: prompt 156,580 / cached 4,992 = 3.2%. The trace
  ring stops at 10:07:35 (mirror mtime 10:07:43 - tracing appears to have
  stopped there), so there is NO byte record for it. The leading candidate is
  the same boundary pattern (a new plan frame changes the advertised surface
  and the resolved model); UNVERIFIED - re-enable tracing to pin it.
* Context: the rest of the window runs 94-99% (rows 10:11:39-10:12:17), so
  there is no standing cache problem.

## Conclusion (short answer)

Two of the last three calls reset: (1) a compaction batch rewriting 11
already-sent tool results in place (prefix breaks at message 23 -> 16.2% +
9.4 s TTFT, one full re-bill), and (2) the plan-completion boundary switching
model+endpoint and the advertised tool surface (-> 12.5%, cold). The call
between them was 98.2%, i.e. both events are one-shot. No fold-vendor
regression.
