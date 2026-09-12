+++
title = "2026-08-23 hangs = main-thread futex wait (ntdll!RtlWaitOnAddress); caller frame still missing"
created = "2026-08-23"
+++

BUG (partial root cause, 2026-08-23 9:12 hang, log hang-1787490751792.txt): stack capture worked — main thread blocked in ntdll!RtlWaitOnAddress→NtWaitForAlertByThreadId (rvas 0x163FD4/0x30543, ntdll 0x7FFDA45A0000 v26100.8972) = Rust futex/park family (std thread::park, channel recv, parking_lot) — deadlock/missed wakeup. NOT WebView2 COM, NOT busy loop (0% CPU), NOT trace locks (fixed). Only 2 of 64 frames walked — caller unnamed. Follow-up: fix walk early-stop + per-frame module names. Method: same-boot module bases + ntdll export-table parse.
