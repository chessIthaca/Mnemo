# Review: Diff tab — changed-files dropdown per top-level plan

**Date:** 2026-04-16
**Branch:** feat/diff-tab-plan-file-dropdown
**Reviewer:** automated review subagent

## Summary

The diff adds a `top_plan_id` field to the `WorkflowStateChanged` event (Rust + TS), a new `planDiffs` list in the frontend store that accumulates one `LastDiff` per path (deduped, newest first), a `selectedDiffPath` user override, and a dropdown header in the DiffViewer for browsing per-plan changed files. The list resets exactly when the root plan id changes for the main agent.

All tests pass (cargo test warning-free under `#![deny(warnings)]`, tsc + vite build, 131 vitest tests).

---

## Correctness

### 1. Reset semantics — CORRECT

The dispatcher (`agentEventReducer.ts:682-698`) handles `workflow_state_changed`:

- `topPlanId` is unconditionally updated from any agent's event (line 685). This is intentional: agents share the root plan, so the tracker converges.
- The **reset** (lines 690-697) fires only when BOTH conditions hold:
  - `agentId === selectMainAgentId(s.agentParents, s.agents)` — main-agent-only gate.
  - `event.top_plan_id !== s.topPlanId` — the root plan id actually changed.

**Main-agent detection with empty maps:** `selectMainAgentId` (`agentState.ts:258-268`) returns `null` when both `agentParents` and `agents` are empty. Since `agentId` in the dispatch is always a `number` (AgentId = `u64` → TS `number`), the equality `agentId === null` is `false`, so the reset is correctly skipped when no agents are registered. ✓

**Sub-agent events cannot reset:** A subagent's `agentId` will not equal the main agent's id (the parentless agent with the smallest id), so the gate blocks the reset. The `topPlanId` tracker does get updated by the subagent's event (line 685), but this is harmless: the tracker is just a reference point for the *next* comparison, and the test at line 685 confirms the subagent event correctly does NOT clear `planDiffs`. ✓

**Sub-plan push/pop:** The root plan id is `stack.first()` (`src/workflow/mod.rs:221`), constant across push/pop, so no reset fires. Skill transitions don't change the stack root either. ✓

### 2. Serde contract — CORRECT

- `SerializableAgentEvent` uses `#[serde(tag = "kind", rename_all = "snake_case")]` (`channels.rs:226`).
- The struct variant fields are `state: WorkflowState` then `top_plan_id: Option<String>` in declaration order (`channels.rs:257-265`).
- `WorkflowState::Executing` serializes as `"executing"` (lowercase, from `rename_all = "snake_case"` on the `WorkflowState` enum).
- `Option<String>` serializes as a plain string when `Some` (no `skip_serializing_if` on this field).
- The golden fixture `event-workflow-state-changed.json` has `"kind": "workflow_state_changed", "state": "executing", "top_plan_id": "plan-1a2b3c"` — matching the contract test's `SerializableAgentEvent::WorkflowStateChanged { state: WorkflowState::Executing, top_plan_id: Some("plan-1a2b3c".into()) }` (`contract_fixtures.rs:169-172`).
- The contract test uses `serde_json::to_value` → `Value` comparison (`contract_fixtures.rs:47`), which is key-order-independent, so field order doesn't affect equality. ✓
- The TS type (`types.ts:119-127`) has `kind: "workflow_state_changed"`, `state: WorkflowState`, `top_plan_id: string | null` — matching the Rust shape. ✓
- `src-tauri/src/ipc/events.rs:188` destructures `SerializableAgentEvent::WorkflowStateChanged { state }` — this still compiles because Rust struct-variant patterns support partial destructuring (the new `top_plan_id` field is ignored). ✓

### 3. DiffViewer logic — CORRECT with one cosmetic observation

Traced through all branches:

**Pending file approval (live):** `showPending = pendingApproval !== null && isFileEdit && selectedEntry === null` (line 186). When the user hasn't picked a past diff, `selectedEntry` is `null` → `showPending` is `true` → the pending diff renders. ✓

**User pick freezes:** When the user picks a path from the dropdown, `selectDiffPath(path)` sets `selectedDiffPath`, `selectedEntry` finds the match, `showPending` becomes `false`, and `shownEntry = selectedEntry` renders that file. ✓

**Fallback to newest:** When no selection and no pending file approval, `shownEntry = fallbackEntry = entries[0]` (the newest). ✓

**Non-file pending approval:** The `else if (pendingApproval && !isFileEdit)` branch (line 209) shows "Approval pending: X / No file diff for this tool." — not the empty state. ✓

**Empty entries:** When `entries` is empty and no pending approval, `showDropdown` is `false`, `shownEntry` is `null`, `header` is `null`, `body` is `undefined` → the empty state renders. ✓

