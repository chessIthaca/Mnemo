+++
title = "Hand Claude-agent workers back to Mnemo only when the lead is really gone"
created = "2026-11-24"
+++

Symptom: Mnemo hands Claude Code agent workers back to its own models too eagerly. Two incidents on 2026-11-24, in another project with Claude Code as the lead: - watersim1: the lead had launched the agent but not yet called team_agent_started when its connection dropped briefly. hand_back_due() handed back the "awaiting_launch" worker, which resumed on GLM-5.3 in the same pool checkout. Two writers worked on the same uncommitted files, and the GLM run cost $12. - ovendrill3x3: the Agent launch waited behind the user's permission prompt for about 35 minutes. The lead was treated as gone, the worker was handed back, and the lead's later team_agent_started was refused. · regression test: agent_worker_is_not_handed_back_before_the_grace_period

Full record for plan ee73bd2f (see .coding/plans/ee73bd2f.md for the plan file).

regression test: agent_worker_is_not_handed_back_before_the_grace_period · path .coding/plans/ee73bd2f.md · branch wt/mnemo @ 4283b01e (unmerged — exists only on this branch)
