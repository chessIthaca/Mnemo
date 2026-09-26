+++
title = "Model/reasoning changes land on the next request (per-request provider resolution)"
created = "2027-01-11"
+++

Landed 2027-01-25 (backlog fe499e37; commits da43df2 + c4af5ca on wt/mnemo): the turn loop resolves its provider + context manager per REQUEST, not per turn.

Mechanism (src/agent/turn.rs): run_turn snapshots nothing — `resolve_iteration_provider` re-reads the LIVE default pair (self.provider() / self.context_manager()) at the top of every loop iteration; `handle_pending_swap` also runs at the LOOP top, so a deferred (smaller-context) pick completes mid-run instead of at the next turn. AgentEvent::ModelChanged emits from that per-request seam via `AgentLoop::note_served_effective(model, endpoint, effort)` (src/agent/loop_impl.rs) — fires only when the served triple actually changes, seeded silently at turn top (so a first serve never emits spuriously).

Invariants to preserve:
1. `handle_pending_swap` returns `PendingSwapOutcome { rewrote, early }`; the loop-top caller must `state.token_accounting.reset()` when `rewrote` (a pre-swap summary rewrites `messages` inside the counted prefix — same reset gate as `maybe_compact`; the turn-top caller ignores `rewrote`, it runs before TurnState::fresh()).
2. Resolution ORDER in `resolve_turn_provider` (skill → pin/explicit → forced → subagent → state [models.*]) is authoritative: a plain default-slot swap can never displace a forced/subagent/pin model.

Tests: src/agent/tests.rs `mid_run_*` (5 — model swap, deferred pick, effort-only, Reviewing state, forced-model guard; 4 red before the fix); frontend `useAgentStore.test.ts` ("model_changed mid-run retargets the tab label"). Review trail: .coding/reviews/2026-09-26-95a176e9-mid-run-model-landing-review.md (round 1: 0 high, 1 low) and …-round2-accounting-reset-review.md (round 2: PASS, finish gate).
