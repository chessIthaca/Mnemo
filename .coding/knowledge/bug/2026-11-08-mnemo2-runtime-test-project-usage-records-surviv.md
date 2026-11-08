+++
title = "mnemo2 runtime test project_usage_records_..._survives_restart_and_resume failed once under heavy load; cause unknown"
created = "2026-11-08"
+++

Symptom: src-tauri/src/mnemo2/runtime/tests.rs `project_usage_records_lead_and_worker_usage_and_survives_restart_and_resume` failed once in a full `cargo test -p mnemo-app --features mnemo2 --lib` run while several suites, builds and Mnemo2 workers ran on the same Windows machine. The panic text was lost (the run's output was grep-filtered). It passed alone, in reruns, in 3 full lib runs in parallel, and in 36 targeted runs, 12 at once (2026-11-08). Status: unresolved, deliberately left unchanged — a blind timeout bump would be a guess.
Candidates (unconfirmed): `next_event` allows 3 s per lead event during `drain_turn` of the lead's usage turn; `wait_for_worker_status` allows 10 s (500 x 20 ms). Usage accounting itself looks sound: the pump records Usage events before the Finished event flips the worker to Completed (src-tauri/src/mnemo2/team/pump.rs).
Next time it fails: keep the full test output (save it to a file, do not grep it down) so the panic message names the failing wait or assertion, then fix that point.
