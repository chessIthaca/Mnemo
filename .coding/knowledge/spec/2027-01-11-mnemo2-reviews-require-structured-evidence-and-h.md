+++
title = "Mnemo2 reviews require structured evidence and high-risk independent audits"
created = "2027-01-11"
+++

Mnemo2 reviews are lead-led and evidence-gated. Every review records an unanchored diff pass plus nonempty intent, execution, integration, test_quality, and residual_risk. Standard work merges on that evidence. review_risk=high also requires an observable `## Acceptance scenarios` plan section, a fresh read-only audit worker at the exact implementation tip, and nonempty lead audit_adjudication; any fix invalidates the prior review and stale audit. Audit workers exclude all file/shell/git and durable-memory mutations, plan-board changes, and step ticks, and cannot merge. Closing integration re-verifies the original acceptance scenarios. See agent.md, .coding/design/mnemo2.md, and .coding/reviews/team-292139df-20261030-190630.md.
