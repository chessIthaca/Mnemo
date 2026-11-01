+++
title = "Reopened Mnemo2 lead loses injected provider on model restore"
created = "2027-01-11"
+++

Root cause: restoring lead.json passes the saved model as SpawnOptions.model. Engine::spawn installs that as a forced model, so AgentLoop's per-turn ConfigModelResolver replaces the EngineBuilder-injected provider with a newly configured client; the resumed prompt reaches request_stream but the ScriptedLlm is never called and the turn stalls. Fix: Engine tracks provider injection and keeps that transport for model-pinned sessions; configured resolution is unchanged without injection. Regression coverage: src-tauri/src/mnemo2/runtime.rs::tests::a_reopened_lead_answers_a_new_prompt and tests/integration/headless.rs::model_pinned_resume_uses_the_injected_provider.
