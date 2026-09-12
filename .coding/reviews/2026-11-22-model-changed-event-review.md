# Review — ModelChanged event (feat/model-changed-event)

**Scope:** All uncommitted changes on `feat/model-changed-event` (11 files, +154/-38).
**Goal:** Drive StatusBar + agent-tab model-label updates through a new `ModelChanged`
agent event fired on model switching, instead of ad-hoc `listAgents` re-fetches; also
fix `save_endpoints` not clearing `resolved_model` after a provider swap.

## Verdict: no findings

The diff is correct, complete, and constitution-compliant. Every focus point checks out.

---

## Focus-point verification

### 1. ModelChanged emitted for EVERY live agent (set_model + save_endpoints) — ✓
- **set_model** (`src-tauri/src/ipc/agent.rs:492-518`): collects `agent_ids` inside the
  `agent_loops` lock scope, then emits in a separate loop after the lock drops. One emit
  per live agent; single iteration → no double-emit.
- **save_endpoints** (`src-tauri/src/ipc/settings.rs:381-399`): identical pattern, gated
  inside `if let (Some(provider), Some(context_manager), Some(model)) = ...` (i.e. only
  when `provider_swapped`). No agent missed, no double-emit.
- Both emit the *resolved default model id* (`model.clone()`), which is exactly what
  `list_agents` would report post-swap (provider built with that model +
  `resolved_model` cleared → `l.provider().model()`).

### 2. save_endpoints now clears resolved_model (bug fix) — ✓
`settings.rs:385` calls `agent_loop.set_resolved_model(None)` **inside** the
`for agent_loop in agent_loops.values()` swap loop, for every loop — mirroring `set_model`
(agent.rs:500). This closes the stale-override hole where `list_agents` reported the
previous turn's per-context override after an endpoints save.

### 3. Frontend reducer signature + merge — ✓
`agentEventReducer.ts:658-663`:
```ts
export const reduceModelChanged =
  (agentId: AgentId): Reducer<Ev<"model_changed">> =>
  (agent, event) => ({
    agent,
    effects: { agentModels: { [agentId]: event.model } },
  });
```
- Correctly curried: outer takes `agentId`, returns a `Reducer<Ev<"model_changed">>`
  (i.e. `(agent, event) => ReducerResult`). The inner reducer reads `event.model`, so it
  genuinely takes `(agent, event)` — not the `(agent)`-only closure-over-agentId mistake.
- Dispatcher (`agentEventReducer.ts:790`):
  `case "model_changed": result = reduceModelChanged(agentId)(agentStamped, event); break;`
  — passes `agentId` then `(agentStamped, event)`. Matches the established
  `reduceWorkflowStateChanged` / `reduceSuggestionInjected` pattern.
- Merge (`agentEventReducer.ts:869-871`): `agentModels: { ...s.agentModels, ...effects.agentModels }`
  — mirrors `agentNames`. Idempotent stamp (last-write-wins) is fine for a display-only field.
- `Effects.agentModels` field added (`agentEventReducer.ts:85-86`) with a doc comment.
- `AppStateLike.agentModels` already existed (`agentEventReducer.ts:42`); the `exited`
  removal path already deletes from it (`agentEventReducer.ts:839-840`).

### 4. Manual listAgents removal doesn't drop a needed refresh path — ✓
`StatusBar.tsx`: the two `listAgents().then(registerAgents)` blocks (in `selectModel` +
`selectReasoningEffort`) are removed. Remaining refresh paths are intact:
- **set_model path:** `setModelStore(modelId)` / `setProviderStore(endpointName)` /
  `setReasoningEffortStore(effort)` update the global stores immediately after the await.
- **save_endpoints path:** `configVersion` effect → `resyncFromBackend()` (StatusBar.tsx:302-306)
  re-syncs global model/provider/effort. Confirmed `bumpConfigVersion()` is called in
  `ProvidersSection.tsx:223` right after `await saveEndpoints(...)`.
- **Per-agent label:** now event-driven via `model_changed`. The `effectiveModel`
  computation (`agentModels[activeAgent] ?? model`) reads the stamped value.
