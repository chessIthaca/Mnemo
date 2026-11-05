+++
title = "reach a Mnemo2 worker's AgentLoop from runtime.rs tests (trace identity over engine.host().agent_loops)"
created = "2026-11-03"
+++

To inspect a Mnemo2 worker's or lead's AgentLoop from a test in src-tauri/src/mnemo2/runtime.rs (mod tests): TeamRuntime.live is PRIVATE to team.rs, so runtime-side tests cannot walk team.live. Instead get the engine with lead.engine_handle().await, lock engine.host().agent_loops, and find the loop by its stamped trace identity — worker loops carry identity.worker_id == Some(<dispatch id>), the lead loop carries actor_kind == TraceActorKind::Lead. The identity is stamped at spawn (before registration), so the loop is findable as soon as dispatch_with_id returns. Drop the loops guard before any later await that might take engine-side locks.
