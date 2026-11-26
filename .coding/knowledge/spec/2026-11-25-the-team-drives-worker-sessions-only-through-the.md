+++
title = "the team drives worker sessions only through the WorkerSession trait"
created = "2026-11-25"
evidence = "src-tauri/src/mnemo2/team/worker_session.rs; tests local_worker_session_is_open_until_closed, local_worker_session_reports_closed_after_close; the acceptance grep matches only worker_session.rs on merged wt/mnemo."
+++

Team code (src-tauri/src/mnemo2/team*) never names AgentSession or calls engine.session(id) for a worker. It holds a SharedWorkerSession (Arc<dyn WorkerSession>) from team/worker_session.rs, with methods id, events, is_open, send, snapshot, set_model, close and close_and_wait; LocalWorkerSession wraps the engine session. pump_worker takes the handle, not the engine. Trap: AgentSession::snapshot() on a CLOSED session returns Ok(empty), not Err, so any site that must treat a closed session as absent checks is_open() first (ask_audit_for_its_report, the post-turn refresh task). Errors cross the seam as display strings. This is the plug point for remote worker sessions (phase 3: a RemoteWorkerSession over mnemo::remote::hub::NodeHandle, whose is_open means "registered on the node").
