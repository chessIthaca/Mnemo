+++
title = "shell timeout tests must measure the shell boot, never use a fixed window"
created = "2026-11-19"
evidence = "Observed failures with 1 s/4 s/8 s fixed windows in `cargo test -p mnemo --lib tool::agent::shell`; green (80 passed) with the measured window. Code: measured_shell_boot/silence_window in src/tool/agent/shell.rs."
+++

The shell-tool limit tests in src/tool/agent/shell.rs (chatty/silent/hard-cap/tree-kill) depend on a command's FIRST output landing inside its silence window. A PowerShell boot on the Windows box measured ~1.7 s idle but more than 8 s when the test suite spawns shells in parallel or other builds run, so fixed windows were flaky. Pattern to reuse: `measured_shell_boot(dir)` times `echo boot`; `silence_window(boot) = (boot*4 + 8s).min(30s)`; holding commands sleep/ping for 300 s so the deadline always ends the run first; the kill reason is platform-dependent (chatty grandchild ⇒ hard cap, silent ⇒ silence timeout), so assert either.
