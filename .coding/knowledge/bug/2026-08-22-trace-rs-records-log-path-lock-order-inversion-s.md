+++
title = "trace.rs records↔log_path lock-order inversion + sync trace commands — fixed, needs rebuild"
created = "2026-08-22"
+++

BUG: ~10s UI freezes (12 watchdog episodes 8/21–8/22, main thread 0% CPU). Cause: trace.rs writer_main held log_path guard across records.lock() + 8MiB file write (if-let temp) = AB-BA inversion vs with_record; sync list/clear/get_trace_logging commands waited on those locks on the main thread. Fix in tree: scoped path clones + async spawn_blocking; regression writer_never_holds_log_path_while_waiting_for_records. Running exe (13:36) predates fix (14:10) — rebuild pending.
