+++
title = "Enter never ran /compact — slash-menu completion loop"
created = "2026-12-30"
+++

Symptom: Enter on /compact (or /new) never ran it — Enter re-completed to "/compact " forever; only the Send button worked. Root cause: InputBar.tsx completeSlashCommand's immediate-run arm hardcoded clear|panel|help — compact/new fell to the arg-completion default (trailing space), and handleKeyDown's hasArgs trims that space so Enter never reached a run path. Fix: data-driven SlashCommandInfo.takesArgument + resolveArglessCommand(); Enter runs resolved argless commands via completeSlashCommand (direct invocation, bypassing steer interception). Regression: frontend/src/lib/slash.test.ts (resolveArglessCommand suite) + InputBar.test.ts source pins. Plan cb91d6ea.
