# Batch F — Implementation Spec (F1 EventCoordinator + F2 settings-policy→lib)

**Scope:** precise, verified implementation spec for Batch F of plan
`e342c141-002c-4705-92ed-42dfbb5478e0` (step 6). READ-ONLY — no source edits.
Verified against current HEAD; line refs are advisory (batches A–E will drift
them) — **the design (trait shapes, module boundaries, ordering semantics) is
what the implementer must follow**, re-verifying line refs at execution time.

---

## PRE-RESOLVED CHOICES — verification verdict

### Choice #1 (F1 EventSink) — **PARTIALLY CONFIRMED, one material gap**

- ✅ **Emit path is SYNC.** `app.emit(AGENT_EVENT_CHANNEL, &payload)` at
  `events.rs:395` (and `emit_child_finished` `:508`, `emit_agent_event`
  `:724`) all call `tauri::Emitter::emit` with **no `.await`** — Tauri's
  `Emitter::emit` returns `tauri::Result<()>` synchronously. So
  `EventSink::emit` is a **sync** `fn`, NOT `async fn`. Confirmed.
- ✅ The coordinator owns: `TurnResolveLatch`, running-flag flips,
  `notify_parent_on_completion`, `cleanup_inactive_subagents`, per-agent
  workflow-state tracking. Confirmed against the forwarder body.
- ✅ `AgentManager` methods the coordinator needs (`set_running`, `remove`,
  `main_agent_id`, `has_running_descendants`, `parent_id`, `get`, `list`,
  `send`) are all `pub` on the lib type (`src/runtime/mod.rs`). `AgentLoop`
  accessors (`last_review_report` `loop_impl.rs:494`, `set_provider` `:730`,
  `set_resolved_model` `:460`, `workflow_handle` `:779`) are `pub`. So the
  coordinator can hold `Arc<Mutex<AgentManager>>` +
  `Arc<Mutex<HashMap<AgentId, Arc<AgentLoop>>>>` directly — no new lib API
  surface required for the manager/loop side.
- ❌ **MATERIAL GAP — the trait as stated (`fn emit`) is INSUFFICIENT.** The
  coordinator's `handle()` triggers two side-effects that live in
  `crate::ipc::run_all` and touch `IpcState` (backlog store, run_all state,
  project root, config lock, git ops): `on_main_turn_resolved(&app, success,
  error)` (`run_all.rs:531`, async) and `halt_run_all_for_approval(&app)`
  (`run_all.rs:672`, async). These **cannot move to lib** (they're I/O +
  Tauri-state). So the `EventSink` trait must carry them as methods, OR the
  coordinator must be given a second trait/closure. **Recommendation: extend
  `EventSink` to 3 methods** (see §1.3). Flagging so the implementer doesn't
  build a too-narrow trait and then discover the run_all calls have nowhere to
  go.

  `[CHOICE: F1 — confirmed sync emit; confirmed coordinator ownership; BUT
  EventSink trait must be extended beyond `emit` to carry
  `on_main_turn_resolved` + `halt_run_all_for_approval` (adapter-side
  side-effects the coordinator triggers but cannot own). See §1.3.]`

### Choice #2 (F2 boundary) — **CONFIRMED**

- ✅ Pure validation + patch-application (no I/O, no Tauri types) moves to
  `myharness::config::patch`. Wire structs (`EndpointDto`, `SettingsSaveDto`,
  `ModelsConfigDto`, `ModelRefDto`, `*Wire`, `*Dto`) STAY in the adapter.
- ✅ Command bodies become: parse DTO → convert to lib inputs →
  `config::patch::validate_*` → `config::patch::apply_*` → `save_all` +
  reload + swap into `state.project.config` (I/O, STAYS) → rewire
  provider/vision/embedder/safety (Tauri/state, STAYS).
- ✅ Fixtures byte-identical: wire structs don't change shape; only where the
  validation logic lives moves. The contract fixtures
  (`ipc/contract_fixtures.rs` + `ipc-contract.test.ts`) assert the wire
  payload shape, which is untouched.
- ✅ New lib tests cover: endpoint/model validation, `[models]` double-Option
  patch semantics, summarize-range/theme/sentinel rules.

  `[CHOICE: F2 — confirmed as stated.]`

---

## (1) F1 — Forwarder arm-by-arm enumeration + coordinator design

### 1.1 Forwarder arm-by-arm enumeration (events.rs `spawn`, the `loop` at `:169-399`)

The forwarder reads `(agent_id, event)` from `rx.recv()`, calls
`event.into_serializable()` → `SerializedEvent { event: serial,
approval_sender, question_sender }`, then runs **four sequential phases** per
event. The coordinator's `handle()` must reproduce all four phases in this
exact order.

**Phase A — WorkflowStateChanged cleanup (`:188-196`):**
- Arm: `SerializableAgentEvent::WorkflowStateChanged { state, .. }`.
- State mutated: `prev_workflow_state: HashMap<AgentId, WorkflowState>`
  (insert `agent_id → new_state`). If `was_complete (prev == Complete) &&
  new_state == Executing` → `cleanup_inactive_subagents(&manager_arc,
  agent_id).await`.
- `cleanup_inactive_subagents` (`:540-557`): locks manager once, snapshots
  inactive subagents (`id != trigger && parent_id.is_some() && !is_running`),
  sends `AgentCommand::Cancel` to each **while holding the lock** (try_send is
  non-blocking — FIFO guarantee vs concurrent `Prompt`).

**Phase B — notify-parent-on-completion (`:204-226`):**
- Arms: `Finished { .. }` OR `Error { retrying: false, .. }`.
- State mutated: `notified_children: HashSet<AgentId>` (insert; only notify
  on first finish per child).
- On insert: `notify_parent_on_completion(&manager_arc, &agent_loops,
  agent_id, success).await` → returns `Option<SerializableAgentEvent>` (a
  `ChildFinished`). If `Some(ev)` → `emit_child_finished(&app, ev)` (a
  **second** `app.emit` on the same channel, tagged `child_id`).
