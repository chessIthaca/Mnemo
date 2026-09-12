## Verdict: FINDINGS (2 high, 5 low)

The code change is CORRECT — the new keep-InFlight arm, the check-and-set pointer restore, the narrowed drained-run requeue, and the regression test all verify clean against the surrounding machinery (resolution path, adoption sweep, exit drain, lock discipline). All 7 findings are documentation-sync: the OLD single-dispatch-requeue contract is still stated in 6 places the plan's doc-sync pass missed (2 of them inside the changed file, directly contradicting the new arm), plus one trivial style nit.

## Verified correct (no action)

1. **The keep arm** (`run_all.rs:6702-6731`): `BacklogStatus::InFlight if !from_run_all` annotates with the exact run-all-arm wording ("{reason}, kept in flight — the plan stays active, resume to continue") and restores the pointer check-and-set under one lock hold — byte-identical to the amplifier's pinned restore pattern (`resolve_single_dispatch_turn` :5382-5391, the 96e2862a round-2 pattern). `slot.is_none()` guard means a concurrent ▶ dispatch that set the slot inside the take→restore window wins (never clobbered); the steered item would then be pointer-less InFlight, recovered by run-start adoption or the exit drain — the safe disposition either way.
2. **Lock discipline**: the std `single_in_flight` mutex is a leaf acquisition (no `await` inside the arm), nested store(tokio)→single_in_flight(std) — the same nesting as `adopt_orphaned_in_flight` (:6473-6479). No path holds single_in_flight across an await or acquires another lock while holding it. No deadlock.
3. **The narrowed drained-run arm** (:6732-6743, `from_run_all && !interrupted_run_active`): still requeues with the merged note + checkpoint sha — unchanged behavior for that case. Run-all keep arm (:6684-6701), Pending arm (:6744-6748), and terminal catch-all (:6749-6752) are untouched. Match is exhaustive: (from_run_all=T, active=T)→keep, (F,*)→keep+restore, (T,F)→requeue, plus Pending and `_`.
4. **Source tracking unchanged**: latch id → run `current_item` → `single_in_flight.take()` (:6598-6627) is byte-identical to before; the identity-guarded stop-flag logic is untouched.
5. **The completing turn resolves the kept item**: `resolve_single_dispatch_turn` takes the restored pointer → `single_dispatch_disposition` → Done via plan-loop gate + `plan_linkage_allows_done` (InFlight + plan_id match — `annotate` doesn't touch plan_id); non-closure → annotate + re-restore (amplifier); Failed only on abandonment. `stamp_backlog_in_flight` only lifts still-Pending items (idempotent for a kept-InFlight item); `adopt_orphaned_in_flight` excludes the live restored pointer (the item stays linked to its plan); on main-agent exit the pointer is cleared and the item becomes adoptable / done-orphan-guarded — the same recovery model as the run-all arm.
6. **Never-terminal / never-continue guarantees intact**: the handler body still contains no `BacklogStatus::Failed/Done/CantResolve`, no dispatch/auto-feed, no `end_run`.
7. **Regression test adequacy** (`intervention_keeps_a_single_dispatch_item_in_flight` :1995-2040): source-contract style matches the file's established discipline for this function (it needs a Tauri AppHandle — the sibling b83e891f tests are source-contract for exactly that reason, cf. the `steer_halt…` test's explicit note). It fails without the fix (the arm literal `BacklogStatus::InFlight if !from_run_all` does not exist in the old code — the old catch-all was `BacklogStatus::InFlight =>`) and pins the load-bearing shape: the guard split, annotate-not-transition, the check-and-set restore, and the drained arm following. The behavioral downstream (restored pointer → resolution → Done) is pinned by the existing amplifier + plan-linkage tests. Adequate pinning.
8. **Root cause documented**: plan file `.coding/plans/cace17a6.md` (detailed Bug section with line pointers + the b83e891f asymmetry rationale), BUG memory 5341fc8c, and the new knowledge file `2027-01-07-single-dispatch-steer-requeues-the-item-while-it.md` (symptom → root cause → fix + regression test name). All consistent.
9. **Multi-platform / security / warnings**: no platform-specific code, no new input surface (`iv.reason` interpolation matches the existing run-all arm), all new bindings used — consistent with the reported green suites (root 2158+16, src-tauri 293+4+2, unpiped).
10. **Knowledge doc sync that WAS done**: spec line 15 + 2027-01-09 amendment, decision record (title + body + amendment), bug record amendment, README main sentence, module-doc intervention section (:46-64), on_main_turn_resolved comment, both sibling-test pin retargets — all verified consistent with the new contract. Frontend has no hardcoded contract text (searched — no hits).

## Findings

### HIGH-1 — `handle_user_intervention`'s own doc comment still states the OLD requeue contract
`src-tauri/src/ipc/run_all.rs:6585-6587` (the `///` doc block on the fn, :6571-6592):

> "A SINGLE-DISPATCH item transitions back to `Pending` with its checkpoint sha preserved (its pointer is consumed here, so the queue + auto-feed is its only recovery)."

