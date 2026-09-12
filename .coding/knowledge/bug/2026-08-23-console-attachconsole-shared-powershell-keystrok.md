+++
title = "console AttachConsole shared PowerShell keystrokes"
created = "2026-08-23"
+++

symptom: release mnemo-app.exe -console from PowerShell: logs print in PS window, PS returns immediately, typed chars only partially echo / REPL unusable.
root cause: AttachConsole(ATTACH_PARENT_PROCESS) shares parent console; GUI-subsystem means PS does not wait and keeps reading keystrokes.
fix: AllocConsole only (never AttachConsole); rewire CONIN$/CONOUT$ as before. Debug console-subsystem early-returns via GetConsoleWindow; fully redirected launches (all std handles valid) skip the window.
regression: attach_console_allocates_own_console_not_parent, attach_console_skips_alloc_when_std_handles_already_valid (src-tauri/src/console.rs)
path: src-tauri/src/console.rs attach_console
