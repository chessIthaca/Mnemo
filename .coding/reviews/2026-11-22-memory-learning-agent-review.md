# Review — feat/memory-learning-agent

Reviewed all uncommitted changes (`git diff HEAD`, 18 files, +515/-20). The plan set three goals: (1) a TOOL STRATEGY block in the stable prompt head, (2) wire a `SwappableProvider` so the manual `memory_consolidate` tool runs full extraction, (3) a config-gated `show_memory_activity` flag threaded through config → IPC → frontend store → reducer → rendering → Settings UI.

Overall the change is well-structured and faithful to the plan. The Rust side is clean and the consolidation fix is correct. The frontend has a few real bugs in the memory-entry parsing and the suppression-path Output-tab labeling. Findings below by severity.

---

## Correctness

### C1 — `parseMemoryEntry` recall regex never matches the actual recall output (bug)
**File:** `frontend/src/hooks/agentEventReducer.ts:195-204`

The recall tool emits results in this exact format (`src/tool/memory/mod.rs:176-186`):
```
2 memories matched:

[semantic] auth fact (score: 0.75, strength: 0.50)
  this project uses jose for JWT
```

The parser splits on `"\n\n"` and then matches the first block against:
```js
const tm = first.match(/\[(\w+)\]\s+(.+?)\s+\(score:/);
```
Two problems:
- The **first** `"\n\n"`-delimited block is the header line `"2 memories matched:"` (the header is followed by a blank line before the first entry). So `first === "2 memories matched:"`, the regex finds no `[tier]`, and `tier`/`title`/`snippet` are all `undefined`.
- Even if it matched the right block, the content-snippet regex `/\n\s+(.+)/` only captures the first line of content (content is truncated to 200 chars but may contain newlines); minor, but the primary issue is the header.

Result: when `showMemoryActivity` is on, every `memory_recall` renders as just `🧠 memory_recall ✓` with no tier/title/snippet — defeating the "readable line" goal. The `memory_write` and `memory_consolidate` paths parse fine (they key off the result string, which is correct).

**Fix:** skip the header. Either split on `"\n\n"` and take index `1` (the first real entry), or match the first `\[...\]` line directly across the whole output, e.g.:
```js
const tm = out.match(/\[(\w+)\]\s+(.+?)\s+\(score:/);
const tier = tm?.[1]?.toLowerCase();
const title = tm?.[2];
// snippet = the indented content line following the matched title line
```

### C2 — `SwappableProvider::is_configured` is dead code → fails `#![deny(warnings)]`
**File:** `src/provider/mod.rs:370-376`

`is_configured` is defined on `SwappableProvider` but never called anywhere (confirmed: no `provider_slot()` accessor exists on the factory, and the only `is_configured` call sites are on `SwappableVision`, a different type). Under the crate's `#![deny(warnings)]` this is a hard build failure (dead code). The `SwappableVision::is_configured` survives because the factory exposes `vision_slot()` and tests/IPC call it; `SwappableProvider` has no such accessor.

**Fix:** either add a `provider_slot()` accessor + a test that exercises `is_configured` (mirroring the vision pattern at `factory.rs:830-845`), or remove `is_configured` entirely. Removing is simpler and correct given nothing reads it.

### C3 — Suppressed memory tools mislabel the Output-tab entry as the wrong tool
**File:** `frontend/src/hooks/agentEventReducer.ts:334-366`

When `showMemoryActivity` is **off**, `reduceToolCallStart` suppresses the ToolCard (returns early, line 227-229), so no `tool` entry is pushed. When the matching `tool_result` arrives, the memory-finalization loop (317-333) finds no running `memory` entry (none was pushed), sets `memoryFinalized = false`, and falls through to the generic loop (334-348) — which also finds no `tool` entry (none was pushed), so `completedToolName` stays `null`.

Then the Output-tab `toolName` is computed (360-366):
```js
const toolName =
  completedToolName ??
  transcript.slice().reverse().find((e) => e.kind === "tool")?.name ??
  "tool";
```
With `completedToolName === null`, this falls back to **the most recent `tool` entry of any kind in the transcript** — i.e. the last *unrelated* tool (e.g. `file_edit`). So a hidden `memory_write` result gets logged in the Output tab under the name of whatever tool ran before it. The Output tab still logs the result (so the "still emits toolOutputEntry" requirement is met), but it's mislabeled.

This is a real correctness bug in the suppression path the plan explicitly asked about. It's low-severity (Output tab only, and only when the flag is off), but it's wrong.

**Fix:** the `tool_result` event doesn't carry a tool name (confirmed: `types.ts:108` is `{ kind: "tool_result"; tool_call_id: string; result: ToolResult }`), so the reducer can't know the name directly. Cleanest fix: in `reduceToolCallStart`, even when suppressing, record the call id→name mapping somewhere the result reducer can read (e.g. a transient `Map` on the agent state, or push a hidden marker). A lighter fix: in `reduceToolResult`, when `completedToolName` is null, default to `"tool"` instead of scanning the transcript — accepting that suppressed memory results show as generic "tool" in the Output tab (better mislabeled-as-generic than mislabeled-as-another-tool).

---

## Bugs

### B1 — `getOrCreate` does not stamp `showMemoryActivity` for events arriving before `listAgents`
**File:** `frontend/src/hooks/agentState.ts:282-287` (with `useAgentStore.ts:504-508`)

`getOrCreate` returns `agents[id] ?? emptyAgentState()`, and `emptyAgentState()` hardcodes `showMemoryActivity: false` (`agentState.ts:277`). The flag is only stamped onto an agent in the `listAgents` handler (`useAgentStore.ts:506-508`), which creates the agent with `{ ...emptyAgentState(), showMemoryActivity: s.showMemoryActivity }`.

