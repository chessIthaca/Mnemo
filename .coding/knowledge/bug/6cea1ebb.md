+++
title = "Specs page keeps config.yaml intact, section headings keep the plan title, finished audits stop looking unfinished"
created = "2026-11-08"
+++

Symptom: Live checks on 2026-11-08 found: Make active on Settings > Specs cut .coding/openspec/config.yaml from 6,264 bytes to 18, losing governed_files and all comments; a '# ' heading in a custom schema section renamed the plan to that heading; the session-start summary counts finished audits as waiting on you; a Stop button shows on finished audits. · regression test: make_active_keeps_governed_files_and_comments

Full record for plan 6cea1ebb (see .coding/plans/6cea1ebb.md for the plan file).

regression test: make_active_keeps_governed_files_and_comments · path .coding/plans/6cea1ebb.md · branch wt/mnemo @ 4afa7403 (unmerged — exists only on this branch)
