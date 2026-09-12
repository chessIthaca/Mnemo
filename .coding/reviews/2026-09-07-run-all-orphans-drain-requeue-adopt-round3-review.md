## Verdict: PASS

Round-3 verification of commit b908a34 (HEAD on wt/agenticcoding; tree verified clean — `git diff HEAD` and `git status --short` both empty, so every file read reflects the committed tree). The round-2 LOW finding (incomplete supersession forward pointers) is fully resolved: both prescribed edits are present, accurate against the implemented behavior, and content-complete. No new issues introduced — the commit touches only the two knowledge files plus the round-2 review report itself (expected closing-sequence bookkeeping); no source, config, or frontend changes.

### Round-2 finding — RESOLVED

**(a) In-body SUPERSEDED banner on the 2026-12-06 spec — present, accurate, house-shaped.** `.coding/knowledge/spec/2026-12-06-backlog-status-plan-lifecycle.md:7` now carries the banner; the diff (`@@ -4,6 +4,8 @@`) is a pure two-line insertion (banner + blank line) — the entire original body (lines 9-20, including the historical drain sentence at line 15 and the file's own backward supersedes note at line 20) is byte-preserved. The banner mirrors the 2026-08-31 house shape exactly — `SUPERSEDED {date} by {amendment} ({plan id}, {successor path}): {what stands} — but {what changed}: {delta}. {old claim} is historical (pre-{plan})`:
- names the successor: `.coding/knowledge/spec/2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md` (plan 5f6515f5);
- states what stands: "the whole status⇄plan-lifecycle contract stands";
- states the delta: exit drain REQUEUES to Pending (checkpoint sha preserved) instead of keeping InFlight, run-all start adopts crash-orphaned InFlight items, exit clears single_in_flight;
- marks the old claim historical: the "the item keeps its status" drain claim is historical (pre-5f6515f5).

**(b) Decision record Contract line — repointed with amendment context.** `.coding/knowledge/decision/2027-01-07-run-all-intervention-pauses-the-item-kept-inflig.md:7` now ends "Contract: .coding/knowledge/spec/2027-01-07-backlog-item-status-plan-lifecycle-failed-means.md (the 2027-01-07 successor of the 2026-12-06 file — the run-all-orphans plan 5f6515f5 further amended the dead-owner paths: exit drain requeues to Pending, run-all start adopts orphans, exit clears single_in_flight)." The diff replaces only that one sentence; the rest of the decision body is byte-identical — no content lost from either file.

### Banner claims vs. the implemented behavior — all verified

- **Drain requeues to Pending, sha preserved, note snapshotted first**: `drain_run_all_on_main_exit` (`run_all.rs:2253-2285`) — note snapshot at 2271-2275 strictly before `store.transition(&id, BacklogStatus::Pending, Some(full))` at 2282 (transition overwrites the note; the `"{n} | {suffix}"` shape keeps the checkpoint sha at the note head); the store guard drops before `end_run`'s awaits.
- **Run-all start adopts crash-orphaned InFlight items**: `adopt_orphaned_in_flight` (`run_all.rs:2318-2354`) — InFlight-only filter (2340) excluding the live single-dispatch pointer (2341) and the main agent's active root plan (2342), note snapshotted before each transition (2344, 2352); called from `backlog_run_all` (`backlog_cmds.rs:432`) after the already-active check (424-426) and before the pending count (433-441), so adopted items count toward the run total.
- **single_in_flight read AFTER the store lock (round-1 LOW-1 ordering)**: store lock at 2320, the single read at 2327-2332 under the guard, with the LOW-1 comment at 2321-2326 — unchanged by b908a34 (no source touched), re-confirmed on the committed tree.
- **Exit clears single_in_flight**: `events.rs` Exited arm — `was_main` captured under the mgr lock (654), `if was_main` (681) runs the drain (682) then sets `single_in_flight = None` (683-687).
- The banner's delta is verbatim-consistent with the successor spec's own amendment note (line 18) and amended drain sentence (line 13) — one contract, one story, three surfaces (banner, successor spec, decision Contract line) all agreeing.

### Memory hygiene — clean

Exactly ONE live SPEC record for the status⇄plan-lifecycle contract: 68312f14, pointing at the successor path. The old record 45e4d970 remains marked [superseded] — its digest now carries the banner text (naming the successor and the delta), so even the history record forwards correctly. Not resurrected, no duplicates. The live b83e891f DECISION record (1c16f119) derives from the amended file.

### Whole-repo pointer sweep — no stale live pointer remains

Every remaining reference to `2026-12-06-backlog-status-plan-lifecycle` is either (i) the house immediate-successor chain — the 2026-08-31 steering spec's banner and the 2026-09-01 resolution-paths spec's banner point at the 2026-12-06 file as their immediate successor (the sanctioned pattern; each hop now carries its own in-body warning and forwards), (ii) the 2026-09-01 steer-interrupt decision's "Current contract" line — that record is itself `status = "superseded"` (historical), and its pointer now lands on the bannered file, (iii) the successor's own `supersedes` frontmatter, or (iv) dated review reports (historical artifacts). No LIVE record routes a reader to the old drain semantics without an in-body warning — the round-2 reader impact is fully closed.

### No new issues from b908a34

The commit's diff is exactly three files: the two knowledge-file edits above plus the new round-2 review report (`.coding/reviews/2026-09-07-run-all-orphans-drain-requeue-adopt-round2-review.md` — the closing sequence commits the review report with the fix). Zero source/config/frontend changes → no code-regression surface; the reported green matrix (src-tauri 239+4 passed, exit=0, zero warnings under deny(warnings)) is consistent, as the code is byte-unchanged from the round-2-verified state. HEAD is b908a34 on wt/agenticcoding with a clean tree.

### Constitution

Documentation sync: this WAS the finding, and it is now closed — every surface (superseded-spec banner, successor spec, decision Contract line, module/fn docs, events.rs comment) is updated, accurate, and mutually consistent; README/PLAN.md do not cover drain semantics at this granularity. Multi-platform neutrality: no code touched — pure Markdown knowledge-file edits. Security: no inputs, parsing, or attack surface. Style: the banner mirrors the established house shape verbatim.
