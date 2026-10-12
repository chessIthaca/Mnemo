+++
title = "job 2a step 2a.4 — the head consults the policy; the tail's workflow-half is app-side (NoPolicy = empty app-tail by design)"
created = "2027-01-11"
+++

Job 2a step 2a.4 landed (plan 339dfe72, wt/mnemo @0843a08-verified bytes): the head consults the policy (ConstitutionHolder::stable_head(policy: &dyn AgentPolicy) — the L4 cache + reload unchanged; both arms = assemble_stable_head(&KernelPromptBlocks::workflow(), c) = today's bytes; the golden workflow_stable_head_bytes_unchanged pins it) and the tail's workflow-half lives in the app: WorkflowPolicy::volatile_tail locks the wf and assembles the same bytes via the moved workflow_section (src/app/workflow_policy.rs, caps-free — CAPABILITIES stays kernel-side); kernel build_volatile_tail(caps, app_tail: &str, memories) = memories + app_tail + CAPABILITIES, byte order pinned by volatile_tail_section_order_pinned.

DESIGNED SEMANTIC (the one behavior note): NoPolicy-built loops (direct constructions — tests, any non-factory path) compose an EMPTY app-tail — the free-agent trajectory per the master spec (2c gives the free agent its own tail semantics); production loops (factory-attached WorkflowPolicy, 2a.2) are byte-identical to pre-2a.4. The two turn-path tests that assert the section reaches the request attach WorkflowPolicy exactly like the factory (tests.rs deepseek_vendor + local_provider tail_rides…) — assertions unchanged.

Regression: volatile_tail_section_order_pinned (order + separator bytes + the literal-backslash-n sentinel — born from the round-1/round-2 escape bugs; see the HOW record 6b0daed5 for the transport lesson). Reviews: 4 rounds on 2a.4 (1H/1L, 1H, 0H/1L, PASS — .coding/reviews/2026-10-12-job2a-step2a4-*).
