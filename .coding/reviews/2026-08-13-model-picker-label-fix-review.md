# Review: Fix — model picker label not updating after switch

**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + untracked):
`src-tauri/src/ipc/agent.rs`, `frontend/src/components/layout/StatusBar.tsx` (the fix),
plus pre-existing `build.bat`, `frontend/src/components/layout/Sidebar.tsx`,
`.coding/plans/stack.json`, and two untracked plan files.

**Verdict:** The fix is correct. Both changes target the root cause precisely and the
pre-existing changes are consistent. Two minor findings below (one frontend consistency
gap on the effort path, one doc nit); nothing blocking, no security issues.

---

## Correctness verification of the fix

### 1. Backend — `set_resolved_model(None)` in `set_model` (agent.rs:492-499)

- **Every loop is cleared, unconditionally.** The call sits inside
  `for agent_loop in agent_loops.values()` with no early `continue`/conditional, so no
  path leaves a stale override. ✓
- **`AgentLoop::set_resolved_model` exists and is `pub`** (src/agent/loop_impl.rs:431-436),
  takes `Option<String>`, mutates through `&self` via a `Mutex` — callable through the
  `agent_loops` map values without extra imports in agent.rs. No warning risk. ✓
- **`list_agents` preference chain** (agent.rs:376-379): `resolved_model().unwrap_or_else(|
  | provider().model())` — after the clear, it reports the NEW provider's model. Verified
  against `AgentLoop::provider()` (loop_impl.rs:680-685), which reads the provider slot
  that `set_provider` (loop_impl.rs:701-711) just swapped. ✓
- **Turn re-resolution:** `resolve_turn_provider` (loop_impl.rs:502-551) re-records
  `resolved_model` on EVERY path (forced → 519, no-resolver → 528, no-override → 539,
  override → 549), so the next turn re-establishes any per-context override. The comment
  on the new code ("The next turn re-resolves any override and re-records it") is accurate. ✓
- **In-flight-turn race:** a turn in flight resolved its provider before `set_model` ran
  and does not re-resolve mid-turn; it finishes against its starting provider (documented
  on `set_provider`, loop_impl.rs:697-700). The clear cannot be re-staled by that turn. ✓
- **Forced-model subagents** transiently report the new default model (not their forced
  model) between the switch and their next turn. Benign, self-correcting, and strictly
  better than leaving a stale value — acknowledged design trade-off, NOT a finding.
- **No other `set_provider`-on-loop callers** exist (only agent.rs:493), and no other
  writer of `resolved_model` besides `resolve_turn_provider` and this new clear. ✓

### 2. Frontend — `listAgents` refresh in `selectModel` (StatusBar.tsx:316-318)

- **Success-path only:** inside the `try`, after `await setModel` resolves — a failed
  switch takes the `catch` (which re-syncs via `resyncFromBackend`) and never refreshes
  `agentModels`. ✓
- **Non-fatal:** `void promise.then(...).catch(console.error)` — no unhandled rejection,
  no impact on the switch result. ✓
- **`registerAgents` no-clobber semantics preserved** (useAgentStore.ts:491-509): the
  `if (info.model)` guard keeps prior values for absent models; present models overwrite.
  The existing test "registerAgents: populates agentModels from info.model (absent model
  keeps prior)" (useAgentStore.test.ts:687-708) pins this. The new call doesn't alter it. ✓
- **Picker highlight untouched:** highlight keys off store `model === m && provider ===
  ep.name` (StatusBar.tsx:480, 486) — `agentModels` is not involved. ✓
- **`listAgents` is exported from `lib/tauri`** (already imported by App.tsx:9 and
  useAgentEvents.ts:29) and the `useAgentStore.getState().registerAgents(infos)` pattern
  mirrors the existing name-resolution path (useAgentEvents.ts:195-198). ✓
- **Rapid-switch race considered, not a bug:** `list_agents` reads live loop state at
  execution time (not a request-time snapshot), so any `listAgents` executing after the
  latest `set_model`'s lock section reports the latest model. The only staleness window
  is sub-millisecond (a read that executes between two `set_model` runs but resolves
  last) and self-corrects on the next refresh/turn. Negligible. ✓
