## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on wt/agenticcoding for plan a0d3defd "Un-inline run-all dispatch from the event forwarder (perf review L2)" (bug_fixing). The fix is correctly implemented: evidence-before-spawn at all ten sites, latch consumption / wf_complete gate / re-mark inline, lock semantics untouched, and the new concurrency lands in already-guarded paths. Both findings are low-severity, accept-or-follow-up — neither blocks the merge.

**Changeset confirmation**: `git status` shows exactly `M src-tauri/src/ipc/events.rs` (+331/−96), `M .coding/backlog.jsonl` (the c946cbd4 status flip pending→in_flight with plan_id/plan_title), and untracked `.coding/plans/a0d3defd.md`. Nothing else changed. The backlog flip is the correct in-flight marking for this plan's item; the plan file is the standard bookkeeping artifact.

## Verification detail (review checklist)

### 1. Latch semantics — PASS

- **Evidence before spawn, owned values**: verified at all ten sites (six in the forwarder loop — error arm spawned-lane/main, finished spawned Success/Failure, main Success/Failure; four in the flush helpers — `try_flush_deferred_main_resolution` Failure + wf_complete Success, `try_flush_deferred_spawned_resolutions` Failure + wf_complete Success). Each reads `workflow_changed` / `plan_abandoned` / `abandoned_plan_id(...).map(str::to_string)` into locals BEFORE `spawn_resolution_continuation`, clones the `AppHandle`, and moves owned values into the future.
- **Timing equivalence**: the original code evaluated the evidence as call arguments (before the call ran); the new code reads before the spawn — the same point relative to the latch consumption. `abandoned_plan_id` becomes an owned `Option<String>` re-borrowed via `as_deref()` inside the block — same value, no lifetime hazard. `agent_id` is `Copy` (used by value several times per loop iteration), `n` (the failure note) is moved, argument tuples (`success`, `Some(n)`/`None`) preserved exactly at every site.
- **Latch consumption inline**: `on_final_error` / `on_finished` / `flush_deferred_main_failure` all run inline — the `match` on their result gates the spawn. The **wf_complete gate** (`main_agent_workflow_state` / `agent_workflow_state` reads) and the **`remark_pending_finished`** re-mark (both else-branches) stay inline. Only the continuation is spawned.

### 2. Ordering / concurrency — PASS

