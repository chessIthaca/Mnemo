+++
title = "laya-serve outlived a force-ended Mnemo; it now runs in a kill-on-close job"
created = "2026-11-03"
+++

Symptom: orphaned laya-serve trees (laya-serve.exe, then python.exe) kept the Laya checkpoint in RAM (about 2 GB each) after Mnemo was force-ended or crashed, and an orderly stop left the python children alive. Root cause: only the root process was killed, and Windows does not end children with their parent. Fix: src/process_tree.rs KillOnCloseJob (a Windows Job Object with KILL_ON_JOB_CLOSE, shared with the shell tool's TreeKillGuard); LayaManager (src/host/laya.rs) assigns laya-serve to it and kill_tree kills the job, then the root; on Unix it uses a process group. Regression test: stopping_the_manager_kills_the_server_tree.
