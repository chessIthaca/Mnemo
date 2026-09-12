+++
title = "Planning auto-continue is mode-gated — unattended (run-all) only"
created = "2027-01-07"
+++

Behavior decision (backlog a6a7727a, 2027-01-07): a premature turn end mid-Planning exploration auto-continues ONLY when the agent was dispatched unattended (run-all — the Prompt embedded UNATTENDED_PROMPT_PREFIX, the single-source core constant; the AgentTask re-derives `unattended` per Prompt, so a plain user prompt clears it and a Suggestion never does). Interactive Planning still parks — a deliberate turn end there is usually a question-wait, and auto-continuing would self-answer it. The unattended continue note explicitly says no user input is coming. Complete/Skill are deliberately NOT covered (the run-all gate needs the final turn to end so it can resolve the item). Commits c08c177 + c92c4cf on wt/agenticcoding.
