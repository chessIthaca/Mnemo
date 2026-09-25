+++
title = "content-index staleness repairs inline under an adaptive budget ("
supersedes = "2027-01-05-content-index-staleness-repairs-inline-8-files-i"
created = "2027-01-11"
+++

Supersedes the hard <=8 cap (knowledge file .coding/knowledge/decision/2027-01-04-content-index-staleness-repairs-inline-8-files-i.md, already status=superseded; leave that file as-is). Landed 2027-01-25 on branch wt/mnemo under plan 90fab97e for backlog 9201704f.

WHAT CHANGED (two-tier adaptive bound instead of a hard count):
- src/tool/agent/search.rs: STALE_REINDEX_CAP 8 -> 32. It is now the adaptive UPPER BOUND (the count ceiling), not the latency guard.
- src/codegraph/mod.rs: new `pub const STALE_REINDEX_BUDGET: Duration = Duration::from_millis(500)`, shared by both repair paths. `CodeGraph::reindex_stale_files(&[String])` -> `reindex_stale_files(&[String], budget: Duration)`: the pass still holds the atomic indexing-flag claim (compare_exchange + IndexFlagGuard) for its WHOLE duration, and now breaks between files once the budget is spent (`Instant::elapsed`, checked BEFORE each file, so Duration::ZERO is a no-op). Holding the claim for the whole budgeted pass is exactly why the budget lives inside that method rather than in batched re-calls (which would drop and re-claim the flag between batches).

BEHAVIOUR: within the ceiling the stale hit set is re-indexed inline and the query re-served ONCE from the fresh index -> "reindexed N stale file(s) - serving fresh index results", where N is the count ACTUALLY refreshed (try_index no longer assumes all of stale_paths.len(); a budget spent mid-set leaves the rest stale, the re-query still trips, and the caller walks - N can never over-report). Walk conditions (FtsOutcome::Stale, "content index stale for N file(s) - serving tree-walk results"): beyond the ceiling, budget spent mid-set, a busy pass (Ok(0)), a failed re-index, or files re-edited during the retry. Unchanged guards: flag claim/IndexFlagGuard, out-of-root containment, the single re-query, prune-vanished / content-only-upsert / source-reparse branches. Deliberately NOT taken: the optional "kick a background pass on a budget overrun" (the watcher refreshes shortly; the walk is authoritative).

OTHER CALLER: src/tool/agent/codegraph.rs::execute (the graph_search symbol-index sweep) passes the shared budget. Its OWN `STALE_REINDEX_CAP = 8` and its `stale.len() <= STALE_REINDEX_CAP` gate are deliberately UNTOUCHED - the symbol-index ceiling is a separate reviewed contract (plan 55163f1e) whose tests pin the 9-file boundary.

TESTS: new `nine_stale_files_reindex_inline` (src/tool/agent/search.rs) - THE live 2027-01-25 nine-file repro; it fails on the old code and asserts engine: index + "reindexed 9 stale file(s)" + no walk + no doubled note prefix. New `reindex_stale_files_respects_the_budget` (src/codegraph/mod.rs): Duration::ZERO refreshes nothing (both files left stale), a generous budget refreshes both. `stale_above_the_reindex_cap_walks` auto-re-pinned to 33 files (its count derives from `let cap = STALE_REINDEX_CAP`). `stale_index_reindexes_and_serves_fresh_index_results`, `stale_file_beyond_the_display_cap_still_surfaces`, `stale_while_an_index_pass_runs_walks` behaviourally unchanged. Full suite green: 2714 passed / 0 failed / 5 ignored, plus 19 integration + 1 doc-test.

DOCS SYNCED: the STALE_REINDEX_CAP doc, FtsOutcome::Stale, try_index, the search.rs call-site comment, the mirrored search_read.rs comment, docs/FEATURES.md (the "at most 8 stale files" sentence) and PLAN.md.

ROOT CAUSE STILL OPEN: this is a mitigation. The staleness itself comes from the 800 ms watcher debounce (src-tauri/src/main.rs:2066) and - the real bug - .coding/ being indexed but NEVER watched (watcher.rs is_indexable_path); see the BUG memory "content-index staleness root causes". Per the user's 2027-01-25 decision the watcher fix is the NEXT plan.

BRANCH: wt/mnemo; main is ruleset-protected (23755694) so landing goes through the PR path.
