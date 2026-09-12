# Review: Per-tab close (x) button on right-panel tool tabs

**Date:** 2026-04-04
**Reviewer:** primary agent (reviewer subagent finished without writing a report; review performed inline)
**Scope:** all uncommitted changes (`git status` / `git diff HEAD`)

## Files changed

- `frontend/src/components/layout/RightPanel.tsx` — the feature
- `.coding/backlog.json`, `.coding/plans/stack.json`, `.coding/plans/96107550-...md` — bookkeeping (backlog status flip, plan stack, plan file); expected, not bugs.

## What the change does

Each enabled tool tab in the right panel now renders a small "x" close button
(a `<span role="button" tabIndex={0}>`, since a nested `<button>` is invalid
inside Radix's `TabsTrigger` which itself renders a `<button>`). Clicking the x
calls the existing store action `toggleTabAndReveal(tab.id)`, which disables
(toggles off) that tab, falls back to the first enabled tab if the active one
was disabled, and hides the panel when the last tool is turned off. The tab can
be re-enabled from the left toolbar (`Sidebar.tsx`), which already calls the
same action. The x is hidden by default and revealed on `group-hover`/`focus`.

## Findings

### Correctness
- **Event propagation — correct.** `onPointerDown` calls `stopPropagation()`,
  which prevents Radix `TabsTrigger`'s pointerdown-based selection (automatic
  activation mode) from firing when the x is clicked, so the tab is disabled
  rather than selected. `onClick` also `stopPropagation()`s before toggling.
  `onKeyDown` handles Enter/Space with `preventDefault` (Space) +
  `stopPropagation`. ✅
- **Disabling the active tab — correct.** `toggleTabAndReveal` falls back to
  the first enabled tab and hides the panel when none remain
  (`useAgentStore.ts:1186-1209`). ✅
- **Disabling a non-active tab — correct.** No fallback needed; panel stays
  visible. ✅

### Accessibility
- `role="button"`, `tabIndex={0}`, `aria-label`, `title`, and Enter/Space key
  handling are present. ✅
- **Minor (non-blocking):** the span's `tabIndex={0}` places each tab's close
  button in the tab order even for non-selected tabs (Radix gives non-selected
  `TabsTrigger`s `tabindex=-1`, but the nested span is always `0`). Acceptable
  for a desktop app; the close button remains keyboard-reachable.

### Security
- No new inputs, no injection surface. ✅

### Constitution compliance
- No new public functions added (no new doc-comment obligation). ✅
- `cargo test` passes (437 + 9 + 5). ✅
- `npx tsc --noEmit` passes (frontend has no test runner). ✅

## Verdict
**No blocking findings.** The diff is clean and correct.