- **Caller set complete**: the code graph confirms the ONLY production callers of `on_spawned_turn_resolved` / `on_main_turn_resolved` are `events.rs::spawn` and the two flush helpers — the ten converted sites. The source-contract test therefore covers the complete caller set.
- **Locks untouched**: the continuations hold no `DISPATCH_LOCK` during resolution (only the tail's `run_all_dispatch_next` takes it); landings go through `LANDING_LOCK` (`land_spawned_branch`, run_all.rs:4381); item transitions are store-locked with a fresh status re-verification before flipping ("a concurrent drain may have requeued it", run_all.rs:4611-4628) — the exact guard the new interleavings need.
- **Precedent verified**: `compact_then_dispatch_next` is already spawned (`tokio::spawn(compact_then_dispatch_next(app.clone()))`, run_all.rs:6064), and `run_all_heartbeat` (spawned for the run's lifetime, backlog_cmds.rs:590) re-drives dispatch every 30s — dispatch already ran concurrently with the forwarder before this change.
- **No latch races**: the spawned blocks contain no `turn_resolve.` access, and the graph shows no `TurnResolveLatch` methods among the continuations' callees — the latch stays forwarder-single-threaded. At most one continuation per agent per turn (the latch is once-per-turn; deferred vs consumed are mutually exclusive).
- **retire→Exited ordering safe**: `remove_spawned_run` (run_all.rs:4897) precedes `retire_spawned_agent` (:4906), so a continuation-caused agent exit finds no spawned entry — the Exited arm's `owns_spawned_run` is false and `drain_spawned_on_exit` no-ops.
- **Double resolution impossible**: both the continuation (re-verify `still` InFlight under the store lock) and the exit drain (re-verify `still_in_flight`, run_all.rs:4985-4993) gate their transitions on a fresh locked status read; the done-counter bump is gated on one's own successful transition. `end_run` is None-safe/idempotent; the stopped wind-down converges (each continuation removes its own entry before checking, so the last remover ends the run).
- **Main-lane variant**: a delayed main continuation racing the main agent's exit lands in handled, pre-existing territory — the drain requeues/ends the run; the continuation then finds run-all inactive and `resolve_single_dispatch_turn` with no `single_in_flight` pointer resolves nothing (auto-feed-with-no-item is by-design, backlog 45dcf577; dispatch-to-dead-main is the handled "no-main-agent path clears the run and returns Err" case). Recovery-path only, bounded.

### 3. Regression-test adequacy — PASS

- **Behavioral test discriminates**: with `start_paused = true`, an inline await of the 30s sleep would trigger the runtime's auto-advance (clock +30s) before the caller resumes → `Instant::now() == t0` fails. With the spawn, the continuation's `started_tx.send` wakes the caller while the sleep timer is the only pending timer and the caller is runnable — no auto-advance occurs, so the assert holds. Sound construction; the `handle.abort()` cleanup is correct.
- **Source-contract test catches re-inlining**: `wrappers == continuations` over the production region (before the first `#[cfg(test)]`); the graph-verified complete caller set means an unwrapped call anywhere breaks the count. The RED-before claim (wrappers=0, continuations=10) is consistent with the pre-fix code.
- **Existing pins unperturbed**: I enumerated all seven `include_str!("events.rs")` sites in run_all.rs (:1193, :1298, :1330, :2607, :2822, :3169, :3194). The load-bearing whole-file scans — the flush interleaving `[lane, main, lane, main]` (counts `try_flush_deferred_` occurrences) and the `owns_spawned_run(&app, agent_id).await` counts (≥2 / ≥4) — are unperturbed because the new test module contains neither literal (verified by reading the module). The contains-pins (`on_spawned_turn_resolved(`, `drain_spawned_on_exit`, `stamp_spawned_in_flight`, `prev_top_plan_id` read-before-insert ordering, `was_main` before `mgr.remove`, ContextUsage arm, CompactStarted/Compacted/compact_signal) all sit in untouched regions. The green suite confirms.

### 4. Bug-plan checks — PASS

Both regression tests exercise the changed path (the helper seam behaviorally; the production wiring by source contract). Root cause is documented in the helper's doc comment AND the plan file (Context + Bug sections). The BUG: memory is auto-captured at finish per the bug-plan flow.

### 5. Constitution — PASS

- **Documentation sync**: agree with the plan's position — an internal concurrency fix with no README/PLAN.md/config surface; the helper doc, per-site comments, and plan file carry the rationale.
- **Multi-platform neutrality**: `tokio::spawn` / `AppHandle::clone` / `str::to_string` — no platform-specific code.
- **File-tools-first**: nothing in the diff suggests shell surgery; the ten conversions are uniform, structured edits.
- **Warning-free**: the discarded `JoinHandle<()>` is fine — tokio's `JoinHandle` is not `#[must_use]` and the run_all.rs:6064 precedent discards it identically; the return value is exercised by the behavioral test (`handle.abort()`).
- **Doc comments / style**: the private helper is fully documented (rationale, caller contract, ordering note); per-site comments match the file's established style.

### 6. Exited-arm drains — correctly out of scope

Confirmed NOT on the normal dispatch path: the drains fire only on an agent's actual exit, and in the steady state a continuation-caused exit finds the spawned entry already removed (remove precedes retire) → no-op. Leaving `drain_run_all_on_main_exit` / `drain_spawned_on_exit` inline is correct scoping.

## Findings

### LOW-1: The done-counter can miss a bump in the new continuation-vs-exit-drain interleaving (accept, or follow-up)

**Scenario** (narrow, recovery-path): a lane finishes its item successfully → the forwarder spawns the continuation → the agent exits *independently* (user cancel / crash — not the continuation's own retire) before the continuation's transition lands → the forwarder's Exited arm runs `drain_spawned_on_exit`, whose done-orphan guard auto-resolves the item `Done` (work landed) — and the drain **never bumps `r.done`** (the only bumps are run_all.rs:4894 and :6014, both in the resolution paths). The delayed continuation then finds the item already `Done` → blocked row → no transition → no bump. Before this change the continuation always completed before the Exited event was processed (inline await), so the bump always happened; the spawn widens the window.

**Impact — display-only**: `done` is read solely at backlog_cmds.rs:162 (the run-all status payload for the UI progress line); run completion gates on `lanes_in_flight` / item eligibility, never on the counter. The item's disposition is correct in both orders (the store-lock re-verification in both paths prevents any double transition, and the bump is gated on one's own successful transition — a double bump is impossible).

**Disposition**: acceptable as-is — this is a pre-existing class (the drain's auto-Done arm never bumped, even for crashed lanes; the change only makes the race window reachable), and touching the drain is explicitly out of this plan's scope. If desired, a one-line symmetric `done.fetch_add` in the drain's landed arm would close it as a separate follow-up.

### LOW-2: Source-contract evidence-read check scans only to the first `}` (optional hardening)

`every_continuation_invocation_is_spawned_not_awaited_inline` bounds each spawned block at `prod[i..].find('}')` — the first closing brace after the wrapper. Today's ten blocks are flat (no inner braces), so the check covers the full block and correctly proves no `turn_resolve.` read inside. A future block containing a nested brace (a `match`/`if`/closure) with a `turn_resolve.` read *after* it would only be partially scanned. The primary defense (`wrappers == continuations`, which catches any re-inlined site) is unaffected. Optional hardening: scan to the wrapper's closing `});`, or additionally assert each block contains the continuation call.

## Summary

The change does exactly what the plan says, at exactly the ten sites the plan names, with the latch discipline (evidence-before-spawn, consumption/gate/re-mark inline) preserved verbatim. The new concurrency is absorbed by guards that already exist in the continuations and drains (entry re-find, store-locked status re-verification, LANDING_LOCK, DISPATCH_LOCK at the dispatch tail), and by two pre-existing concurrent-dispatch precedents (the spawned compact path and the run-all heartbeat). The regression tests are adequate and discriminating, and every existing source-contract pin on `events.rs` remains meaningful. Both findings are low-severity edge-hardening notes that do not block the merge.
