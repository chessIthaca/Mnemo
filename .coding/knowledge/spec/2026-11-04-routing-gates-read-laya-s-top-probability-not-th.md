+++
title = "routing gates read Laya's top probability, not the margin confidence"
created = "2026-11-04"
+++

A live `/v1/systemone` choice answer carries TWO confidence numbers: the pick's lower, margin-like `confidence` (e.g. 0.6742) and the top option's probability — Laya's `answer_confidence` (e.g. 0.9113, floor 1/number-of-labels). `src/memory/classifier.rs` `parse_answer_for` keeps both in `Answer::Choice { label, confidence, probabilities }`.

The routing gates in `src/agent/route_question.rs` (DEFAULT_MIN_CONFIDENCE_3 = 0.475, DEFAULT_MIN_CONFIDENCE_2 = 0.60) are calibrated for the TOP PROBABILITY, not the margin. Reading only `confidence` made dispatch routing almost never clear.

Use `route_question::answer_confidence(&Answer) -> f64`: for `Answer::Choice`, the maximum over `probabilities` when non-empty, else the pick's `confidence`; other primitives keep `Answer::confidence()`. Implement the max with `max_by(|a, b| a.total_cmp(b))` — f64 is not `Ord`, so `Iterator::max()` does not compile. Consumers: Mnemo2 `dispatch_routing::classify_dispatch` (decision `confidence` and `cleared`, so logged route rows carry the top probability), Mnemo2 `route_eval::classify_corpus` (the eval must score what the gate reads), Mnemo1 `model_routing::classify_task_text`.

Keep `parse_answer_for`, the `Answer` type, failure triage, memory auto-typing and tool choice on `Answer::confidence` — those gates ARE calibrated on the margin-like number and must not move.