But `applyAgentEvent` calls `getOrCreate` for **any** event kind, including `tool_call_start` for a memory tool. If a memory tool event arrives for an agent before `listAgents` has registered it (e.g. a freshly-spawned subagent whose first action is a memory write, racing the `listAgents` refresh), `getOrCreate` returns a bare `emptyAgentState()` with `showMemoryActivity: false` — so the memory entry is suppressed even though the user opted in. The `setShowMemoryActivity` sync (615-624) only fixes agents already in the map; it doesn't help a not-yet-registered agent.

This is an edge case (subagent whose very first event is a memory tool, before listAgents lands), and the consequence is only that one memory line is hidden until the agent is registered. Low severity, but it's a gap in the "sync the flag onto all existing agents" requirement.

**Fix:** either have `getOrCreate` read the store flag (it can't — it's a pure function taking only `agents`), or stamp the flag in `applyAgentEvent` after `getOrCreate` when the agent was freshly created. Simplest: in `applyAgentEvent`, after `const agent = getOrCreate(...)`, if `agent` was just created (compare reference to `agents[agentId]`), spread the store flag onto it before passing to the reducer. This mirrors how `listAgents` already does it.

### B2 — `arePropsEqual` for `memory` is correct but the `default` case returns `false` (pre-existing, not a regression)
**File:** `frontend/src/components/chat/Message.tsx:61-69, 70-72`

The new `memory` case compares all six fields correctly. Note the `default: return false` means any unrecognized kind always re-renders — this is the existing pattern (not introduced here) and is safe. No action needed; noted for completeness.

---

## Security

**No findings.** No secrets, API keys, or paths are leaked. The `SwappableProvider` holds an `Arc<dyn LlmClient>` (already trusted, shared with the agent loop) — no new trust boundary. The memory-entry parser operates on tool-result strings already shown in the Output tab; no injection surface (React escapes by default). Config field is a plain bool with no path/URL semantics. Windows-path compliance: all Rust paths use `PathBuf`/`std::path`; no hardcoded Linux paths introduced.

---

## Constitution compliance

### K1 — `is_configured` dead code violates the warning-free build rule (also listed as C2)
**File:** `src/provider/mod.rs:370-376`

The project constitution requires a warning-free build under `#![deny(warnings)]` and explicitly forbids `#[allow(...)]` to silence it. `SwappableProvider::is_configured` is uncalled dead code → `dead_code` warning → build failure. This must be fixed (remove the method or add a caller) before `cargo test` will pass. This is a blocking constitution issue, not a style nit.

### Doc comments — compliant
All new public functions have doc comments: `SwappableProvider::{new,set,get,is_configured}` (mod.rs:350-376), `MemoryConsolidateTool::{new,with_provider}` (memory/mod.rs:206-227), `UiConfig::show_memory_activity` field doc (general.rs:213-217), `GetSettingsUi::show_memory_activity` (settings.rs:594-595), `SettingsSaveDto::show_memory_activity` (settings.rs:930-932). Frontend JSDoc present on `parseMemoryEntry`, `MEMORY_TOOLS`, `setShowMemoryActivity`, the `showMemoryActivity` state field, and `readShowMemoryActivity`. ✓

### No `#[allow(...)]` suppressions — compliant
None added. ✓

### Tests — present and appropriate
- `stable_head_carries_tool_strategy` (prompt.rs) ✓
- `show_memory_activity_defaults_false` + `show_memory_activity_round_trips` (general.rs) ✓
- `consolidate_with_provider_runs_full_extraction` + `consolidate_without_provider_is_synthetic_only` (memory/mod.rs) ✓ — these correctly assert the no-provider path does NOT claim full extraction.
- DTO fixtures updated in both test cases (settings.rs:1480, 1551) + `contract_fixtures.rs` ✓
- **Gap:** no test asserts the *with-a-real-provider* path produces the "semantic/procedural" message. The two new tests both exercise the `None`-provider fallback. A test with a mock `LlmClient` (the factory tests already define `MockProvider`) asserting the full-extraction message would close the loop on goal #2. Not blocking — the wiring is correct by inspection — but recommended.

---

## Summary of actionable findings

| ID | Severity | File | Issue |
|----|----------|------|-------|
| C1 | Bug (correctness) | agentEventReducer.ts:195-204 | Recall parser matches the header line, not the first entry → memory_recall lines render empty. |
| C2/K1 | **Blocking** (constitution + correctness) | provider/mod.rs:370-376 | `is_configured` is dead code → `#![deny(warnings)]` build failure. Remove it or add a caller. |
| C3 | Bug (correctness) | agentEventReducer.ts:360-366 | Suppressed memory tools mislabel Output-tab entry as the last unrelated tool. |
| B1 | Bug (edge case) | agentState.ts:282-287 | `getOrCreate` doesn't stamp `showMemoryActivity` for agents not yet in the map. |

**Clean areas:** prompt strategy block (goal 1) ✓, swappable-provider wiring / no locks held across await (goal 2 — `slot.get()` snapshots under a read lock, released before the `consolidate_session().await` call; no deadlock) ✓, config round-trip ✓, IPC get/save/apply/fixture ✓, Settings UI checkbox + draft + commit ✓, `setShowMemoryActivity` syncs onto all existing agents ✓, memo equality for `memory` kind ✓, reducer purity ✓, security ✓, doc comments ✓, no `#[allow]` ✓.

Recommend fixing C2/K1 (blocking) and C1 before commit; C3 and B1 are low-severity but should be addressed in the same pass.
