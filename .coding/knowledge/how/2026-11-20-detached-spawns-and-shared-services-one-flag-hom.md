+++
title = "detached spawns and shared services (one flag home; lease dirs under ~/.mnemo/run)"
created = "2026-11-20"
evidence = "src/process_tree.rs, src/host/shared_service.rs, src/host/laya.rs, src/host/models_host.rs (merged 2026-11-20)."
+++

Detached-spawn flags live in ONE place: src/process_tree.rs detach_command(&mut Command) (Windows CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP, Unix process_group(0)), plus detached_command(program); shared_service::detach is a re-export. Never use DETACHED_PROCESS: a console-less child makes every console-subsystem grandchild pop a new terminal window. Never put a shared service in a KillOnCloseJob: surviving the spawner is the point. Reap a detached Child you drop (a reaper thread calling child.wait()), or it stays a Unix zombie; pid_alive treats zombies as dead.

Shared services (laya-serve, mnemo-models) use src/host/shared_service.rs:
- ~/.mnemo/run/<service>/service.json, written atomically and user-only;
- lease-<instance pid>.json per attached instance, with dead-pid pruning;
- a spawn lock: re-read the descriptor under the lock and attach to a winner, never spawn a second;
- idle exit after 60 s with no live lease (a watchdog subcommand for Laya; built into mnemo-models).
An instance's stop or Drop only releases its lease; it never kills a shared server. mnemo-models uses a uuid-v4 bearer token with a constant-time compare. MNEMO2_MCP_EXE and MNEMO_MODELS_EXE override the binary lookup (otherwise a sibling of the exe, then <config>/bin).
