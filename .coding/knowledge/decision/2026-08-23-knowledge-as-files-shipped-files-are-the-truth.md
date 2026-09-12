+++
title = "knowledge-as-files shipped — files are the truth"
created = "2026-08-23"
+++

DECISION: knowledge-as-files SHIPPED (plan d16b3c22, commit 86fd957, review PASS). Typed records = md files in .coding/knowledge/<type>/ (bare titles, unbounded bodies; budgets on digests only); DBs are caches; backlog jsonl+UUID+union; wt/*→develop→main.

CORRECTION (2026-08-24): the `wt/* → develop → main` topology stated above is obsolete — the develop tier was removed; it is now `wt/* → main`. Everything else in this record stands. See .coding/knowledge/decision/2026-08-24-branch-topology-collapsed-to-wt-main-develop-rem.md
