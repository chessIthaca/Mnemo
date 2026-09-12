+++
title = "windows-sys CONTEXT lacks align(16) — GetThreadContext fails 998"
created = "2026-08-23"
+++

BUG (2026-09-04, watchdog stack capture): windows-sys 0.59 x64 CONTEXT has plain #[repr(C)] (align 8) but SDK requires DECLSPEC_ALIGN(16) — GetThreadContext on misaligned buffer fails ERROR_NOACCESS (998), deterministic per binary layout (any eprintln shifts the stack, flips pass/fail). Fix in src-tauri/src/watchdog.rs: #[repr(C, align(16))] struct AlignedContext(CONTEXT). Also: SuspendThread on the CALLING thread deadlocks (deferred self-suspend) — never self-walk; test walks a spin worker + ready flag. 15/15 stable.
