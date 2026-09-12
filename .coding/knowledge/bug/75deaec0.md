+++
title = "Steer mid-flight never orphans tool calls + auto-continue covers Reviewing"
created = "2026-12-31"
+++

Symptom: In reviewer-finish-resumed turns, a user "c" nudge arriving while a tool call is in flight soft-stops the turn and silently skips the emitted call (finish never executes — workflow state unchanged), the orphaned call's result slot renders the steer text ("c"), and the agent parks until the user nudges again (auto-continue covers only Executing, not Reviewing). · regression test: auto_resume_fires_when_workflow_is_reviewing

Full record for plan 75deaec0 (see .coding/plans/75deaec0.md for the plan file).

regression test: auto_resume_fires_when_workflow_is_reviewing · path .coding/plans/75deaec0.md · branch wt/agenticcoding @ 17fc6c2 (unmerged — exists only on this branch)
