+++
title = "prove Unix-only (#[cfg(unix)]) tests red/green in WSL Ubuntu on this Windows box"
created = "2026-11-20"
evidence = "2026-11-20 v2.7.0 macOS fix: lead WSL run, RED 13 passed/2 failed (pid_alive_treats_a_zombie_as_dead, watchdog_stops_service_after_idle), GREEN 15/15; first attempt failed on the Git Bash path rewrite."
+++

The Windows checks and Mnemo's regression-proof runner never run #[cfg(all(test, unix))] tests (the proof comes back inconclusive with 0 tests, and team_merge then refuses). Prove them in WSL Ubuntu, then record team_check "regression test fails without the fix" with the RED and GREEN evidence.

Setup: user-level rustup in ~/.cargo (run scripts with `bash -l` or source ~/.cargo/env). gcc/cc are present, and reqwest uses rustls. sudo needs a password, so apt packages cannot be installed, and the tauri system deps (webkit2gtk, gtk3 and others) are missing. `cargo test --workspace` is therefore infeasible; use `cargo test -p mnemo --lib -- filterA filterB` (more than one filter must come after `--`).

Recipe:
- Write the commands into a script file instead of a `bash -lc '...'` one-liner: wsl.exe re-joins argv and mangles quotes, parentheses and pipes.
- From Git Bash, prefix MSYS_NO_PATHCONV=1, or /mnt/c/... is rewritten to "C:/Program Files/Git/mnt/c/...".
- Build off drvfs: `git archive <tip>` into ~/dir and extract there, or set CARGO_TARGET_DIR=/tmp/... so the Windows target/ is never touched.
- RED: restore the pre-fix source while keeping the new test. GREEN: run the tip unchanged.
- /tmp is wiped when the WSL VM idles out, so keep the runs back to back.
- A cold build of -p mnemo --lib takes about 2-7 min; WSL builds also queue behind the machine build-slot limiter.
