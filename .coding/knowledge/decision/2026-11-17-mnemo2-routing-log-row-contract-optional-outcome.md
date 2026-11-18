+++
title = "Mnemo2 routing-log row contract — optional outcome fields and non-label revert rows"
created = "2026-11-17"
evidence = "Code: src-tauri/src/mnemo2/dispatch_routing.rs (enum + serde attrs), team/routing.rs::outcome_row, team/merge.rs::revert_locked. Tests: mnemo2::dispatch_routing::tests::{outcome_row_carries_merge_commit, old_outcome_row_without_merge_commit_parses, reverted_row_roundtrips, training_readers_ignore_reverted_rows} and mnemo2::team::tests::revert_merge_appends_a_reverted_row; cargo test --workspace and cargo clippy --workspace --all-targets exit 0."
+++

Mnemo2's Laya routing log (DispatchRoutingRow, src-tauri/src/mnemo2/dispatch_routing.rs) is a tagged JSON lines log (`{"event": ...}` in `dispatch_routing.jsonl`). Two contract rules hold:

1. `Outcome` carries `merge_commit: Option<String>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`, filled from the worker's merge record only when status == "merged" (built by `outcome_row` in team/routing.rs). It is omitted, never null, otherwise, so log lines written before the field existed still parse — keep any new optional outcome field in that same shape.
2. `Reverted { ts, worker_id, plan_id, step, revert_commit }` (event tag `reverted`) is appended by TeamRuntime::revert_locked (team/merge.rs) after the record persists. It marks that a merged worker's merge was undone. It is logged in EVERY routing mode: it is a merge-quality fact, not a Laya routing decision, so it is never a training label.

When adding a row variant, the exhaustive `match` on DispatchRoutingRow is the reader checklist — the compiler flags every site. Current readers: laya_settings::summarize_rows (ignore arm), team/persistence convert_legacy_row_tiers (no tier ids), route_eval::outcome_cases and the JSON-based kNN RouteKnn::row_label (both label-driven, `_ => None`). laya_learnings treats log lines as raw text and needs no change. Never gate a non-Laya fact (e.g. a revert) behind the routing-mode switch that gates outcome rows.

Pairing convention: a `reverted` row attaches to the merged `outcome` row with the same worker_id; `step` is 1-based (plan_step + 1). Readers should go through the enum, never re-parse the JSON.