- `notify_parent_on_completion` (`:419-470`): reads `parent_id` + child name
  under one manager lock; reads `last_review_report()` from `agent_loops`
  under one tokio lock (clone String, no await held); builds suggestion text
  via `completion_suggestion_text` (pure, already tested `:479`); sends
  `AgentCommand::Suggestion(text)` to parent under a second manager lock
  (best-effort `try_send`); returns the `ChildFinished` event.

**Phase C — running-flag + Run-All resolution (`:239-345`):** the big match.
Each arm takes the manager lock **at most once**; delta events take it zero
times.
- `Started` (`:240-245`): `turn_resolve.on_started(agent_id)`; lock manager →
  `set_running(agent_id, true)`.
- `Error { retrying: false, error }` (`:246-278`): lock manager →
  `set_running(agent_id, false)`; read `is_main = main_agent_id() ==
  Some(agent_id)`; read `descendants_running =
  has_running_descendants(agent_id)`; **drop lock**;
  `pending_approvals.cleanup_for_agent(agent_id)`;
  `pending_questions.cleanup_for_agent(agent_id)`; if `is_main` → build note
  (empty → "agent turn ended in an error") →
  `turn_resolve.on_final_error(agent_id, note, descendants_running)` → on
  `ResolveAction::Failure(n)` → `on_main_turn_resolved(&app, false,
  Some(n)).await`.
- `Finished { .. }` (`:279-321`): lock manager → `set_running(agent_id,
  false)`; read `is_main` + `descendants_running`; **drop lock**;
  `pending_approvals.cleanup_for_agent(agent_id)`;
  `pending_questions.cleanup_for_agent(agent_id)`; if `is_main` →
  `turn_resolve.on_finished(agent_id, descendants_running)` → on `Success` →
  `on_main_turn_resolved(&app, true, None).await`; on `Failure(n)` →
  `on_main_turn_resolved(&app, false, Some(n)).await`; on `None` → no-op;
  **else (child finished)** → `try_flush_deferred_main_failure(&app,
  &manager_arc, &mut turn_resolve).await`.
- `Exited` (`:322-343`): lock manager (mut) → `remove(agent_id)`; **drop
  lock**; `agent_loops.lock().await.remove(&agent_id)`;
  `pending_approvals.cleanup_for_agent(agent_id)`;
  `pending_questions.cleanup_for_agent(agent_id)`;
  `notified_children.remove(&agent_id)`;
  `prev_workflow_state.remove(&agent_id)`;
  `turn_resolve.on_exited(agent_id)`; →
  `try_flush_deferred_main_failure(&app, &manager_arc, &mut
  turn_resolve).await`.
- `_ => {}` (delta events: TextDelta, ToolCallArgDelta, Usage,
  ContextUsage, StepCompleted, SuggestionInjected, PromptDispatched,
  SkillStarted, ModelChanged, ToolResult, ToolCallStart, etc.): **no lock,
  no state mutation** — fall through to emit.

**Phase D — approval/question sender storage + emit (`:347-397`):**
- If `approval_sender.is_some()` AND `serial` is `ApprovalRequest { .. }`:
  `pending_approvals.insert(agent_id, tool_call_id, sender, tool_name, args,
  core_operation)`; then lock manager to read `is_main`; if `is_main` →
  `halt_run_all_for_approval(&app).await`.
- If `question_sender.is_some()` AND `serial` is `UserQuestion { .. }`:
  `pending_questions.insert(agent_id, question_id, sender)`.
- **Always (every event):** `app.emit(AGENT_EVENT_CHANNEL, &AgentEventPayload
  { agent_id, event: serial })` — best-effort, log on err.

`try_flush_deferred_main_failure` (`:568-585`): lock manager → read
`main_agent_id` + `has_running_descendants(main_id)` (None → return); drop
lock; `turn_resolve.flush_deferred_main_failure(main_id,
descendants_running)` → on `Failure(n)` → `on_main_turn_resolved(&app, false,
Some(n)).await`.

### 1.2 Proposed `EventCoordinator` (lib-side)

New lib module: `src/runtime/forwarder.rs` (or `src/runtime/coordinator.rs` —
prefer `forwarder` to match the existing `spawn_event_forwarder` re-export
name in `src-tauri/src/ipc/mod.rs:40`). Declare `pub mod forwarder;` in
`src/runtime/mod.rs` (after `correction`, before the `pub use channels::`
block). Re-export `pub use forwarder::{EventCoordinator, EventSink};`.

```rust
// src/runtime/forwarder.rs
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::agent::AgentLoop;
use crate::runtime::channels::{SerializableAgentEvent, SerializedEvent};
use crate::runtime::{AgentCommand, AgentEvent, AgentId, AgentManager};
use crate::workflow::WorkflowState;

/// The adapter boundary the coordinator drives. The coordinator owns all
/// orchestration (latch, running flags, parent-notify, cleanup, workflow-state
/// tracking) and calls these methods for the side-effects that must live in
/// the adapter (Tauri emit + Run-All resolution, which touch IpcState).
///
/// `emit` is SYNC (Tauri's `Emitter::emit` is sync). The two Run-All methods
/// are ASYNC (they lock backlog state + do git ops). Implementations must be
/// `Send + Sync + 'static` (the coordinator runs in a `tauri::async_runtime::spawn`).
#[async_trait::async_trait]
pub trait EventSink: Send + Sync + 'static {
    /// Emit a serializable agent event to the frontend on the shared channel.
    /// Sync — mirrors `app.emit(AGENT_EVENT_CHANNEL, &payload)`.
    fn emit(&self, agent_id: AgentId, event: SerializableAgentEvent);

    /// The MAIN agent's turn fully resolved (idle + no running descendants).
    /// `success` is false when the turn ended in a final error; `error` carries
    /// the note. Mirrors `crate::ipc::run_all::on_main_turn_resolved`.
    async fn on_main_turn_resolved(&self, success: bool, error: Option<String>);

    /// The main agent requested an approval while Run-All is active — halt the
    /// loop. Mirrors `crate::ipc::run_all::halt_run_all_for_approval`.
    async fn halt_run_all_for_approval(&self);
}

