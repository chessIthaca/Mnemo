# Phase 6c — Docs + remaining debt (Maint M5/M6/M7) review

**Date:** 2026-08-11
**Reviewer:** read-only subagent
**Scope:** all uncommitted changes (`git diff HEAD` + 3 untracked files)
**Files changed:** `src/agent/factory.rs`, `frontend/src/components/layout/RightPanel.tsx`, `frontend/vitest.config.ts`, `PLAN.md`, `.coding/plans/*.md`, `.coding/plans/stack.json`; **new:** `frontend/src/hooks/rightPanelViews.tsx`, `frontend/src/hooks/rightPanelViews.test.ts`, `.coding/plans/4e06e0f0-….md`

**Verification performed:**
- `cargo test --workspace` → 522 lib + 1 contract + 9 ipc_bridge + 3 provider (ignored) + 5 workflow-integration + 49 tauri, all pass (exit 0).
- `npx vitest run` (frontend) → 82 tests pass (incl. the 4 new `rightPanelViews.test.ts`).
- `npx tsc --noEmit` → clean (exit 0).
- Cross-checked every tool's `fn name()` in `src/tool/**` against the test's expected list.
- Compared the old flat `build_registry` body (from the diff `-` lines) against the 6 new grouped helpers.
- Verified PLAN.md architecture claims against the codebase (`spawn_blocking`, `restrict_permissions`, `SerializableAgentEvent`, `DenyAll`, IPC module split, `sub_agent_has_plan_mutations_denied`).

---

## Correctness

**No findings.**

- **M5 — factory grouping is behavior-preserving.** The old flat registration sequence and the new 6 grouped helpers register the *exact same* tools under the *exact same* conditionals:
  - `register_agent_tools`: FileRead, FileEdit, FileWrite, FileAppend, Shell, Search, Git (always) ✓
  - `register_workflow_tools`: CreatePlan, CompleteStep, UpdatePlan, AbandonPlan (always) ✓
  - `register_skill_tools`: SkillStart, SkillEnd, AbandonSkill (only `if let Some(skills)`) ✓
  - `register_vision_tool`: DescribeImage (always) ✓
  - `register_memory_tools`: MemoryWrite, MemoryRecall (only `if let Some(store)`) ✓
  - `register_spawn_tool`: SpawnAgent (only `if let Some(spawner)`, parent-aware when `agent_id` is `Some`) ✓
  The only difference is registration *order* (vision now registers before memory). `ToolRegistry` is a `HashMap` keyed by `tool.name()` (`src/tool/mod.rs:248-267`), so order has zero behavioral effect. No tool was dropped, added, or re-conditioned.
- **M5 — the 18 expected tool names are all correct.** Each was verified against its `fn name()` impl: `file_read` (file_read.rs:51), `file_edit` (file_edit.rs:174), `file_write` (file_write.rs:36), `file_append` (file_append.rs:39), `shell` (shell.rs:91), `search` (search.rs:93), `git` (git.rs:131), `create_plan` (plan.rs:66), `complete_step` (plan.rs:258), `update_plan` (plan.rs:152), `abandon_plan` (plan.rs:346), `skill_start` (skill.rs:64), `skill_end` (skill.rs:172), `abandon_skill` (skill.rs:220), `describe_image` (describe_image.rs:146), `memory_write` (memory/mod.rs:65), `memory_recall` (memory/mod.rs:128), `spawn_agent` (spawn_agent.rs:69). The list correctly *omits* `memory_consolidate`, which is defined (`memory/mod.rs:208`) but never registered in the factory (pre-existing, unchanged by this phase).
- **M5 — count assertion is safe.** `registry.iter().count()` uses a `HashMap`, so iteration order is non-deterministic, but `.count()` and `.get(name).is_some()` are both order-independent — the test cannot false-fail on ordering. The exact-count assertion *will* fail if a tool is added later without updating the expected array; this is the intended guard (it forces the test author to acknowledge a new registration), and is acceptable per the phase goal ("guards against a registration being silently dropped").
- **M5 — second test's memory claim is correct.** `make_factory_with` (factory.rs:477-503) passes `Some(store)` (line 494), so a bare `make_factory` *does* wire memory → `memory_write`/`memory_recall` are present, while skills + spawner are absent. The test asserts exactly this.
- **M6 — registry renders identically.** `RIGHT_PANEL_VIEWS` (rightPanelViews.tsx:45-54) has the same 8 ids, labels, icons, and order as the old `TABS` array (RightPanel.tsx, removed). The content-body lookup (`RIGHT_PANEL_VIEWS.find(v => v.id === shownTab)`) correctly renders the active view's component, and the `shownTab === undefined` "All tools are off" fallback (RightPanel.tsx:121-125) is preserved. The disabled-tabs default is intact: `disabledTabs: ALL_RIGHT_PANEL_TABS.filter((t) => t !== "plan")` (useAgentStore.ts:403) — all tabs disabled except "plan", unchanged.
- **M6 — no circular-import risk.** `rightPanelViews.tsx` imports `RightPanelTab` (type-only) from `./agentState` (line 17); `agentState.ts` does **not** import from `rightPanelViews.tsx` (verified by reading the full file). The dependency is one-way. `ALL_RIGHT_PANEL_TABS` remains a hardcoded array in `agentState.ts:95-104` and is re-exported via `useAgentStore.ts:111`; `Sidebar.tsx:14` still imports it from there — all intact.

