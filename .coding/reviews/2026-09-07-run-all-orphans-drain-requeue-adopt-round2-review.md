## Verdict: FINDINGS (0 high, 1 low)

Round-2 verification of commit 50f7d36 (HEAD on wt/agenticcoding; tree verified clean — `git diff HEAD` empty, so every file read reflects the committed tree). Both round-1 findings are genuinely resolved in the committed tree and the fixes introduced no code issues; ONE new LOW documentation finding from the LOW-2 fix's supersede mechanics (incomplete forward pointers — a live decision record still routes readers to the old drain semantics with no in-body warning). Details in the sections below.

### Round-1 LOW-1 (TOCTOU in `adopt_orphaned_in_flight`) — RESOLVED, reorder sound

**Committed order is exactly as described** (`run_all.rs:2318-2354`): `main_agent_top_plan_id(state).await` (2319 — all its locks released on return) → `store.lock().await` (2320) → the `single_in_flight` read at 2327-2332 UNDER the store guard, with the explanatory LOW-1 comment at 2321-2326. No await under the store lock (the single read, `items()`, and `transition` are sync statement-local / `&mut self` calls).

**Leaf-lock claim verified at every site — no lock-ordering hazard.** `single_in_flight` is `Arc<std::sync::Mutex<Option<String>>>` (`state.rs:190`). All seven usage sites are statement-local leaf acquisitions (lock → expect → assign/clone/take → guard dropped at statement end; never held across an await, never held while acquiring another lock):
- `dispatch_item` set + send-failure clear (`backlog_cmds.rs:378-382`, `393-397`) — under the manager guard;
- `on_main_turn_resolved` take (`run_all.rs:2032-2037`) — no guard held;
- `handle_user_intervention` take (`run_all.rs:2402-2407`) — the run_all guard's scope ended at 2400;
- `stamp_backlog_in_flight` read (`run_all.rs:2591-2596`) — under the run_all guard;
- the Exited-arm clear (`events.rs:683-687`) — mgr dropped at 656, the agent_loops guard is statement-local, the drain's locks are released;
- the new adopt read (2327-2332) — under the store guard.

The only nestings are manager→single, run_all→single, and store→single; no single→X edge exists anywhere, so no acquisition cycle is possible — and since no guard is ever held across a yield point, this holds even on a single-threaded runtime.

**The window is genuinely closed (happens-before).** `dispatch_item` sets the pointer strictly before `manager.send` (372-382 — the comment pins exactly this ordering), and the Pending→InFlight stamp requires the store lock (`stamp_backlog_in_flight`: store lock at 2602, transition at 2608-2609). Any item InFlight in the adoption snapshot therefore had its stamp's store-lock critical section complete before the adoption's began, and the pointer set happened-before the stamp began — so the pointer is necessarily visible to the adoption's read under the same lock. A just-dispatched item can no longer look orphaned.

**The test pin genuinely guards the ordering.** `fn_body` (`run_all.rs:545-555`) extracts from the `fn adopt_orphaned_in_flight(` signature (doc comment excluded; neither the doc comment nor the LOW-1 comment contains the literal `single_in_flight`) through the first column-0 `}` — the full body. First `store.lock().await` = line 2320, first `single_in_flight` = line 2329 → `store_lock < single_read` holds on the committed code. Reverting the order puts the first `single_in_flight` occurrence before the store lock and fails the assert; the pin is fail-closed against decoys too (any earlier `single_in_flight` literal — e.g. in a comment — flips the inequality to failing, never passing). Sibling pins also verified against the committed source: wiring `active_check < adopt_call < pending_count` (backlog_cmds.rs 425 < 432 < 443 — the only `adopt_orphaned_in_flight` occurrence in that file is the call itself), snapshot-before-transition in both fns (drain 2275 < 2282; adopt 2344 < 2352).

### Round-1 LOW-2 (stale spec doc) — RESOLVED

- Old file `2026-12-06-backlog-status-plan-lifecycle.md`: `status = "superseded"` frontmatter (line 4), body preserved as history.
- Successor `2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md`: `supersedes` frontmatter + the amended drain sentence (line 13) + the amendment note (line 18). Every amended claim matches the committed behavior:
  - drain REQUEUES to `Pending` with the checkpoint sha preserved at the note head — `drain_run_all_on_main_exit` (`run_all.rs:2266-2283`): note snapshotted before the transition, `"{note} | {suffix}"` shape;
  - "dispatch only considers `Pending`, the guarded requeue refuses `InFlight`" — the pinned store contract (`requeue_refuses_pending_in_flight_and_unknown`; `pending_item`/`next_pending`);
  - run-all START adopts orphans, "requeues every `InFlight` item with no live dispatch pointer, excluding the live single-dispatch pointer and the main agent's active root plan" — the filter at `run_all.rs:2340-2342` verbatim, call site in `backlog_run_all` after the already-active check and before the pending count (`backlog_cmds.rs:424-441`), so adopted items count toward the run total;
  - the main-agent exit clears `single_in_flight` — `events.rs:681-688`, inside `if was_main` (captured under the mgr lock before `mgr.remove`, 654-655).