/// Owns the forwarder's orchestration state + the shared handles it drives.
/// Constructed in the adapter's `spawn()`; `handle()` is called once per
/// fanned-in event, in arrival order, on a single consumer task.
pub struct EventCoordinator<S: EventSink> {
    manager: Arc<Mutex<AgentManager>>,
    agent_loops: Arc<Mutex<HashMap<AgentId, Arc<AgentLoop>>>>,
    pending_approvals: Arc<crate::ipc_shim::PendingApprovals>,   // see note below
    pending_questions: Arc<crate::ipc_shim::PendingQuestions>,
    sink: S,
    // per-session state (was locals in spawn()):
    notified_children: HashSet<AgentId>,
    prev_workflow_state: HashMap<AgentId, WorkflowState>,
    turn_resolve: TurnResolveLatch,
}
```

**`handle(&mut self, agent_id, event)`** — async, reproduces Phases A–D in
order (see §1.1). It takes the already-`into_serializable()` `SerializedEvent`
(or does the conversion itself from `AgentEvent` — prefer taking
`SerializedEvent` so the adapter keeps the `into_serializable` call site
identical, minimizing drift). It calls `self.sink.emit(...)` for the main
emit + ChildFinished, `self.sink.on_main_turn_resolved(...)` for Run-All
resolution, `self.sink.halt_run_all_for_approval()` for approval halt.

**`PendingApprovals`/`PendingQuestions` ownership — DESIGN DECISION (flag for
implementer):** these are currently adapter types
(`src-tauri/src/ipc/approval.rs:43`, `questions.rs:34`) with sync
`cleanup_for_agent`/`insert`. Two options:
- **(a) Keep them adapter-side, pass as a trait object.** Define a small lib
  trait `PendingStores: Send + Sync` with `cleanup_for_agent(id)`,
  `insert_approval(...)`, `insert_question(...)`. The coordinator holds
  `Arc<dyn PendingStores>`. Cleanest — no lib dependency on the concrete
  types.
- **(b) Move `PendingApprovals`/`PendingQuestions` to lib.** Heavier — they
  carry oneshot senders + tool metadata; moving them drags in `serde_json::Value`
  + approval-preview types. Out of scope for F1.

  **Recommend (a).** The coordinator holds `Arc<dyn PendingStores>` (or two
  separate small traits). The adapter's `TauriAppSink` (or a separate shim)
  implements it by delegating to the real `PendingApprovals`/`PendingQuestions`.
  This keeps the coordinator testable with a mock `PendingStores` + mock
  `EventSink`.

**`TurnResolveLatch` + `ResolveAction` + `completion_suggestion_text`:** move
verbatim to `src/runtime/forwarder.rs` (they're already pure + self-contained;
their tests at `events.rs:587-706` travel with them). They currently have NO
dependency on Tauri — confirmed.

**`notify_parent_on_completion` + `cleanup_inactive_subagents` +
`try_flush_deferred_main_failure`:** move into `EventCoordinator` as private
async methods (they take `&manager`, `&agent_loops` — now `&self`). They call
`self.sink.emit(child_id, ChildFinished{...})` instead of `emit_child_finished
(&app, ev)`. `cleanup_inactive_subagents` is also called from
`spawn_agent_shared` (`spawn.rs`) — keep a thin `pub(crate) async fn` wrapper
in the adapter that locks the same `manager_arc` (or expose a coordinator
method). **Flag:** verify the second call site (`spawn.rs`) still compiles —
it currently calls `cleanup_inactive_subagents(&manager_arc, agent_id)`
directly; after the move it must call the coordinator method or a lib re-export.

### 1.3 `EventSink` trait — final shape (sync emit + async run_all)

```rust
#[async_trait::async_trait]
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, agent_id: AgentId, event: SerializableAgentEvent);
    async fn on_main_turn_resolved(&self, success: bool, error: Option<String>);
    async fn halt_run_all_for_approval(&self);
}
```

**Adapter impl** (`src-tauri/src/ipc/events.rs`):
```rust
struct TauriAppSink { app: AppHandle }
#[async_trait::async_trait]
impl EventSink for TauriAppSink {
    fn emit(&self, agent_id, event) {
        let payload = AgentEventPayload { agent_id, event };
        if let Err(e) = self.app.emit(AGENT_EVENT_CHANNEL, &payload) {
            eprintln!("failed to emit agent event: {e}");
        }
    }
    async fn on_main_turn_resolved(&self, success, error) {
        crate::ipc::run_all::on_main_turn_resolved(&self.app, success, error).await;
    }
    async fn halt_run_all_for_approval(&self) {
        crate::ipc::run_all::halt_run_all_for_approval(&self.app).await;
    }
}
```
`AgentEventPayload` + `AGENT_EVENT_CHANNEL` stay in the adapter (they're the
wire payload). The coordinator calls `sink.emit(agent_id, serial)`; the
adapter wraps it. `emit_child_finished` + `emit_agent_event` +
`emit_prompt_dispatched` collapse into `sink.emit(...)` calls (the latter two
are called from outside the forwarder too — `run_all.rs:518`,
`events.rs:718-747` — so keep them as adapter helpers that build the
`SerializableAgentEvent` and call `app.emit` directly; they don't go through
the coordinator).

**The forwarder `spawn()` becomes:**
```rust
pub fn spawn(app, fanin_rx, manager_arc, pending_approvals, pending_questions, agent_loops) {
    let sink = TauriAppSink { app: app.clone() };
    let mut coordinator = EventCoordinator::new(manager_arc, agent_loops,
        pending_approvals, pending_questions, sink);
    tauri::async_runtime::spawn(async move {
        let mut rx = fanin_rx;
        while let Some((agent_id, event)) = rx.recv().await {
            let serialized = event.into_serializable();
            coordinator.handle(agent_id, serialized).await;
        }
    });
}
```
`spawn()` keeps only: `rx.recv()` + `into_serializable()` + `coordinator.handle()`
+ the `TauriAppSink` construction. ~15 lines, down from ~250.

---

## (2) F2 — pure-vs-adapter split + `myharness::config::patch` API

### 2.1 Pure (movable) vs adapter (stays) split table

| Block | Current loc (advisory) | Pure? | Destination |
|---|---|---|---|
| `EndpointDto::into_endpoint` (name/base_url/kind/models validation) | settings.rs:118-162 | ✅ pure | `config::patch::validate_endpoint` |
| save_endpoints: dup-name + default_provider/model refs + resolvable-model | settings.rs:210-283 | ✅ pure (given `Vec<Endpoint>` + `old_default_model`) | `config::patch::validate_endpoint_set` |
| save_endpoints: build `new_config` (general.default_*, KeyStore filter-empty, carry pricing/projects) | settings.rs:286-312 | ✅ pure (given `current` + inputs) | `config::patch::apply_endpoints` |
| `parse_safety_mode` / `safety_mode_wire` | settings.rs:709-727 | ✅ pure | **optional** move to `config::patch` (or `config::general` as `SafetyMode::from_kebab`/`to_kebab`) — recommend move for testability |
| save_settings: summarize range (0.05..=0.95) | settings.rs:996-1003 | ✅ pure | `config::patch::validate_settings_patch` |
| save_settings: theme ∈ {dark,light,system} | settings.rs:1004-1012 | ✅ pure | `config::patch::validate_settings_patch` |
| save_settings: vision_model endpoint/model non-empty | settings.rs:1017-1026 | ✅ pure | `config::patch::validate_settings_patch` |
| save_settings: pricing non-empty model + non-negative rates | settings.rs:1027-1040 | ✅ pure | `config::patch::validate_settings_patch` |
| save_settings: default_provider/vision/embedding/[models] endpoint-ref validation | settings.rs:1042-1119 | ✅ pure (given `current.endpoints`) | `config::patch::validate_settings_patch` |
| save_settings: apply patch → `new_config` (clear/set default_*, safety, vision/embedding, **bundled sentinel**, summarize, theme, show_*, [models] double-Option, pricing replace, skip_dirs trim) | settings.rs:1122-1240 | ✅ pure | `config::patch::apply_settings_patch` |
| `[models]` double-Option patch semantics (None=keep, Some(None)=clear, Some(Some(x))=set) | settings.rs:1182-1208 | ✅ pure | `config::patch::apply_models_patch` (called by `apply_settings_patch`) |
| bundled_embedding sentinel rule (clear → `Some(EMBEDDING_MODEL_SENTINEL_HASH)`, NOT None) | settings.rs:1155-1166 | ✅ pure | `config::patch::apply_settings_patch` |
| `deserialize_optional_nullable` (DoubleOption serde workaround) | settings.rs:868-907 | ✅ pure (serde) | **STAYS in adapter** — it's tied to the `ModelsConfigDto` wire struct's `#[serde(deserialize_with=...)]`. The lib `patch` takes already-deserialized `Option<Option<ModelRef>>`. |
| `EndpointDto`, `SettingsSaveDto`, `ModelsConfigDto`, `ModelRefDto`, `VisionModelDto`, `EmbeddingModelDto`, `PricingDto` | settings.rs:82-987 | wire structs | **STAY in adapter** (per choice #2) |
| `*Wire` response structs (`EndpointWire`, `GetSettingsResponse`, …) | settings.rs:444-658 | wire structs | **STAY in adapter** |
| `endpoint_wire`, `models_config_wire`, `model_ref_wire`, `endpoint_kind_wire` | settings.rs:661-706 | pure (lib→wire) | **STAY in adapter** (wire-mapping, tied to wire structs) |
| save_endpoints: `state.project.config.lock()` reads (old_default_model, current general/pricing/projects) | settings.rs:206,287,323,336,412 | ❌ I/O (lock) | STAYS — adapter reads, passes values to lib |
| save_endpoints: `save_all` + `Config::load` + swap into `state.project.config` | settings.rs:315-325 | ❌ I/O | STAYS |
| save_endpoints: rebuild provider + `factory.set_provider` + `agent_loop.set_provider/set_resolved_model` + `emit ModelChanged` | settings.rs:334-407 | ❌ Tauri/state | STAYS |
| save_endpoints: `rewire_vision_and_embedder` + `sync_model_resolver` | settings.rs:411-418 | ❌ Tauri/state | STAYS |
| save_settings: `save_all` + reload + swap + `safety_mode` write + `approvals.re_evaluate` + rewire + sync_resolver | settings.rs:1242-1274 | ❌ I/O/Tauri | STAYS |

### 2.2 Proposed `myharness::config::patch` API

New file `src/config/patch.rs`; declare `pub mod patch;` in `src/config/mod.rs`
(after `projects`, before the `use` block). Re-export the public fns:
`pub use patch::{validate_endpoint, validate_endpoint_set, apply_endpoints,
validate_settings_patch, apply_settings_patch, ModelsPatch, SettingsPatch};`

The lib `patch` module operates on **lib types only** (`Config`, `Endpoint`,
`EndpointKind`, `ModelRef`, `ModelsConfig`, `GeneralConfig`, `SafetyMode`,
`KeyStore`, `PricingEntry`, `EMBEDDING_MODEL_SENTINEL_HASH`). No `serde_json`,
no Tauri. The adapter deserializes its DTOs, trims/converts to these inputs,
and calls in.

```rust
// src/config/patch.rs
use std::collections::HashMap;
use crate::config::{
    Config, EmbeddingModel, Endpoint, EndpointKind, GeneralConfig, KeyStore,
    ModelRef, ModelsConfig, PricingEntry, SafetyMode, VisionModel,
    EMBEDDING_MODEL_SENTINEL_HASH,
};

// ── Endpoint save ───────────────────────────────────────────────────────

/// Validate one endpoint's fields + convert to `Endpoint`.
/// name non-empty (trimmed), base_url non-empty + ends '/', kind ∈ {openai,local},
/// drop empty/whitespace-only model ids.
pub fn validate_endpoint(
    name: &str,
    kind: &str,
    base_url: &str,
    models: Vec<String>,
    max_context: Option<usize>,
    max_output_tokens: Option<usize>,
    multimodal: bool,
    supports_reasoning_effort: bool,
    reasoning_effort: Option<String>,
) -> Result<Endpoint, String>;

/// Cross-endpoint validation: unique names, default_provider refs an endpoint,
/// default_model exists at host (lenient re: old_default_model), default
/// endpoint resolvable to a model.
pub fn validate_endpoint_set(
    built: &[Endpoint],
    default_provider: Option<&str>,
    default_model: Option<&str>,
    old_default_model: Option<&str>,
) -> Result<(), String>;

/// Build the new `Config` from an endpoint save. Preserves pricing + projects +
/// non-general sections; sets general.default_provider/default_model; builds
/// KeyStore keeping only keys for existing endpoints (drops empty values).
pub fn apply_endpoints(
    current: &Config,
    built: Vec<Endpoint>,
    default_provider: Option<String>,
    default_model: Option<String>,
    api_keys: &HashMap<String, String>,
) -> Config;

// ── Settings (non-endpoint) save ───────────────────────────────────────

/// A lib-side [models] patch. Each field is Option<Option<ModelRef>>:
/// outer None = keep existing; inner None = clear; Some(m) = set.
/// `skill`, when present, fully replaces the per-skill map.
#[derive(Debug, Clone, Default)]
pub struct ModelsPatch {
    pub planning: Option<Option<ModelRef>>,
    pub executing: Option<Option<ModelRef>>,
    pub reviewing: Option<Option<ModelRef>>,
    pub complete: Option<Option<ModelRef>>,
    pub subagent: Option<Option<ModelRef>>,
    pub skill: Option<HashMap<String, ModelRef>>,
}

/// A lib-side settings patch (the non-endpoint sections). Mirrors the adapter's
/// `SettingsSaveDto` but in lib types — the adapter converts its DTO into this.
#[derive(Debug, Clone, Default)]
pub struct SettingsPatch {
    pub default_provider: Option<String>,
    pub default_model: Option<String>,
    pub clear_default_provider: bool,
    pub clear_default_model: bool,
    pub safety: Option<String>,
    pub vision_model: Option<VisionModel>,
    pub clear_vision_model: bool,
    pub embedding_model: Option<EmbeddingModel>,
    pub clear_embedding_model: bool,
    pub bundled_embedding_model: Option<String>,
    pub clear_bundled_embedding_model: bool,
    pub summarize_at_fill_rate: Option<f64>,
    pub theme: Option<String>,
    pub show_token_usage: Option<bool>,
    pub show_memory_activity: Option<bool>,
    pub models: Option<ModelsPatch>,
    pub pricing: Option<Vec<PricingEntry>>,
    pub skip_dirs: Option<Vec<String>>,
}

/// Validate the settings patch fields (summarize range, theme, safety parse,
/// vision/embedding/[models] endpoint refs against `current_endpoints`, pricing
/// rates). Returns the parsed `SafetyMode` when `safety` was in the patch.
pub fn validate_settings_patch(
    patch: &SettingsPatch,
    current_endpoints: &[Endpoint],
) -> Result<Option<SafetyMode>, String>;

/// Apply the settings patch into a new `Config` (pure). Preserves endpoints +
/// keys + projects. Implements: clear/set default_*, safety, vision/embedding
/// (clear wins), bundled sentinel (clear → Some(SENTINEL_HASH), NOT None),
/// summarize, theme (lowercased), show_*, [models] double-Option semantics,
/// pricing replace, skip_dirs (trim + drop empty).
pub fn apply_settings_patch(current: &Config, patch: &SettingsPatch) -> Config;

/// Apply just the [models] double-Option patch (extracted for direct testing).
/// None = keep; Some(None) = clear; Some(Some(m)) = set; skill replaces map.
pub fn apply_models_patch(models: &mut ModelsConfig, patch: &ModelsPatch);
```

**Adapter command bodies become (sketch):**
```rust
// save_endpoints
let old_default_model = { state.project.config.lock().await.general.general.default_model.clone() };
let built: Vec<Endpoint> = endpoints.iter().map(|dto| {
    myharness::config::patch::validate_endpoint(&dto.name, &dto.kind, &dto.base_url,
        dto.models.clone(), dto.max_context, dto.max_output_tokens, dto.multimodal,
        dto.supports_reasoning_effort, dto.reasoning_effort.clone())
}).collect::<Result<_,_>>()?;   // NOTE: into_endpoint's trim moves into validate_endpoint
myharness::config::patch::validate_endpoint_set(&built, default_provider.as_deref(),
    default_model.as_deref(), old_default_model.as_deref())?;
let new_config = {
    let current = state.project.config.lock().await;
    myharness::config::patch::apply_endpoints(&current, built, default_provider.clone(),
        default_model.clone(), &api_keys)
};
// ... save_all + reload + swap + rewire (UNCHANGED) ...

// save_settings
let patch = SettingsPatch::from_dto(&patch_dto);  // adapter converts DTO → lib patch (trims here)
let parsed_safety = {
    let current = state.project.config.lock().await;
    myharness::config::patch::validate_settings_patch(&patch, &current.endpoints)?
};
let new_config = {
    let current = state.project.config.lock().await;
    myharness::config::patch::apply_settings_patch(&current, &patch)
};
// ... save_all + reload + swap + safety_mode write + re_evaluate + rewire (UNCHANGED) ...
```

**`SettingsPatch::from_dto`** lives in the adapter (it knows the DTO types);
it does the trimming that `into_endpoint`/the apply block currently inline
(e.g. `vm.endpoint.trim().to_string()`). Keeping the trim in the adapter means
the lib `patch` fns take already-trimmed values — cleaner + the lib tests pass
untrimmed values to assert validation rejects them.

---

## (3) Ordering-preservation argument for F1

The extraction is ordering-safe **iff** the implementer preserves these
invariants (each is currently guaranteed by the single-task forwarder; the
coordinator must not break them):

1. **Single consumer.** `fanin_rx` is owned by exactly one task
   (`tauri::async_runtime::spawn`). The coordinator is constructed INSIDE
   that task and `handle()` is called sequentially from the `while let Some
   = rx.recv().await` loop. No `handle()` concurrency → no interleaving. The
   coordinator's mutable state (`notified_children`,
   `prev_workflow_state`, `turn_resolve`) is `&mut self`, accessed only from
   this one task. ✅ preserved by construction.

