+++
title = "pre-prompt model routing (Laya chain item 2, backlog 091e694d)"
created = "2027-01-11"
+++

Shipped on wt/mnemo: commits b7d6ab5 (feature, 26 files), c9dc2f9 (round-1 fixes), 674aabe (round-2 PASS report). Reviews: .coding/reviews/2026-09-27-pre-prompt-model-routing-review.md (FINDINGS 0 high/4 low) + -round2.md (PASS). Plan .coding/plans/d69abf72.md.

Config (src/config/general.rs, src/config/settings_dto.rs): `[general.laya] routing = true` opt-in (skip-serialized while off) + `[general.routing] { cheap, capable, threshold = 0.80, enforce = false }` (full ModelRefs, so cheap can also mean low effort). DTO patch semantics: absent = unchanged, null target clears, threshold validated.

Behavior (src/agent/model_routing.rs, src/agent/turn.rs, src/model_resolver.rs): ONE calibrated `Question::Choice` (trivial vs architectural) per MAIN-AGENT turn at `run_turn` head, before the first request (task text capped at 2000 chars). The routed arm sits BELOW skill / picker-pin / forced-model and ABOVE the state chain; `ModelResolver::resolve_routed` is the single refusal point for skill / subagent / bug-fixing contexts. Classify-once-memoized per turn; policy read from LIVE config per turn so a Settings save lands next turn. Best-effort: below gate, no answer, unknown label, unset/dangling target, cheap == capable -> today's model byte-identical.

Shadow-first staging: `enforce = false` default = classify + log every turn, model untouched; flipping to enforce needs the classifier `ready` with a fine-tuned checkpoint (base checkpoints are near-chance; the log IS the fine-tune corpus). Settings → Classifier 6th block (ClassifierSection.tsx): opt-in, enforce (locked ON until ready, always switchable OFF), two model pickers, threshold input (empty/out-of-range edits rejected + snapped back).

Log: `~/.mnemo/laya/training/routing.jsonl` — decision row at turn start, outcome row via `TurnRoute` Drop (the one hook every exit path runs), rows join on turn_id. Invariants to preserve: (1) an unresolved turn-start target is memoized as a MISS and never armed mid-turn (a Settings save cannot contradict the row's `enforced: false`); (2) `iterations` counts provider-request iterations — the increment sits below the deferred-swap early exit; (3) subagents, reviewers and compaction are never classified and never logged (corpus stays user-task text); (4) classifier/log calls can never fail a turn.

Tests: 8 unit (model_routing.rs) + 9 turn-level (src/agent/tests.rs, agent::tests::routing_*), incl. the L2 pin `routing_memoizes_an_unresolved_target_so_a_mid_turn_target_cannot_route` (proven red pre-fix); frontend 1305/1305; cargo test exit 0. Docs: docs/CONFIGURATION.md, PLAN.md, README.md.
