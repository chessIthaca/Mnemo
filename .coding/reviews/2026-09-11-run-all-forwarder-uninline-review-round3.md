## Verdict: PASS

Round-3 verification of plan a0d3defd ("Un-inline run-all dispatch from the event forwarder", bug_fixing) on wt/agenticcoding. The round-2 fix is correctly and completely implemented: both continuation-side done-counter bumps are now gated on their transitions' OWN results (`flip_done_if_linked` returns the Done transition's bool as its tail expression; `on_spawned_turn_resolved`'s success arm assigns `terminal_resolution` from the transition), the `Proceed { terminal_resolution: false }` downstream semantics advance the run with no stall, and the regression test discriminates both reverts. Every existing source-contract pin I re-checked still anchors, the changeset is exactly the two source files plus bookkeeping, and nothing new broke. Two below-threshold residuals are documented with justification at the end (no action recommended).

## Scope confirmed

`git diff HEAD` + untracked: exactly `src-tauri/src/ipc/events.rs` (helper + ten converted sites + `resolution_continuation_tests` — the state round 2 approved, unchanged since), `src-tauri/src/ipc/run_all.rs` (the round-1 drain bumps, unchanged since round 2, plus the round-2 gating fix and its regression test), `.coding/backlog.jsonl` (the c946cbd4 pending→in_flight flip carrying plan_id/plan_title — correct bookkeeping), plus untracked `.coding/plans/a0d3defd.md` and the round-1/round-2 review files. Nothing else changed. The events.rs production region is byte-identical to what round 2 approved, so rounds 1-2's production-region verification (evidence-before-spawn at all ten sites, latch consumption / wf_complete gate / re-mark inline, lock semantics) carries forward; this review focuses on the round-2 delta plus a full re-check of the pins and the unchanged round-1 pieces.

## Round-2 fix — VERIFIED

### (a) `flip_done_if_linked`'s tail-expression return (run_all.rs:5643-5694)

The `if may_flip` block's tail expression is now the `transition(&item.id, BacklogStatus::Done, None)` call itself (:5676-5680) — the unconditional `true` after the transition is gone; the else arm still returns `false` (:5692). Both arms produce `bool`, so the function returns the transition's own result. The doc comment is amended exactly as claimed (:5637-5642: "as does a transition refused because the exit drain's Done flip won the race in the commit_success window"). The code comment at the transition (:5671-5675) documents the raced window. Caller set verified via the code graph: `run_all_success_disposition` (:5934) is the ONLY caller of `flip_done_if_linked`, so the return-value change has exactly one consumer.

### (b) `Proceed { terminal_resolution: false }` downstream — no stall

`run_all_success_disposition` (:5934-5942) maps `false` to `Proceed { terminal_resolution: false }` — NOT `Waited` (the waiting arm is only the non-closure catch-all). In `resolve_run_all_turn`, `terminal_resolution` gates ONLY the done bump (:6101-6103); the in-flight pointer is cleared unconditionally (:6112), and `item_resolved = terminal_resolution` (:6115) feeds only the between-items auto-compact gate (:6142-6150) — `false` routes to the direct `run_all_dispatch_next` (:6153). So the run advances: current_item cleared, dispatch-next proceeds. The skipped auto-compact is the safer path in the raced case by construction: the race counterpart is an exit drain, which only runs on `Exited` — the main agent is dead, and `compact_then_dispatch_next` would send a `Compact` to a dead agent and wait out `AUTO_COMPACT_WAIT` (600s) before dispatching anyway. (Pre-fix, this ordering set `terminal_resolution = true` and could burn that wait — the fix improves the raced path, not just the counter.) The no-main-agent dispatch failure is the handled, logged case (:6153-6157).

### (c) The spawned arm's gated assignment (run_all.rs:4681-4695)

`terminal_resolution` is initialized `false` (:4598); the success arm now assigns `terminal_resolution = state...transition(&item.id, BacklogStatus::Done, note)` (:4689-4694) — the `still` pre-check (:4674-4680) surviving but the transition refusing leaves `false`. The bump (:4958-4963) is the ONLY thing `terminal_resolution` gates in this function: `remove_spawned_run` (:4964), worktree removal (:4965-4972), `retire_spawned_agent` (:4973), the stopped check (:4980-5001), and `run_all_dispatch_next` (:5002) all run regardless — no stall, no leaked lane state. The comment (:4682-4688) documents the raced lock-re-acquisition gap.

