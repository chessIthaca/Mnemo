+++
title = "memory_search refreshes live through a .coding/ file watcher"
created = "2026-11-17"
evidence = "src/host/memory_watcher.rs tests memory_watcher_source_paths, memory_watcher_indexes_a_new_spec_live, memory_watcher_ignores_its_own_index_writes"
+++

How the derived memory index (plans, reviews, knowledge/<type>/*.md, openspec/specs/**, backlog.jsonl) stays fresh while Mnemo runs.
- Startup: brain.rs runs the full reconcile, with its sweeps and HostEvent::Reconcile events, the same as before.
- Live: src/host/memory_watcher.rs (MemoryWatcher, held on Brain::_memory_watcher) watches .coding/ recursively.
  - is_memory_source_path accepts only index sources. It ignores knowledge/INDEX.md, HEALTH.md, dotfiles, *.db*, logs/, openspec/changes/** and board.json, because reacting to the indexer's own writes would loop.
  - After a 1.5 s quiet window, one silent pass runs: claim_busy (retrying, never dropped), then corpus_is_stale, then index_derived. It sends no HostEvent (in Mnemo1 that event would open the reconcile splash) and does not run the startup sweeps.
- A separate watcher from codegraph's GraphWatcher on purpose: that one refreshes only the code graph and the text `search` index.
- openspec/changes/ (archives) is deliberately never indexed. Open spec deltas live in the plan files.
