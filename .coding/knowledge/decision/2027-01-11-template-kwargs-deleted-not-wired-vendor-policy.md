+++
title = "template_kwargs deleted, not wired — vendor-policy direction rolled back"
created = "2027-01-11"
+++

DECISION (2027-01-11, plan d4b951be, backlog be85a2a2, commit b5a1b83 on wt/agenticcoding, provider-comms review finding 4 / BUG 10): policy.rs template_kwargs (GLM clear_thinking: false, Qwen/Kimi preserve_thinking: true) was DELETED, not wired — no request builder ever read the field, neither param name appears in current public vendor docs, and the wire path is gated on the vendor-policy config direction the user rolled back 2027-01-09 (main reset to e0ac5dd). Re-adding vendor template kwargs requires: (1) the vendor-policy direction re-decided, (2) the param names verified against the proxy's actually-accepted params (single live request, provider-errors log checked for 400s). include_params (the sibling field) is NOT dead — the Responses builder hardcodes its include value. Regression guard: policy_source_carries_no_never_sent_template_kwargs (src/provider/policy.rs). Detail: .coding/knowledge/bug/2027-01-11-policy-rs-template-kwargs-must-send-claims-never.md + the amendment in .coding/knowledge/spec/2026-12-21-reasoning-state-continuity-raw-payload-source-of.md.
