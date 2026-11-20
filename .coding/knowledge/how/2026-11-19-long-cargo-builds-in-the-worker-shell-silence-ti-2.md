+++
title = "long cargo builds in the worker shell (silence timeout 20 min, hard cap 60 min)"
supersedes = "2026-11-19-long-cargo-builds-in-the-worker-shell-silence-ti"
created = "2026-11-19"
evidence = "src/tool/agent/shell.rs DEFAULT_SILENCE = 20 min and test default_silence_outlasts_a_long_single_crate_compile; worker sccache01 was killed twice after \"Compiling mnemo v2.6.0\" with the 5-minute limit (2026-11-19)."
+++

The worker shell kills a command only after 20 minutes with no output on stdout or stderr, or after 60 minutes in total. An earlier 5-minute silence limit killed healthy builds, because cargo prints "Compiling <crate>" and then nothing while rustc compiles one large crate (mnemo, mnemo-app, Bevy crates take more than 5 minutes cold). A long cargo build or test runs in one foreground shell call; for work that runs alongside other calls, use job_start and then job_wait (no silence limit, 3 h cap). A killed command's result names the reason and keeps the last 16 KiB. Background processes still die when the call ends. Live after a Mnemo2 rebuild that includes the fix; until then, rerun `cargo build --workspace --tests` after a silence kill, since it resumes where it stopped.