- Memory system: exactly one live SPEC record (68312f14) and it points at the successor path; the old record (45e4d970) is marked [superseded]. No duplicate live records — the supersede is clean.

### New finding (introduced by the LOW-2 fix)

**LOW — the supersede's forward pointers are incomplete: a live record still routes readers to the old drain semantics with no in-body warning.** Two surfaces, one root cause:
1. The superseded `2026-12-06` spec carries ONLY the frontmatter `status = "superseded"` — no in-body banner pointing at the successor. The house supersede pattern for this exact chain (prescribed in the 45dcf577 round-4 review, verified in round-5, and visible on the `2026-08-31` steering spec) is frontmatter PLUS a one-line SUPERSEDED banner naming the successor and the delta. Frontmatter alone is easy to miss when reading the rendered body — and the body's line 13 still describes the old keep-InFlight drain.
2. The LIVE b83e891f decision record (`.coding/knowledge/decision/2027-01-07-run-all-intervention-pauses-the-item-kept-inflig.md:7`) still ends "Contract: .coding/knowledge/spec/2026-12-06-backlog-status-plan-lifecycle.md (amended same day)" — accurate when written (the 28bc06a2 round-2 review verified it then), now a one-hop-stale pointer landing on the superseded file with no forward link.

Net reader impact: following the live decision's contract pointer (or any direct link to the old spec) yields the WRONG drain semantics with no in-body superseded warning; the correct text is reachable only by re-searching. Fix (small): add the banner to the 2026-12-06 file mirroring the 2026-08-31 shape (SUPERSEDED 2027-01-07 by the successor; one-line delta: the exit drain now requeues to Pending, run-all start adopts orphans, exit clears single_in_flight) and repoint the decision file's Contract line at the successor (or append a supersede note); refresh the corresponding memory records. Constitution: documentation sync.

### Whole-diff sanity check (the parent's checklist) — all verified

- **Drain requeue** (`run_all.rs:2253-2285`): note snapshot (2271-2275) strictly before `store.transition(&id, BacklogStatus::Pending, …)` (2282) — required, `transition` overwrites the note; the store guard drops at the if-let close (2283) BEFORE `end_run`'s awaits (2284). The pre-Executing-exit edge (item still Pending) is refused by the same-status row — the item stays correctly queued, only the informational exit annotation is lost (round-1 accepted, unchanged).
- **Adoption sweep + call site**: `adopt_orphaned_in_flight` (2318-2354) — InFlight-only filter with the pointer and active-root-plan exclusions, guarded `transition` (never `requeue`/`set_status`), note snapshotted before transition; called at `backlog_cmds.rs:432` after the already-active check (424-426 — the run_all guard there is a statement-local temporary, so no run_all guard is held during adoption) and before the pending count (433-441), so adopted items count toward `total`.
- **events.rs clear** (681-688): inside `if was_main`, after the drain; no guard held at the clear site.
- **Both regression tests**: `main_agent_exit_drains_an_active_run_all` (953-1022 — requeue/transition/snapshot-ordering/end_run pins + the forwarder `.single_in_flight` assertion) and `run_all_start_adopts_orphaned_in_flight_items` (1033-1104 — ownership literals, the LOW-1 ordering pin, snapshot-before-transition, the call-site wiring pin). Every assertion verified true against the committed source; each genuinely fails on its corresponding revert.
- **Module-doc updates** (both amended sections) and the two new fn docs: accurate (round-1 verified; untouched by the fix commits).
- **b83e891f contract untouched**: none of the five pinning tests, `handle_user_intervention`, or `halt_run_all`'s arms appear in the diff — only dead-owner paths changed.
- **Scope notes**: the commit also carries three new pending backlog items in `.coding/backlog.jsonl` (998f85fc, 207dc316, d9b560a0) — ordinary user-queued side-car bookkeeping, unrelated to the fix (same treatment round-1 gave the 3b395d27 done-stamp). The plan file `.coding/plans/5f6515f5.md` is committed and accurate (goal, checked steps, bug, regression test name).

### Constitution

Documentation sync: the one open item is the finding above; every other surface (module doc, both fn docs, the events.rs comment, the successor spec) is updated and accurate; README/PLAN.md do not cover drain semantics at this granularity. Multi-platform neutrality: pure Rust backend logic — no platform APIs, paths, or shell. Security: internal state transitions only — no new inputs, parsing, or attack surface. Style: doc comments on the new `pub(crate)` fns, house comment style throughout; the reported green matrix (src-tauri 239+4 passed, exit=0, zero warnings under deny(warnings)) is consistent with the code read — the diff introduces no dead code or unused items.
