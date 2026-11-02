+++
title = "Engine::shutdown(&self) left the session bus open — integration suite hung"
created = "2027-01-11"
+++

Symptom: `tests/integration/headless.rs::headless::shutdown_leaves_no_tasks` hung forever (the whole integration suite exceeded a 300s cap), because `rx.recv().await` waited on a broadcast channel that never closed.

Root cause: Phase 2 changed `Engine::shutdown` from consuming `self` to borrowing `&self` so a shared `Arc<Engine>` (Mnemo2 lead + team) can shut down. The engine's `bus: broadcast::Sender<SessionEvent>` therefore stayed alive inside the still-live engine, so the channel never reached `RecvError::Closed` even after the pump aborted and the session handle dropped.

Fix (src/headless/engine.rs): `bus` is now `std::sync::Mutex<Option<broadcast::Sender<SessionEvent>>>`; `session_bus()` clones it or returns None; `shutdown(&self)` TAKES the sender (dropping it) after aborting the pump, so subscribers observe Closed; `spawn` on a shut-down engine now fails with code `session_closed` instead of resurrecting an unobservable bus.

Regression test: `tests/integration/headless.rs::headless::shutdown_through_a_shared_arc_closes_the_bus` — holds the engine in an Arc, spawns, shuts down, drops the session, asserts Closed within 5s and that a later spawn yields session_closed. The original `shutdown_leaves_no_tasks` still passes. Integration suite: 85 passed.
