+++
title = "Mnemo2 team scaling and lead tiny-work cap"
created = "2027-01-11"
+++

User-directed refinements for Mnemo2 Phase 0 (2026-10-24): do not hard-code a three-worker ceiling. Worker concurrency is a per-project setting, separate from budget/rate limits, and Mission Control must remain usable with 8+ workers (prototype should use a larger mock team). The lead tiny-work/review_fix cap is an either/or rule: up to 5 files with at most 10 changed lines per file, OR a single-file edit that may exceed 10 lines. Keep the scope exclusions (no new files, schema/API/dependency changes, or cross-module refactors). This supersedes the previously proposed default of 3 workers and the 20-line/1-file-only cap; update `.coding/design/mnemo2.md` during the Phase 0 implementation plan.