### (d) Exactly one bump per item — both orderings, both lanes

`BacklogStore::transition` (src/backlog.rs:625-652) reloads under the store lock, consults the legal-move table (same-status Done→Done falls to `_ => false` — refused, no mutation, no persist), and returns `true` only on an actual transition; the store lock serializes both sides' transitions, so exactly one succeeds. Enumerated:

- **Continuation transitions first** (either lane): the drain's `still_in_flight` re-verify sees Done → no transition → `transitioned`/`main_done_bump` stay false → no bump. One bump (the continuation's).
- **Drain transitions first, continuation's pre-check already passed** (the round-2 target): the drain's Done flip lands in the continuation's `commit_success` window (main lane) or lock re-acquisition gap (spawned lane) → the continuation's transition is refused → gated `false` → no bump. One bump (the drain's — correctly gated on its own transition).
- **Drain transitions first, continuation's pre-check sees Done**: `may_flip`/`still` false → blocked arm → `false` → no bump. One bump (the drain's).

The wide `land_spawned_branch`/`orphan_work_landed` windows sit on the DRAIN's side of its own check→transition; they only make the drain the losing (refused, suppressed-bump) side more often — never a double count. Impact is display-only, re-confirmed: `done` is read solely at backlog_cmds.rs:162 (the `RunAllProgress` payload in `emit_backlog_changed`); run completion gates on `any_lane_in_flight` / item eligibility, never the counter.

### (e) Round-1 pieces intact

Both drain bumps are unchanged since round 2 and re-verified: the spawned drain captures `let transitioned = ...` (:5095-5100, store guard dropped at the end of the `let` — the bump at :5109-5112 takes run_all with no other lock held); the main drain captures `main_done_bump = store.transition(...)` inside the store-guard scope (:6459) and bumps after the scope closes (:6477-6482) — run_all is never acquired under store. Requeue arms do not bump. `exit_drain_done_bumps_the_run_counter` (:1385-1417) still pins all four literals.

## Regression test discrimination — VERIFIED

`continuation_done_bumps_are_gated_on_the_transition` (:1419-1444, placed directly after the round-1 drain test, inside the `fn_body`-owning tests module):

- **Pin 1** — `!flip.contains("\n        true\n")`: the pre-fix code's `true` was a standalone statement at 8-space indent inside `if may_flip` (the diff's removed line). Reverting fix (a) restores exactly that formatting → the assert fires. The current body contains no `\n        true\n` (the else arm's `false` does not match; no other `true` literal at that indent).
- **Pin 2** — `spawned.contains("terminal_resolution = state")`: the pre-fix code had `transition(...);` as a statement plus `terminal_resolution = true;` — no `terminal_resolution = state` anywhere in the function. Reverting fix (b) removes the only occurrence → the assert fires.
- **fn_body resolution**: the composed needles (`fn flip_done_if_linked(`, `fn on_spawned_turn_resolved(`) first-occur at the production signatures (:5643, :4545); the test module's own string arguments never compose those literals (the `fn_body` doc's design, :1633-1650), and the existing pins using the same calls (:2299, :2548, :2602) passing in the green suite confirm resolution. Both fn bodies span their full functions (first column-0 `}` after the signature), covering the pinned arms.

## Existing source-contract pins — all hold

