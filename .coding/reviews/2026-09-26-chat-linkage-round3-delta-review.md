## Verdict: PASS

Round-3 delta verification (plan 822c524a / backlog 9e859618): the round-2 LOW finding is fixed, the replacement sentence's three factual claims are each true against the code, no other delta doc claim is contradicted, and the round really is doc-only.

Reviewed-state: f38e56ba8b1ea1be496a55d9ae44038418e57220
## Scope read (empty-diff rule)

`git log` shows HEAD **is** the base `f38e56ba…` — zero commits since it — so the delta is entirely the uncommitted working tree (`git status`: 4 modified + 4 untracked files, exactly the dispatch-supplied changed-file set). Read from the current tree: the fixed TRADE-OFF paragraph and `chat_linkage_candidate` doc+body (src-tauri/src/ipc/run_all.rs:7211-7272), `adopt_orphaned_in_flight` filter + Phase-3 requeue (run_all.rs:6867-6973) and its sole call site (src-tauri/src/ipc/backlog_cmds.rs:525-554), `resolve_linkage_target` doc+body (run_all.rs:7274-7299), `stamp_backlog_in_flight` doc (run_all.rs:7307-7329), `plan_linkage_allows_done` doc+body (run_all.rs:462-497), `set_plan_id` + `requeue` docs/bodies (src/backlog.rs:701-769), the workflow-tool module-doc paragraph (src/tool/workflow/backlog.rs:56-59), the plan file `.coding/plans/822c524a.md`, and the round-1/round-2 review reports. `git_read(op="diff")` stat matches: 310/18/5 added lines across run_all.rs / backlog.rs / workflow backlog.rs. No code was run (read-only); the dispatcher's fresh `cargo test` evidence (root 2790 passed / 0 failed / 5 ignored, mnemo-app 331 passed / 0 failed, both exit 0) is consistent with everything read.

## 1. The round-2 finding is fixed

run_all.rs:7247-7251 now reads "Before this helper the orphan had no recovery path **within the pointer-less chat flow** — it stranded until a run-all start (the `adopt_orphaned_in_flight` sweep) or a hand-stamp intervened — and a wrong `Done` is recoverable through the legal `Done → Pending` requeue." The overclaim ("no recovery path at all — it stranded forever") is gone. The paragraph is now internally consistent: :7246-7247 cites the sweep and notes it "runs only at run-all start, which a chat-driven session never reaches", the fixed sentence then scopes the stranding claim to exactly that flow, and the recoverable-`Done` argument (the adopted orphan can be requeued if the adoption was wrong) still follows. ✅

## 2. The NEW claims are true against the code

- **(a) The sweep keeps an unlinked `InFlight` item and requeues it to `Pending`** — run_all.rs:6890-6894: the candidate filter is `status == InFlight && Some(i.id) != single && !(plan_id.is_some() && plan_id == active_plan)`; a `plan_id == None` item passes all three clauses (it is neither the single-dispatch pointer nor the active plan's item). Phase 3 (:6922-6949) re-verifies ownership, then `store.transition(id, BacklogStatus::Pending, …)` with the note suffix "orphaned in flight (no live dispatch pointer) — requeued by run-all start" (:6935, :6944). ✅
- **(b) Reached only at run-all start** — the code graph reports exactly one caller: `backlog_run_all`; the call is at backlog_cmds.rs:548, inside the `#[tauri::command] backlog_run_all` "Start the Run-All loop" handler (:531-532), executed before the pending-count check (:541-548). A chat-driven session never invokes that command, so "within the pointer-less chat flow" is the correct qualifier. ✅
- **(c) The `Done → Pending` requeue is legal** — src/backlog.rs:756-769: `requeue` requires a TERMINAL status, and `Done` is in the accepted set (:761-763); it then sets `status = BacklogStatus::Pending` (:767) and clears the note. Exactly the "user's reset of an item the agent marked `Done` that the user disagrees with" the sentence promises. ✅

## 3. No other delta doc claim contradicted (diff-scoped)

- `chat_linkage_candidate` three-case list (:7228-7233) matches the body (:7259-7266): unlinked → candidate; differently-linked → re-link candidate (`plan != linked`); with `current_plan == None`, `Some(linked)` items fail the `matches!` arm, so "only the unlinked case is claimed" is exact. Zero-or-several → `None` (:7267-7269) matches :7235-7238.
- `resolve_linkage_target` (:7274-7299): "exactly one live `InFlight` item whose `plan_id` IS `completed_plan`" — body filter at :7289-7293 enforces all three conjuncts; ambiguity → `None` (:7295-7296). ✅
- `stamp_backlog_in_flight` doc (:7307-7329): pointer-first, `chat_linkage_candidate` fallback for the pointer-less path, idempotent stamp (no-op off `Pending`) with linkage refresh on every entry — consistent with the body (:7330-7348 pointer read) and with round-2's verified body, which is unchanged this round.
- `plan_linkage_allows_done` (:462-497): doc's linkage-provenance claim ("recorded … for BOTH a dispatched item … and a hand-stamped one … via `chat_linkage_candidate`; a pointer-less session is then resolvable by that linkage alone") matches `stamp_backlog_in_flight`'s wiring and `resolve_linked_chat_item` (run_all.rs:5650, called at :5626). Predicate body (:486-496) unchanged. ✅
- `set_plan_id` (src/backlog.rs:701-712): "on the dispatched path and on the pointer-less chat path alike, so the linkage always names the plan the item's status is derived from" — consistent with `stamp_backlog_in_flight`'s linkage refresh; the body (:712-719) never touches status. ✅
- Workflow tool module doc (src/tool/workflow/backlog.rs:56-59): "The tools own the item's STATUS only. The … linkage is recorded by the app layer when the main agent's workflow enters `Executing` … and a pointer-less (chat-driven) session is resolved by that linkage alone" — matches the app-layer seams (`events.rs` forwarder → `stamp_backlog_in_flight`; `on_main_turn_resolved` → `resolve_linked_chat_item`). ✅

## 4. Doc-only + build

The fixed sentence uses plain backticks only (`adopt_orphaned_in_flight`, `Done → Pending`) — no `[...]` rustdoc link forms were added, and the pre-existing `[`plan_linkage_allows_done`]` reference at :7254 resolves to the function at run_all.rs:481. No `#[allow(` was added, no `cfg(windows)`, no shell-based mutation. Both `cargo test` runs green under `#![deny(warnings)]` (root 2790/0/5 ignored; mnemo-app 331/0; both exit 0 — the fresh dispatcher evidence), and the mnemo-app count's +1 over the plan's step-4 record (330) reflects the round-2 regression test added since, not a doc inconsistency.

## Bookkeeping (one-line accuracy)

`.coding/plans/822c524a.md`, `backlog.jsonl`, the two prior review reports, and the new knowledge file match what shipped — the plan's incremental landing notes track the helper's evolution (the step-1 note predates the `current_plan` widening; the shipped shape is fully documented in the code docs), and the Reviews base stamps all correctly name `f38e56ba…`.

## Process remark (one line)

The delta re-shows the round-1/round-2 PASS-verified hunks (the feature was never committed), so they were treated as carry-over per contract and not re-line-reviewed; only the round-2 finding's fix was verified against its code claims.