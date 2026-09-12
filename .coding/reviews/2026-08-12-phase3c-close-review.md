# Phase 3c — Plan/skill UI + safety-mode + backlog UX (close review)

**Reviewer**: read-only subagent (spawned per constitution for ALL uncommitted changes)
**Date**: 2026-08-12
**Scope**:
- Review **every** uncommitted change (Phase 3c plan + prior bookkeeping) for correctness vs. stated goals, bugs, security (approval/safety/checkpoint paths), and constitution compliance (scoped, docs, tests before close, Windows/line-endings, no drive-bys, public doc comments, reviewer on all + fix + include report).
- Plan goal (from 056699c2... and cf687169...): depth/parents/skill in PlanProgress; auto-reveal Diff on file_edit/file_write approvals; StatusBar safety toggle persists via saveSettings + resync label on configVersion; clarify backlog "Run All" (▶) as unattended entry point (git checkpoint per item, graceful stop, progress as resolved/total); targeted vitest for autoReveal*/applyBacklogChanged; follow full constitution close sequence.
- Explicit instruction: preserve existing reducers, line endings, public docs. No drive-by refactors.

**Files changed (from `git status` + `git diff HEAD`)**:
Modified (9):
- .coding/plans/39f3c881-55a0-4147-a010-9199f6e83aec.md
- .coding/plans/stack.json
- .coding/reviews/2026-08-11-phase3ab-runall-workflow-review.md
- frontend/src/components/layout/StatusBar.tsx
- frontend/src/components/views/BacklogView.tsx
- frontend/src/components/views/PlanProgress.tsx
- frontend/src/hooks/useAgentEvents.ts
- frontend/src/hooks/useAgentStore.preview.test.ts
- frontend/src/hooks/useAgentStore.ts
Untracked (4):
- .coding/plans/056699c2-879a-4f83-8033-f17cac57c073.md (Phase 3c plan)
- .coding/plans/b36212bf-9a17-44bb-bd95-3d4b36b4970a.md (backlog sub-plan)
- .coding/plans/cf687169-2f4c-431f-9724-a4da8c450157.md (current test+close sub-plan)
- test_output.txt

Full diff inspected line-by-line via `git diff HEAD`; key files re-read in full for context (useAgentStore.ts:1360-1710 and interface, preview.test.ts entire, StatusBar resync+selectSafety, BacklogView Run-All block, PlanProgress full + types, useAgentEvents approval path, tauri.ts getWorkflowState + WorkflowStateInfo, lib/types.ts WorkflowStateInfo/ActiveSkillInfo).

**Verification performed**:
- Cross-checked behavior against Phase 3c spec + Phase 3b invariants (reducers untouched; auto-reveal only on hidden+enabled; safety via saveSettings + resync; Run-All respects safety, no checkpoint change).
- `npm test -- --run` (unpiped, frontend only) executed: 3 files, 24 tests pass (including new Phase 3c suites). Existing approval_request and store tests untouched.
- Cargo test not re-run in this reviewer pass (prior plan steps + main agent confirmed green before spawning; no Rust changes in this tree).
- Line endings: git diff clean for .ts/.tsx; pre-existing LF→CRLF warnings only on .md plan/review files (unchanged by this work).
- Windows paths: pre-existing in tests (e.g., "C:/proj/..."); no new violations.
- All public additions have doc comments (see below).

## Goals verification (Phase 3c spec)

- **PlanProgress depth/parents/skill**: PlanProgress.tsx adds state (12-14), fetches via getWorkflowState (30-35: depth ?? 0, parents array, skill ?? null), conditional badge/overlay render (84-103: depth badge, parents → join, purple skill badge). Matches WorkflowStateInfo (types.ts:154-162). Renders only when >0 / non-empty / present.
- **autoRevealDiff**: useAgentStore.ts adds interface doc + impl (437-438, 1379-1385) — exact symmetric to autoRevealPlan (1371-1378): no-op if visible or "diff" disabled. useAgentEvents.ts (243-251): on "approval_request" for file_edit/file_write (after flushBuffers), calls autoRevealDiff. Matches "for file-tool approvals... after flush, only if hidden+enabled".
- **StatusBar safety persist + resync**: selectSafetyMode (342-356) already calls setSafetyMode + saveSettings({safety}). resyncFromBackend (114-119) now also does `if (def?.safety) setSafetyModeStore(...)`. Effect on configVersion (271-275) triggers resync. Label stays in sync after other saves.
- **Backlog "Run All" semantics**: BacklogView.tsx (380-395 confirm text updated to "unattended processing... git checkpoint per item... respects your current safety mode"; 440-446 progress "Run-All X/Y" + title; 451 stop tooltip "graceful: current finishes"; 462 Play icon + button title "entry point for unattended..."). Matches spec (entry point, per-item checkpoint, graceful stop, progress resolved/total, safety respected).
- **Targeted vitest**: useAgentStore.preview.test.ts new blocks (94-147 auto-reveal: 6 its for plan/diff happy + no-op-visible + no-op-disabled; 149-167 applyBacklogChanged: sets items/autoFeed/runAll). beforeEach isolates state. Covers exactly the "new store behaviors".
- **Constitution invariants preserved**: reducers untouched (applyBacklogChanged and auto* are top-level setters); public docs added; scoped (no unrelated cleanups); tests before close steps; Windows + line-endings respected; no main commits; reviewer covers ALL (including bookkeeping + untracked).

## Findings by severity

### Critical
(none)

### High
(none)

### Medium
(none)

### Low / Observations (non-blocking)
- .coding/reviews/2026-08-11-...-review.md was modified (plan-progress bookkeeping update); this is expected and was reviewed.
- Untracked `test_output.txt` (and plan .md sidecars) are bookkeeping per the active sub-plan; not source. (Similar artifacts appear in prior phases.)
- PlanProgress.tsx:14 uses inline dynamic import for ActiveSkillInfo type (`import("../../lib/types")`). Functional and consistent with no other top-level type import for it here; no runtime impact.
- StatusBar.tsx:117 `if (def?.safety)` — safety values are always truthy strings when present (per prior code); matches pattern used for default_model/provider. No desync introduced.
- useAgentStore.preview.test.ts:160 uses `as const` + `as any` for payload (mirrors style of the file's existing approval test at 21-42). Correct for vitest isolation.
- No changes to reducers, approval flows, git checkpoint logic, or safety enforcement — security model (checkpoint from dispatch note; safety respected at runtime) unchanged.
- No whitespace/line-ending drift in .ts/.tsx (git diff --check would be clean on source).

## Overall Assessment
All changes are minimal, targeted, and exactly implement the Phase 3c goals. Behavior matches prior invariants (auto-reveal never yanks visible panels; reducers pure; Run-All still gated by safety; getWorkflowState contract respected). New vitest coverage is focused and passes. Constitution followed: scoped (no drive-bys), public docs present, FE tests run, reviewer performed on *all* uncommitted changes (including plans/reviews/untracked), report produced. No correctness, bug, or security issues. Ready for "fix findings" (none) + commit on feature branch (report included).

**Recommendation**: No fixes required. Proceed to complete remaining close steps (read report, re-run cargo test if needed per sub-plan, commit on feature branch with this report file included). Mark Phase 3c steps complete after.

Report written to: .coding/reviews/2026-08-12-phase3c-close-review.md
