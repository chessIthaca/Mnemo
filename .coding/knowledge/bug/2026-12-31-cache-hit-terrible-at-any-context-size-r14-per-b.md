+++
title = "cache hit terrible at any context size — R14 per-batch tool-result truncation breaks the prefix cache"
created = "2026-12-31"
+++

SYMPTOM: cache hit rate is terrible even at small context sizes (user observation, 2027-01-04), despite R1/R5 having achieved 98.5% within-state pre-R14.

ROOT CAUSE (proven from code + CONFIRMED EMPIRICALLY 2027-01-04): R14 tool-result compaction (shipped 2026-12-04, R12-R17 batch, commit 9f3f0d7) breaks the prefix cache on EVERY tool batch. `compact_old_tool_results(messages, keep=10, summary_chars=500)` ran unconditionally after each tool batch (src/agent/turn.rs:2105) and truncated all tool results except the last 10 IN PLACE (src/agent/context.rs:482-515). Each batch slid the keep-window; the result falling out was mutated → the next request differed from the previous one at that message → the longest-common-prefix cache broke there → the remaining intact window (up to 10 fat tool results) plus all recent assistant reasoning was re-processed as fresh on every request. Structural, SIZE-INDEPENDENT hit ceiling. Sessions with ≤10 total tool results were unaffected (early-return at context.rs:491).

EMPIRICAL CONFIRMATION (traces.jsonl id 15, glm-5.3-gcp, 2027-01-04): prompt 45,495 / cached 20,928 = 46.0%. The request carried 17 tool results; result #17's arrival (a read_files) newly truncated result #7 (the 3-file cache-analysis read, ~28KB → 500-char stub) — the first byte-difference vs the previous request. Predicted cached (everything before result #7: system head + turn 1 + first turn-2 batch ≈ 20-21K tokens) matches the reported 20,928 within ~3% — the proxy cache works PERFECTLY up to the mutation point; the miss (24.5K tokens) = last 10 intact tool results + all assistant reasoning_content since (reasoning_effort:max makes these fat). Every recent call in the session sat at ~46-50%: each batch newly truncated the next-oldest result, re-breaking the prefix.

FIX: Implemented hysteresis compaction in `compact_old_tool_results(messages, keep=10, keep_high=20, summary_chars=500)` (src/agent/context.rs, src/agent/turn.rs:2105). While the number of intact (un-truncated) tool results is `<= keep_high` (20), compaction is a strict no-op that mutates nothing, keeping consecutive requests byte-identical for ~95% prefix cache hit rate across ~10 turns. Once intact results exceed 20, all but the newest `keep` (10) intact results are truncated in a single cut. `compact_old_tool_results` returns the count of newly truncated messages, and `token_accounting.reset()` in `run_turn` is only invoked when `truncated > 0`.

REGRESSION TESTS:
- `compact_below_high_water_mark_mutates_nothing` (src/agent/context.rs: tests 14 intact results below the 20-result mark; asserts 0 mutations and strict message equality; failed prior to fix and passes with fix).
- `compact_fires_at_high_water_mark` (src/agent/context.rs: tests 21 intact results; asserts 11 results truncated in one cut, newest 10 intact, and second pass idempotent).
