+++
title = "Compaction-limit fallback — cumulative +10% temporary raise (backlog 11513ee5)"
created = "2027-01-11"
status = "superseded"
+++

Compaction-limit fallback (backlog 11513ee5, plan 88da1367). A compaction pass that still leaves the conversation at/over the summarize trigger (kept-verbatim tail dominates) no longer re-compacts to the bounded abort: the session temporarily RAISES its effective limit by +10% per failed pass, cumulatively.

Architecture / where:
- src/agent/context.rs: `CompactionRaise` (Arc<AtomicUsize> steps, +10% each, <= `MAX_COMPACTION_RAISE_STEPS` = 10), `ContextManager::effective_limit()` (base fill-rate trigger x factor, clamped to `raise_cap()`), `compaction_fit(used) -> CompactionFit { Fits, Raised{steps,factor,limit}, StillOver{limit} }`, `compaction_raise_note()`, `with_compaction_raise()`. `should_summarize` and the in-loop trigger read `effective_limit()`; `effective_summarize_at()` is the BASE dial.
- src/agent/loop_impl.rs: the canonical ladder is the SESSION's (`AgentLoop::compaction_raise`, fresh per loop in `from_config`), stamped on every manager hand-out (`context_manager()`) and in `resolve_iteration_provider` (turn.rs) so the resolver's per-iteration manager cannot drop it; `set_provider` and `/new` (`clear_context`) reset it. Subagents build their own loops -> own ladders (no cross-session leak through the factory's cloned template).
- Both compaction paths complete through `compaction_fit`: `maybe_compact` (turn.rs) and `compact_context` (runtime/agent.rs, manual `/compact` + run-all between items). Each raise surfaces as an `AgentEvent::Error { retrying: true }` transcript note + an `eprintln!`. `CompactOutcome` deliberately gained no variant.

Invariants to preserve: the raised limit stays STRICTLY below the advertised window — `raise_cap()` = `max_tokens - max(compact_headroom_tokens, 1)`, with a `.max(base trigger)` floor so a huge headroom can never shrink the trigger (round-1 review L1 fixed the headroom=0 hole); `hard_ceiling()` itself must stay unfloored (it backs the preflight keep_recent=3 selector); `Fits` clears the ladder only when the result also fits the UN-raised trigger and KEEPS it when only the raise makes it fit (clearing there would re-trigger compaction - the oscillation the doc comment names); `StillOver` leaves the existing bounded abort ladder + `MAX_COMPACTIONS_PER_TURN` in charge, so the burn stays bounded.

Tests: `boundary_session_raises_the_limit_instead_of_aborting` (src/agent/tests.rs, verified red when `MAX_COMPACTION_RAISE_STEPS` = 0) + 9 unit tests in context.rs (raise, cumulative, reset, cap, step budget, shared clone/handle, no-shrink, headroom-0 floor).

Status: branch wt/mnemo, commits 25083e4 (feature + L1 fix) and eb1bae9 (round-2 review PASS, report .coding/reviews/2026-09-27-compaction-raise-fallback-round2.md); NOT yet merged to main.
