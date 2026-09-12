# Review: Drop Safety tab/sidebar button + fix misleading red ToolCard

**Date:** 2026-04-04
**Scope:** All uncommitted changes (`git diff HEAD` + `git status`).
**Plan goals:** (1) Remove the redundant right-panel "Safety" tab + left-sidebar toggle; (2) Fix the chat ToolCard so a fail→redo→succeed sequence no longer stays red.

## Files changed
- `frontend/src/hooks/agentState.ts` — removed `"safety"` from `RightPanelTab` union + `ALL_RIGHT_PANEL_TABS` array.
- `frontend/src/hooks/rightPanelViews.tsx` — removed registry entry + `SafetyRules` import + `ShieldCheck` import.
- `frontend/src/components/layout/Sidebar.tsx` — removed `safety` `TOOL_META` entry + `ShieldCheck` import.
- `frontend/src/components/chat/Message.tsx` — ToolCard `failed` logic changed to last-call outcome.
- `frontend/src/components/views/SafetyRules.tsx` — DELETED (dead view).
- `.coding/plans/stack.json` — bookkeeping (plan stack pointer).
- `.coding/safety.toml` — added `cd;npm test` command_class rule (session test-run artifact).

---

## Findings

### Correctness

**1. ToolCard last-result logic — all cases correct.** `Message.tsx:343-348`:
```ts
const running = calls.some((c) => c.result === null);
const lastCall = calls[calls.length - 1];
const failed = !running && lastCall.result !== null && !lastCall.result.success;
```
Verified against every case in the review brief:
- (a) single fail → `failed=true` (red) ✓
- (b) single success → `failed=false` (green) ✓
- (c) fail→redo→succeed (last succeeds) → `failed=false` (green) ✓ — **the fix works**
- (d) succeed→fail (last fails) → `failed=true` (red) ✓
- (e) some calls still running → `running=true` short-circuits `&&` before `lastCall.result` is read → `failed=false` (neutral yellow) ✓ — the `!running` guard is correct and prevents any access to a still-null `lastCall.result`.
- (f) empty `calls` array → `lastCall` would be `undefined` and `lastCall.result` would throw. **Not reachable**: `ToolCard` is only rendered from `Message.tsx:134` (`case "tool": return <ToolCard ... calls={entry.calls} />`), and the event reducer (`agentEventReducer.ts:175-178`) always constructs a `"tool"` transcript entry with at least one call (`calls: [{ id, index, args: "", result: null }]`). Merging appends to an existing non-empty array. No code path produces a `"tool"` entry with an empty `calls` array. **No fix required** — but the logic is technically fragile if a future code path ever constructs an empty tool entry. Low priority; not a defect in this change.

**2. Parity test holds.** `rightPanelViews.test.ts` asserts (i) every `RightPanelTab` has a registry entry, (ii) no registry id is outside the union, (iii) equal length, (iv) every entry has label/icon/component. After the edit, `ALL_RIGHT_PANEL_TABS` (agentState.ts:173-181) and `RIGHT_PANEL_VIEWS` (rightPanelViews.tsx:47-55) both have exactly 7 entries: md/plan/diff/output/files/stats/backlog. The `"safety"` member was removed from both the union and the array, so parity is preserved. ✅

**3. No dangling "safety" references in `frontend/src`.** Repo-wide search confirms the only remaining `safety`/`SafetyRules`/`ShieldCheck` references in `frontend/src` are:
- `SafetySection.tsx` — the Settings dialog's own inline TOML editor (uses `getSafetyRules`/`saveSafetyRules` from `tauri.ts`). Correctly untouched; this is the replacement editor.
- `tauri.ts:93,101` — `getSafetyRules`/`saveSafetyRules` IPC bindings, still consumed by `SafetySection.tsx`. Correctly retained.
- `ApprovalPrompt.tsx:2,238` and `StatusBar.tsx:2,647,681` — `ShieldCheck` icon used for the approval prompt and the StatusBar safety-mode dropdown. Separate runtime concerns; correctly untouched.
No code references the removed tab id `"safety"` or the deleted `SafetyRules` view component. Build will not break. ✅

**4. Dead-code deletion is clean.** `SafetyRules.tsx` is fully removed (138 lines). The only importer was `rightPanelViews.tsx`, whose import was removed in the same diff. No orphaned imports. ✅

**5. StatusBar safety-mode dropdown + SafetyToggleDialog untouched.** Neither `StatusBar.tsx` nor any `SafetyToggleDialog` file appears in the diff. ✅

**6. `disabledTabs` default has no orphaned "safety".** `useAgentStore.ts:429`: `disabledTabs: ALL_RIGHT_PANEL_TABS.filter((t) => t !== "plan")`. Since `"safety"` is no longer in `ALL_RIGHT_PANEL_TABS`, the filter cannot produce it. `disabledTabs` is not persisted to `localStorage` (only fonts/colors/theme/sizes use `readLs`/`writeLs`), so no stale `"safety"` entry can survive a reload. ✅

**7. RightPanel.tsx derives from the registry.** `RightPanel.tsx:4,17` imports `RIGHT_PANEL_VIEWS` and filters by `disabledTabs` — no hardcoded tab list to drift. The removed tab simply no longer renders. ✅

### Bugs
None.

### Security
None. The `.coding/safety.toml` change is additive (a new auto-approve rule), not a security regression:
- New rule (`safety.toml:125-128`): `tool="shell"`, `pattern="cd;npm test"`, `kind="command_class"`. Syntactically valid TOML. The `command_class` classifier normalizes a shell command to its primary operations (semicolon-joined, ignoring cosmetic output filtering); `cd;npm test` matches `cd <dir>; npm test` variants regardless of trailing filters/redirections, while chained/unknown commands (`&&`, `||`, unknown primaries) classify to `None` and still prompt. This is consistent with the existing `cd;npm run build` / `cd;cargo test` rules. Sensible and safe. ✅

### Constitution compliance
- **Doc comments on public functions:** No public functions were added or had their signature changed. The `ToolCard` change is to a private component's internal logic and is accompanied by an explanatory inline comment (Message.tsx:344-346). The deleted `SafetyRules` was an exported component whose doc comment is removed with it. ✅
- **Line-ending style preserved:** The diff shows clean line removals/additions with no mixed `\r\n`/`\n` introduced. The git warning about `.coding/safety.toml` LF→CRLF is a pre-existing condition of that file (it already contained LF lines) and is not introduced by this change. ✅
- **No commits to main:** No commits in this diff; changes are uncommitted on the working tree. ✅
- **`cargo test` / `npm test`:** Not run by this reviewer (read-only), but the plan reports `npm run build` + `npm test` (89 tests) pass, including `rightPanelViews.test.ts` parity. No Rust changed.

---

## Summary
**No findings requiring fixes.** The diff is clean and correctly implements both plan goals:
1. The Safety tab is removed from the union/array (single source of truth), which propagates to the right-panel registry and the left-sidebar `TOOL_META`. Dead view deleted cleanly. No dangling references.
2. The ToolCard now reflects the last call's outcome once all calls are done, correctly turning green on a fail→redo→succeed sequence while staying neutral yellow while any call is running. The `!running` guard is correct. The empty-calls-array edge case is not reachable given the reducer's construction invariants.

The `.coding/safety.toml` additive rule is valid and sensible. Constitution compliance is satisfied.