- No other caller relied on the removed StatusBar refresh; `listAgents`/`registerAgents`
  still run on mount (`App.tsx:167`) and on unknown-agent name resolution
  (`useAgentEvents.ts:195`), so the map stays seeded.

### 5. serde wire shape — ✓
- `SerializableAgentEvent` carries `#[serde(tag = "kind", rename_all = "snake_case")]`
  (channels.rs:264). `ModelChanged { model: String }` → `{"kind":"model_changed","model":"..."}`.
- Fixture `frontend/src/lib/ipc-fixtures/event-model-changed.json` matches exactly:
  `{ "kind": "model_changed", "model": "gpt-5" }`.
- TS type (`types.ts:137`): `| { kind: "model_changed"; model: string }` — matches.
- `AgentEvent::ModelChanged` → `SerializableAgentEvent::ModelChanged` arm
  (channels.rs:468-472) sets both oneshot senders to `None` (display-only event). ✓
- Contract fixture test (`tests/contract_fixtures.rs:205-210`) includes the sample.
- Rust roundtrip test (`channels.rs:733-754`) asserts kind + model + round-trip.
- TS contract test (`ipc-contract.test.ts:215-218`) + store test
  (`useAgentStore.test.ts:654-660`) assert the stamp lands.

### 6. No `#[allow(...)]` suppressions — ✓
None added. All new public items have doc comments (`AgentEvent::ModelChanged`,
`SerializableAgentEvent::ModelChanged`, `Effects.agentModels`, `reduceModelChanged`).
The removed local `use SerializableAgentEvent` in `enter_skill` is replaced by the
top-level import (agent.rs:14) — no duplicate, no dead import.

### 7. Lock handling — ✓
Both `set_model` (agent.rs:492-503) and `save_endpoints` (settings.rs:381-388) collect
`agent_ids` inside a block scope:
```rust
let agent_ids: Vec<AgentId> = {
    let agent_loops = state.runtime.agent_loops.lock().await;
    // ... set_provider + set_resolved_model ...
    agent_loops.keys().copied().collect()
};  // ← lock dropped here
for id in agent_ids {
    emit_agent_event(&app, id, SerializableAgentEvent::ModelChanged { model: model.clone() });
}
```
`emit_agent_event` (`events.rs:718-727`) is `pub(crate) fn` (sync, non-async) — it calls
`app.emit(AGENT_EVENT_CHANNEL, &payload)` directly (the Tauri event system), not the
agent-loop fan-in channel. No lock is held across the emit. This is the same direct-emit
path used by `enter_skill` (SkillStarted) and `emit_prompt_dispatched`.

### Additional checks
- **`app: tauri::AppHandle` first param:** Tauri injects this automatically (like `State`).
  The frontend `invoke("set_model", ...)` / `invoke("save_endpoints", ...)` wrappers
  (`tauri.ts`) don't pass `app`. This matches the existing `enter_skill` command, which
  already had `app` as its first param. ✓
- **Event ordering:** the provider swap + `resolved_model` clear happen BEFORE the emit,
  so by the time the frontend receives the event, `list_agents` would already report the
  new model. The event is display-only and doesn't touch running state, Run-All, or
  approvals — correct to bypass the event forwarder. ✓
- **`model.clone()` per agent:** cheap String clone in the emit loop; `model` remains
  owned after the loop (last iter clones, doesn't move). Compiles warning-free. ✓
- **Empty `agent_ids`:** no-op loop. Fine. ✓
- **`AgentId` import path:** `settings.rs:15` imports from `myharness::runtime::AgentId`
  (re-exported, same type as `channels::AgentId` used in agent.rs). ✓

## Constitution compliance
- Doc comments on all new public items: ✓
- No `#[allow(...)]`: ✓
- Warning-free under `#![deny(warnings)]`: code is clean (no unused imports — the removed
  local `use` in `enter_skill` is replaced by the top-level import; new imports in
  settings.rs are all used).
- CRLF on new fixture: content matches siblings; the contract-fixture test reads via
  `serde_json::from_str` (whitespace-agnostic), so line endings don't affect the assertion.

**No findings.**
