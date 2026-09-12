# Code Review — Phase 5b: Split useAgentStore god object into pure modules

**Reviewer:** read-only reviewer (spawn_agent)
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree per `git status` / `git diff HEAD`:
- NEW `frontend/src/hooks/agentState.ts`
- NEW `frontend/src/hooks/appearance.ts`
- NEW `frontend/src/hooks/agentEventReducer.ts`
- REWRITTEN `frontend/src/hooks/useAgentStore.ts` (thin facade)
- REWRITTEN `frontend/src/hooks/useAgentStore.test.ts` (table-driven vitest)
- MODIFIED `frontend/vitest.config.ts` (include the test in the suite)
- `.coding/plans/*` bookkeeping (ignored)

**Verification performed:**
- Read all four `frontend/src/hooks/*.ts` modules in full.
- Compared every reducer + the `applyAgentEvent` dispatcher + `handleAgentEvent`
  against `git show HEAD:frontend/src/hooks/useAgentStore.ts` (line-by-line for
  the moved logic).
- Enumerated every consumer import of `useAgentStore` across `frontend/src`
  (28 files) and cross-checked each named/type import against the facade's
  re-export block.
- Checked import graph for cycles (no cycles).
- Ran `npm test` (vitest): **4 files, 49 tests passed** (exit 0).
- Ran `npx tsc --noEmit`: **passed** (exit 0).
- Checked line-ending style of every touched/new file (CRLF vs LF counts).
- Checked every exported function in the new modules has a doc comment.

---

## Correctness / behavior preservation — NO findings

The split is behavior-preserving.

1. **Exports completeness.** Every symbol consumers import from
   `useAgentStore` is re-exported by the facade. The full set of consumer
   named/type imports is:
   `useAgentStore`, `emptyAgentState`, `selectMainAgentId`,
   `ALL_RIGHT_PANEL_TABS`, `applyFontVars`, `applyTheme`, `applyColors`,
   `applyCodeColors`, and types `PendingApproval`, `AgentState`,
   `ActivityEntry`, `RightPanelTab`, `Theme`.
   All of these appear in the facade's `export { … }` / `export type { … }`
   blocks (`useAgentStore.ts` lines 96–158). No consumer imports
   `LastDiff`/`TokenUsage`/`Effects`/`AppState`/`AppStateLike` from the
   facade, so those being facade-local / reducer-local is fine.

2. **`applyAgentEvent` dispatcher single write path.** Identical to HEAD:
   `toolOutputLog` append + `slice(-50)` (line 572), `lastDiff` via
   `"lastDiff" in effects` (line 574), `planVersionBump` (575),
   `workflowStates` merge (576–578), `agentNames` merge (579–581), and the
   `removeAgent` path for `exited` (550–564) with `selectMainAgentId`
   fallback. Verified byte-for-byte against HEAD.

3. **Steer scheduling side-effect.** `handleAgentEvent`
   (`useAgentStore.ts` 821–854) is byte-identical to HEAD: it captures
   `effects` from `applyAgentEvent`, then schedules the `setTimeout`
   removal using `effects?.scheduleSteerRemoval?.steerId` gated on
   `event.kind === "suggestion_injected"`. The `setTimeout` ownership
   moved correctly — reducer stays pure, dispatcher owns the timer.

4. **`AppStateLike` interface.** Declares exactly the slices the dispatcher
   touches (`agents`, `agentNames`, `agentParents`, `activeAgent`,
   `workflowStates`, `toolOutputLog`, `lastDiff`, `planVersion`).
   `AppState extends AppStateLike` (line 161) and `tsc --noEmit` passes, so
   the structural conformance holds with no drift.

5. **`setCodeColor`** moved to `appearance.ts` and generalized to
   `<S extends Record<CodeColorKey, string>>`. The function body is
   unchanged; the facade's `setCode*Color` setters still pass zustand's
   `get`/`set`, and `tsc` confirms the generic constraint is satisfied.

6. **`reduceToolResult` `completedCall` type** narrowed from
   `ToolInvocation | null` to `{ args: string } | null` — only `.args` is
   read (line 266), which exists on `ToolInvocation`. Runtime behavior
   unchanged.

---

## Bugs — 1 finding (low severity)

### B1 (low): Dead/duplicated `export let nextSteerId` in agentEventReducer.ts
**File:** `frontend/src/hooks/agentEventReducer.ts:50`
`export let nextSteerId = 1;` is exported but **never imported or used** by
any consumer. The facade (`useAgentStore.ts:95`) declares its own local
`let nextSteerId = 1` and uses it in `addSteer` (line 724). At HEAD there was
a single `let nextSteerId = 1` used by `addSteer`; the split duplicated it.

**Impact:** None on behavior — both start at 1 and increment identically, so
`addSteer` id generation is unchanged. But the exported mutable in the
reducer is misleading dead code: it implies the reducer owns steer ids, yet
it does not, and exporting a `let` invites accidental external mutation.

**Suggested fix:** Remove line 50 (`export let nextSteerId = 1;`) from
`agentEventReducer.ts`. The reducer never reads it; the facade's local copy
is the sole source of truth. (If the intent was to share it, the facade
should `import { nextSteerId }` instead — but since nothing in the reducer
uses it, removal is correct.)

---

## Security — NO findings

The changed files contain only pure state-reduction, localStorage/DOM
appearance helpers, and store wiring. No new network calls, no `eval`, no
dynamic property access on untrusted input, no changes to the approval
gate. The `JSON.parse(completedCall.args || "{}")` in `reduceToolResult`
(unchanged from HEAD) is wrapped in try/catch and only feeds a `path` string
into a display snapshot — no command execution.

---

## Constitution compliance — NO findings

- **Public functions have doc comments:** verified every `export function`
  in `agentState.ts`, `appearance.ts`, and `agentEventReducer.ts` has a
  `/** … */` doc comment immediately above it. (Type aliases `Reducer<E>`
  and `ReducerResult` lack comments, but the rule applies to *functions*,
  not type aliases.)
- **Line-ending style:** Repo has `core.autocrlf=true` and no
  `.gitattributes`; git stores LF and checks out CRLF on Windows. The
  modified `useAgentStore.ts` (CRLF=857) and `useAgentStore.test.ts`
  (CRLF=385) preserve their CRLF working-copy style. The three NEW files
  are LF-only on disk — consistent with how `core.autocrlf` normalizes new
  files (git's "LF will be replaced by CRLF" warnings on commit are the
  repo's normal behavior, also emitted for `vitest.config.ts`). No file
  has mixed `\r\n`/`\n` endings. No violation.
- **No direct commits to main:** All Phase 5b changes are uncommitted;
  `git log` shows HEAD at the Phase 5a commit (`43a91a4`). The agent has
  not run `git merge`/`git push` to main. No violation.
- **`cargo test` / `npm test`:** This is a frontend-only change; `npm test`
  (vitest) passes (49/49). No Rust changes in this diff.

---

## Summary

The Phase 5b split is a clean, behavior-preserving refactor. The reducer
logic, dispatcher write path, steer side-effect wiring, and the full export
surface that ~80 consumers depend on are all preserved verbatim. `tsc
--noEmit` and `npm test` both pass. The only finding is a low-severity dead
`export let nextSteerId` in `agentEventReducer.ts` that should be removed
for clarity (it has no behavioral effect).