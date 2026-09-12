+++
title = "premature Planning turn ends park unattended run-all sessions"
created = "2027-01-07"
+++

Symptom: a premature turn end mid-Planning exploration parked the session (auto-continue None-arm gate = Executing|Reviewing only); in run-all mode nothing resumed it → the run halted (live 2027-01-07, backlog a6a7727a). Root cause: the runtime had no dispatch-context signal — covering Planning blindly would self-answer interactive question-waits. Fix: UNATTENDED_PROMPT_PREFIX (core constant) — run_all_prompt builds the dispatched prompt with it; the agent task re-derives `unattended` per Prompt (a plain prompt clears it); workflow_expects_progress(unattended) covers Planning iff unattended; the unattended note says no user input is coming. Regression test: unattended_planning_auto_continues.
