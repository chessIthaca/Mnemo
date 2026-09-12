+++
title = "auto-cont M2 ships gate-pair e2e tests only (exhaustion/reset assertions dropped consciously)"
created = "2026-08-31"
+++

DECISION taken during plan 'Auto-continuation & context safety' closing sequence (plan id 64fe27df): M2 remediation ships with THREE e2e regression tests only (is_workflow_executing_true_after_create_plan / auto_resume_fires_when_workflow_is_executing / auto_resume_does_not_fire_when_workflow_is_planning in src/runtime/agent.rs tests mod). The review's two remaining suggested assertions (streak==MAX_AUTO_CONTINUE parks; Prompt resets streak after exhaustion) were consciously DROPPED after repeated inline-composition failures corrupted working-tree state twice — restoring pristine three-test state was chosen over risking further corruption right before mandatory review+commit.

Rationale disclosed to reviewer explicitly so adequacy-of-three-vs-five is judged knowingly rather than discovered as truncation; reviewer retains authority to raise additional-test demands as formal findings through normal fix-every-finding flow if coverage is judged insufficient.

Related context: M1 wiring fixes landed fine (main.rs + model_resolver config threading), L1/L2 doc rewrites complete incl residual headroom/hard_ceiling doc sites in src/agent/context.rs, L3 README [context] knobs documented in Configuration paragraph + hard-ceiling aggressive-compaction sentence added to Key features bullet. See .coding/reviews/2026-12-16-auto-cont-fixes-review.md once verifier report lands for final disposition.
