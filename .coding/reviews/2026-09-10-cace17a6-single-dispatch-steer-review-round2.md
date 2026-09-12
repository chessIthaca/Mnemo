## Verdict: PASS

Round-2 verification of bug_fixing plan cace17a6 ("Single-dispatch steer keeps the item in flight") at commit b3f6f1b — HEAD of wt/agenticcoding with a clean tree (`git diff HEAD` and `git status --short` both empty, so the working files are exactly the commit). All 7 round-1 findings are fixed correctly, the extra site (the 2026-12-06 spec) carries its amendment, the repo-wide stale-phrasing sweep finds the old contract only in immutable history and superseded/amendment lines, and the fix round introduced no regressions (comment/prose-only + one blank line, all outside every `fn_body` region the source-contract tests pin).

### Fix verification (all 7 + the extra)

1. **HIGH-1 — FIXED.** `src-tauri/src/ipc/run_all.rs:6588-6592` (the `handle_user_intervention` doc block): now reads "A SINGLE-DISPATCH item is KEPT `InFlight` too (plan cace17a6): the handler restores the `single_in_flight` pointer it consumed (check-and-set), so the turn that completes the plan resolves it through the normal plan-tied rules. Only a run-all item whose run was drained before this resolution requeues (no pointer left — the queue is its only recovery)." — the round-1 suggested text verbatim; the side-note error ("checkpoint sha preserved" for a path with no git checkpoint) is gone with it.
2. **HIGH-2 — FIXED.** `run_all.rs:174-179` (module doc status-contract section): escape hatches are now "the deliberate `InFlight` → `Pending` deferral (via the `backlog_status` tool or the UI) and the terminal → `Pending` requeue … a steer/interrupt is NOT one (any dispatched item is kept InFlight — backlog b83e891f for run-all items, plan cace17a6 for single-dispatch; only a drained run's item requeues)" — matches the amended spec line 15 word-for-word; the module doc no longer contradicts itself.
3. **LOW-1 — FIXED.** `PLAN.md:111-114`: "a single-dispatch item stays `in_flight` with its dispatch pointer restored (the plan stays active — the turn that completes it resolves the item; only a run-all item whose run was drained before the intervention requeues)" — the round-1 suggested delta, verbatim.
4. **LOW-2 — FIXED.** `src-tauri/src/ipc/agent.rs:332-334` (interrupt command doc): "a single-dispatch item is kept InFlight with its pointer restored, plan cace17a6".
5. **LOW-3 — FIXED.** `src-tauri/src/ipc/state.rs:213-215` (`user_intervention` field doc): same delta as LOW-2.
6. **LOW-4 — FIXED.** `README.md:79` tail: "the manual in_flight→pending deferral (backlog_status / the UI) and terminal requeue remain the escape hatches" — search-confirmed in the live file; the same line's main intervention sentence also carries the new "any dispatched item stays `in_flight` — … only a run-all item whose run was drained before the intervention requeues" wording.
7. **LOW-5 — FIXED.** `run_all.rs:2042-2044`: exactly one blank line between `intervention_keeps_a_single_dispatch_item_in_flight`'s closing brace and the `#[test]` for `non_closure_resolution_keeps_the_run_armed` — matches the module's adjacent-test style.
8. **Extra (beyond the round-1 list) — LANDED.** `.coding/knowledge/spec/2026-12-06-backlog-status-plan-lifecycle.md:22`: "Amended 2027-01-09 (plan cace17a6): the single-dispatch intervention requeue escape hatch named above is REMOVED — … Canonical contract: the 2027-01-09 amendment in `2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md`." The file is frontmatter-marked `status = "superseded"` with a SUPERSEDED banner, so its line-17 body text is properly framed as historical — the established amendment pattern.

### Stale-phrasing sweep (task b)