2. **`handle()` is awaited to completion before the next `recv()`.** The loop
   is `while let Some(ev) = rx.recv().await { coordinator.handle(ev).await; }`
   — the `.await` on `handle()` means the next event is not dequeued until the
   current one's orchestration (including any `sink.on_main_turn_resolved`
   which itself awaits git ops) fully completes. This matches today's
   behavior (the forwarder awaits `on_main_turn_resolved` inline). ✅
   preserved — **the implementer MUST NOT spawn `handle()` onto a separate
   task or fire-and-forget the sink calls**; they must be awaited inline.

3. **Run-All calls stay behind the same await points.**
   `on_main_turn_resolved` is called from Phase C (Error/Finished arms) and
   from `try_flush_deferred_main_failure` (Finished-else + Exited). Today
   these are `.await`ed inline in the forwarder loop. After extraction they
   become `self.sink.on_main_turn_resolved(...).await` — same await, same
   ordering relative to the next `recv()`. The Run-All dispatch
   (`run_all_dispatch_next` inside `on_main_turn_resolved`) still happens
   before the next event is read. ✅ preserved.

4. **`cleanup_inactive_subagents` FIFO guarantee.** The snapshot + all
   `Cancel` sends happen under ONE manager lock. The coordinator's method
   must keep the identical structure: lock → snapshot → send-all-Cancels
   while held → drop. If the implementer splits this (snapshot under lock,
   send after drop), a concurrent `send_prompt` could enqueue a `Prompt`
   between snapshot and Cancel, landing the Prompt ahead of the Cancel in
   the FIFO — breaking the "Cancel lands ahead of later Prompt" invariant
   documented at `events.rs:532-539`. ✅ preserved iff the method body is
   moved verbatim.

