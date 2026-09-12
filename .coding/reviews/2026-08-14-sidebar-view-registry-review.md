# Review: Sidebar VIEW_BY_ID registry fix (d40c078, feat/sidebar-view-registry)

## Scope

One source file changed: `frontend/src/components/layout/Sidebar.tsx` (TOOL_META replaced with VIEW_BY_ID derived from RIGHT_PANEL_VIEWS). Bookkeeping files under `.coding/plans/*` excluded from review.

## Findings

### Correctness — no findings

- `VIEW_BY_ID.get(tab)!` at `Sidebar.tsx:39` cannot return undefined at runtime. The vitest file `rightPanelViews.test.ts` asserts bidirectional parity: every `ALL_RIGHT_PANEL_TABS` id has a registry entry (test 1), the registry has no unknown ids (test 2), and lengths match (test 3). A fourth test asserts every entry has a non-empty label, icon, and component.
- `ALL_RIGHT_PANEL_TABS` (agentState.ts:203-214) is a hardcoded array independent of the registry. This is the documented design — the file header comment at `rightPanelViews.tsx:12-15` explicitly states drift is "caught at test time, not prevented by derivation." The 10-element union type `RightPanelTab` (agentState.ts:190-200) matches the 10 registry entries 1:1 (`md, plan, diff, output, files, stats, trace, backlog, browser, game`).

### Bugs — no findings

- No leftover references to `TOOL_META` anywhere in the frontend (searched all 107 frontend files; the only hit is the doc comment inside `Sidebar.tsx:10` explaining the history).
- No leftover references to any removed icon imports (`FileText, ListChecks, GitCompare, Terminal, FolderTree, BarChart3, Activity, Inbox, Globe, Gamepad2`) in `Sidebar.tsx`.
- The lucide import block at `Sidebar.tsx:1` contains exactly `Code2, PanelRight, Settings, Folder` — all four are used in the component.
- `meta.icon` and `meta.label` usage at `Sidebar.tsx:40-51` matches the `RightPanelView` interface shape (`{ id, label, icon, component }`) at `rightPanelViews.tsx:33-42`.

### Security — no findings

Pure UI mapping change. No user input, no IPC, no dynamic evaluation.

### Constitution compliance — no findings

- The doc comment on `VIEW_BY_ID` (Sidebar.tsx:8-11) accurately describes what it is, why it exists, and what it replaced.
- No `#[allow]`, `eslint-disable`, `@ts-ignore`, `@ts-expect-error`, or `@ts-nocheck` suppressions found in `Sidebar.tsx`.
- Build is warning-free (`tsc --noEmit` exit 0, confirmed by the requester; not rerun per read-only constraint).

## Verdict

**No findings.** The change is clean, correct, and well-guarded by the existing parity test. The sidebar can no longer drift out of sync with the right-panel view registry without a test failure catching it.