Regex sweep across the repo (`single-dispatch item (still )?(requeues|returns)` · `returns to the queue` · `returned to queue` · `single-dispatch intervention requeue` · `single-dispatch in_flight` · `transitions back to` · `its pointer is consumed` · `single-dispatch items still`): 38 hits in 19 files, every one accounted for:

- **Immutable review history** — `.coding/reviews/` files: 2026-09-07 (28bc06a2 round-2/round-3), 2026-12-05 (steer-intervention-requeue r2), 2026-12-30 (show-tool-activity round-2/3), 2027-01-04 (show-knowledge-activity), 2027-01-07 (descendant-tracker), and the round-1 report itself (it quotes the old text as its findings).
- **Plan files** — 0a58db61 (the 28bc06a2-era design, history), 1727f14c (quotes an old item note), and cace17a6's own exploration/bug/step sections describing the defect and the fix.
- **Knowledge files, amendment-framed** — the 2026-12-06 spec (superseded + the new amendment), the 2027-01-07 spec's amendment line, the b83e891f bug record's superseded sentence + its 2027-01-09 amendment, and the new single-dispatch bug record (describes the OLD behavior as the defect — correct). The b83e891f decision record no longer hits at all: its body was updated inline (old sentence replaced) in addition to the appended amendment.
- **Backlog history** — the 0296d448 done-note quotes the old "steered by the user, returned to queue" note to explain it.
- **Live code, correct current behavior** — `run_all.rs:6373` (main-agent-exit drain requeue), `:5035` (spawned-agent-exit requeue), `:6688` (the `", returned to queue"` suffix — used only by the narrowed drained-run arm; verified by reading the disposition match: keep `from_run_all && interrupted_run_active` :6690, keep+restore `!from_run_all` :6708, requeue `from_run_all && !interrupted_run_active`), and `src/backlog.rs:1821/1824` (the `annotate` unit test's note-accumulation fixture — parser compatibility with historical note shapes, not contract text).

No live doc (README, PLAN.md, module/command/struct doc comments, canonical spec body, tool docs, frontend) states the old contract as current. Supplementary sweeps: `src/tool/**/*.rs` — zero "single-dispatch" hits (the `backlog_status` tool doc carries no stale text); `frontend/**/*.{ts,tsx}` — zero hits for `single-dispatch|returns to the queue|returned to queue`.

### Regression check (task c)

- The fix-round delta (b3f6f1b minus the round-1-verified uncommitted set) is exactly the 7 sites + the 2026-12-06 spec amendment: doc comments, prose, and one blank line. Everything else in the commit matches what round 1 already verified clean — the keep arm (:6708+, annotate + check-and-set restore under one lock hold), the narrowed drained-run requeue arm, the regression test `intervention_keeps_a_single_dispatch_item_in_flight` (:1997-2042), and the sibling-test pin retargets.
- All edited doc comments sit above/outside `fn_body(...)` extraction regions; the blank line sits between items — no source-contract pin can be affected, and comment/prose/whitespace changes cannot alter compilation or behavior.
- Preceding commit 647ab1e is side-car only (3 knowledge/plan files, 19 insertions, no source) — no interaction with any of the sites.
- Test evidence (root 2158 + 16 doc-tests; src-tauri 293 + 4 + 2; 0 failed) is the main agent's reported run — I could not execute tests (read-only reviewer), but it is consistent with everything verified statically: a comment/prose/blank-line-only fix round over a round-1-verified code change.

### Notes

- The 2026-12-06 spec's line-17 body still names the old escape hatch, but the file is superseded-marked and the 2027-01-09 amendment explicitly removes it with a canonical-contract pointer — the project's established amendment pattern (same as the b83e891f bug record). Not a finding.
- Bookkeeping verified: backlog item 0296d448 is `done` with a note pointing at f4a2e6d..50969db that explains the stale steer note; the round-1 report is committed alongside the change (closing-sequence requirement).
- Round-2 verdict: **PASS** — the plan's closing sequence can complete (commit already landed; `finish` with this report).
