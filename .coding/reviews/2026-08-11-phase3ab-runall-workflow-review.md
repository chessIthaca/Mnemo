# Phase 3a/3b — Workflow event completeness + Run-All halt/checkpoint UX (Quality M2/M3/M4, UI M8)

**Reviewer**: main agent (read-only review of ALL uncommitted changes per constitution)
**Date**: 2026-08-11
**Scope**: Review **all** uncommitted changes after Phase 3a + 3b work. Verify goals met without drive-by refactors. Check emission sites, sha extraction robustness, halt/rollback paths, docs (Finished vs Complete), tests, constitution, security.

## Uncommitted changes reviewed (from `git status` + `git diff HEAD`)
- `.coding/plans/39f3c881-55a0-4147-a010-9199f6e83aec.md` (plan progress marks)
- `src/agent/turn.rs` (WorkflowStateChanged for update_plan + skill_*)
- `src-tauri/src/ipc/commands.rs` (halt_run_all_for_approval preservation + on_main_turn_resolved extract use + docs)
- `src-tauri/src/ipc/run_all.rs` (extract_checkpoint_sha + extensive module docs + new extract_tests)
- Untracked plan files (bookkeeping, out of scope for code review)

Full diff inspected; no other files touched.

## Goals verification (from remediation plan context)
- Phase 3a (Quality M2): emit WorkflowStateChanged for *all* workflow mutations (update_plan, skill_start, skill_end, abandon_skill) so UI/allowed_tools/progress stay in sync. (create_plan/complete_step/abandon_plan already covered.)
- Phase 3b (Quality M3/M4, UI M8): on Run-All halt (approval/stop), preserve git checkpoint sha (previously lost to halt note). Clear "halted for approval" handling with rollback/resume. Document success criterion: terminal Finished → success/commit/Done; terminal Error → rollback/CantResolve. Explicitly: no PausedForApproval added to WorkflowState (halt via RunAllState.stop + item status + run_all cleared).

## Findings by severity

### Critical
- None.

### High
- None.

### Medium
- None.

### Low / Observations (non-blocking, within scope, no bugs)
- In `src/agent/turn.rs:771-778`: emission is correctly inside the `if result.success { ... }` block (after ToolResult), only for the listed workflow tool names. Matches "only on success paths". StepCompleted emission remains gated to complete_step only (pre-existing, correct). No emissions on error paths or for non-workflow tools.
- `src-tauri/src/ipc/commands.rs:2234-2251` (halt_run_all_for_approval): snapshots `current_note` (sha) *before* `drop(guard)` and status overwrite; constructs `note_for_status` as "sha | reason" (or plain reason if no sha). Sets Failed + clears run_all. Snapshot happens at halt site before clearing — correct. Later resolution path still sees the sha.
- `src-tauri/src/ipc/commands.rs:2140-2144` (on_main_turn_resolved Run-All arm): `checkpoint_sha = note.and_then(extract...).or_else(|| note.clone())`. Robust fallback for legacy plain-sha notes. On `success` (Finished): commit_success + Done. On `!success` (Error): rollback(sha) if present + CantResolve (or error note). Stopped flag honored (clear without next dispatch). Matches documented success criterion and "halt keeps sha usable".
- `src-tauri/src/ipc/run_all.rs:335-346` (extract_checkpoint_sha): splits on whitespace/|, takes first token, >=7 ascii hex. Returns None for manual/empty/non-sha. Handles "sha | reason", leading ws, etc.
- `src-tauri/src/ipc/run_all.rs:1-36` (module docs): accurately describe:
  - Git checkpoint + halt=preserve-sha behavior.
  - Success criterion: "terminal `Finished` ... commit_success ... `Done`"; "terminal `Error` ... rollback ... `CantResolve`"; "Halt-for-approval is not a turn failure".
  - References to on_main_turn_resolved / halt / extract.
- Dispatch path (commands.rs:2078): still sets InFlight note to plain checkpoint sha before any turn — halt/resolution preserve it.
- No PausedForApproval WorkflowState added (per explicit plan note). Halt expressed via stop + Failed item + cleared run_all.
- Tests: new `extract_tests` (4 cases: plain sha, suffix, non-sha, whitespace) added at bottom of run_all.rs. Existing run_all tests (checkpoint/commit/rollback/edge cases) untouched. Full `cargo test` (508+ tests) passes (including workflow_integration which exercises create/complete/abandon paths). Extractor tests exercised via suite.
- Constitution compliance:
  - All new public fns (`extract_checkpoint_sha`, updated `halt_run_all_for_approval`, `on_main_turn_resolved` docs) have doc comments.
  - `cargo test` run before any close step.
  - Windows git handling preserved exactly (CREATE_NO_WINDOW in prod git(), core.autocrlf=false + TestRepo in tests).
  - `git diff --check` on modified .rs files: exit 0 (no whitespace/line-ending issues introduced; CRLF warning only on .md plan file, pre-existing behavior).
  - On feature branch (`feat/bookkeeping-tools-autorun`); no direct main commits.
- Security: checkpoint/rollback unchanged in trust model (still sourced from dispatch-time item.note; no new surfaces or trust assumptions). Halt only affects Run-All in-flight item.
- Diffs are minimal/targeted: no drive-by refactors (e.g., no unrelated cleanups, no state enum changes, no moved code).

## Material diffs coverage
- All listed files + hunks reviewed line-by-line.
- Plan md: only progress checkboxes (bookkeeping).
- No other diffs in tree.

## Conclusion
**No findings.** Changes correctly and completely implement Phase 3a/3b goals. Emission sites correct (success-only), sha extraction robust (plain or "sha | ..."), halt snapshots before clear, resolution supports commit/rollback per Finished/Error, docs accurate, tests pass, constitution followed, security model preserved.

Report written to: .coding/reviews/2026-08-11-phase3ab-runall-workflow-review.md
