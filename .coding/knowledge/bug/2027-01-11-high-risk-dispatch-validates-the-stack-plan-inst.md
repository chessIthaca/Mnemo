+++
title = "High-risk dispatch validates the stack plan instead of the requested plan"
created = "2027-01-11"
+++

Symptom: high-risk team_dispatch can reject the requested plan because another stale plan is active. Root cause: require_acceptance_scenarios(root) reads the last id from .coding/plans/stack.json and dispatch_locked calls it without the requested PlanRef, so validation is not tied to plan_id. active_plan_document has the same stack-only assumption for audit context. Fix should validate/read the requested worker plan and add a regression where the stack names a different plan.
