+++
title = "slash commands execute in every agent state — never steer"
created = "2026-12-30"
+++

SPEC: slash commands execute in every agent state — never steer (plan 2a3b35d8, commit f556a3d, backlog b47b3f44). Input starting with "/" is control input: handleSend (frontend/src/components/layout/InputBar.tsx) runs the images-guarded parseSlash check BEFORE the steer branch, so /compact, /new, /clear, /model x, /save, /load etc. execute mid-run (running or idle) and never go to the model as steer suggestions. Precedence: freeform/numeric ask_user question interception stays ahead of the slash check; image-bearing input is never a command (prompt/steer); slash commands never enter prompt history. The menu-open path (completeSlashCommand) invokes handleSlashCommand directly — purely for the stale-closure reason, not steer bypass. Every handler is mid-run-safe: /clear /new /load interrupt + drain first; /compact resumes mid-task (StopReason::Compact); /model /provider /save /panel /help are frontend-only. HELP_TEXT documents the carve-out. Regression test: slashCheckPrecedesSteerBranch (InputBar.test.ts source-order pin).
