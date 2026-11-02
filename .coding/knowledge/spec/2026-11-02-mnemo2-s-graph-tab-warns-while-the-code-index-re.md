+++
title = "Mnemo2's Graph tab warns while the code index rebuilds and reloads when it ends"
created = "2026-11-02"
+++

While a code-index pass runs (CodegraphStatus.indexing or $app.indexing.code running), GraphView.svelte shows "Rebuilding the code index — results may be incomplete" with file progress (code pass done/total, else the symbol count so far). When the pass ends it reloads the status, the overview or current module drill-in, the current search and the selected symbol once; a view error during the rebuild shows "Not indexed yet — retrying when the index is ready" and retries on completion, and a selected symbol that no longer exists after the pass shows a "moved or removed" note instead of the unknown-symbol error. Long passes happen on a first index or after a code-graph extractor version bump, which re-parses every file once.
