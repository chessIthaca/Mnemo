# Review: Fix stuck-open reasoning panel (missing collapse triangle)

**Date:** 2026-08-12
**Scope:** All uncommitted changes (`git diff HEAD`).
**Feature file:** `frontend/src/components/chat/InflightBar.tsx`
**Other changed files (bookkeeping, skimmed):** `.coding/backlog.json`, `.coding/plans/c0387ccf-*.md`, `.coding/plans/stack.json`, `.coding/plans/3f157dac-*.md` (untracked).

## Summary of the change

Three coordinated edits in `InflightBar.tsx`, all changing the toggle gate from
`hasActivity` to `expanded || hasActivity`:

1. `onClick` (line 149–151): `() => { if (expanded || hasActivity) setExpanded(!expanded); }`
2. `className` hover/cursor (line 152–154): `expanded || hasActivity ? "hover:bg-bg-tertiary" : "cursor-default"`
3. Chevron render condition (line 176): `{(expanded || hasActivity) && (...)}`

The empty-state `—` placeholder (line 320) was intentionally left unchanged.

---

## Correctness

**no findings** — the fix is correct and complete.

Verified all four state combinations against the new `(expanded || hasActivity)` gate:

| State | `expanded` | `hasActivity` | Chevron shown | Chevron icon | onClick behavior | Correct? |
|-------|-----------|---------------|---------------|--------------|------------------|----------|
| open + no activity | true | false | yes | ChevronDown | collapses → false | ✓ |
| closed + activity | false | true | yes | ChevronRight | expands → true | ✓ |
| open + activity | true | true | yes | ChevronDown | collapses → false | ✓ |
| closed + no activity | false | false | no | — | no-op | ✓ |

**Original bug confirmed fixed:** the `startDrag` path (line 113–122) calls
`setExpanded(true)` unconditionally when `!expanded`. After that call,
`expanded` becomes `true`, so the chevron condition `(expanded || hasActivity)`
evaluates `true` even when `hasActivity` is false → the ChevronDown renders and
the bar is collapsible via click. The stuck-open state (open + empty + no
chevron + click does nothing) can no longer occur.

**`showBar` vs. chevron alignment:** `showBar` (line 130–131) is true whenever
`running || hasActivity || hasTokens || hasContext || showTokenUsage`. The bar
can therefore render while `hasActivity` is false (e.g. `showTokenUsage` on, or
`hasTokens`/`hasContext` true). In exactly that scenario the old code hid the
chevron; the new code shows it because `expanded` is the controlling term once
the panel is open. Correct.

---

## Bugs

**no findings** — the three conditions are aligned and consistent.

- **onClick / className / chevron alignment:** all three use the identical
  expression `expanded || hasActivity`. The button looks interactive
  (`hover:bg-bg-tertiary`) exactly when it acts (toggles), and the chevron is
  present exactly when a toggle is possible. No drift between the three.
- **Drag handle still works:** `startDrag` (line 113) is untouched and still
  calls `setExpanded(true)` when `!expanded`, then sets `dragging.current =
  true`. The resize `useEffect` (line 91–111) is untouched. No regression.
- **No leftover `hasActivity`-only gating:** searched the file — the remaining
  `hasActivity` references (lines 86, 131, 315) are all correct and unrelated
  to the toggle gate:
  - line 86: definition `activityLog.length > 0` — correct.
  - line 131: `showBar` includes `hasActivity` as one of several OR terms —
    correct (bar visibility, not toggle).
  - line 315: panel body renders activity lines vs. the `—` placeholder —
    correct (content gating, intentionally left as-is per the plan).
- **`expanded` persistence across agents/turns (pre-existing, not introduced
  by this change — informational only):** `InflightBar` is rendered as
  `{activeState && <InflightBar state={activeState} />}` in `App.tsx:398`
  with no `key` prop. `activeState` is the agent object from
  `useAgentStore` (`App.tsx:83–85`). Switching the active agent changes the
  `state` prop but does **not** remount the component (no `key` change), so
  `expanded`/`height` persist across agent switches. This is pre-existing
  behavior unaffected by this diff; the fix neither worsens nor improves it.
  Not a blocker for this change.

---

## Security

**no findings.** UI-only change; no new inputs, network calls, `dangerouslySetInnerHTML`,
or state mutations beyond a local `setExpanded` boolean toggle. No injection or
XSS surface introduced.

---

## Constitution compliance

**no findings.**

- **Public function doc comments:** `InflightBar` is an exported component. The
  diff does not alter the component's signature or remove any doc structure;
  helper functions (`formatTokens`, `formatRate`) retain their doc comments.
  The new inline comment block (lines 172–175) documents the non-obvious
  `expanded` term — good practice, satisfies the "public functions must have
  doc comments" spirit for the changed logic.
- **Windows 11 / PowerShell:** no shell or path changes in this diff; N/A.
- **No commits to main:** this is a review of uncommitted working-tree changes;
  no commit has been made. The plan's closing sequence (test → review → fix →
  commit to feature branch) is in progress. No violation.
- **Line-ending style:** the diff introduces only LF content into a file the
  tooling normalizes; the `file_edit` tool preserves the file's detected style.
  No mixed endings introduced.
- **Bookkeeping files** (`.coding/backlog.json` status `pending`→`in_flight`,
  plan checkbox flips, `stack.json` stack pointer swap, untracked plan file)
  are expected agent state transitions — no constitution issue.

---

## Overall verdict

**ship.**

The fix is minimal, correct, and internally consistent across all three
coordinated sites (`onClick`, `className`, chevron render). It directly
eliminates the stuck-open state by ensuring the collapse chevron and
click-to-toggle are available whenever the panel is open — including the
`startDrag`-opened-but-empty path that caused the original bug. No correctness,
bug, security, or constitution findings. The pre-existing `expanded`-persists-
across-agents behavior is noted for awareness only and is out of scope for this
change.
