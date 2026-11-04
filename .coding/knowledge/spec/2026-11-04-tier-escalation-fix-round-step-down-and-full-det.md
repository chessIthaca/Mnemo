+++
title = "Tier escalation, fix-round step-down and full-detail fix prompts"
created = "2026-11-04"
+++

Mnemo2 escalates a worker one tier up (from high to escalate) on any of four triggers: a second failed review round on the same finding, a route-fix round after which its finding is still open, a strike streak, or a high-severity audit finding after a fix round. In off, shadow and suggest modes this is a lead notice suggesting team_resume with a tier; in auto mode an idle worker is switched in the same session and the lead is told. team_route_fix takes an optional `tier` (a ladder id, or `auto`, which classifies the finding with the routing question). Auto never lowers a high-severity or shared-contract finding. A stepped-down round that leaves its finding open returns to the previous tier. Reroute rows are logged with source escalation, fix_downshift or fix_downshift_restore. Every route-fix prompt includes the full detail of every open finding on the worker, not only the routed one. In Mnemo1, a strike streak nudges one tier up, and from high to escalate, unless escalate resolves to the session's current model.
