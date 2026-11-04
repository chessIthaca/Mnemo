+++
title = "Mnemo2 workers can switch model live at their next call"
created = "2026-11-02"
+++

A working Mnemo2 worker's model and effort can change from mission control without a stop. change_model calls AgentSession::set_model (src/headless/session.rs), which builds the provider via build_turn_provider, seeds the served baseline, updates the effort display state (as Mnemo1's config_io swap does), then calls AgentLoop::set_explicit_provider. The call in flight finishes on the old model and the next uses the new; a smaller window compacts first. ModelChanged announces the new model and effort. Audit workers are refused; idle and stopped workers restart as before. Laya learning from overrides is deferred.
