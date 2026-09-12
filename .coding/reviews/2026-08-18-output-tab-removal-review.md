# Review — Output right-panel tab removal (plan c9b8580f)

**Scope:** ALL uncommitted changes (`git diff HEAD`), excluding pre-existing dirty `.coding/` bookkeeping (backlog.json, stack.json, analysis/*, 2d142320 plan) as instructed. Changed files: PLAN.md, frontend/src/components/views/ToolOutput.tsx (deleted), frontend/src/hooks/{agentState.ts, rightPanelViews.tsx, agentEventReducer.ts, useAgentStore.ts, useAgentStore.test.ts, useAgentStore.preview.test.ts}, frontend/src/lib/ipc-contract.test.ts.

**Verified test state (reported by main agent):** frontend vitest 236/236, `npm run build` (tsc strict + vite) green, root `cargo test` 1068/1068.

---

## Findings

### Minor — stale doc comment referencing the removed Output tab (documentation / constitution: doc comments must be accurate)

**Where:** `frontend/src/hooks/agentEventReducer.ts:747-751` — `reduceError` doc comment:

```
 * error: preserve partial text, surface the error in the transcript +
 * activity log, stop the agent on a final (non-retrying) error, and log to
 * the Output tab so the raw stream / error text is debuggable there.
```

The last clause ("and log to the Output tab…") describes the deleted `toolOutputEntry` effect; the Output tab no longer exists. The reducer's *behavior* is correct (see check #2 below) — only the doc is stale. **Fix:** drop the final clause, e.g. end at "…stop the agent on a final (non-retrying) error."

Severity: minor (no runtime impact), but the plan explicitly included removing Output-tab references and this one survived; per project style, public functions' doc comments should match behavior.

---

## Verification of the requested checks

1. **No dangling references — PASS.** Repo-wide grep for `toolOutputLog|ToolOutputEntry|ToolOutput|truncateOutput|truncateError` and literal `"output"`: **zero hits in frontend/src**. Remaining matches are only in `.coding/` plans/reviews (historical docs, expected) and `frontend/src/lib/ipc-fixtures/event-tool-result.json:4` — that `"output": "ok"` is the tool-result payload field (unrelated to the tab id). Note: any live `tab === "output"` comparison would have failed `tsc` since `"output"` is no longer in the `RightPanelTab` union, and tsc is green.

2. **Reducer error path — PASS.** `reduceError` (agentEventReducer.ts:752-776): transcript `error` entry (capped via `capTranscript`) + `activityLog` entry both intact; `running = false` only when `!event.retrying`; effects are `!event.retrying ? { planVersionBump: true } : {}` — final error still bumps `planVersion`, retrying error bumps nothing and leaves the agent running. Matches the pre-change semantics minus the removed log append. Test coverage retained at useAgentStore.test.ts ("error (final): adds error entry, stops the agent, and bumps planVersion" asserts transcript + `running === false` + `planVersion` bump; the retrying-error test was untouched).

3. **reduceToolResult — PASS.** (agentEventReducer.ts:384-445) `resolvedPreview` snapshot taken *before* `pendingApproval` clearing (385-393); `toolName = completedToolName ?? "tool"` fallback retained (396) and still used for the lastDiff `toolName` field (429); file_edit/file_write/file_append diff capture with `resolvedPreview` diff/content/path preference preserved (403-439); effects still conditionally spread `{ lastDiff, planDiff }` (443).

4. **Registry↔union parity — PASS.** rightPanelViews.test.ts is generic/length-based (3 assertions: every union id has an entry, every entry id is in the union, lengths equal) — holds with the 9-entry registry. `Terminal` icon import and `ToolOutput` component import both removed from rightPanelViews.tsx; tsc (strict, noUnusedLocals would flag dead imports) is green, so no dead imports remain.

5. **truncateOutput/truncateError deletion justified — PASS.** Grep confirms zero remaining callers anywhere in frontend/src (only historical .coding docs mention them). They existed solely to shape Output-tab log entries.

6. **Constitution compliance — PASS (aside from the stale doc above).**
   - No `#[allow(...)]` / eslint suppressions introduced (pure deletion diff).
   - All touched public functions retain doc comments; `useAgentStore` AppState fields' doc comments removed together with the fields (correct).
   - Line endings: git's CRLF normalization warnings for PLAN.md, agentState.ts, rightPanelViews.tsx, useAgentStore.preview.test.ts, useAgentStore.ts show those files still carry CRLF in the working tree (preserved); agentEventReducer.ts, useAgentStore.test.ts, ipc-contract.test.ts were already LF — unchanged style. No mixed-ending introduction.
   - Zero-warning build holds (green `cargo test` under `#![deny(warnings)]`; green tsc). No Rust changes, so root `cargo test` coverage is appropriate.
   - Comment updates in agentState.ts:330-333 and useAgentStore.test.ts:961-964 correctly de-reference the deleted `slice(-50)` cap without losing the tradeoff note (export loses pre-cap history still documented).

7. **Session-only tab state — PASS.** `rightPanelTab` and `disabledTabs` are hardcoded in the store initializer (`rightPanelTab: "plan"`, `disabledTabs: ALL_RIGHT_PANEL_TABS.filter(t => t !== "plan")`) with no `readLs`/localStorage read for either (only `LS_RIGHT_PANEL_WIDTH` persists, per appearance.ts key list + App.tsx localStorage usage — all geometry/theme/colors/memory flags). No localStorage key can hold a stale `"output"` tab; no migration needed. The `disabledTabs` default can no longer produce `"output"` since it's derived from the trimmed `ALL_RIGHT_PANEL_TABS`.

## Summary

The removal is complete and surgical: union member, registry entry + icon/component imports, view file, store state field + re-export, reducer constructions + dispatcher apply, helpers, test fixtures/assertions, and PLAN.md docs all removed consistently; error/tool-result reducer semantics preserved. **One minor finding** (stale "log to the Output tab" clause in reduceError's doc comment) — trivially fixed with a one-line edit; no correctness, bug, or security issues.