- **Mid-turn switch works:** the refresh reports the new provider's model immediately,
  even while an agent is streaming. ✓

### 3. Pre-existing changes (reviewed, no issues)

- **build.bat:** path change `frontend\node_modules\.bin\tauri.cmd` →
  `node_modules\.bin\tauri.cmd` is consistent with npm-workspace hoisting, and the script
  runs `call npm install` at the root first (build.bat:6), so the hoisted CLI exists by
  the time it's invoked. The updated comment matches the new behavior. ✓
- **Sidebar.tsx Game tab:** `game` is a member of `RightPanelTab` and
  `ALL_RIGHT_PANEL_TABS` (agentState.ts:200, 213) and `TOOL_META` is
  `Record<RightPanelTab, ...>` — without this entry the frontend would not compile.
  `GameView` exists and is registered (rightPanelViews.tsx:60). Complete and consistent. ✓
- **`.coding/plans/stack.json` + two untracked plan files:** workflow bookkeeping
  (AutoRun-sandboxed state); the diff reflects a completed merge_to_main skill and the
  new active plan. ✓

### 4. Constitution compliance

- No `#[allow(...)]` suppressions added (verified in the diff).
- No new public functions, so the doc-comment rule is not engaged by the fix itself (see
  Finding 2 for a pre-existing doc that aged).
- Comment style, line endings, and code style match surrounding code in both files.

---

## Findings

### Minor — bugs/correctness

**M1. `selectReasoningEffort` doesn't refresh `agentModels`, but the backend clear now fires on that path too.**
`frontend/src/components/layout/StatusBar.tsx:338-348`.

`selectReasoningEffort` calls the same backend `set_model` command
(`setModel(provider, model, effort)`), which now clears every loop's `resolved_model`
(agent.rs:498). Unlike `selectModel`, it does NOT re-fetch `listAgents` afterward. So
when a per-context override was in effect (e.g. `[models.executing]`), an effort-only
change leaves the frontend `agentModels` holding the override while the backend would
report the default provider's model — frontend/backend disagreement until the next turn
re-resolves the override (or the next restart/config save re-syncs). Before this fix the
two sides agreed (both kept the stale override); now they diverge on this path.

User-visible impact is cosmetic and self-correcting (the label shows the model that the
next turn will re-resolve to anyway, since effort changes never change the model id),
but the symmetric fix is the same three lines already proven in `selectModel`:

```ts
void listAgents()
  .then((infos) => useAgentStore.getState().registerAgents(infos))
  .catch((e) => console.error("failed to refresh agent models:", e));
```

placed in `selectReasoningEffort`'s try-block after `setReasoningEffortStore(effort)`.

### Nit — documentation accuracy

**M2. `set_resolved_model` / `resolved_model` doc comments name only the turn-path caller.**
`src/agent/loop_impl.rs:427-431` (method doc: "Called by `resolve_turn_provider` on every
resolution path") and `src/agent/loop_impl.rs:141-147` (field doc: "Set by
`resolve_turn_provider` ... so it never goes stale").

The method now has a second caller — the IPC layer's `set_model`
(src-tauri/src/ipc/agent.rs:498), which clears the field on provider swap. The field's
"so it never goes stale" claim is precisely the invariant that was false and that the new
caller restores, so the docs now under-describe how the invariant holds. A one-line
addition (e.g. "also cleared by the IPC `set_model` command when the provider is swapped,
so `list_agents` reports the new provider's model immediately") keeps them truthful.

---

## Security

No findings. No new inputs, no path/sandbox handling, no approval-gate or core-operation
changes. `list_agents` is read-only and already exposed; the new frontend call is an
additional read of existing data.

## Constitution compliance

No findings beyond M2 (doc accuracy nit). Build warning-free status: the change adds a
call to an existing in-scope method (backend) and a new *used* import (frontend), so no
new warning surface; a green `cargo test` + `npm run build` (per plan steps 1-2) confirms.