5. **Manager lock acquired at most once per arm; delta events take it zero
   times.** The coordinator must not introduce extra lock acquisitions in
   the delta (`_ => {}`) path. The `Started`/`Finished`/`Error`/`Exited`
   arms each take the lock once (or twice in `notify_parent_on_completion`,
   which is a separate function). ✅ preserved by moving the arm bodies
   verbatim.

6. **`emit` ordering.** The main `app.emit` (Phase D) happens AFTER all
   orchestration (Phases A–C) for the same event, and the `ChildFinished`
   emit (Phase B) happens before the main emit. The coordinator must call
   `sink.emit` in the same relative order: Phase B's ChildFinished emit
   before Phase D's main emit. ✅ preserved iff `handle()` keeps the phase
   order A→B→C→D.

7. **`halt_run_all_for_approval` ordering.** Today it's called in Phase D
   AFTER `pending_approvals.insert` and AFTER the `is_main` lock read. The
   coordinator must preserve: insert → read is_main → (if main) halt. ✅
   preserved by moving the Phase D block verbatim.

**Net:** the extraction is a pure refactor of WHERE the code lives (adapter
locals → coordinator `&mut self` fields; `app.emit` → `sink.emit`;
`crate::ipc::run_all::*` → `sink.on_main_turn_resolved`/`halt_*`). No
concurrency is added or removed; every `.await` stays at the same logical
position. The only risk is the implementer accidentally re-ordering phases or
hoisting a sink call off the await path — the test plan + reviewer must
check this.

