+++
title = "discount cross-plan memory recalls — verify plan/branch tags before acting (recall noise)"
created = "2027-01-11"
+++

Recall-noise guardrail (2027-01-20, plan efd1f4c3 clippy-sweep session; user chose "guardrail memory only" over a de-noise pass): the auto-recalled RECALLED MEMORIES block routinely surfaces accurate-but-irrelevant records from OTHER, closed plans during plan execution — observed scores 0.67-0.75. Concrete pattern from the clippy sweep session: the auto-continue-fix plan's records surfaced repeatedly because the session text literally contains "continue" (harness continuation notes) — a pure vocabulary collision; plus closed-plan digests (a stale set_plan_id line pointer from a past layout, a "What-I-read" diff digest, a token-optimizer SPEC, a job-3c review) all matching on plan/commit jargon.

RULE for any executing session (especially lesser-reasoning dispatched sessions that might ACT on a recall): treat any recalled memory with score ≤0.75, or whose subject names a different plan id / branch / date than the active plan, as noise until proven otherwise — verify the record's plan/branch tag against the current plan (current_plan / plan file header) BEFORE letting it steer any code edit or decision. Recalls are hints, not facts about THIS plan.

Durable fix is already queued: backlog 4100f5e3 (memory recall overhaul — working-tier exclusion, stale-session de-ranking; design in PLAN e2b5b686) + backlog c68a8c3b (relevance gate + dismissal feedback + digest integrity). Do NOT delete or supersede the correct-but-irrelevant records themselves (hygiene: delete = junk/dupes only; supersede = contradictions only) — the precedent from the 2026-10-12 de-noise pass (supersede stale digests as "closed history — not recall material") remains available as a fallback if the noise worsens before the overhaul dispatches.
