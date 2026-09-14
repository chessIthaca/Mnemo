+++
title = "compaction gate permanently open — a sent tool result rewritten every request (70-78% vs 99% cache hit)"
created = "2027-01-11"
+++

SYMPTOM (user, 2027-01-11): "so many poor cache hits" after enabling the opt-in trace log; the same session's requests alternate between 99.0-99.7% and 5.0% within minutes (measured, `request_stats` + traces).

ROOT CAUSE 1 — THE COMPACTION GATE CAN NEVER CLOSE (dominant, steady state). `compact_old_tool_results` (src/agent/context.rs:939) fires while the INTACT population exceeds `keep_high`; but `truncate_tool_result_at` (829-851) REFUSES any result of `<= keep_chars + 100` WITHOUT adding COMPACTED_MARKER, so short results stay intact forever. Traces prove it: intact = 68-71 vs keep_high = 20 on EVERY request, of which 60-63 are permanently unmarkable and only 7-8 truncatable (`.coding/analysis/cache-hit-6-aggregates.txt` section B3). The sliding keep window therefore rewrites one already-sent tool result per request, ~20 messages from the end, at byte offset ~504-517 inside the result (4,336→717 chars etc.). The provider's cached value equals the calibrated pre-break prefix (prediction CONFIRMED, section B2) → the whole tail, 36-41K tokens, is re-billed EVERY request. Steady state capped at 70-78% where 99% is demonstrably reachable.
FIX: the gate now counts `tool_result_is_truncatable` (mirrors the refusal rules exactly). Regression test `agent::context::tests::short_results_do_not_hold_the_gate_open` (verified failing with the fix neutered: left 2 / right 0). Round-6 plan c43ad4a3, report `.coding/analysis/cache-hit-6-report.md`.

ROOT CAUSE 2 — HEAD REWRITE PER PLAN KIND (avoidable, design). `Workflow::schema_filter` (src/workflow/mod.rs:458-474) freezes the ADVERTISED schema per plan but picks `ExecutingResearch` for kind=research (test `schema_filter_research_plan_uses_executing_research`, line 1836), so a plan transition rewrites system+tools: measured pair 5→6 HEAD, system 26,993→25,831 chars, tools 30→26, cached collapsing to 6,016 (the common system prefix), 7/24 window rows at ~5% with TTFT 15.7/17.9 s. Advertisement-only (dispatch enforces), so it COULD be one stable superset — needs the user's call; NOT changed.

NOT the cause (measured): the 340K cliff (320-360K bin = 96.9% hit), non-reporting models (none here), the round-4 min(prev,curr) heuristic (fixed). Instrument: the trace file is appended while streaming, so its last line is a partial record — consumers must tolerate it (the round-5 extractor crashed).

Related: R12-R17 batch (plan 24b50248) added the hysteresis this bug defeats; bug record `.coding/knowledge/bug/2026-12-31-cache-hit-terrible-at-any-context-size-r14-per-b.md` is the same family (already fixed per se).

Amended 2027-01-11: Update (2027-01-11, merge_to_main skill): this fix is MERGED into main at 4cbc3b0 (4cbc3b0b78bea5d4ab22967d3dd634b4851f9496); branch wt/mnemo deleted (pre-merge tip 4c08077). The regression test `short_results_do_not_hold_the_gate_open`, the round-6 analysis artifacts (.coding/analysis/cache-hit-6-*.*) with the §7 post-restart effect check, and this record all ride the merge — verify the effect on the first real session after the app is rebuilt and restarted.
