+++
title = "automatic memory re-embeds go through reembed_if_needed, resume stale rows and show on the status line"
created = "2026-11-22"
evidence = "tests reembed_if_needed_only_touches_stale_rows, install_embedder_publishes_the_reembed_on_the_store_watch, embeddings_pass_folds_reembed_events"
+++

Every automatic memory re-embed after an embedding-model change calls crate::memory::reembed_if_needed. That covers brain startup, the deferred model load, the Settings rewire, and install_embedder (used by Models → Install, Setup::reembed and the startup load).

What it does:
- It re-embeds only stale rows (embedding NULL, or embed_model/embed_dim different, compared null-safe with IS NOT), so a pass cut off by closing the window resumes.
- It publishes ReembedEvent (started, progress, done, failed) on MemoryStore::reembed_events(), a tokio watch so a late subscriber gets the last state.
- src-tauri/src/mnemo2/indexing.rs folds it into the snapshot's 'embeddings' pass. mnemo2/src/components/IndexingStatus.svelte shows "Re-embedding memories · n/N memories".

Only the manual Memory tab "Rebuild search" (rebuild_search) uses reembed_all on every row. A new automatic re-embed must go through reembed_if_needed, or it is silent and restarts from row 1.