This directly contradicts the new arm in the same function (:6702-6731: keep InFlight + restore the pointer) and the updated module doc (:56-61). The diff updated the module doc, the in-function comments (:6599-6607, :6643-6648), and `on_main_turn_resolved`'s comment — but missed the function's own doc comment. A maintainer reading this doc would implement/reason against the exact behavior this plan removes. (Side note: "checkpoint sha preserved" was already wrong for single-dispatch items — that path has no git checkpoint, per `single_dispatch_disposition`'s doc :5417-5418.)

**Fix**: replace :6585-6587 with the new contract, mirroring the module doc :56-61, e.g. "A SINGLE-DISPATCH item is KEPT `InFlight` too (plan cace17a6): the handler restores the `single_in_flight` pointer it consumed (check-and-set), so the turn that completes the plan resolves it through the normal plan-tied rules. Only a run-all item whose run was drained before this resolution requeues (no pointer left — the queue is its only recovery)."

### HIGH-2 — module doc's status-contract section still lists the single-dispatch intervention requeue as an escape hatch
`src-tauri/src/ipc/run_all.rs:174-178` (the `InFlight ⇔ plan active` section of the module doc):

> "The deliberate `InFlight` → `Pending` deferral (the single-dispatch intervention requeue — a run-all item is kept InFlight with its run stopped instead, backlog b83e891f) and the terminal → `Pending` requeue remain the escape hatches."

This is the exact sentence class the spec's line-15 amendment removed (the spec now defines the deferral as "via the `backlog_status` tool or the UI"). The plan updated the module doc's intervention section (:46-64) but missed this second occurrence in the same module doc — the file now contradicts itself.

**Fix**: amend :174-178 to match the amended spec line 15: the escape hatches are the deliberate `InFlight → Pending` deferral (via the `backlog_status` tool or the UI) and the terminal → `Pending` requeue; a steer/interrupt is NOT an escape hatch (any dispatched item is kept InFlight — plan cace17a6; only a drained run's item requeues).

### LOW-1 — PLAN.md backlog section not synced (explicitly in the plan's own step list)
`PLAN.md:111-112`:

> "…backlog b83e891f), a single-dispatch item returns to `pending` (checkpoint sha preserved in its note), and the run never dispatches…"

The plan's detailed step 4(c) explicitly lists "PLAN.md backlog section — same delta", and the exploration notes (:17) list PLAN.md among the docs to sync — but PLAN.md is absent from `git status`. The step is marked [x] while the deliverable was skipped.

**Fix**: "…a single-dispatch item stays `in_flight` with its dispatch pointer restored (the plan stays active — the turn that completes it resolves the item; only a run-all item whose run was drained before the intervention requeues)…".

### LOW-2 — `interrupt` command doc still says single-dispatch requeues
`src-tauri/src/ipc/agent.rs:332-333`:

> "(a run-all item is kept InFlight with its run stopped, backlog b83e891f; a single-dispatch item requeues)"

**Fix**: "…; a single-dispatch item is kept InFlight with its pointer restored, plan cace17a6)".

### LOW-3 — `UserIntervention` struct doc still says returns-to-queue
`src-tauri/src/ipc/state.rs:213-215`:

> "(a run-all item is kept InFlight with its run stopped, backlog b83e891f; a single-dispatch item returns to the queue)"

**Fix**: same delta as LOW-2.

### LOW-4 — README line 79's unchanged tail still names the removed escape hatch
`README.md:79`, in the tail of the very line this plan edited (context the diff didn't touch):

> "…the single-dispatch in_flight→pending deferral and terminal requeue remain the escape hatches…"

Under the amended spec, the deferral escape hatch is the manual `backlog_status`/UI one — the "single-dispatch" qualifier described the intervention requeue this plan removed, so the phrase is now stale inside an otherwise-updated line.

**Fix**: "…the manual in_flight→pending deferral (backlog_status / the UI) and terminal requeue remain the escape hatches…".

### LOW-5 — missing blank line between the new test and the next
`src-tauri/src/ipc/run_all.rs:2040-2041`: the new test's closing brace is immediately followed by `#[test]` for `non_closure_resolution_keeps_the_run_armed` — every other adjacent test pair in the module has exactly one blank line. Trivial; the blank line sits outside every `fn_body` region the source-contract tests extract, so adding it cannot break any pin.

## Notes for the fix round

- All 7 findings are doc-comment/prose/style only — zero code changes required. The doc-comment edits sit outside every `fn_body(...)` extraction the source-contract tests pin, so they are test-safe; re-run the full suites anyway per the closing sequence.
- The uncommitted set also carries items outside this plan's delta, all benign and expected to ride the commit: (a) the 0296d448 done-marking in `.coding/backlog.jsonl` (this plan's bookkeeping step — verified, note points at f4a2e6d..50969db and explains the stale steer note); (b) a new user-added pending item a21993a4 (status-bar tok/sec — user activity, harmless under the union merge driver); (c) the previous plan's (eda38891) untracked knowledge records + its plan-file regression-test append — side-car state that belongs in git.
- Test evidence (root 2158 + 16 doc-tests; src-tauri 293 + 4 + 2; the 5 intervention tests filtered) is consistent with everything I verified statically; I could not execute tests myself (read-only reviewer).