**Stale selection after dedupe:** `entries.find((e) => e.path === selectedDiffPath) ?? null` (line 177). If the selected path was removed by a reset (`selectedDiffPath` is also reset to `null` at line 695, so this shouldn't happen in practice), the `?? null` fallback handles it gracefully — `selectedEntry` becomes `null` and `shownEntry` falls through to `fallbackEntry`. ✓

**file_append content:** `DiffEntryBody` (line 76) checks `entry.content !== undefined` → renders via `NewFileView`. In `reduceToolResult` (line 316-317), `content` is populated from `previewContent ?? parsedArgs.content`. For `file_append`, the args have a `content` field, so this works. ✓

**Applied/failed badge:** The entry header shows "applied" (green check) or "failed" (red X) based on `entry.success` (lines 229-239), which comes from `event.result.success` in the reducer (line 319). A failed `file_edit` (success: false) lands in `planDiffs` with `success: false` and displays the "failed" badge. ✓

**Cosmetic observation (not a bug):** The dropdown `value` (line 251) uses `selectedEntry?.path ?? (showPending ? "" : (fallbackEntry?.path ?? ""))`. When `showPending` is true, the value is `""` which matches the `<option value="">pending: ...</option>` — this correctly highlights the pending option. When the user selects this option (`value === ""`), `onChange` maps it to `selectDiffPath(null)`, which sets `selectedDiffPath = null` → `selectedEntry = null` → `showPending` returns to `true`. The round-trip is correct. ✓

### 4. Reducer correctness — CORRECT

**`upsertPlanDiff` dedupe:** `[d, ...list.filter((e) => e.path !== d.path)]` (line 104). One entry per path, newest first, re-edits replace and move to front. The test confirms: `edit("a.ts")`, `edit("b.ts")` → `["b.ts", "a.ts"]`; `edit("a.ts")` again → `["a.ts", "b.ts"]` with length 2. ✓

**`lastDiff` effect for existing consumers:** The `Effects.lastDiff` field is still emitted (`lastDiff != null ? { lastDiff, planDiff: lastDiff } : {}` at line 327). The dispatcher applies it via `effects && "lastDiff" in effects ? (effects.lastDiff ?? null) : s.lastDiff` (line 680-681). Existing consumers reading `lastDiff` from the store are unaffected. ✓

**No double-bookkeeping:** The `planDiff` effect is consumed only by the dispatcher (line 674-676: `upsertPlanDiff(s.planDiffs, effects.planDiff)`). It doesn't leak into any other state field. ✓

**`lastDiff != null` narrowing:** The old code used `lastDiff !== undefined`; the new code uses `lastDiff != null`. Since `lastDiff` is typed as `LastDiff | null | undefined` and is only ever assigned an object or left `undefined`, both checks are equivalent in practice. The `!= null` form is strictly safer (also catches `null`). No behavior change. ✓

---

## Bugs

No bugs found.

---

## Security

No security concerns. The `top_plan_id` is a UUID string generated internally by the workflow engine; it carries no user input and is used only for identity comparison in the frontend reset logic. No injection, XSS, or data-leak vectors introduced.

---

## Constitution compliance

- **No `#[allow(...)]` suppressions added.** Searched the entire diff — no `#[allow` attribute appears in any changed file. ✓
- **Build warning-free under `#![deny(warnings)]`.** `cargo test` passed (green `test result:` lines), which under the crate-level `#![deny(warnings)]` at both `src/lib.rs` and `src-tauri/src/main.rs` proves zero warnings. ✓
- **No commits to main.** Work is on `feat/diff-tab-plan-file-dropdown`. ✓
- **Public Rust fns have doc comments.** `Workflow::top_plan_id()` (`src/workflow/mod.rs:212-222`) has a thorough doc comment explaining the contract. The `AgentEvent::WorkflowStateChanged` variant's `top_plan_id` field has a doc comment (`channels.rs:121-127`). The `SerializableAgentEvent::WorkflowStateChanged` variant's `top_plan_id` field has a doc comment (`channels.rs:259-264`). ✓
- **`.coding/plans/stack.json` change:** Harness-internal bookkeeping (plan stack id + removed skill definition). Nothing alarming. ✓
- **Untracked `.coding/plans/8b7ae055-....md`:** The current plan's file. Expected. ✓

---

## Edge cases verified

| Edge case | Status | Detail |
|-----------|--------|--------|
| Failed `file_edit` (`success: false`) lands in `planDiffs` | ✓ | `reduceToolResult` doesn't gate on `event.result.success`; the snapshot includes `success: false` and the UI shows a "failed" badge |
| Identical path re-edits replace older snapshot | ✓ | `upsertPlanDiff` filters by path and prepends; test confirms |
| `selectedDiffPath` pointing at removed path | ✓ | `entries.find(...) ?? null` → `selectedEntry = null` → falls back to `fallbackEntry` or empty state |
| No agents registered (empty maps) | ✓ | `selectMainAgentId` returns `null`, `agentId === null` is `false`, reset skipped |
| Sub-agent `workflow_state_changed` with different root id | ✓ | Main-agent gate blocks reset; test confirms |
| Skill transitions (same root id) | ✓ | `top_plan_id` unchanged → `event.top_plan_id === s.topPlanId` → no reset |
| Sub-plan push/pop (same root id) | ✓ | `stack.first()` constant → no reset |
| `file_append` auto-reveal | ✓ | `useAgentEvents.ts:249` includes `file_append` in the auto-reveal check |
| `file_append` in DiffViewer `isFileEdit` | ✓ | `DiffViewer.tsx:168` includes `file_append` |
| `file_append` in reducer capture | ✓ | `agentEventReducer.ts:288-290` includes `file_append` |
| Empty `entries` with pending non-file approval | ✓ | `showDropdown = false`, body shows "Approval pending: X" |
| Dropdown round-trip (pick pending option) | ✓ | `value=""` → `selectDiffPath(null)` → `selectedEntry = null` → `showPending = true` |

---

## Conclusion

**No findings.** The diff is clean: correct reset semantics, exact serde contract match, sound DiffViewer branch logic, proper reducer dedupe, no constitution violations, and comprehensive test coverage for the new behavior (including sub-agent isolation, dedupe ordering, and reset-on-new-plan).
