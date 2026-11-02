+++
title = "mnemo2 attach_requires_confirmation_for_a_live_incumbent races its timeout helper"
created = "2026-11-02"
+++

`src-tauri/src/mnemo2/project.rs:605 attach_requires_confirmation_for_a_live_incumbent` is flaky on Windows.

Mechanism: the test spawns its "live incumbent" helper with `cmd /c timeout /t 5` (Unix branch: `sleep 5`), writes an instance marker for that pid, then at :622 asserts `attach(root, false).conflict_pid == Some(child.id())`. `cmd /c timeout /t 5` does NOT wait 5 s when its stdin is not a usable console — it prints "ERROR: Input redirection is not supported, exiting the process immediately" and exits in ~163 ms (measured directly). The helper can therefore be dead before `instance_marker::pid_alive` probes it, so `attach` sees no conflict and returns `conflict_pid: None`.

Observed signature: `panicked at src-tauri\src\mnemo2\project.rs:622: left: None, right: Some(<pid>)`.

Rarity: passes 8/8 in isolation and most full-suite runs (240 passed), so it reads as green until it loses the race on a loaded machine. Reproduce with a repeated loop of the full `cargo test --features mnemo2 --lib mnemo2::` suite.

Correct fix (not a retry): spawn a genuinely long-lived helper (e.g. a Rust helper that blocks on a stdin/channel read), or assert against the marker file rather than a spawned process's liveness. Any worker whose scope touches project.rs / instance_marker.rs should be aware; unrelated steps should not be blamed for a red run that names this test.
