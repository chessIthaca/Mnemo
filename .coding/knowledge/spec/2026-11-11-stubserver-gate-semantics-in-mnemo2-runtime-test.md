+++
title = "HOW: StubServer gate semantics in mnemo2 runtime tests — dropping the sender releases the turn"
created = "2026-11-11"
evidence = "src-tauri/src/mnemo2/runtime/tests.rs StubServer::start gate wait (`let _ = gate.await;`); compile error E0382 during commit f9405326's battery; src/agent/turn.rs:2206 comment; 21-run green battery log .coding/tmp/flaky-switch-battery.log"
+++

Mnemo2's test StubServer (src-tauri/src/mnemo2/runtime/tests.rs, StubServer::start) waits a gated turn with `let _ = gate.await;` and IGNORES RecvError — so a DROPPED oneshot sender releases the held reply immediately, and a sender kept alive keeps the turn held until sent. Consequences when gating stub turns:
- `drop(release_x)` mid-test does NOT keep the request held — it un-holds it (race reintroduced).
- Binding as `_release_x` holds to end of scope — a worker awaiting that reply stalls past `wait_for_worker_status(Completed)` and panics on timeout.
- Oneshot `Sender::send` consumes `self`: using one sender in a mid-test `if` AND a later end-of-test branch fails NLL (E0382) — fold both uses into ONE `let release_x = if path { ...; sender_a } else { sender_b };` binding so each sender moves along exactly one path (pattern from commit f9405326, change_model_to_a_smaller_context test).
Also verified there: when a deferred compact-then-swap is skipped (conversation fits), the kernel emits NO events at all (no CompactStarted, no ModelSwapSummarized — src/agent/turn.rs ~2206 "swaps silently"), so the pump never sets worker.compacting and the test's compacting poll passes immediately while the model-b request is held.
