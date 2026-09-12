+++
title = "finish-capture BUG routes to knowledge file"
created = "2026-08-23"
+++

DECISION: finish capture (impl): BUG: digest for bug_fixing plans routes to .coding/knowledge/bug/<plan-id>.md via KnowledgeStore::write_at (idempotent by plan id) when a KnowledgeStore is wired; PLAN: digest stays an authored row (plans are already files).