---

## (4) Test plan for the newly-testable arms

The forwarder's orchestration is currently untestable without a Tauri
`AppHandle` + live agent loops. After extraction, the coordinator takes a
mock `EventSink` + mock `PendingStores` + a real `AgentManager` (already
constructible in lib tests, see `src/runtime/mod.rs:266-450` tests) + a real
`HashMap<AgentId, Arc<AgentLoop>>` (or a minimal stand-in). New tests in
`src/runtime/forwarder.rs`:

**F1-T1 — Started flips running true.** Register an agent (not running).
Feed `Started`. Assert `manager.get(id).is_running() == true` + the sink
recorded `emit(id, Started)`. Assert `turn_resolve` cleared (feed a prior
final Error, then Started, then Finished → Success — reuses the latch test
shape).

**F1-T2 — Finished flips running false + resolves main as success.**
Register main agent (id=1, no parent). Feed `Started` then `Finished`.
Assert `is_running() == false`; assert sink got `on_main_turn_resolved(true,
None)` exactly once.

**F1-T3 — Final Error flips running false + resolves main as failure.**
Feed `Started` then `Error { retrying: false, error: "boom" }`. Assert
`is_running() == false`; assert sink got `on_main_turn_resolved(false,
Some("boom"))`. Empty error → note "agent turn ended in an error".

**F1-T4 — Error-then-Finished does not double-resolve.** Feed `Started` →
`Error { retrying: false, "x" }` → `Finished`. Assert sink got
`on_main_turn_resolved(false, Some("x"))` exactly ONCE (the Finished after a
resolved failure is idle bookkeeping — no second resolution).

