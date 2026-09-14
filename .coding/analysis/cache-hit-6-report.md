# Cache-hit round 6 — why hits are poor, and the fix

**Date:** 2027-01-11 (epoch clock 2026-09-14)
**Instruments:** `.coding/logs/traces.jsonl` (13 records captured / 11 parseable, 1 session, full
request bodies + provider `usage`), `.coding/memory.db` `request_stats` (972 rows, 20 sessions,
2026-09-12..14), `.coding/logs/provider-errors.jsonl`.
**Artifacts:** `.coding/analysis/cache-hit-6-extract.py` (the extractor) →
`.coding/analysis/cache-hit-6-aggregates.txt` (250 lines, sections A–F).

**Sample honesty:** the trace log covers 76 seconds of ONE session (03:01:14–03:02:30 UTC) — it is
proof of MECHANISM (byte-exact breaks + real provider `cached`), not of rates. Every rate claim below
comes from `request_stats`. Numbers are marked measured (M) or estimated (E).

---

## 1. Headline

| window | requests | hit% (M) | resets <50% (M) | avg prompt (M) |
|---|---|---|---|---|
| round 5, all-time reporting tier | 27,544 | 88.6% | 12.5% | ~120–300K |
| round 5, post-R12 | 14,751 | 79.8% | 18.1% | — |
| **round 6, all rows (2026-09-12..14)** | 1,037 | **79.0%** | 17.6% | 127K |
| **round 6, traced window (03:01–03:02:30)** | 25 | **63.5%** | — | 120K |

**Snapshot, not truth.** These are the numbers the extractor printed at the run that produced
`cache-hit-6-aggregates.txt` (1,043 rows / 21 sessions then). `request_stats` keeps accumulating —
including from the very session that wrote this report — so re-running the extractor yields slightly
different counts (an earlier draft of this table was checked against 972 rows / 24 window rows).
Only the *shape* is load-bearing: ~79% overall with a ~63% session, against 99% when the prefix is
stable. Re-run `.coding/analysis/cache-hit-6-extract.py` for current figures.

Per model (main-loop rows, M, same snapshot): deepseek-v4-flash 83.7% (n=454) · deepseek-v4-flash:0731
74.7% (n=270) · deepseek-v4.1-flash 71.7% (n=152) · glm-5.3-flash 78.7% (n=130) · glm-5.3 79.7%
(n=19) · kimi-k3 42.4% (n=12). All six models report cache in this dataset — **no non-reporting tier
this round**.

The within-session shape is the signal: the same conversation reaches **99.0–99.7%** on some requests
and **5.0%** on others, minutes apart. So the ceiling is not the provider — it is what we send.

## 2. Cause 1 (dominant, steady state) — the compaction gate is permanently open, so we rewrite an already-sent tool result on EVERY request

**Measured evidence**

- Section B (traces): **5 of 5** same-model consecutive pairs are `MID` — i.e. a message that was
  already sent is *mutated in place*, never a pure append. First differing message index k ≈ 271, 275,
  276, 280, 286 with the conversation at 293–304 messages → the rewrite always sits **~20 messages from
  the end**.
- Section B (byte level): the mutated message is always a `tool` result, always at byte offset 504–517
  inside it, and it is rewritten **from its full form to a truncated one**: 4,336 → 717 chars,
  2,831 → 546, 2,697 → 589, 8,745 → 711, 1,193 → 544. The replacement text carries
  `[… truncated for context efficiency]` — i.e. `COMPACTED_MARKER` (`src/agent/context.rs:646`).
- Section B2 (the falsifiable prediction): for pairs where the provider and our own accounting agree,
  **cached ≈ the calibrated pre-break prefix** — "YES (our mutation)" on 3→4 and 4→5. The provider
  reuses everything up to our rewrite and **re-bills the rest: 36–41K tokens per request (M)**. (The
  `NO` verdicts on two pairs can be estimator artifacts: the token estimate omits the tools-array
  chars that the calibration includes, so `pre` runs low — the conclusion rests only on the `YES`
  pairs, which need no estimate to interpret.)
- Section B3 (the mechanism): `compact_old_tool_results(messages, 10, 20, 500)` fires only while the
  *intact* population exceeds `keep_high = 20`. Measured on every request: **intact = 68–71**, of which
  **60–63 are results ≤ 601 chars** and only **7–8 are truncatable** → gate **OPEN (fires) on every
  single request**. The intact count can never fall below 20, because…

**Root cause (code, read not guessed)**

