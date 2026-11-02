+++
title = "DECISION: knowledge records are dated by the wall clock only"
supersedes = "c8b3228d"
created = "2027-01-11"
+++

KnowledgeStore (src/memory/knowledge.rs) stamps new knowledge records with the wall clock date, nothing else. The former monotonic floor, max(wall clock, newest date prefix on disk), was removed. It had been added after exports carried a stale-clock date (2026-09-01). It then copied one clock-ahead session's future date (2027-01-11) onto every later record, while the real date was 2026-11-01. An investigation found no pinned clock in code: KnowledgeStore::new is the only live clock source and with_clock is test-only. Trade-off accepted: a wrong OS clock misdates only the records written while it is wrong. Existing misdated files are never renamed, because slugs are referenced by supersedes links and plan or review notes. Regression test: write_uses_the_wall_clock_even_when_a_future_dated_file_exists.
