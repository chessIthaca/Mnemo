+++
title = "never hold the team's records and live locks at the same time in new code"
created = "2026-11-25"
evidence = "src-tauri/src/mnemo2/team/control.rs:196-253; delta audit deltaCore02 on plan 9a2f4b67 step 4"
+++

TeamRuntime has two tokio Mutexes, `records` (Vec<WorkerRecord>) and `live` (HashMap<String, LiveWorker>). The existing team_message delivery path (team/control.rs, message_inner_with_kind) holds `live` across update_worker (takes `records`), persist and session.send, so that order (live then records) is already in use. Any new code that takes records then live deadlocks against it. Remote phase 3 hit this: a "records before live" rule made poll_hosts/reconcile_hosts hang the team whenever team_message ran during a monitor pass. Rule for new code: take one lock, clone what you need, drop the guard, then take the other; never nest them.
