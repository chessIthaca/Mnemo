## Verdict: PASS

Review of ALL uncommitted changes (`git diff HEAD` + untracked) on `wt/agenticcoding` for plan bc09914f / backlog 68c4c9a5 — the `show_knowledge_activity` setting (default ON). 19 files changed (Rust backend + TS/TSX frontend + tests + backlog bookkeeping). Method: full `git diff HEAD`, read of every changed source file, the reducer that builds transcript entries, the steering-stats marker table, and the prompt.rs test suite.

### 1. Correctness — default true end-to-end ✓

| Layer | Site | Value |
|---|---|---|
| Rust Default | `UiConfig::default()` (general.rs:359) | `show_knowledge_activity: true` |
| Rust serde | `#[serde(default)]` on `UiConfig` (general.rs:312) | missing field in old config.toml → Default → `true` |
| Frontend store seed | `useAgentStore.ts:605` | `showKnowledgeActivity: true` |
| Hydration | `App.tsx` `settings.ui.show_knowledge_activity !== false` | absent/`true`→ON, `false`→OFF |
| Fixture JSON | `dto-get-settings.json` | `"show_knowledge_activity": true` |
| Contract fixture | `contract_fixtures.rs:231` | `show_knowledge_activity: true` |

The `!== false` pattern is correct backward-compat: `UiConfig` has `#[serde(default)]` so an older config.toml without the key deserializes to `true` (Default), and `GetSettingsUi.show_knowledge_activity` is a non-optional `bool` always populated from the config — the frontend always receives a boolean. The `!== false` is belt-and-suspenders for the pre-hydration window (store seed `true`).

### Render filter logic ✓

`Conversation.tsx`: `showToolActivity || !isActivityEntry(entry) || (showKnowledgeActivity && isKnowledgeActivityEntry(entry))`

Truth table verified:
- `showToolActivity=true` → renders everything (short-circuit). ✓
- `showToolActivity=false` + non-activity entry (user/assistant/error/steer/qa) → `!isActivityEntry` true → renders. ✓
- `showToolActivity=false` + activity but NOT knowledge (shell/read_files/file_edit/vision/skill) → `!isActivityEntry` false, `isKnowledgeActivityEntry` false → hidden. ✓
- `showToolActivity=false` + knowledge activity + `showKnowledgeActivity=true` → third clause true → renders. ✓
- `showToolActivity=false` + knowledge activity + `showKnowledgeActivity=false` → third clause false → hidden. ✓

`isKnowledgeActivityEntry` is a proper subset of `isActivityEntry` (memory ⊂ activity; graph_ tool ⊂ tool ⊂ activity), so the filter is logically sound. No runtime risk: tool entries always carry `name` (required by the `TranscriptEntry` type), so `entry.name.startsWith("graph_")` is only reached when `kind === "tool"` (narrowed).

### 2. isKnowledgeActivityEntry coverage ✓

`agentState.ts:442`: `entry.kind === "memory" || (entry.kind === "tool" && entry.name.startsWith("graph_"))`

Verified against the actual reducer (`agentEventReducer.ts`):
- **graph_* tool calls** (graph_search/context/impact/path): NOT in `MEMORY_TOOLS` set (line 327-336), so `reduceToolCallStart` pushes them via the generic path at line 562-566 with `kind: "tool"`. Predicate matches via `name.startsWith("graph_")`. ✓
- **Memory tool calls** (memory_write/search/consolidate/update/supersede/delete): all in `MEMORY_TOOLS`; `reduceToolCallStart` (line 496-514) pushes `kind: "memory"`. Predicate matches via `kind === "memory"`. ✓
- **Auto-recall entries**: `reduceMemoryRecalled` pushes `kind: "memory"`, `name: "auto-recall"` (line 1165). Predicate matches via `kind === "memory"`. ✓
- **Excludes**: shell/read_files/file_edit (`kind:"tool"`, name not `graph_*`) → false; vision (`kind:"vision"`) → false; skill (`kind:"skill"`) → false; conversation kinds → false. ✓

The doc comment's claim that "`memory` entries cover BOTH memory tool calls and auto-recall entries" is accurate — confirmed by reading the reducer.

### 3. Prompt guidance ✓

New TOOL_STRATEGY bullet (prompt.rs:227-228): *"Auto-recalled memories are injected each turn — use them silently; if they are not useful to the current task, do not narrate or apologize for them."*