**F1-T5 — Finished with running descendants defers.** Main + a running child.
Feed main `Started` → main `Error { retrying:false, "parent failed" }`.
Assert sink got NO `on_main_turn_resolved` (deferred). Then feed child
`Finished` (child is not main → `try_flush_deferred_main_failure`). Assert
sink got `on_main_turn_resolved(false, Some("parent failed"))` once, AFTER
the child finished. **This is the notify-parent lock-dance + deferred-flush
path — previously untestable.**

**F1-T6 — Exited removes from manager + loops map + flushes deferred.**
Register main + child. Main fails (deferred, child running). Feed child
`Exited`. Assert child removed from `manager` + `agent_loops`; assert
`notified_children` + `prev_workflow_state` dropped the child; assert sink
got the deferred `on_main_turn_resolved(false, ...)`.

**F1-T7 — notify-parent-on-completion lock dance.** Register main (id=1) +
child (id=2, parent=1). Feed child `Started` → `Finished`. Assert: (a) sink
got `emit(2, ChildFinished { child_id:2, success:true, name:"child" })`; (b)
the parent's command channel received a `Suggestion` containing the child
name + "finished"; (c) a second child `Finished` does NOT re-notify
(`notified_children` dedup). Use a child `AgentLoop` with
`set_last_review_report` set → assert the Suggestion text contains the
report path ("read its report at <path>"). **This is the notify-parent lock
dance — previously untestable end-to-end.**

**F1-T8 — cleanup_inactive_subagents FIFO.** Register main + two inactive
children + one running child. Call the coordinator's cleanup method (or feed
a `WorkflowStateChanged { Complete → Executing }` for the main). Assert: the
two inactive children each received `Cancel`; the running child did NOT;
the main did NOT. Assert all `Cancel`s were enqueued under a single lock
(hard to assert directly — assert the observable: no `Prompt` interleaved
between snapshot and Cancel; a concurrent `send_prompt` test is out of
scope for unit tests, document as an integration concern). **This is the
cleanup FIFO path — previously untestable.**

**F1-T9 — approval halt.** Feed main `ApprovalRequest` (with a oneshot).
Assert `pending_approvals.insert` was called; assert sink got
`halt_run_all_for_approval()` exactly once (main agent). Feed a non-main
`ApprovalRequest` → assert sink did NOT get `halt_*`.

**F1-T10 — delta events take no lock + still emit.** Feed `TextDelta`,
`ToolCallArgDelta`, `Usage`, `ContextUsage`. Assert `manager` lock count
unchanged (or just: no state mutated) + sink got `emit` for each.

**F1-T11 — ordering: ChildFinished emit precedes main emit.** Feed child
`Finished`. Assert the sink's recorded call order is: `emit(child,
ChildFinished)` THEN `emit(child, Finished)` (Phase B before Phase D).

**F2 tests** (in `src/config/patch.rs`):

**F2-T1 — validate_endpoint:** name empty → err; base_url no trailing `/` →
err; unknown kind → err; empty models dropped; Debug-form kind ("OpenAI")
rejected (exact match).

**F2-T2 — validate_endpoint_set:** dup name → err; default_provider not in
set → err; default_model not at host + != old_default → err; default_model
== old_default → ok (lenient); no endpoints + default_model set → err;
default endpoint with no models + no default_model → err.

**F2-T3 — apply_endpoints:** preserves pricing/projects/non-general; sets
default_provider/model; KeyStore drops keys for deleted endpoints + drops
empty values.

**F2-T4 — validate_settings_patch:** summarize 0.04 / 0.96 → err; theme
"purple" → err; safety "nope" → err; vision_model endpoint not in
current_endpoints → err (when endpoints exist); [models] override endpoint
not in set → err; pricing negative rate → err; empty pricing model → err.

**F2-T5 — apply_settings_patch [models] double-Option:** outer None → keep;
`Some(None)` → clear; `Some(Some(m))` → set; skill `Some({})` → clears all
skill overrides; skill `Some(map)` → replaces. (Mirrors the existing
`models_config_dto_patch_semantics` test but at the lib apply layer.)

**F2-T6 — bundled sentinel:** `clear_bundled_embedding_model=true` →
`general.bundled_embedding_model == Some("hash")` (NOT None); set path →
trimmed value; empty trimmed → no-op (keeps existing).

**F2-T7 — apply_settings_patch misc:** clear_default_provider wins over set;
theme lowercased; skip_dirs trimmed + empties dropped; pricing fully
replaces; safety applied; vision/embedding clear-wins-over-set.

**F2-T8 — fixtures byte-identical:** the existing contract fixtures
(`ipc/contract_fixtures.rs` + `ipc-contract.test.ts`) MUST still pass
unchanged — they assert the wire shape, which is untouched. Run them as the
regression gate.

---

## (5) Risks

1. **[F1, HIGH] Ordering regression from re-ordering phases.** The four
   phases (A→B→C→D) must stay in this order. The most subtle: Phase B's
   `ChildFinished` emit must precede Phase D's main emit; Phase C's
   `on_main_turn_resolved` must precede Phase D's emit (so the frontend sees
   the backlog status update after the terminal event, same as today). If
   the implementer moves the main emit to the top of `handle()` (a natural
   "emit first" refactor), the Run-All resolution would race the frontend's
   view. **Mitigation:** the spec's `handle()` skeleton lists phases in
   order; the reviewer must diff the phase order against the original
   forwarder body line-by-line.

2. **[F1, HIGH] `cleanup_inactive_subagents` FIFO broken by splitting lock.**
   If the implementer snapshots under lock then sends Cancels after dropping
   it, a concurrent `send_prompt` can interleave. **Mitigation:** move the
   method verbatim; add F1-T8; reviewer checks the lock is held across the
   whole send loop.

3. **[F1, MED] `cleanup_inactive_subagents` second call site (`spawn.rs`).**
   It's called from `spawn_agent_shared` too. After moving it into the
   coordinator (or lib), that call site must still compile + behave
   identically. **Mitigation:** either expose a `pub(crate)` lib fn that
   takes `&Arc<Mutex<AgentManager>>` (the coordinator method delegates to
   it), or keep a thin adapter wrapper. Verify `spawn.rs` call site at
   execution time.

4. **[F1, MED] `EventSink` trait too narrow (the headline gap).** If the
   implementer follows choice #1 literally (trait = just `fn emit`), the
   coordinator has nowhere to call `on_main_turn_resolved` /
   `halt_run_all_for_approval` — they'd be forced back into the adapter
   loop, defeating the extraction. **Mitigation:** this spec extends the
   trait to 3 methods (§1.3). The implementer MUST use the 3-method trait.

5. **[F1, MED] `async_trait` dependency in lib.** The `EventSink` trait has
   async methods → needs `#[async_trait]` (or `impl Trait` in trait, stable
   but requires edition 2024 / `+ Send` bounds care). `async_trait` is
   already a dep (used by `AgentSpawner`/`ParentAwareSpawner`/
   `DescendantTracker` in `src/runtime/mod.rs:183,221,258`). ✅ no new dep.

