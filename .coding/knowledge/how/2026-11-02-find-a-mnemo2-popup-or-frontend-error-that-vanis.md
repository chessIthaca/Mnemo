+++
title = "Find a Mnemo2 popup or frontend error that vanished — read .coding/logs/mnemo2-ui.jsonl"
created = "2026-11-02"
+++

Every Mnemo2 toast (info or error) and every uncaught frontend exception or unhandled rejection is appended as JSONL {ts, level, source (toast|window|promise), message, route, worker_id?, detail?} to <project>/.coding/logs/mnemo2-ui.jsonl (gitignored), or <config_dir>/logs/mnemo2-ui.jsonl when no project is open; the file rotates to .1 past 2 MB. Written by src-tauri/src/mnemo2/ui_log.rs via mnemo2/src/lib/uiLog.ts (withToast logs each toast; installUiErrorLogging in main.ts). Error toasts stay at least 12 s or until dismissed, pause on hover and have a Copy button. When the user mentions a popup they could not read, read the tail of this file.
