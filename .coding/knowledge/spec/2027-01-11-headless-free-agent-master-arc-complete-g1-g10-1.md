+++
title = "headless-free-agent master arc complete (G1-G10, 11/11, wt/mnemo)"
created = "2027-01-11"
+++

SPEC (the arc-close record; master plan .coding/plans/694f5a97.md,
11/11): the headless-free-agent master arc
(.coding/specs/headless-free-agent.md, rev 6 APPROVED) is COMPLETE on
wt/mnemo — every job landed, individually reviewed to PASS, and
boundary-committed; the G1-G10 proof is demonstrated end-to-end at
the final tree.

THE NINE JOBS (each: step file → per-step sub-plans → review rounds
to PASS → boundary commits; the per-job SPEC/PLAN memory records
carry the detail):

- Job 1 — the host extraction
  (.coding/specs/job1-host-extraction.md; landed @ef65642).
- Job 2a — the AgentPolicy seam (8 steps, 2a.0 goldens → 2a.8
  spend_tags/on_session_end; the real policy delegates in
  src/app/workflow_policy.rs; the routing golden green throughout).
- Job 2b — the app module graph under mnemo::app (the PolicyFactory
  + the wiring move, the kernel decoupling, the LIVE boundary
  test).
- Job 2c — the free agent (NoPolicy's real semantics +
  [models.free] routing live).
- Job 3a — the Engine + HostRuntime + mnemo::headless begins (no
  process-global state, G4).
- Job 3b — the session builders (conversation start/resume, G5).
- Job 3c — the permissions + the public surface (the three policy
  tiers in mnemo::headless).
- Job 4 — the standalone setup + config (the Laya manager's
  Tauri-free core; the config split, G9).
- Job 5 — the final acceptance, the G10 proof
  (.coding/specs/headless-free-agent/job-05-final-acceptance.md;
  delta 10ccef4..e979afa + the docs(5.7) close): the kernel/app/
  headless feature lattice (default = kernel, app, headless;
  headless = ["kernel"]), examples/second_consumer.rs (the
  public-surface-only second consumer), tests/boundary.rs LIVE
  (kernel_builds_without_app_feature + tauri_uses_only_public_api,
  7 banned tokens + the string-aware `//` split), the src-tauri
  public-API route (the curated type re-exports), the Engine core at
  the app's holdouts, the §7 rows 1-35 sweep, the §8 battery, the
  §9 audit; three review rounds to PASS (the g10, -round2, -round3
  reports).

STOP-SPLITS STANDING (filed + tracked, none silently dropped):
#51f3be28 the app-unification (job-5 D5); #5da6c9b2 the clippy gate
leg (job-5 D6 — 183 pre-existing lib lints, none in the job-5
delta); #c96a96ca the example's configured-run CI wiring (job-5
review round 1, H1 — needs a provider seam the example can reach).

FINAL STATE: battery cargo test --workspace exit 0 (re-run after the
round-2 fix; both crates, zero warnings by construction under
#![deny(warnings)]); the boundary scan zero misses; the tree clean on
wt/mnemo. LANDING: merge_to_main is the only path to main (ruleset
23755694 → PR + human review).

The umbrella's own full-arc closing review (the kind=implementation
closing sequence, master step 11) is the final gate on this record.