**Steering-marker safety**: checked against the full `MarkerKind::marker()` table in `steering_stats.rs` (10 markers): `is an indexed symbol`, `TIP: for file-content search`, `No symbols matched`, `RECALLED CONTEXT`, `SYMBOL NUDGE:`, `TIP: pattern has no regex metacharacters`, `known memory hit:`, `working-memory events accumulated this session`, `TIP: output redirection detected`, `Re-read the file`. **None appear in the new bullet.** Additionally, `observe_result` is tool-scoped and scans only tool RESULTS (not the system prompt), so the bullet cannot self-fire steering_stats regardless.

**Byte-stable head / ordering**: the bullet is a pure INSERTION between the memory_search bullet (ends "…the search is for a GAP, not a reflex.") and the memory_consolidate bullet — no existing text reordered or rewritten. Verified against all pinning tests:
- `tool_strategy_lists_graph_tools_before_search` (line 1437): `graph < search` still holds (insertion is before both). ✓
- `stable_head_byte_stable_across_workflow_changes` (line 1538): `build_stable_head` is workflow-independent (const only); identical across states. ✓
- `stable_head_carries_tool_strategy` (line 1176): new assertion `head.contains("do not narrate or apologize")` added (line 1212-1215); all existing assertions hold. ✓
- `stable_head_carries_memory_records_block` (line 1219): `TOOL_STRATEGY < MEMORY RECORDS` still holds. ✓
- `stable_head_carries_phase3_retrieval_habits` (line 1271): memory_search bullet text unchanged; `strategy < pack < records` still holds; `!TOOL_STRATEGY.contains("memory_supersede")` still holds. ✓

### 4. Constitution checks ✓

**(a) Documentation sync**: The precedent toggle `show_tool_activity` is NOT documented in `README.md` or `PLAN.md` (verified — no matches outside code/.coding). The README documents high-level features (memory, graph, workflow), not individual Settings → Chat toggles. `show_knowledge_activity` follows the same pattern; no doc update required. All new public items carry doc comments (`isKnowledgeActivityEntry`, `UiConfig` field, `GetSettingsUi` field, store field, `tauri.ts` field). The `setShowKnowledgeActivity` setter follows the existing grouped-setter pattern (no individual doc, matching `setShowToolActivity`).

**(b) Multi-platform neutrality**: All changes are config/UI/prompt code — no Windows-only APIs, paths, or shell syntax. ✓

**(c) No `#[allow(...)]`**: None added. ✓

**(d) Public functions documented**: `isKnowledgeActivityEntry` has a thorough doc comment; all Rust struct fields have doc comments. ✓

### 5. Bugs/security — no missing wiring ✓

Searched every `show_tool_activity` / `showToolActivity` occurrence (Rust `*.rs` + TS/TSX) and confirmed each has a corresponding `show_knowledge_activity` / `showKnowledgeActivity`:
- Rust: struct fields (general.rs, patch.rs, settings_dto.rs, settings.rs), Default, apply blocks, populate, 2 wire-test literals, contract fixture, `save_round_trips_all_sections` literal + assertion. ✓
- TS/TSX: `tauri.ts` (GetSettingsUi + SettingsSavePatch), `useAgentStore.ts` (interface + seed + setter), `App.tsx` hydration, `types.ts` ChatDraft, `ChatSection.tsx` (read/save/checkbox), `Conversation.tsx` (import + store read + filter), `agentState.ts` predicate. ✓
- Test seeds: `useAgentStore.test.ts:44`, `ipc-contract.test.ts:112`, `ChatSection.test.ts:79` (ChatDraft base). ✓
- Fixture: `dto-get-settings.json`. ✓

No literal constructs the new shape without the field. The `SettingsPatch` / `SettingsSaveDto` use `Option<bool>` with `#[serde(default)]` (optional patch semantics — absent = no change), correct.

### Tests

- `Conversation.test.ts`: source pins (store read + filter usage) + pure `isKnowledgeActivityEntry` predicate tests covering graph_*→true, memory→true (tool call + auto-recall), shell/read_files/file_edit/vision/skill→false, conversation kinds→false. Matches the repo's established source-contract pattern (no DOM test infra; the `show_tool_activity` precedent uses the same approach).
- `general.rs`: `show_knowledge_activity_defaults_true`, `show_knowledge_activity_round_trips`, `save_round_trips_all_sections` updated.
- `settings.rs`: 2 wire-test literals + assertion updated.
- `ChatSection.test.ts`: 4th-toggle checkbox/persist/commit assertions + `serializeChat` base updated.

### Backlog bookkeeping

`backlog.jsonl` item 68c4c9a5: text extended with the prompt-guidance requirement; note set to "steered by the user, returned to queue"; status remains `pending` (correct — the plan's step 4, which marks it done, is the current closing-sequence step).

**No findings. The implementation is correct, complete, and constitution-compliant.**
