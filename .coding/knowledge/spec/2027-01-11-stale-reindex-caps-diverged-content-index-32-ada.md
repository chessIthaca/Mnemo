+++
title = "stale-reindex caps diverged — content index 32 (adaptive, 06276aa) vs symbol index 8 (codegraph.rs:239)"
created = "2027-01-11"
status = "superseded"
+++

SPEC: the stale-reindex caps are TWO INDEPENDENT constants and have diverged. Content index (`search`/`search_read`): src/tool/agent/search.rs:371 `STALE_REINDEX_CAP = 32`, raised from 8 by commit 06276aa ("Content index: adaptive-budget inline stale reindex", backlog 9201704f) and documented as an ADAPTIVE upper bound whose real latency guard is the 500 ms `STALE_REINDEX_BUDGET` (src/codegraph/mod.rs:209) — it is not a latency guard itself. Symbol index (`graph_search`): src/tool/agent/codegraph.rs:239 is still `STALE_REINDEX_CAP: usize = 8` (backlog 95f21af0), and its doc comment still claims it "mirror[s] the search tool's cap" — false since 06276aa. CONSEQUENCE: a graph_search TOTAL symbol miss with 9+ stale files serves the staleness note instead of repairing (observed 2026-09-26: 61 stale files), while search repairs up to 32; a drift beyond BOTH caps (e.g. 61, a checkout/landing-scale burst) is left to the tree walk / the startup index, so results are still correct but unindexed. Manual repair: `codegraph_refresh` (src-tauri/src/ipc/codegraph_cmds.rs::143); `finish` force-reindexes when a regression-test symbol is missing.
