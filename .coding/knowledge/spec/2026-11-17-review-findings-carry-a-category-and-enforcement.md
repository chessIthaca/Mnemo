+++
title = "review findings carry a category and enforcement; recurring categories become backlog proposals"
created = "2026-11-17"
evidence = "findings_ledger.rs tests third_worker_with_category_adds_one_backlog_item, review_category_does_not_propose, append_new_writes_once_per_finding"
+++

team_review findings take optional `category` (kebab-case slug, 3-40 chars) and `enforce` (code | build | test | review), validated in phase3::validate_findings.

Ledger: every newly recorded or recategorized finding appends a row to `.coding/reviews/findings.jsonl` (committed, union-merged; src-tauri/src/mnemo2/team/findings_ledger.rs). Written after the review persists; a write failure is only logged.

Recurrence: when a category reaches exactly 3 DISTINCT workers and its latest enforce is code/build/test, propose_recurring adds ONE backlog item ending `[finding-category:<category>]` (deduplicated by that marker).

Review answer: the team_review answer lists the categories in use and one line per category at or above 3.

Lead practice: tag every finding with a category and enforce, so recurrence is visible.
