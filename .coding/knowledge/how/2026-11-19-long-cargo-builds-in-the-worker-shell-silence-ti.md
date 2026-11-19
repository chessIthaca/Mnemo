+++
title = "long cargo builds in the worker shell (silence timeout 5 min, hard cap 60 min)"
supersedes = "2026-11-14-long-cargo-builds-in-this-worker-environment-300"
created = "2026-11-19"
evidence = "src/tool/agent/shell.rs run_command and DEFAULT_SILENCE/DEFAULT_TOTAL at merge e13502d4; audit silenceAud1 found no defects."
status = "superseded"
+++

Since plan 5b85ab61 step 1 (merge e13502d4 on wt/mnemo, live after a Mnemo2 rebuild), the worker shell kills a command only after 5 minutes with no output on stdout or stderr, or after 60 minutes in total. The old fixed 300-second wall clock is gone. A long cargo build/test/clippy that keeps printing (cargo prints a line per crate and per test) finishes in one foreground shell call: no detaching, WMI processes or sleep polling. A killed command's result names the reason (silence timeout / hard cap) and keeps the last 16 KiB of output. Background processes are still killed when the call ends (tree ownership, plan bfe2b09f). For work to run alongside other calls, plan 5b85ab61 step 2 adds job_start/job_wait (in progress, 2026-11-19).
