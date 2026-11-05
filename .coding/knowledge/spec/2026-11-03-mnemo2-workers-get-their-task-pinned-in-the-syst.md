+++
title = "Mnemo2 workers get their task pinned in the system head and a short kickoff; read_back is opt-in"
created = "2026-11-03"
+++

src-tauri/src/mnemo2/team.rs: every worker session start or restart (spawn_worker_session via WorkerSessionSpec, audit workers included) sets SpawnOptions.pinned_task = opening_prompt (plan header + step + brief), falling back to the brief. The first user message is KICKOFF, or READ_BACK_KICKOFF when WorkerRecord.read_back is set; kickoff_message() picks it. team_dispatch has an optional boolean read_back, compared on idempotent re-dispatch; the Control Room dispatch passes false. A read-back worker messages the lead with numbered questions or "No questions." and waits. The lead session pins nothing.
