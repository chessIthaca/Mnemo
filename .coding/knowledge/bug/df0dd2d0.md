+++
title = "Reopened Mnemo2 lead answers new prompts"
created = "2027-01-11"
+++

Symptom: After a project is reopened (app restart or project switch back), Mnemo2's in-app lead restores its saved transcript but ignores every new prompt: send_prompt returns Ok, yet no LLM request is made and no session events are emitted, so the lead looks frozen. Worker sessions resumed the same way accept messages normally. · regression test: a_reopened_lead_answers_a_new_prompt

Full record for plan df0dd2d0 (see .coding/plans/df0dd2d0.md for the plan file).

regression test: a_reopened_lead_answers_a_new_prompt · path .coding/plans/df0dd2d0.md · branch wt/mnemo @ d7ed239 (unmerged — exists only on this branch)
