+++
title = "step-2 seams for unified tiers (four-label routing)"
created = "2026-11-04"
+++

Step 2 (four-label routing question, commit bfeb3648) left interim seams that steps 3 and 4 must know:

1. Mnemo1 strike-escalation hang (root cause): `src/agent/turn.rs` ~2938 walks `route_question::label_above(&settled, policy.offered)` and maps TRIVIAL/Routine/HARD → Cheap/Routine/Capable, `_ => None`. With `involved` offered, label_above("routine") returns "involved" → no target → the nudge never fires and tests awaiting strike resolution (blocking resolvers) hang forever. Fix applied: `routing_policy_with_threshold` in src/agent/tests.rs pins `involved: false` (legacy 3-label set) with a doc comment. Step 4 must add the involved/medium rung to turn.rs's match (RouteTarget::Medium exists) BEFORE flipping any Mnemo1 fixture to four offered labels.

2. dispatch_routing.rs (step 3 owns it) was touched three times beyond pure compile:
   - `EnabledTiers::offered()` pins `involved: false` (doc: step-3 placeholder) — dispatch still offers the 3-label interim question.
   - private `label_for_tier` now maps ladder ids via `route_question::label_for_tier` first, then the legacy TIER_LADDER table — required because route_knn now votes ladder ids; without it knn_consistent_rows_answer_before_laya and a_slow_overlay_build_never_starves_laya fail (vote drops, Laya answers).
   - `label_tier` is UNCHANGED: it still returns legacy ids (simple/average/difficult), so kNN-backed decisions still write legacy tier ids to the routing log. Step 3 rewires both through route_question::tier_for_label and flips offered() to all-four.

3. Eval three-label run uses offered {trivial, routine, hard} (interim EnabledTiers shape); two-label run {trivial, hard}; four-label run is what dispatch will ask. eval_record(four, three, two); EvalRecord gained min_confidence_4 (serde default). RoutingEvalWire gained `four`.

4. Route-dataset export lives in src-tauri/src/ipc/finetune.rs, feature-gated `#[cfg(feature = "mnemo2")]` (main.rs's `mod ipc` is unconditional; mnemo2 is lib-only and feature-gated — bin-side ipc files must reference it as `mnemo_app::mnemo2::...` under cfg). Hook: top of run_startup_finetune, BEFORE the gate, best-effort; missing dispatch_routing.jsonl = no-op, nothing written. Audit workers are identified by their route row's skip:"audit" (route_eval::logged_tier_label is the shared tier-id→Laya-label mapper: convert_old_id then route_question::label_for_tier).
