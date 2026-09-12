+++
title = "Console REPL owns its window on Windows"
created = "2026-08-23"
+++

Symptom: Release `mnemo-app.exe -console` from PowerShell: startup logs appear in the PS window, PS returns immediately, and typed input only partially echoes / is unusable because the parent shell still owns the shared console. · regression test: attach_console_allocates_own_console_not_parent

Full record for plan b4288ccf-a999-47d8-9ada-4bbeccccfcaa (see .coding/plans/b4288ccf-a999-47d8-9ada-4bbeccccfcaa.md for the plan file).

regression test: attach_console_allocates_own_console_not_parent · path .coding/plans/b4288ccf-a999-47d8-9ada-4bbeccccfcaa.md · branch wt/console-own-window @ 3fa776f (unmerged — exists only on this branch)