6. **[F1, LOW] `PendingApprovals`/`PendingQuestions` trait seam.** Introducing
   a `PendingStores` trait (§1.2 option a) adds a small abstraction. Risk:
   the `insert` methods carry different arg arities (approval has 6 args,
   question has 3). **Mitigation:** two separate traits or one trait with
   two methods — either is fine; keep it minimal.

7. **[F2, MED] Trim-location drift.** Today `into_endpoint` trims
   name/models inline; the apply block trims vision/embedding/[models]
   inline. If the lib `patch` fns expect pre-trimmed values but the adapter
   forgets to trim, validation could pass untrimmed values through to disk
   (e.g. a model id `" gpt-4o "`). **Mitigation:** the spec puts trimming in
   the adapter's `SettingsPatch::from_dto` / `validate_endpoint` call sites;
   F2-T1/F2-T5 assert trimmed output. Reviewer checks no trim was dropped.

8. **[F2, MED] `deserialize_optional_nullable` stays in adapter.** The
   double-Option serde workaround is tied to the `ModelsConfigDto` wire
   struct. The lib `ModelsPatch` takes already-deserialized
   `Option<Option<ModelRef>>`. Risk: the adapter conversion
   (`ModelsConfigDto` → `ModelsPatch`) must preserve the three states
   (absent/null/object). **Mitigation:** F2-T5 tests the apply layer with
   all three states directly; the existing `models_config_dto_patch_semantics`
   test (settings.rs:1670) still covers the DTO→Option<Option> deserialization.

9. **[F2, LOW] `parse_safety_mode`/`safety_mode_wire` duplication.** If
   moved to lib, the adapter's `get_settings` (which uses `SafetyMode`'s
   native serde, not `safety_mode_wire`) is unaffected, but `save_settings`
   + `SaveSettingsResponse.safety` use them. **Mitigation:** move them to
   lib `config::patch` (or `config::general`) + re-export; or leave them —
   they're pure either way. Recommend move for testability.

10. **[F2, LOW] Fixture byte-identity.** The wire structs don't change, but
    the apply logic now lives in lib. If the lib `apply_*` produces a
    `Config` that serializes differently (e.g. field order, None handling),
    `save_all` could write a different `config.toml`. **Mitigation:** the
    fixtures assert the wire payload (JSON to frontend), not the TOML on
    disk; but the existing `save_all_round_trips_everything` +
    `save_all_preserves_unrelated_general_sections` tests (config/mod.rs:591,764)
    guard the TOML round-trip. Run them as a gate.

11. **[BATCH ORDERING, MED] Line-ref drift.** Batches A–E merge before F.
    The events.rs/settings.rs line refs in this spec are against current
    HEAD. By F-execution, A (events.rs forwarder git-stall wrap), B
    (config_io.rs unification — `save_endpoints`/`save_settings`/`set_model`
    rewired), D (run_all end_run helper dedup), E (delete `get_config`) will
    have shifted/moved code. **Mitigation:** the spec is framed on durable
    design (trait shapes, module boundaries, phase order, pure-vs-adapter
    split). The implementer MUST re-verify every line ref + re-confirm the
    phase enumeration against the then-current forwarder body before
    extracting. The B2 `config_io.rs` unification (persist_and_reload +
    swap_live_provider) is especially relevant to F2: after B2, the
    save_endpoints/save_settings bodies already delegate persist+reload to
    `config_io.rs`, so F2's "command bodies become validate-via-lib + persist
    + helpers" is partly pre-done — F2 only moves the validate+apply logic,
    not the persist glue.

---

## SUMMARY

- **F1:** Extract the forwarder's 4-phase orchestration into a lib
  `EventCoordinator` (`src/runtime/forwarder.rs`) holding the manager +
  agent_loops + pending-stores Arcs + the 3 local state maps. `handle(ev)`
  reproduces Phases A→B→C→D verbatim. `EventSink` trait = **3 methods**
  (sync `emit` + async `on_main_turn_resolved` + async
  `halt_run_all_for_approval`) — NOT just `emit` (the headline correction to
  choice #1). Adapter `TauriAppSink` wraps `app.emit` + delegates the two
  run_all calls. `spawn()` becomes rx.recv + into_serializable +
  coordinator.handle. Ordering preserved by construction (single consumer,
  inline awaits, verbatim phase order). 11 new lib tests cover the previously
  untestable arms.
- **F2:** Move pure validation + patch-application to `myharness::config::patch`
  (`src/config/patch.rs`): `validate_endpoint`, `validate_endpoint_set`,
  `apply_endpoints`, `validate_settings_patch`, `apply_settings_patch`,
  `apply_models_patch` + `SettingsPatch`/`ModelsPatch` lib structs. Wire DTOs
  + `*Wire` structs + `deserialize_optional_nullable` STAY in the adapter.
  Command bodies = parse DTO → convert to lib patch → validate-via-lib →
  apply-via-lib → persist+reload+rewire (unchanged). 8 new lib tests +
  existing fixtures as the byte-identity gate.
