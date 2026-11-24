+++
title = "Mission Control does not refresh a Claude Code agent worker after resume or agent registration"
created = "2026-11-24"
evidence = "team_status detail versus a Mission Control screenshot on 2026-11-24 03:45; the code paths named in the body (team.ts syncTeam/connectTeam, pump.rs events.send, agent_watch.rs agent_started with no events.send)."
+++

Symptom (2026-11-24): after the lead's Claude Code session restarted, an agent worker was handed back as `interrupted`. The lead ran team_resume (tier high, so it runs as a Claude Code agent) and team_agent_started. team_status then showed status working, agent_phase running, model claude-code/opus and live agent activity. Mission Control kept showing the worker as "Interrupted" on its old Mnemo model (Ollama Cloud/deepseek), with its old cost, its old read-back thread and an unlocked model picker, so the window did not follow the agent although the backend did.

Likely cause (unconfirmed): the window's team store (mnemo2/src/lib/store/team.ts) refreshes records only through syncTeam(), which runs on connect and on non-stream events on TEAM_EVENT_CHANNEL ("mnemo2://team-event"). Those events come only from the session pump (src-tauri/src/mnemo2/team/pump.rs). A Claude Code agent worker has no pump session, and the record changes made by resume to a Claude agent (team/control.rs resume), team_agent_started (team/agent_watch.rs agent_started) and agent-watch transcript progress are saved with save_workers but send no TeamEvent. Nothing triggers a resync, so the window keeps its last snapshot. A lead restart, and a failed team_resume while the disk was full (os error 112, "could not write the team ledger"), may contribute.

Check: with Mission Control open, resume or register a Claude Code agent worker and see whether its card updates without reloading the window.

Fix direction (not started): emit a lifecycle TeamEvent, or a "records changed" signal the forwarder turns into a resync, whenever a worker record changes outside the pump (resume or reroute, agent_started, agent-watch progress, finish, stall), plus a store test that the card updates.
