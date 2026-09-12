+++
title = "Fix Enter not running slash commands (/compact, /new)"
created = "2026-12-30"
+++

Symptom: Typing a slash command like /compact (or /new) and pressing Enter never sends it — Enter re-completes the text to "/compact " (trailing space) and every further Enter re-completes identically; only the Send button runs the command. /clear, /panel, /help work via Enter. · regression test: resolvesExactLoopStates

Full record for plan cb91d6ea (see .coding/plans/cb91d6ea.md for the plan file).

regression test: resolvesExactLoopStates · path .coding/plans/cb91d6ea.md · branch wt/agenticcoding @ 9f5b22d (unmerged — exists only on this branch)