## Bugs

**No findings.**

- No behavior drift in the right panel: same tabs, same enable/disable logic, same fallback, same close-button behavior (`toggleTabAndReveal`).
- The `RIGHT_PANEL_VIEWS.find()` body lookup runs on every render (O(8)) — negligible; and the vitest guarantees every `RightPanelTab` has an entry, so `find` always succeeds for a valid `shownTab` (the `if (!view) return null` guard is defensive only).
- The 2 compiler warnings (`variable does not need to be mutable` at `src/runtime/agent.rs:448` and `:511`) are **pre-existing** — `src/runtime/agent.rs` is not in this diff. Not introduced by Phase 6c.

## Security

**No findings.**

- This phase is docs/registry/test-only: no new mutation paths, no new IPC surfaces, no auth/permission changes. The factory grouping preserves the exact registration set (verified above), so no tool-gating regression. The right-panel registry is pure display metadata with no security surface.

## Constitution compliance

**1 finding (low severity — line endings on new files).**

- **New frontend files use LF; the existing frontend codebase uses CRLF.** `frontend/src/hooks/rightPanelViews.tsx` and `frontend/src/hooks/rightPanelViews.test.ts` are LF-only, while sibling files (`agentState.ts`, `RightPanel.tsx`, `vitest.config.ts`) are CRLF (verified via `Get-Content -Raw … -match "\r\n"`). The project constitution requires: "Never introduce mixed `\r\n`/`\n` endings" and "Preserve the line-ending style of existing files." The file tools normalize content to an *existing* file's detected style, but for brand-new files there is no existing style to detect, so they were written with LF. **Fix:** re-save both new files with CRLF endings (e.g. `((Get-Content -Raw file) -replace "(?<!`r)`n", "`r`n") | Set-Content -NoNewline file`, or write via an editor that matches the repo's CRLF convention). All *modified* existing files (`factory.rs`, `RightPanel.tsx`, `vitest.config.ts`, `PLAN.md`) correctly retained CRLF — only the two new files are affected.

**Otherwise compliant:**
- **Doc comments on public functions.** The 6 new `register_*` helpers in `factory.rs` are *private* (`fn`, not `pub fn`) but carry doc comments anyway (good practice). `build_registry` (private) gained an expanded doc comment. The new public TS exports `RightPanelView` (interface) and `RIGHT_PANEL_VIEWS` (const) in `rightPanelViews.tsx` both have doc comments (lines 27-37, 39-44). The `RightPanelView` fields are individually documented. ✓
- **No drive-by refactors.** The diff is tightly scoped to the three M5/M6/M7 changes + plan bookkeeping. No unrelated code was touched. The factory's registration *set* is unchanged (only reorganized). ✓
- **No commits to main.** No git operations performed by this phase (changes are uncommitted on the feature branch). ✓

## Documentation accuracy (PLAN.md)

**1 finding (low severity — stale test count).**

- **PLAN.md states "520 lib" but the actual count is now 522.** The two new factory tests (`expected_tool_names_registered_when_fully_wired`, `conditional_tools_absent_without_their_wiring`) raised the lib count from 520 → 522 (confirmed: `cargo test` reports `running 522 tests` / `522 passed`). PLAN.md line: `**Test counts:** 520 lib + 49 tauri (IPC) + 5 workflow-integration + 82 vitest (frontend)`. The other three counts are correct (49 tauri ✓, 5 workflow-integration ✓, 82 vitest ✓). **Fix:** change "520 lib" → "522 lib".

