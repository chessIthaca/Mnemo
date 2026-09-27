+++
title = "chat-driven backlog items reach Done via plan linkage (plan 822c524a)"
created = "2027-01-11"
+++

Behavior fix landed as `fix: resolve chat-driven backlog items by plan linkage` (commit 696e394, branch wt/mnemo; plan 822c524a, backlog 9e859618; reviews: .coding/reviews/2026-09-26-chat-linkage-resolution-review.md 0h/2l, -round2-delta-review.md 0h/1l doc, -round3-delta-review.md PASS).

Problem: a pointer-less (chat-driven) session recorded NO item↔plan linkage. The workflow tools (src/tool/workflow/backlog.rs execute) carry no plan context, and linkage was only stamped on the dispatched paths at create_plan (src/backlog.rs:609-612), so an item whose plan reached Complete kept its non-terminal status — breaking the Done ⇔ plan-Complete identity (src/backlog.rs:606-608).

Shape (src-tauri/src/ipc/run_all.rs): reuse the existing main-agent Executing forwarder seam (src-tauri/src/ipc/events.rs:499 → should_stamp_in_flight → stamp_backlog_in_flight). Its candidate lookup is now `pointer.or_else(|| chat_linkage_candidate(&items, top_plan_id))` — the dispatch pointer still short-circuits, so dispatched-path behavior is bit-identical. `chat_linkage_candidate` claims a live InFlight item that is unlinked, or linked to a SUPERSEDED plan (plan != current_plan); zero-or-several candidates → None. `resolve_linkage_target` resolves strictly by linkage and refuses ambiguity/dead items. A new pointer-less arm in on_main_turn_resolved (after the run-all/single-dispatch paths) resolves the linked item by linkage alone through run_all_success_disposition → flip_done_if_linked.

Invariants: `plan_linkage_allows_done` keeps `(None, Some(_)) => false` — an item with no plan linkage never flips to Done; the status guard requires InFlight. Accepted tradeoff (documented in the helper): a stale unlinked InFlight item left by a dead session is indistinguishable from this session's hand-stamp, so the next pointer-less plan may adopt it; recovery is the run-all-start `adopt_orphaned_in_flight` sweep (requeues unlinked InFlight orphans to Pending) or the legal Done → Pending requeue (src/backlog.rs:756-769).

Regression/contract tests: chat_stamped_item_is_linked_then_resolves_by_linkage, chat_linkage_candidate_relinks_a_superseded_plan, linkage_target_refuses_ambiguity_and_dead_items, plan_linkage_allows_done_matrix, on_main_turn_resolved_is_plan_tied. Verified: root cargo test 2790/0, cargo test -p mnemo-app 331/0, exit 0.
