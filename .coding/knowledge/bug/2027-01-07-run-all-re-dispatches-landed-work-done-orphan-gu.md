+++
title = "Run-All re-dispatches landed work — done-orphan guard at the drain/adoption (6c6966b9)"
created = "2027-01-07"
+++

Symptom: run-all re-dispatched a fully-landed item (35b94671, plan 225e0dad, commit eac25dc) — the session died between its closing-sequence commit and finish, leaving it orphaned InFlight; the main-exit drain and run-start adoption sweep (src-tauri/src/ipc/run_all.rs) requeue orphans to Pending unconditionally → duplicate re-execution. Root cause: only finish marks done; neither requeue site checks landed-work evidence. Fix: orphan_work_landed = plan file all steps checked AND ≥1 commit after the pre-item checkpoint sha (note head) → auto-resolve Done with explanatory note; unverifiable → requeue (safe default). Regression: drain_run_all_on_main_exit_consults_landed_evidence + adopt_orphaned_in_flight_consults_landed_evidence (source contracts).