**All other PLAN.md architecture claims verified true:**
- "Multi-agent runtime … each spawned agent owns its own `AgentLoop` … `Workflow` + `ToolRegistry`" — true (factory.rs `build_registry` is per-build; `two_builds_produce_independent_workflows` test guards it).
- "only the main agent may mutate plans (sub-agents are denied)" — true (`sub_agent_has_plan_mutations_denied` test, factory.rs:755).
- "`keys.toml` is written with user-only permissions (Unix `0600` / Windows DACL)" — true (`restrict_permissions` in `src/config/keys.rs:136`, Unix `set_mode(0o600)` line 141, Windows DACL `restrict_permissions_windows` line 166).
- "All synchronous file-system I/O … runs inside `tokio::task::spawn_blocking`" — true (file_read.rs:103, file_edit.rs:226, file_write.rs:79, file_append.rs:85, search.rs:137, describe_image.rs:107).
- "`SerializableAgentEvent` Agent→UI" — true (`src/runtime/channels.rs:180`).
- "`DenyAll` latches at the turn level" — true (`src/agent/dispatch.rs:42`, `src/agent/turn.rs:75`).
- "IPC layer (`src-tauri/src/ipc/`) is split into domain modules (agent, settings, files, backlog, spawn, events, run_all, state)" — true (all 8 files present + `contract_fixtures.rs`).
- "right panel is driven by a single view registry (`rightPanelViews.tsx`)" — true (this phase).
- Terminology tool-category paths are accurate (`tool/agent/`, `tool/workflow/plan.rs`, `tool/workflow/skill.rs`, `tool/memory/`, `tool/agent/spawn_agent.rs`).

**Minor (informational, not a finding):** PLAN.md Terminology lists `memory_consolidate` under Memory tools. That tool *exists* in `tool/memory/mod.rs` but is *not registered* in the factory (pre-existing, unchanged by this phase). The Terminology section describes the tool *category/files*, not the registered set, so this is arguably accurate — but a reader could infer `memory_consolidate` is available at runtime. Not a Phase 6c regression; noting for completeness.

## rightPanelViews.tsx comment accuracy

**1 finding (low severity — overstated claim).**

- **The module comment overstates how `ALL_RIGHT_PANEL_TABS` stays in sync.** `rightPanelViews.tsx:10-12` says: *"`ALL_RIGHT_PANEL_TABS` is now *derived* from this registry's ids, so the two can never drift."* This is **factually incorrect** — `ALL_RIGHT_PANEL_TABS` is still a *hardcoded* array in `agentState.ts:95-104`, not derived from `RIGHT_PANEL_VIEWS`. What *is* true is that the vitest (`rightPanelViews.test.ts`) asserts bidirectional consistency (every tab has a registry entry and vice-versa, plus equal length), so drift is *caught at test time* — but it is not *prevented by derivation*. **Fix:** correct the comment to say `ALL_RIGHT_PANEL_TABS` is kept in sync *by the vitest*, not *derived* (e.g. "kept in sync by `rightPanelViews.test.ts`, which asserts every `RightPanelTab` has a registry entry and vice-versa"). This is a doc-accuracy issue only; no runtime impact (the test does enforce the invariant).

---

## Overall verdict

**APPROVE with 3 low-severity fixes recommended before commit:**

1. **Line endings (constitution):** re-save `frontend/src/hooks/rightPanelViews.tsx` and `frontend/src/hooks/rightPanelViews.test.ts` with CRLF to match the existing frontend codebase.
2. **PLAN.md test count:** change "520 lib" → "522 lib".
3. **rightPanelViews.tsx comment:** correct the "derived" claim to "kept in sync by the vitest".

All three are low-severity (line-ending hygiene + doc accuracy). The core changes — M5 factory grouping (behavior-preserving, well-tested), M6 right-panel registry (renders identically, no circular imports, disabled-tabs default intact), M7 PLAN.md refresh (architecture claims verified true) — are **correct, complete, and constitution-compliant**. `cargo test` (522+49+5+…), `vitest` (82), and `tsc --noEmit` all pass clean. No correctness bugs, no security issues, no behavior drift.
