+++
title = "Ollama Cloud limits stop all glm/deepseek workers together"
created = "2026-11-02"
+++

All glm-5.3 and deepseek-flash tier models run on Ollama Cloud, so its session usage limit (HTTP 429 "session usage limit") fails every running worker at once. Kimi is the only tier model on another provider. Recover with team_resume model Kimi (or wait for credit), then team_message the worker to continue; uncommitted files survive in its worktree.
