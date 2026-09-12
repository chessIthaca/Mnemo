+++
title = "Vitest include allow-list guard — unregistered test files fail loudly"
created = "2027-01-07"
+++

Symptom: New frontend test files silently never run: frontend/vitest.config.ts pins an explicit `include` allow-list instead of vitest's default discovery glob, so an unregistered *.test.ts(x) is invisible to the runner — `npm test` reports green while the file's tests are never executed (live case 2027-01-07: src/lib/tauri.test.ts was written, the suite total stayed flat at 71 files / 1007 tests before AND after, and the gap was caught only by noticing the file missing from the run list). · regression test: everyTestFileUnderSrcIsRegistered

Full record for plan f6ea6fc0 (see .coding/plans/f6ea6fc0.md for the plan file).

regression test: everyTestFileUnderSrcIsRegistered · path .coding/plans/f6ea6fc0.md · branch wt/agenticcoding @ 677bd03 (unmerged — exists only on this branch)
