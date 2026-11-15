+++
title = "long cargo builds in this worker environment (300s shell cap, detached children are killed)"
created = "2026-11-14"
evidence = "Session 2026-11-14 (wt/agent-sidecarB): detached cargo test --workspace died twice with an empty/trickling log; the foreground chunked reruns went green (4564 passed, 0 failed). Misread failure count from the case-insensitive -match check until re-parsed."
+++

The shell tool caps a single command at ~300s (a `Start-Sleep -Seconds 300` poll is killed at 300s), and a background process started by a call is killed when that call returns — `Start-Process ... -PassThru` (with redirects) and even a hidden `cmd.exe /c cargo ... > log` both died mid-build before reaching the tests. So run a heavy build (e.g. `cargo test --workspace`, `cargo clippy --workspace --all-targets`) as repeated FOREGROUND calls: cargo's incremental cache means each rerun resumes and the last one finishes (the workspace debug build completed inside one 300s chunk after warm-up). Capture with `cargo ... *> <gitignored log>` then parse the summary lines. When counting failures with Select-String, remember `-match` is case-insensitive: 'FAILED' also matches "0 failed" — parse `(\d+) failed` instead.
