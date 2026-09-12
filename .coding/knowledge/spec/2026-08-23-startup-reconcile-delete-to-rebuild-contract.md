+++
title = "startup reconcile + delete-to-rebuild contract"
created = "2026-08-23"
+++

SPEC: startup reconcile (impl): corpus_is_stale (hash vs derived_index_state + removals; empty state = stale) gates index_derived at startup in build_brain_inner(app: Option<AppHandle>); ReconcileEvent on memory://reconcile (started/progress/done/failed) → wait dialog in App.tsx; silent when in sync. Delete .coding/memory.db → full rebuild from files (regression test deleted_memory_db_rebuilds_everything_from_files). Settings "Rebuild derived index" now covers knowledge too.
