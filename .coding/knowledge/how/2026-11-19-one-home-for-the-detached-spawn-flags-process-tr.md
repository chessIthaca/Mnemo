+++
title = "one home for the detached-spawn flags — process_tree::detach_command"
created = "2026-11-19"
evidence = "src/process_tree.rs:123-176 (detach_command, detached_command, DETACHED_CREATION_FLAGS/DETACHED_PROCESS); src/host/shared_service.rs:420-434 (re-exports); verified by cargo clippy --workspace --all-targets and cargo test --workspace green on the tree that unified them."
+++

In the `mnemo` crate the detached-spawn flags (Windows `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`, Unix `process_group(0)`) are defined in exactly ONE place: `src/process_tree.rs::detach_command(&mut Command)`, with the public pins `process_tree::DETACHED_CREATION_FLAGS` / `DETACHED_PROCESS` (cfg(windows)).

- `process_tree::detached_command(program)` = `Command::new(program)` + `detach_command` (the sccache helpers use it).
- `host::shared_service::detach` and its flag consts are pure re-exports of those items (`pub use crate::process_tree::detach_command as detach;`), so the laya spawn path in `src/host/laya.rs` keeps calling `shared_service::detach`.

Never use `DETACHED_PROCESS`: a console-less child makes every console-subsystem grandchild pop a new Terminal window (user-reported swarm). Never put a machine-shared service in a KillOnCloseJob either — surviving the spawn that started it is the point.
