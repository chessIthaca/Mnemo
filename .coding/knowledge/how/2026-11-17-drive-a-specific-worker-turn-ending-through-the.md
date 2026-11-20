+++
title = "drive a specific worker turn ending through the real pump in Mnemo2 team tests"
created = "2026-11-17"
evidence = "The parked_turn_end_triggers_no_proof and steer_timeout_completion_triggers_proof tests in src-tauri/src/mnemo2/team/tests.rs pass using this fixture."
+++

Mnemo2 team tests that need a specific worker turn ending (a LimitReached park, a steer-timeout settlement, or any turn whose events the real pump loop must observe) cannot be driven through team.dispatch: the team's session spawn path hard-codes `limits: None`, the pump's steer flag lives inside the pump, and `EventReceiver.rx` is pub(crate) in the mnemo crate so mnemo-app tests cannot inject synthetic events.

The seam that works (see `manual_pump_worker` in src-tauri/src/mnemo2/team/tests.rs):
1. Build the team with a scripted engine (EngineBuilder + ScriptedLlm::new(Vec::new()), config like the nudge fixture).
2. Spawn the session DIRECTLY on the engine with engine.spawn(SpawnOptions { host_tools: Vec::new(), guardrails: None, skip_turn_routing: true, limits: Some(SessionLimits { max_turns: Some(0), ..Default::default() }), ... }) — max_turns: Some(0) parks the very first request with Finished{LimitReached}; a parked turn is the only deterministic non-Completed turn end reachable at this layer.
3. Wire the REAL pump_worker over session.events() using the team's own fields (tests.rs is team::tests, a child module, so private fields are visible), then session.send(SessionCommand::Prompt { .. }) BEFORE inserting the LiveWorker — AgentSession has no Clone; the session moves into the live map.
4. Preset the LiveWorker's steer_pending Arc<AtomicBool> before the prompt to force a steer-held finish; FOLLOW_UP_GRACE is 200 ms under cfg!(test), so the deadline settles within ~400 ms.
