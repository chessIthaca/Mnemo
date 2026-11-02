+++
title = "Delete old workers: instant dialog with progress, faster removal, failures don't stop the rest"
created = "2026-11-02"
+++

Symptom: In mission control, "Delete old" takes a long time before its dialog appears, so the button can be pressed twice and open the dialog twice; the deletion itself is slow; and when one worker's deletion fails, the remaining workers are not deleted. · regression test: delete_workers_continues_past_a_failed_worker

Full record for plan 9cd9d1fc (see .coding/plans/9cd9d1fc.md for the plan file).

regression test: delete_workers_continues_past_a_failed_worker · path .coding/plans/9cd9d1fc.md · branch wt/mnemo @ d97dea78 (unmerged — exists only on this branch)
