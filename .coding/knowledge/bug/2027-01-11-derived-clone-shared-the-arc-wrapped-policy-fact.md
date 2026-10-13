+++
title = "derived Clone shared the Arc-wrapped policy_factory slot — a tier-1 spawn leaked into the base (job 3c F1, fixed ecfd939)"
created = "2027-01-11"
+++

Symptom (job-3c review round 1, 2026-10-13): after ANY tier-1 named-policy spawn, every later non-tier-1 spawn of the same headless Engine silently built under the named factory (its tools, gates, and head); concurrent tier-1 spawns raced the one slot.

Root cause: AgentLoopFactory (src/agent/factory.rs) is #[derive(Clone)] and the D4 tier-1 arm cloned it then called set_policy_factory(named) — but the policy_factory field was Arc<RwLock<Option<Arc<dyn AgentPolicyFactory>>>>, and a DERIVED clone shares the Arc. The "per-spawn clone" write therefore hit the shared base slot (the arm comment's "the shared factory is never mutated" was false for that field). The other Arc-wrapped seams (provider, context_manager, spawner, descendant_tracker) are shared ON PURPOSE — live settings swaps must reach every clone's build.

Fix (commit ecfd939 on wt/mnemo): policy_factory is now PolicyFactorySlot — a snapshot-clone newtype (a plain RwLock whose manual Clone seeds a FRESH lock from the current read; factory.rs:248-276) with set/get accessors; the three build-time consumption sites read an owned Option. Per-spawn tier-1 sets stay clone-local and die with the clone.

Regression: tier1_named_factory_isolated_to_its_spawn (tests/integration/headless_kernel.rs) — sentinel identity-only heads A → B → plain (None, None); leg 3 asserts the plain head carries no sentinel. Red-checked: simulating the write onto the shared base fails leg 3 with "SENTINEL-B identity" in the plain session's head.

Lesson: Arc-wrapping a struct's fields to make it Clone turns every derived clone into a share — any field whose writes must stay clone-local needs a snapshot-clone newtype (or a manual Clone), never a shared Arc.

Paired low finding F2 (documented; job-5 forward): Engine::register_policy's CONTRACT doc states the build runs with PolicyFactoryCtx.prompt = None and a refusing factory (KernelError::PromptRequired) does NOT fail the spawn — the infallible 2b build's factory-level fallback attaches the GATED NoPolicy::with_coding_default(); failing the spawn on refusal needs a fallible build path (job 5's follow-up).

Round-2 verdict: PASS (.coding/reviews/2026-10-13-job-3c-permissions-review-round2.md) — both findings verified resolved, no new findings.
