+++
title = "knowledge exports back-dated by stale OS clock — fixed with monotonic artifact floor (d7b2e722)"
created = "2026-09-01"
+++

BUG (backlog d7b2e722, review LOW 2 of .coding/reviews/2026-12-06-reviewer-spawn-only-reviewing-slot.md): knowledge exports always carried filename date + created = "2026-09-01" regardless of the real record date (e.g. the 2026-12-06 decision exported as .coding/knowledge/decision/2026-09-01-models-reviewing-is-reviewer-spawn-only-main-age.md; the 2026-12-05 429 bug digest likewise).

ROOT CAUSE: no hardcoded constant — KnowledgeStore (src/memory/knowledge.rs) stamped the filename date prefix AND front-matter created solely from the OS wall clock (SystemTime::now() via KnowledgeStore::new). This machine's clock is stale/pinned at 2026-09-01 (fresh git commits read the same), so every export was back-dated. The review's "frozen/stale clock or hardcoded default" guess resolved to: stale clock, single-sourced.

FIX (plan c8b3228d): monotonic artifact floor — new stamped_date() = max(wall clock, artifact_date_floor()) where the floor is the newest strict YYYY-MM-DD stem prefix across the four knowledge type dirs AND the sibling .coding/reviews/ dir (review reports carry authored dates ahead of a stale clock). Junk guard: floor candidates need valid month/day ranges and must stay ≤ wall clock + 400 days (a hostile/typo'd far-future filename cannot pin exports). Routing: write(), write_at()'s slug-date fallback, and the supersede successor all stamp through stamped_date(); update() still preserves the record's original created (unchanged). When the clock is ahead of all artifacts (normal case) behavior is identical.

REGRESSION TEST: knowledge::tests::write_dates_from_artifact_floor_when_clock_stale — pins the store clock to 2026-02-01, pre-creates .coding/reviews/2026-12-19-levers-review.md, asserts write() stamps decision/2026-12-19-… + created = "2026-12-19". Fails on pre-fix code (stamps 2026-02-01).
