## Verdict: FINDINGS (0 high, 1 low)

Round-4 verification of the committed state on wt/agenticcoding (HEAD 2e02552, clean tree): LOW-4 is fixed and fully consistent; rounds 1-3 findings (LOW-1 both spec amendments, LOW-2 wrapper doc comment, LOW-3 README clause) all remain fixed with zero behavioral deltas since the round-1-verified fcc1010. One new low from the repo-wide sweep the brief requested: the two dead-owner call-site comments — events.rs:686-687 (drain) and backlog_cmds.rs:448-449 (adoption) — still state the requeue categorically. They are the call-site twins of LOW-3/LOW-4, in files the change never touched and that no prior round's sweep enumerated (verified: zero mentions of either file in the three round reports).

### LOW-5 (new, low — documentation sync): the drain/adoption call-site comments still state the unqualified requeue

**Files:** `src-tauri/src/ipc/events.rs` :680-692 (stale clause at :686-687) · `src-tauri/src/ipc/backlog_cmds.rs` :445-449 (stale clause at :448-449)

events.rs — the Exited arm's comment at the drain's only call site:

> Drain it: the item is requeued to Pending (the queue is its recovery — run-all orphans, 2027-01-07).

backlog_cmds.rs — `backlog_run_all`'s comment at the adopt call site:

> … is skipped by every future run-all — requeue orphans to Pending so the dispatch order picks them up and they count toward the run's total.

Both are live code comments (not historical records, not test rationales) describing the dead-owner disposition, and both became conditionally false at fcc1010: a landed orphan now auto-resolves `Done` — not requeued, not picked up by dispatch order, not counted toward the run's total. Neither file carries the landed-work exception anywhere, so a reader at either call site gets the pre-guard contract. Same class as LOW-1 (specs), LOW-3 (README), LOW-4 (module-doc lifecycle section) — each received a one-clause fix; these need the same:

- events.rs:686 — "… Drain it: the item is requeued to `Pending` — unless the work already landed (the done-orphan guard, 6c6966b9: plan complete + commits after the pre-item checkpoint → auto-resolves `Done`) — (the queue is its recovery — run-all orphans, 2027-01-07). …"
- backlog_cmds.rs:448 — "… — requeue orphans to `Pending` (a landed one auto-resolves `Done` first — the done-orphan guard, 6c6966b9) so the dispatch order picks them up and they count toward the run's total."

Docs-only; no behavioral impact. Why rounds 1-3 missed them: the change's diff never touched these files, and the sweeps enumerated run_all.rs, README, and `.coding/` surfaces — the callers' comments were never adjudicated (confirmed by searching the three round reports for both filenames: zero hits).


## 1. LOW-4 — verified fixed (2e02552)

The module-doc lifecycle sentence (run_all.rs:139-147) now reads exactly as prescribed: "The one dead-owner exception: a MAIN-AGENT EXIT requeues the item to `Pending` — unless the work already landed (the done-orphan guard, 6c6966b9: plan complete + commits after the pre-item checkpoint → auto-resolves `Done` instead of re-dispatching) — (no turn resolution ever comes for it — run-all orphans, 2027-01-07; [`drain_run_all_on_main_exit`]), and run-all start adopts any `InFlight` orphan a hard crash left behind ([`adopt_orphaned_in_flight`], the same landed-work exception) — no item is ever left `InFlight` without a live owner."

Consistency checks — all pass:
- **Safety-model bullet above it (:38-45):** both sections now carry the exception ("UNLESS the work already landed … then it auto-resolves `Done`"). The wording difference ("commits after the checkpoint" vs "after the pre-item checkpoint") is semantic identity — both name `orphan_work_landed`'s evidence. The trailing parenthetical "(no turn resolution ever comes for it…)" stays accurate in both arms (the guard resolves from evidence, never from a turn), and "no item is ever left `InFlight` without a live owner" holds in both arms (requeue → `Pending`; landed → `Done`).
- **The code:** `orphan_work_landed` (:2720-2742) = `plan_steps_all_done` on `.coding/plans/<plan_id>.md` AND `commits_after_checkpoint` on the sha extracted from the note head; every unverifiable path → `false` (safe default → requeue). The drain (:2763-2823) and adoption (:2862-2942) both consult it and carry the `Done`/`Pending` arms. The doc's "plan complete + commits after the pre-item checkpoint" matches exactly.
- **Every other surface:** README:79, both knowledge-spec amendments, and both fn doc comments (each carries its own "Done-orphan guard (backlog 6c6966b9): BEFORE requeueing…" paragraph — drain :2757-2762, adoption :2846-2852) all agree.
- **Diff discipline:** 2e02552's source delta is exactly this one module-doc hunk (+2 lines, pure prose with intra-doc links — no code fences, no doctest surface) plus the round-3 report file. Docs-only; cannot affect `cargo test`. The intra-doc links point at same-module `pub(crate)` fns, the same pattern as the pre-existing safety-bullet links.

## 2. Rounds 1-3 — all remain fixed, no regressions