`truncate_tool_result_at` (`src/agent/context.rs:829-851`) refuses to touch a result whose length is
`<= keep_chars + 100` **and returns `false` without adding the marker**. The gate's intact list
(`compact_old_tool_results`, `src/agent/context.rs:950-963`) filters on the marker only. So every short
tool result is *permanently intact*: once the conversation accumulates 11+ of them, the hysteresis gate
can never close again. The "keep window" then slides by ~2 messages per turn as the conversation grows
and cuts one more already-sent result per request — a guaranteed prefix break, every request, forever.
The R14 fix added hysteresis but never verified the gate can *close*; this is the same failure family,
still live.

**Consequence:** steady state is capped at 70–78% (M) where the same session demonstrably reaches
99.0–99.7% (M) whenever the prefix is stable. That difference — ~36–41K tokens re-billed per request —
is the bulk of the miss mass.

## 3. Cause 2 (avoidable, deliberate design cost) — the head is rewritten whenever the plan KIND changes

- Section B: pair 5→6 is `HEAD` — system message 26,993 → 25,831 chars **and** the tools array 30 → 26
  entries, in the same request. Provider `cached` collapses to **6,016** — the common prefix of the two
  system prompts (M).
- `request_stats` rows 02:59:40, 02:59:50, 03:00:53, 03:01:22, 03:02:07, 03:02:25, 03:02:29: **7 of 24
  window requests at ~5% hit with TTFT 15.7 s / 17.9 s** (M) — then 99.7% once a request with the new
  head completes and re-primes the cache. Each transition re-bills a ~120K-token prompt (M).
- Cause: `Workflow::schema_filter` (`src/workflow/mod.rs:458-474`) freezes the *advertised* schema once
  per plan — but picks a different surface per plan kind (`ExecutingResearch` for `kind=research`,
  pinned by `schema_filter_research_plan_uses_executing_research`, `src/workflow/mod.rs:1836`). Two
  plans (research → abandoned → implementation) in four minutes = two head rewrites. This is
  advertisement-only (`ToolFilter::PlanFrozen` doc, `src/tool/mod.rs:219-239`): dispatch enforces the
  restriction, so the array *could* be identical for both kinds.
- Verdict: **AVOIDABLE**, but it reverses a documented intent — hence reported, not silently changed.

## 4. What is NOT the cause (so the next round does not re-litigate)

- **The 340K cliff.** The 320–360K bin hits **96.9%** (M); max prompt in the dataset is 322,619 (M).
  The round-4 observation does not reproduce here.
- **Non-reporting models.** None: all six models in `request_stats` report `cached_tokens`.
- **The min(prev,curr) heuristic** (round 4) — fixed; the trace's `usage` values are provider-real.
- **R14 truncation *per se*** — the hysteresis exists; it is defeated, not missing (cause 1).

## 5. Ranked fixes

| # | Fix | Where | Expected win | Risk | Regression test |
|---|---|---|---|---|---|
| **R6-1** | Make the gate honest: count only **truncatable** results (> `keep_chars + 100`) toward the high-water mark — or mark a short result once so it leaves the intact population. Either way the pass stops firing every request. | `src/agent/context.rs::compact_old_tool_results` (gate, ~969); helper at 829-851 | Steady state 70–78% → **~99%** (M: the stable-prefix rows already reach 99.0–99.7%); removes ~36–41K re-billed tokens/request in tool-heavy sessions | Low — contained to the gate; the truncation semantics for long results are unchanged | `short_results_do_not_hold_the_gate_open` (new; must fail on today's code) |
| **R6-2** | Keep the advertised schema byte-stable across plan kinds: use the same frozen surface for `research` and `implementation` plans (dispatch already enforces the research restriction). | `src/workflow/mod.rs::schema_filter` (458-474) + update the test at 1836 | Removes ~29% of window requests at ~5% hit and the 15–18 s TTFT stalls after every plan transition | Medium — reverses a documented intent; needs the user's call | `schema_filter_is_stable_across_plan_kinds` |
| **R6-3** | Trace-writer/instrument hygiene: the log is appended while a request streams, so its last line is a partial record (the round-5 extractor crashes on it). Emit complete records only, or keep consumers tolerant. | `.coding/logs/traces.jsonl` producer; consumers | No hit-rate effect; prevents broken analysis tooling | Low | tolerant-parse already added in `cache-hit-6-extract.py` (section A note) |

**Recommended order:** R6-1 now (contained, largest measured win), R6-2 as a deliberate decision, R6-3
opportunistic.

## 6. Caveats

- The traced session is one 76-second window; the *rate* figures rest on `request_stats` (1,043 rows
  / 21 sessions at the report snapshot), which does not capture request bodies — the two instruments
  are complementary, and the causal claims here rest on the traces.
- `cached` field naming differs per provider; the extractor walks `usage` recursively for any
  cache-named key, so it keeps working across providers.
- Token counts per message are estimates (chars/4) except where `request_stats.prompt_tokens` is used;
  section B2's calibration exists precisely because chars/4 is not trustworthy at this scale.
