+++
title = "OpenSpec spec merges keep one blank line and write AND for extra outcomes"
created = "2026-11-05"
+++

Symptom: Closing a plan whose spec changes modify one requirement adds an extra blank line before every other requirement in the capability's spec.md; each later merge adds another. And a scenario with two outcomes is written as two '- **THEN**' bullets instead of THEN followed by AND. · regression test: modified_delta_keeps_one_blank_line_between_requirements

Full record for plan d590bfad (see .coding/plans/d590bfad.md for the plan file).

regression test: modified_delta_keeps_one_blank_line_between_requirements · path .coding/plans/d590bfad.md · branch wt/mnemo @ 7391bd47 (unmerged — exists only on this branch)
