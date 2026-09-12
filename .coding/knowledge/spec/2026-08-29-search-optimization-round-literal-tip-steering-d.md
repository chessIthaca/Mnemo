+++
title = "search optimization round — literal-tip steering, definition-prefix nudge, pruned walk, gap-driven memory prompt"
created = "2026-08-29"
+++

SPEC (2026-09-15, plan 812e1f0a, commit 7fb4141 on wt/agenticcoder, round-1 FINDINGS 0-high/3-low all fixed, round-2 verify PASS): search/memory layer changes.

(1) search_read literal single-line queries ride the FTS content index (engine: index, bm25-ranked — parity with search; the routing landed earlier in 2da09da, this round added the missing B1-parity pins regex_query_walks_even_with_index + fallback_literal_walks_even_with_index; broken-regex fallback, regex mode, and \n-spanning patterns still walk).

(2) literal_tip (search.rs, pub(crate), shared with search_read): fires when NOT literal:true AND the pattern is metachar-free + tokenizable + single-line (is_index_eligible_regex) AND the content index is populated (content_coverage, two COUNTs) AND the symbol nudge did NOT fire (symbol steering wins) → merged line-1 note "TIP: pattern has no regex metacharacters — literal:true would use the content-index engine (one indexed lookup instead of a tree walk)". Advisory only, never auto-routed (D1: FTS phrase ≠ regex semantics); counted as the 6th steering marker LiteralTip (fired-only — targets &[] because observe_call sees only tool names; detect tool-scoped search|search_read + first line). merged_note is now a 3-way join (fallback; nudge; tip) — in practice at most one component is ever Some.

(3) symbol_nudge widened via strip_definition_prefix (modifiers pub / pub(...) / async / unsafe / extern, then ONE keyword fn/struct/enum/trait/impl/mod/const/static/type/class/interface/function/def; remainder must be a bare identifier): prefixed note reads "the symbol '{name}' (from pattern '{pattern}') is an indexed symbol — graph_context(id=...) ..."; the bare-identifier sentence stays byte-identical (2026-09-15 spec pin); both variants contain SEARCH_NUDGE_MARK so one metric covers both.

(4) walk_searchable (search.rs, pub(crate)): recursive PRUNED descent — ignored dirs are never enumerated (was ~107k stat calls under node_modules/target/.git/dist); skipped now counts pruned DIRECTORIES ("skipped N ignored dirs"); per-directory sorted DFS (glob-crate order parity); glob matched on project-relative '/'-separated paths (same key form as try_index — walk ≡ index scoping); should_search kept as defense-in-depth; only root-read failure is Err (callers degrade to empty, old glob parity). Both tools' walks rewired; symlinked dirs no longer followed (aligns walk with the index coverage walker — codegraph/walk.rs uses the same is_ignored_component + should_search).

(5) prompt.rs TOOL_STRATEGY: memory_search mid-session clause is now GAP-DRIVEN ("search memory only when the auto-recalled hits and the plan rider do not cover the question ... the search is for a GAP, not a reflex"); both MANDATORY triggers verbatim, no reorder (pinned tests untouched); the 2026-09-15 "wait ~2 weeks" DECISION superseded (file-level: status flip + successor file 2026-08-29-recall-rider-vs-explicit-memory-search-mid-sessi.md).

Known residual (logged for the follow-up round): memory_update's single-file knowledge reindex cannot resolve `supersedes` against records outside the touched file (indexer.rs build_knowledge_metas, tool/memory/mod.rs:301 passes only the updated rel) — a successor's digest self-heals at the next startup content-hash reconciliation.
