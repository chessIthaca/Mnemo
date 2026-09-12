+++
title = ".coding merge semantics + working-branch options"
created = "2026-08-23"
+++

SPEC: knowledge-as-files: .coding/knowledge/{spec,decision,bug,how}/<date>-<slug>.md — front matter (title/status/supersedes/created) + unbounded md body with [[links]]. Indexer derives budgeted digests (UUIDv5 key, pointer line, superseded_by from front matter, links in data) → git merge converges instances. DBs stay gitignored caches: startup reconciliation + progress dialog; delete = rebuild. wt/* → develop → main; stack.json local; backlog jsonl+UUID ids; typed rows migrate to files.

CORRECTION (2026-08-24): the `wt/* → develop → main` topology stated above is obsolete — the develop tier was removed; it is now `wt/* → main`. Everything else in this record stands. See .coding/knowledge/decision/2026-08-24-branch-topology-collapsed-to-wt-main-develop-rem.md
