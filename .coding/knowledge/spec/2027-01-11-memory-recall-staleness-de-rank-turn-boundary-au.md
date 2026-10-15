+++
title = "memory recall staleness de-rank + turn-boundary auto-consolidation — landed (084978b3)"
created = "2027-01-11"
+++

Landed 2027-01-26, plan 084978b3 / backlog 4100f5e3, branch wt/mnemo, commits 0e1c14f + 4255f15, review rounds: FINDINGS (0H/2L) → r2 delta PASS (.coding/reviews/2026-10-15-084978b3-memory-recall-overhaul-review{,-r2-delta}.md).

Mechanism (all in src/memory/mod.rs): (A) working-tier recall exclusion was already correct (MemoryFilter.exclude_working default-true, enforced at 3 SQL sites) — closed as verified-no-change. (B) Staleness de-rank in the shared scoring loop (~:1620): score = sim×sim_weight + decayed_strength×0.3 + keyword 0.3 + tier + class − staleness_penalty, where penalty = 0.25 × min(1, (age_days − grace)/23), grace = search_config.decay_half_life_days (tracks the live tau after LOW-2; at default 7d the endpoints stay 7d/30d). Root cause it fixes: closed-session digests ride sim+keyword+tier long after their decayed-strength term hits ~0 (peek paths never bump last_accessed_at), so only an explicit staleness factor can demote them. (C) Turn-boundary auto-consolidation: record_tool_event calls maybe_consolidate_turn_boundary fire-and-forget; TURN_BOUNDARY_GAP_SECS = 300 detects the boundary in-store (no turn metadata on events), pre-gap rows compressed via consolidation::synthesize_synthetic (pub(crate), same synthetic path as the manual tool) then deleted by delete_working_for_session_before (JSON1 json_each session match + created_at <= now−300 bound).

Invariants: grace never penalizes fresh records (age <= tau exempt); the live turn's rows survive (boundary = now−300 < created_at of the just-written row); in-memory ranking only — stored rows untouched; actively-recalled records stay in grace (batch_access refreshes last_accessed_at).

Regression tests (tests.rs): stale_record_ranks_below_fresh_of_equal_relevance (B), turn_boundary_auto_consolidates_previous_turns_working_rows (C), staleness_grace_follows_the_configured_decay_tau (LOW-2). Acceptance evidence: this session's injected closed-session digests scored 0.87–1.14 pre-fix, 0.66–0.68 after (each −0.25). Residual (queued backlog c68a8c3b): a relevance floor for injection — de-ranked digests still appear at low scores when nothing fresher outranks them.