- **LOW-1 (both knowledge-spec amendments):** lifecycle spec line 22 — the amendment naming both sites, the evidence definition (all steps checked AND ≥1 commit after the pre-item checkpoint sha from the note head), the `Done` auto-resolution, and the safe default (no plan id / missing-incomplete plan / no sha / git error → requeue) — present and accurate vs. the code. Resolution-contract spec line 6 (drain exception + the flipped scope-boundary sentence "has since LANDED (plan 5b988455)") and line 10 (the adoption mirroring amendment) — present. The bodies above the amendments stay historical per the established body+amendment convention.
- **LOW-2 (commits_after_checkpoint doc comment):** git_ops.rs:243-249 — the public wrapper's doc naming the checkpoint-sha source (note head) and the mockable impl — present; the impl's doc (:222-237) unchanged.
- **LOW-3 (README clause):** README:79 — "unless the work already landed (plan complete + commits after the pre-item checkpoint), which auto-resolves done" — present.
- **No regressions:** the only deltas since the round-1-verified fcc1010 are d8b0401 (README + plan checkbox + round-2 report — per round 3's diff analysis) and 2e02552 (module doc + round-3 report — verified directly via git show). Zero behavioral deltas; the round-1 code verification carries to HEAD, and I re-confirmed the load-bearing sections at HEAD: `orphan_work_landed`, `plan_steps_all_done`, `extract_checkpoint_sha`, the drain's snapshot→evidence→re-verify→transition structure, the adoption's 3-phase structure with `still_orphan` re-verification, `commits_after_checkpoint[_impl]` + the MockGit `log --oneline <sha>..HEAD` arm, and all four regression tests present (`drain_run_all_on_main_exit_consults_landed_evidence` :462, `adopt_orphaned_in_flight_consults_landed_evidence` :496, `plan_steps_all_done_requires_every_step_checked` :527-565, `commits_after_checkpoint_impl_detects_landed_work` git_ops.rs:1152).


## 3. Repo-wide sweep — full adjudication

Searched: "no turn resolution ever comes", "dead-owner", both function names, "requeu*" in events.rs/backlog_cmds.rs, and "orphan" across src/, src-tauri/, and frontend/. Every hit falls into one of:

- **Updated (exception present in the surface):** run_all.rs safety bullet (:38-45), lifecycle section (:139-147), drain fn doc (:2757-2762), adoption fn doc (:2846-2852), README:79, lifecycle spec amendment, resolution-contract spec (both touches), git_ops.rs guard docs (:224/:245/:1146).
- **Correctly historical (point-in-time records):** the superseded 2026-12-06 spec (SUPERSEDED banner), plan files (5f6515f5, 803d2be0, 8c2edf0f, 5b988455), the 5f6515f5 bug record, the intervention-pause decision record (attributes the requeue amendment to 5f6515f5 — accurate for its era), prior review reports, and the queued backlog.jsonl item texts.
- **Instrumental (cleared, round-3 rationale re-confirmed):** the module-doc deferred paragraph (:20-22) and the deferred spec's point (3) — subject is the deferred flag's semantics; every operative claim (flag survives, non-stranding, selection exclusion) holds in both arms. The 5f6515f5-era test rationales/assert messages (:1351-1355, :1385-1389, :1429) pin that the `Pending` arm must exist — still a true requirement (the safe default), with the new sibling tests (:452-521) pinning the `Done` arm.
- **Stale (LOW-5):** events.rs:686-687 and backlog_cmds.rs:448-449 — the only remaining unqualified dead-owner-requeue statements in live source.

All other "orphan" hits across src/, src-tauri/, and frontend/ are unrelated (orphaned tool results/configs/files/profiles) — no backlog dead-owner semantics.

## 4. Correctness, security, multi-platform — no new findings

- **Correctness/bugs:** no source delta since the round-1-verified fcc1010 beyond doc comments (verified via git show); the guard logic, lock discipline (git check runs without the store lock; ownership re-verified after the unlocked window at both sites), and safe default carry over and were re-confirmed at HEAD.
- **Security:** no new input surfaces; the checkpoint sha flows note → git via the existing argument-vector Command runner (no shell, no injection); plan_id → `PathBuf::join` on app-written dispatch-note data, used read-only as evidence. No findings.
- **Multi-platform neutrality:** no platform-specific additions — `PathBuf` joins, the existing git_ops layer, no `cfg(windows)`, no Windows paths or shell syntax; the MockGit arm is test-only. Clean on both macOS and Windows.

## 5. Test status

Not re-run (read-only reviewer); the delta analysis stands in: HEAD's source differs from the round-1-verified fcc1010 only by README prose and the run_all.rs module-doc comment — pure prose, no code fences, no behavioral surface — so the brief's green runs (cargo test 2084+16; cargo test -p mnemo-app 250+4+2 at fcc1010, zero source deltas to its paths since) carry to HEAD. The known pre-existing `agent::factory::tests::tools_array_stays_within_context_budget` failure is documented pre-existing on the clean tree — not a regression of this change, not flagged.
