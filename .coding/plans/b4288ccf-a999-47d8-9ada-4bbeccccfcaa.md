# Plan: Console REPL owns its window on Windows

## Goal
Make `-console` typing work on Windows release builds by giving the REPL its own console window instead of AttachConsole-sharing PowerShell/cmd (which steals keystrokes because GUI-subsystem launches don't block the parent).

## Kind
bug_fixing

## Context
User confirmed after the prior attach+stdio-rewire fix: startup logs print, but bare `.\target\release\mnemo-app.exe -console` from PowerShell still cannot take typed input cleanly (few chars, cursor wrong). Root cause: release is `windows_subsystem = "windows"`, so PowerShell does not wait; `attach_console()` currently `AttachConsole(ATTACH_PARENT_PROCESS)` and shares the parent console, so PowerShell keeps reading keystrokes. Fix: when no console is attached yet, `AllocConsole()` only (skip AttachConsole). Debug console-subsystem builds still hit `GetConsoleWindow() != null` and return early (in-terminal). Existing regression `attach_console_rewires_std_handles_in_consoleless_child` stays valid (detached child → AllocConsole + rewire). Update `attach_console` docs + README console-mode note.

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
Release `mnemo-app.exe -console` from PowerShell: startup logs appear in the PS window, PS returns immediately, and typed input only partially echoes / is unusable because the parent shell still owns the shared console.

## Regression test
attach_console_allocates_own_console_not_parent
