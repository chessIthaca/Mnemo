+++
title = "merge_to_main two-hop ceremony + reconcile kick"
created = "2026-08-23"
status = "superseded"
+++

DECISION: merge_to_main is two-hop (feature → develop → main); post-merge index reconciliation is automatic at next open (startup reconcile), in-session via Settings → Memory → "Rebuild index from files" (skill can't run the Rust indexer). See .coding/skills/merge_to_main.toml.
