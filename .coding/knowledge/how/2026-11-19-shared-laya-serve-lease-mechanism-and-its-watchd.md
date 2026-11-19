+++
title = "shared laya-serve lease mechanism and its watchdog"
created = "2026-11-19"
evidence = "src/host/shared_service.rs and src/host/laya.rs (attach_shared/acquire_on/finish_start); tests second_instance_attaches_instead_of_spawning, stop_releases_only_this_lease, dead_lease_is_pruned, watchdog_stops_service_after_idle, model_mismatch_does_not_attach, unreachable_service_falls_back_to_spawning"
+++

Mnemo shares one `laya-serve` per machine through a generic lease dir implemented in `src/host/shared_service.rs` (crate-root re-export `mnemo::shared_service`): `service.json` {pid, port, token, version, started_ms, models} written atomically (temp file + rename), `lease-<instance pid>.json` per attached instance, dead-pid pruning via `mnemo::process_tree::pid_alive` (the app's `instance_marker::pid_alive` delegates to it), a token `spawn.lock` (60 s stale takeover, 30 s wait then proceed unlocked), a 2 s loopback `http_post_ok` health probe, `detach()` (Windows DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP, Unix process_group(0) — not setsid, no libc dependency), `kill_detached_tree()`, and the watchdog loop.

`LayaManager` (src/host/laya.rs) entry points: `attach_shared` (probe + lease only), `acquire`/`acquire_on(models, Option<port>)` (spawn lock → attach or spawn), `finish_start` (the startup task; re-points the classifier slot if another instance won the race), `await_ready`, `start_server_models`/`start_managed_server` (no port parameter — acquire allocates). `stop`/`Drop` release only this instance's lease and never kill a shared server; `stop_if_generation` ends only its own child (removing the descriptor first when shared).

Shared spawn = detached, no kill-on-close job, only for catalog checkpoint ids; a fine-tuned artifact, a model mismatch, an unreachable descriptor or a missing watchdog binary fall back to the private kill-on-close child. The watchdog is the hidden `mnemo2-mcp laya-watchdog --run-dir <dir> --server-pid <pid>` subcommand (`shared_service::run_watchdog_command`), spawned once per service under the spawn lock; it exits when service.json is gone or names a different pid (so a stale watchdog never kills a newer server) and kills the tree after 60 s with no live lease. `MNEMO2_MCP_EXE` overrides the launcher lookup (otherwise a sibling of the GUI exe — the bundlers ship mnemo2-mcp there).

Tests live in `cargo test -p mnemo --lib host::shared_service` and `host::laya`; they use scripted processes and temp dirs (no Python), and because leases are pid-keyed the tests pass real live stub pids for the fake instances.
