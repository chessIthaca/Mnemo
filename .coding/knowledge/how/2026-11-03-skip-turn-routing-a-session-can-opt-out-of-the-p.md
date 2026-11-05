+++
title = "skip_turn_routing — a session can opt out of the per-turn routing gate at spawn"
created = "2026-11-03"
+++

The kernel's spawn seam lets a session opt out of per-turn model routing: SpawnOptions (headless) and SessionOptions (host) carry `skip_turn_routing: bool` (default false, serde-skipped when false), passed through the engine beside pinned_task and stamped on the AgentLoop post-build via set_skip_turn_routing (Relaxed AtomicBool, read back with skip_turn_routing()). route_turn_start returns None for a skipped session right after its is_subagent guard, BEFORE any classifier call, so skipped turns are never classified and never write routing.jsonl decision/outcome rows. Only route_turn_start honors the flag — the step-lane ladder (step_lane_target, turn.rs) and the tool-choice reflex have their own independent guards and never consult it. Mnemo2 worker and lead sessions pass true (they route once at dispatch instead); every Mnemo1 initializer passes false.