- `on_main_turn_resolved_is_plan_tied` (:2286-2337): `commit_success(root.clone(), item.clone())` (:5668) and `BacklogStatus::Done` (:5678) present in `flip_done_if_linked`; no CantResolve/rollback.
- `done_transitions_are_guarded_on_status_and_plan_linkage` (:2534-2588): `plan_linkage_allows_done` present (:5659); guard BEFORE `commit_success` (:5659 < :5668); the blocked-flip annotate string (:5690) present in both arms.
- `failed_transitions_are_guarded_on_status_and_plan_linkage` (:2591-2656): both spawned `plan_linkage_allows_failed` sites (:4725, :4903) and the shared helper's guard (:5718) intact.
- `complete_state_turn_end_with_landed_plan_dispatches_next` (:2386-2391): `terminal_resolution` still in `resolve_run_all_turn`.
- `auto_compact_gate_is_spawned_and_run_all_only` (:3032-3087): `let auto_compact = item_resolved` (:6142), the spawned compact (:6152), clear-before-gate (:6112 < :6148), gate-before-dispatch (:6148 < :6153) — all anchor.
- `slid_resolution_spawned_lane_requeued_pending_recovers` (:2471-2531) and the main-path slid pins: untouched regions, all literals present.
- events.rs pins: the production region ends at the first `#[cfg(test)]` (:1606); the helper (:1406) and all ten sites sit inside it; `resolution_continuation_tests` (the file's last module) is excluded from `prod`, and its text contains neither `try_flush_deferred_` nor `owns_spawned_run(&app, agent_id).await` — the flush-interleaving and owns_spawned_run counts are unperturbed. The `prev_top_plan_id` read-before-insert, `was_main`-before-`mgr.remove`, ContextUsage, and CompactStarted/Compacted pins sit in untouched regions.
- No whole-file count pins are perturbed by the new test module's literals (all existing run_all.rs pins are `fn_body`-scoped; the only `r.done.fetch_add` references in test text are the new tests' own contains-asserts).

## Constitution checks

- **Tests**: the reported full-suite run (2296 + 16 + 297 + 4 + 2 + 2 passed, 0 failed, warning-free under `#![deny(warnings)]`) is consistent with every static pin re-verified here; both new regression tests are genuine (each fails on the exact pre-fix code, per the revert analysis above).
- **Multi-platform neutrality**: clean — tokio::spawn, atomics, string ops, store locks; no Windows-only APIs, paths, or shell syntax in the delta.
- **File-tools-first**: the delta is uniform structured edits (ten identical conversions, two gated assignments, two tests); no shell-surgery evidence.
- **Warning-free**: statically consistent — every new binding is consumed (`transitioned`/`main_done_bump` feed their gates; the tail expression is the return value), no unused imports; the discarded `JoinHandle` matches the existing `tokio::spawn(compact_then_dispatch_next(...))` precedent (not `#[must_use]`).
- **Documentation sync**: internal concurrency fix with no README/PLAN.md/config surface; the helper doc, the amended `flip_done_if_linked` doc, the per-site/per-arm comments, and the test docs carry the rationale accurately.

## Below-threshold residuals (documented, no action recommended)

1. **Abandonment flips keep their unconditional-`true` shape** (`flip_failed_if_abandonment_linked` :5732, the spawned success-path abandonment arm :4739, the spawned failure arm :4917) — I AGREE with round 2's deliberate scope. A double-count there requires the conjunction (plan abandoned this turn) ∧ (agent exits immediately after) ∧ (the drain's landed evidence passes — `plan_steps_all_done` on an abandoned plan, near-self-contradictory) ∧ (the drain's Done transition landing inside the continuation's bare lock re-acquisition gap — no `commit_success`/landing window, vs the ~100ms+ window that justified the round-2 fix). Display-only impact. The spawned slid-pending arm's unconditional `true` (:4854) is safe against the drains by construction (its item is Pending; both drains' flips require InFlight).
2. **The blocked-flip annotate's premise in the drain-wins ordering**: when the drain's Done flip lands before the continuation's `may_flip` check, the else arm annotates "completing plan is not this item's plan — item kept, no transition" onto an item the drain already transitioned Done — a stale-premise note string. Not flagged: display-only note text on a terminal item in a crash-recovery race; present through rounds 1-2 (the else arm predates the L2 change and is untouched by the round-2 fix); no functional consumer parses it (`extract_checkpoint_sha` head-parses, so the sha survives; nothing decisions on the note text). Fixing it would mean a status re-check inside the guarded else arm for pure cosmetics — the risk outweighs the benefit.

## Summary

The round-2 fix does exactly what the finding prescribed, at exactly the two sites it named, with the downstream semantics (Proceed{false} → pointer clear + direct dispatch, cleanup ungated) verified safe against stalls in every ordering of the continuation-vs-exit-drain race. The regression test fails on either revert. All existing source-contract pins — including the flip_done_if_linked and resolve_run_all_turn pins the task called out — still anchor, the round-1 drain bumps are intact with their lock order preserved, and the changeset contains nothing beyond the two source files and their bookkeeping. The change is ready to land.
