{"stack":["3182bb4f"],"reviewed":true}# Spec: mnemo kernel, an engine for any UI and any workflow

Status: APPROVED rev 6 (user, 2026-10-11) — ready to delegate; next job is step 1.1
(`headless-free-agent/job-01-host-extraction.md`).
Nothing delegated yet. Author: coordinator, 2026-10-11.

## 1. Goal

A **mnemo kernel**: an agent engine that other code drives through a library
API, usable with any UI and any workflow guardrails (U9), with none of the mnemo
app's workflows built in (plan lifecycle, backlog, auto-continue, skills,
run-all, review; U7), installable standalone (U8). The mnemo GUI and console
become the first consumers of that same API (§5.9.10). It
reuses everything else mnemo has: the tool
registry, model routing (`ConfigModelResolver`, routing gate), config, the
provider prefix-cache discipline (stable head / volatile tail / footer), cost
controls (budget, optimizer, output compactor, read cache), memory recall and
capture, and the safety-rules core.

Callers: Rust code in-process only, for now. JS is undecided (U6), so this spec
builds no binding of any kind. The API must still be binding-ready (§5.5): a
later JS layer should be a pure wrapper that needs no change to the core.

## 2. Non-goals

- No REST/HTTP server (user, 2026-10-11: "Rest is not a requirement"). The
  serde command/event contract is the transport-neutral surface a server would wrap.
- No change to the GUI or `-console` behaviour (they become consumers of the
  extracted host, byte-for-byte same behaviour).
- No change to the workflow agent's prompt bytes, tool array, or routing
  (would reset every existing cached prefix).
- Any JS binding, JSON facade or FFI layer (napi-rs, wasm, Tauri commands):
  undecided, a future spec.
- Skills, backlog, plan lifecycle, auto-continue, run-all and review in the
  kernel (U7). They remain app-layer features attached through `AgentPolicy`.

## 3. User decisions (2026-10-11)

| # | Decision |
|---|----------|
| U1 | "Offline" = headless, no UI whatsoever, driven from APIs; every event a UI would consume must be exposed to the caller. |
| U2 | Of the mnemo guardrails, keep **memory recall + capture**. |
| U3 | The safety-rules core is ported, but the default mode is "override permissions" for now. |
| U4 | REST is not required; a good API for Rust or JS callers is. |
| U5 | "Override permissions" means **no approvals at all**, including merge/push (`never_auto_for`). |
| U7 | Backlog, auto-continue and all other workflows are specific to the mnemo **app**, not its code. They sit outside the kernel. |
| U8 | Assume a standalone installation: setup and config (endpoints, keys, models, semantic-search download/init, project init) must be reachable through the API. |
| U9 | The goal is an engine for *any* UI and *any* workflow guardrails (cloud agents, a different UI, non-coding tasks). Separate the UI and the hardcoded workflow from the engine. Self-review required against that bar. |
| U11 | The direction must be implementable by a much weaker model (DeepSeek Flash). The coordinator makes every decision in step files; workers only move text and type code verbatim (§14). |
| U10 | Rev 5 approved as a whole, including phase 5: the mnemo GUI and console are rebuilt on the public API (§5.9.10). Nine sequential jobs, 1 → 2a → 2b → 2c → 3a → 3b → 3c → 4 → 5. |
| U6 | Never-commit-to-main is dropped for the free agent. JS is undecided: don't code it, but keep the core API easy to extend for it. Rust only for now. |

## 4. Evidence: current state (facts)

F1. The runtime is already transport-neutral. Inbox: `AgentCommand`
(src/runtime/channels.rs:89: Prompt, Suggestion, CancelSuggestion, Interrupt,
Cancel, Compact, Clear). Outbox: `AgentEvent` (channels.rs:182, ~25 variants),
which already has a serde form, `SerializableAgentEvent` / `SerializedEvent`
(channels.rs:535, :724, `into_serializable` :740). Answers: `Approval`
(:993) and `UserAnswer` (:1010).

F2. The "brain" (config load, project resolve, memory store, embedder,
provider, `ConfigModelResolver`, safety rules, codegraph, MCP, skills,
factory) is built in the **app crate**, not the lib:
`build_brain_inner(app: Option<tauri::AppHandle>)` at src-tauri/src/main.rs:1166
(about 1000 lines). `Brain` is at main.rs:1042 and `BrainOutcome` at :1116. The
only Tauri coupling inside it is `app_handle.emit(...)` at about six sites
(main.rs:1541-1676 embedder status, :1795 browser child ensurer, :1847-1893
codegraph progress). With `app = None` it already runs headless (console mode).

F3. An AppHandle-free runtime already exists, but it is crate-private in the
app crate: `ConsoleRuntime` (src-tauri/src/console.rs:799) holds the brain,
factory, `AgentManager`, the agent-loop map, the fan-in
`Receiver<(AgentId, AgentEvent)>`, and `LifecycleBookkeeping`. Its module doc
(console.rs:8) says the split exists "so a future headless daemon can reuse
everything except the REPL loop".

F4. Agents are spawned via `spawn_agent_shared` (src-tauri/src/ipc/spawn.rs:234),
also in the app crate.

F5. Workflow coupling points in the lib:
- `Workflow::allowed_tools` / `schema_filter` (src/workflow/mod.rs:429, ~:490)
  derive `ToolFilter` (src/tool/mod.rs:390) from `WorkflowState` (mod.rs:26:
  Planning/Executing/Reviewing/Complete/Skill/Subagent).
- Stable head (src/agent/prompt.rs:505 `build_stable_head`) always includes
  `CODING_SYSTEM_PREAMBLE` ("operating under a plan-first workflow"),
  `WORKFLOW_LIFECYCLE`, and `APP_RULES` (closing sequence, never-commit-to-main).
- Volatile tail `workflow_section` (prompt.rs:731) emits per-state text.
- Plan nudge: `append_plan_nudge` (prompt.rs:446), called at
  src/agent/turn.rs:3512.
- Auto-continue/unattended gate keyed on Planning (`UNATTENDED_PROMPT_PREFIX`,
  channels.rs).
- Dispatch: plan-mutation gate (src/agent/dispatch.rs:279), Complete-state
  hook (turn.rs:1902), and `research_write_verdict`.
- Model routing: `ModelContext { workflow_state, skill_name, is_subagent,
  plan_kind }` (src/model_resolver.rs:68), resolved per request in
  `resolve_turn_provider_routed` (src/agent/loop_impl.rs:~1530).
  `ModelsConfig` (src/config/general.rs:828) has slots planning / executing /
  bug_fixing / reviewing / complete / subagent / skill.*.

F6. Size of the coupling to move out (case-insensitive line counts, 2026-10-11):

| file | workflow | backlog | skill | review | auto-continue / unattended |
|---|---|---|---|---|---|
| agent/turn.rs | 47 | 62 | 12 | 86 | 0 |
| agent/dispatch.rs | 14 | 21 | 2 | 125 | 0 |
| agent/loop_impl.rs | 64 | 28 | 44 | 69 | 8 |
| agent/factory.rs | 134 | 136 | 83 | 109 | 2 |
| runtime/agent.rs | 243 | 13 | 0 | 57 | 140 |

Auto-continue lives in the runtime task (src/runtime/agent.rs:58
`auto_continue_streak`, :75 `unattended`, :338 the
`AgentLoop::workflow_expects_progress` gate at loop_impl.rs:2410, :805/:1331
the `UNATTENDED_PROMPT_PREFIX` detection). The loop calls only a handful of
`Workflow` methods directly (`state`, `plan`, `plan_id`, `top_plan_id`,
`current_step`, `active_skill`, `active_plan_kind`, `allowed_tools`,
`schema_filter`, `complete_step`). That small read surface is what makes a
trait seam practical.

F7. Approval gate in src/agent/dispatch.rs:483-520:
`force_prompt = tool.never_auto_for(args)` (:493) OR
`approval::needs_approval(safety, mode, ..)` (approval.rs:71; false for
`Autonomous`) leads to an approval. Inside that branch, `SafetyRules::is_safe`
(an **auto-approve** allow-list from safety.toml, not a deny list) can skip
the prompt, but never for `force_prompt`. So under `Autonomous`, merge/push
still block on an `ApprovalRequest` today.

## 5. Design

### 5.1 Phase 1: extract the host into the lib (pure refactor)

New lib module `src/host/` (`mnemo::host`):
- `host::brain`: move `Brain`, `BrainOutcome`, `build_brain(_inner)` from
  src-tauri/src/main.rs. Replace `Option<tauri::AppHandle>` with
  `Option<Arc<dyn HostNotifier>>`:
  ```rust
  pub trait HostNotifier: Send + Sync {
      fn notify(&self, ev: HostEvent);
  }
  #[derive(Clone, Debug, Serialize, Deserialize)]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum HostEvent {
      EmbedderStatus { .. },        // the main.rs:1641-1676 payloads, same shape
      CodegraphProgress { done: usize, total: usize },
      CodegraphReady { .. },
      // one variant per existing app_handle.emit site, payload-identical
  }
  ```
  The browser child ensurer (main.rs:1795) stays in the app: it is truly Tauri
  (a webview child). Expose `Brain::browser()` so the app can attach it.
- `host::runtime`: move the AppHandle-free core of `ConsoleRuntime` (manager,
  agent-loop map, fan-in receiver, `LifecycleBookkeeping`, the spawner) and
  `spawn_agent_shared` into `HostRuntime`. `ConsoleRuntime` becomes REPL glue
  over `HostRuntime`. The GUI's `IpcState` holds the same pieces and keeps its
  own event forwarder.
- src-tauri implements `HostNotifier` with `AppHandle::emit`, using the same
  event names and payloads as today.

Job 1 is executed as three deepseek steps (`job-01-host-extraction.md`):
1.1 the pure helpers (`host::startup`, `host::maintenance`,
`host::index_progress`) with re-exports at the old paths; 1.2 the notifier
seam and `Brain`/`build_brain` (the managed Laya classifier stays app-owned and
is attached by the app after the build: `attach_laya`); 1.3 the spawn path,
subagent cleanup, `HostSpawner` and `LifecycleBookkeeping` into
`host::runtime`. Deferred from job 1: D1 `host::brain` is feature-gated on
`browser`+`embeddings` until 3a un-gates it; D2 the `HostRuntime` struct (and
`ConsoleRuntime` as a thin wrapper) is built in 3a where the `Engine` needs it;
D3 the Laya manager's Tauri-free core moves in job 4.

Rejected: build the API in src-tauri. Rust callers would then link Tauri and
wry, and the brain would stay app-private.
Rejected: a second, parallel brain builder in the lib. Two copies of 1000 lines
of wiring would drift (routing and caching fixes land in one place only).

### 5.2 Phase 2: kernel / app split (U7)

**Principle (user, 2026-10-11):** the plan workflow, backlog, auto-continue,
skills, run-all and the review lifecycle are features of the *mnemo app*, not
of its code. They live **outside the kernel**. The kernel is the agent loop
plus tools, routing, config, provider/cache/cost machinery, memory, code
graph, safety, stats and the host. A kernel agent with no app layer attached
*is* the free agent. No `WorkflowState::Free` is needed.

**Kernel modules** (must not import any app module): `agent/` (loop, turn,
dispatch, context, prompt kernel blocks, optimizer, reflex, failure triage),
`runtime/`, `provider/`, `model_resolver`, `tool/` (minus `tool/workflow/`),
`tool/agent/*` (minus `write_review_report`), `tool/memory`, `memory/` (minus
`finish_capture`), `codegraph/`, `config/`, `safety_rules`, `mcp/`, `browser/`,
`host/`, `headless/`.

**App modules** (`mnemo::app`, may use the kernel): `workflow/` (plan stack,
states, plan files), `tool/workflow/*` (plan, backlog, skill tools;
`ask_user` moves to the kernel, see below), `backlog`, `skill/`,
`write_review_report`, `memory/finish_capture`, the auto-continue and
unattended logic now in runtime/agent.rs (~lines 58-410, 795-805, 1330),
`append_plan_nudge` + the STATE_* / WORKFLOW_LIFECYCLE / plan lines of the
preamble / APP_RULES, the research write verdict, reviewer role allow-lists,
and run-all (src-tauri/src/ipc/run_all.rs stays in the app crate).

**The seam: `AgentPolicy`** (kernel trait; the app's `WorkflowPolicy`
implements it by wrapping today's `Workflow`; the kernel default `NoPolicy` is
the free agent):
```rust
#[async_trait]
pub trait AgentPolicy: Send + Sync {
    /// Called once at build: register app tools (plan/backlog/skill/review).
    fn register_tools(&self, reg: &mut ToolRegistry, ctx: &PolicyBuildCtx);
    /// The stable head. Default = kernel head (KERNEL_PREAMBLE + KERNEL_RULES
    /// + TOOL_CALL_DISCIPLINE + TOOL_STRATEGY + MEMORY_RECORDS + constitution).
    fn stable_head(&self, k: &KernelPromptBlocks, c: &Constitution) -> String;
    /// The policy's part of the volatile tail (kernel adds recall + CAPABILITIES).
    async fn volatile_tail(&self, caps: &Capabilities) -> String;
    /// Advertised tool array filter + dispatch gate filter.
    async fn tool_surface(&self) -> ToolSurface;            // { advertise, gate }
    /// Per-call veto/redirect beyond the filter (plan-mutation gate,
    /// research write verdict). Default Allow.
    async fn gate_call(&self, name: &str, args: &Value) -> GateVerdict;
    /// Routing key for this request: ordered config slot names + skill name.
    async fn route(&self) -> RouteKey;                      // e.g. ["bug_fixing","executing"]
    /// Prompt intake: may rewrite the volatile tail (plan nudge), read
    /// markers (unattended prefix). Default no-op.
    async fn on_prompt(&self, text: &str) -> PromptHook;
    async fn after_tool(&self, name: &str, result: &ToolResult);
    /// Turn end: Stop, or Continue { note } (auto-continue). Default Stop.
    async fn on_turn_end(&self, t: &TurnSummary) -> Continuation;
    /// Opaque tags stamped on Spend/Savings rows (plan_id, step_index).
    async fn spend_tags(&self) -> SpendTags;
    /// Role allow-lists for spawn_agent (reviewer etc.). Default: none.
    fn spawn_roles(&self) -> &[RoleSpec];
    async fn on_session_end(&self);
}
```
- The policy gets an `EventEmitter` at attach, so the app's own events
  (`WorkflowStateChanged`, `StepCompleted`, `SkillStarted`) are emitted *by
  the policy*. The variants stay in `AgentEvent` for GUI wire compatibility,
  documented as app-owned.
- **Cache contract:** the kernel builds the tool array from
  `tool_surface().advertise`. A policy must keep it byte-stable except at
  points it chooses (today's plan-frozen rule moves into `WorkflowPolicy`
  unchanged). `NoPolicy` returns one constant surface for the agent's life.
- **Routing:** `ModelContext` becomes `{ route: RouteKey, is_subagent }`. The
  resolver walks the slot names in order, then default. `WorkflowPolicy`
  emits exactly today's chain per state, skill and plan kind, so resolution is
  identical (golden table test). `NoPolicy` emits `["free"]`, which resolves
  `[models.free]` → default. `[models.free]` is added to `ModelsConfig`
  (`#[serde(default)]`) and the DTO.
- **Kernel-owned pieces that stay:** `ask_user` (generic; with no subscriber
  it gets the constant answer, §5.3), `spawn_agent` with generic allow-lists,
  memory recall and session-end consolidation (U2), the approval/permission
  gate (§5.4), compaction, budget, optimizer, steering stats.
- `KERNEL_PREAMBLE` = CORE PRINCIPLES without the plan/complete_step lines.
  `KERNEL_RULES` = APP_RULES without the closing sequence and without
  never-commit-to-main (U6). The workflow head is rebuilt by `WorkflowPolicy`
  from the same consts in today's order, giving byte-identical output (golden).
- **Boundary enforcement:** the test `kernel_does_not_import_app` scans kernel
  module sources for `crate::workflow`, `crate::backlog`, `crate::skill`,
  `crate::app`, `tool::workflow`. A single-crate module boundary is enough
  now. A later `mnemo-kernel` crate split is then mechanical.

Sub-phases (each one is its own job, its own review, and leaves everything green):
- **2a: introduce the seam, zero behaviour change.** Add the `AgentPolicy`
  trait and `WorkflowPolicy` as a wrapper over the existing code paths. Route
  every listed coupling site through it. Golden tests guard: head bytes, tool
  array bytes per state, routing table, and the existing auto-continue /
  unattended / nudge / dispatch tests unchanged and green.
- **2b: move the app code out.** Relocate the workflow, backlog, skill, review
  and auto-continue code into `mnemo::app` behind the trait. The factory takes
  `Arc<dyn PolicyFactory>`, and backlog wiring (`set_backlog*`) moves to the
  app's policy factory. The boundary test turns on.
- **2c: `NoPolicy` (the free agent).** Kernel head/tail, the constant surface,
  `["free"]` routing, the `[models.free]` slot.

Rejected (now): `WorkflowState::Free` inside the workflow (previous draft). It
keeps the app's state machine inside the kernel, against U7.
Rejected: `Option<Workflow>` on `AgentLoop`. It still leaves the kernel knowing
the workflow type. The trait inverts the dependency instead.
Rejected (for now): an immediate crate split. 188k lines and long compile times;
the module boundary plus the test gives the same guarantee, and the split stays
possible later.

### 5.3 Phase 3: the public API (`mnemo::headless`, Rust only)

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EngineOptions {
    pub workspace: Workspace,               // { root, state_dir } §5.9.3; no project picker
    pub config_dir: Option<PathBuf>,        // default MNEMO_HOME or ~/.mnemo
    pub permissions: PermissionPolicy,      // default OverridePermissions (U3/U5)
    pub memory: MemoryBackend,              // §5.9.8, default Sqlite { path: None } (= <state_dir>/memory.db)
    pub index_codegraph: bool,              // default true
    pub require_git: bool,                  // default false
    pub auto_install_embedder: bool,        // default false
}
// Rust-only: Engine::builder() adds memory_store(Arc<dyn MemoryStoreTrait>), shared(SharedResources), register_policy, register_tool.
pub struct Engine { /* HostRuntime */ }
impl Engine {
    pub async fn start(opts: EngineOptions) -> Result<Engine, EngineError>;
    pub async fn spawn(&self, s: SpawnOptions) -> Result<AgentSession, EngineError>;
    pub fn session(&self, id: &SessionId) -> Option<AgentSession>;
    pub fn host_events(&self) -> EventReceiver<HostEvent>;
    pub async fn shutdown(self) -> Result<(), EngineError>; // ends sessions, runs consolidation
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SpawnOptions {
    pub name: Option<String>,
    pub model: Option<String>,              // "endpoint/model" string; None = routing
    pub worktree: Option<PathBuf>,          // AgentRootSpec binding (root override)
    pub permissions: Option<PermissionPolicy>, // per-session override of the engine default
    pub guardrails: Guardrails,             // §5.9.6 tier 2: prompt, tools, route, limits, gate rules, question policy
    pub host_tools: Vec<HostToolSpec>,      // §5.9.2
    pub external_policy: Option<ExternalPolicySpec>, // §5.9.6 tier 3
    pub policy: Option<String>,             // §5.9.6 tier 1: a Rust policy registered by name (the app passes "mnemo.workflow")
    pub resume: Option<Transcript>,         // §5.9.5
}
#[derive(Clone)]
pub struct AgentSession { id: SessionId, .. }   // cheap clone, Send + Sync
impl AgentSession {
    pub fn id(&self) -> &SessionId;
    pub async fn send(&self, cmd: SessionCommand) -> Result<(), EngineError>;
    pub fn events(&self) -> EventReceiver<SessionEvent>;  // multi-subscriber
    pub async fn run_to_completion(&self, text: String) -> Result<TurnOutcome, EngineError>;
    pub async fn history(&self) -> Transcript;            // §5.9.5
    pub async fn snapshot(&self) -> SessionSnapshot;      // §5.9.5
    pub fn stats(&self) -> SessionStats;                  // §5.7
    pub async fn close(self) -> Result<(), EngineError>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionCommand {
    Prompt { text: String, images: Vec<String> },
    Steer { text: String },
    CancelSteer { text: String },
    Interrupt, Compact, Clear,
    Approve { request_id: String, decision: Approval },  // only reachable under ApproveViaApi
    Answer { question_id: String, answer: UserAnswer },
    HostToolResult { call_id: String, result: Option<Value>, error: Option<String> }, // §5.9.2
    PolicyAnswer { hook_id: String, answer: PolicyAnswer },                          // §5.9.6
}
pub struct EventReceiver<T> { .. }
impl<T> EventReceiver<T> {
    pub async fn recv(&mut self) -> Result<T, RecvError>; // RecvError::{Closed, Lagged(u64)}
    pub fn into_stream(self) -> impl Stream<Item = Result<T, RecvError>>;
}
```
- `SessionEvent` is a newtype around today's `SerializedEvent` (it carries the
  `AgentId` of the emitting agent, so child-agent events are tagged). Event
  completeness (U1): every `AgentEvent` variant reaches `events()`; everything
  the brain used to emit to the app reaches `host_events()` as `HostEvent`.
  Nothing UI-relevant stays `AppHandle`-only.
- `ask_user` with no subscriber answering: the free agent's prompt says the
  user may be unavailable; `UserQuestion` is emitted and the turn waits for
  `Answer`. A session with zero subscribers at question time gets an immediate
  `Freeform { text: "user unavailable — decide yourself" }` (constant, logged
  as an event).

### 5.4 Permissions (U3, U5) — implemented in phase 2, exposed in phase 3

`pub enum PermissionPolicy { OverridePermissions, ApproveViaApi(SafetyMode) }`,
`#[serde(tag = "mode")]`, default `OverridePermissions`.

- It is a **per-agent loop setting**, not a new `SafetyMode` variant. That
  keeps `config.toml` and the GUI Settings unchanged, so the GUI cannot
  silently enter a no-approval-for-push mode. Stored on `AgentLoop`, read in
  dispatch.
- `OverridePermissions`: dispatch skips the whole approval branch
  (dispatch.rs:493-520). `force_prompt` (`never_auto_for`, merge/push) is
  ignored too, so no `ApprovalRequest` is ever emitted. Each call that
  *would* have been gated emits a new `AgentEvent::ApprovalBypassed { tool,
  reason: "core_op" | "safety_mode" }`, so callers have an audit trail.
- `ApproveViaApi(mode)`: today's semantics exactly (mode + `SafetyRules`
  auto-approve + `never_auto_for`). Approvals surface as `ApprovalRequest`
  and are answered with `SessionCommand::Approve`. This is how the ported
  safety-rules core stays live and testable.
- `SafetyRules` is loaded by the host exactly as now. Under
  `OverridePermissions` it is not consulted: it is an auto-approve allow-list
  (F7), so it has nothing to do when nothing is gated.
- Subagents of a free agent inherit the parent's policy.
- GUI/console agents always get `ApproveViaApi(config mode)` (= today).

### 5.5 Binding-readiness rules (for a future JS layer, not built now)

The core API must obey these so a JS wrapper (napi-rs, wasm or Tauri) is a
pure adapter:
1. Every public input and output type is owned, `'static`, `Send + Sync`, and
   `Serialize + Deserialize` with internally tagged enums (`type`/`mode`) and
   snake_case. No generics, lifetimes, closures or trait objects in public
   signatures. The one exception is `HostNotifier`, which is optional:
   `host_events()` covers the same events.
2. Ids are opaque strings (`SessionId(String)`), not `AgentId(u64)` in public.
3. Models are passed as strings, parsed inside.
4. Events are pull-based (`EventReceiver::recv`). That bridges to a JS
   callback or async iterator without exposing Tokio types. `into_stream` is
   a Rust-only convenience.
5. `EngineError` is an enum with a stable `code()` (`"no_project"`,
   `"session_closed"`, `"invalid_model"`, `"config"`, `"internal"`) plus a
   message, and serialises.
6. Commands are data (`SessionCommand`), not one method per action, so a
   binding needs exactly one `send` entry point.
7. The `Engine` owns its own Tokio runtime handle if it is started outside one
   (`Engine::start_blocking`), since a foreign host has no runtime.
8. A test asserts serde round-trip for every public type and every
   `AgentEvent`/`HostEvent` variant, so the wire stays complete.

### 5.6 Memory and code graph (U2, plus the user follow-up 2026-10-11)

Facts: the brain already builds the `MemoryStore` (+ deferred embedder load)
and the `CodeGraph` (+ file watcher + index progress), and the factory wires
both into every agent (`with_codegraph`, `memory_handle`). The GUI drives
memory and graph maintenance through IPC commands:
`memory_debug_{overview,list,recall,resolve_link,backlinks}`,
`memory_access_log`, `memory_{cleanup,rebuild_search,rebuild_index,index_status}`
(src-tauri/src/ipc/memory_debug.rs, memory_maintenance.rs) and
`codegraph_{status,refresh,graph,rebuild_index}`, `get_index_progress`
(codegraph_cmds.rs).

Design:
- **In the agent:** the `NoPolicy` surface includes every memory tool
  (`memory_search`, `memory_update`, `memory_amend`, …) and the code-graph
  tool (src/tool/agent/codegraph.rs). Auto-recall into the volatile tail and
  session-end consolidation run unchanged.
- **In the engine:** `Engine::start` does what the GUI setup hook and console
  do today. It starts the deferred embedder load (the console's
  `memory_store` path, main.rs:1045) and the code-graph watcher. Their progress
  reaches `host_events()` as `HostEvent::{EmbedderStatus, CodegraphProgress,
  CodegraphReady}` (§5.1). `EngineOptions` gains `index_codegraph: bool`
  (default true) and `memory: bool` (default true; false = no store, no
  recall, no capture).
- **Direct access for callers:** `Engine::memory() -> MemoryApi` and
  `Engine::codegraph() -> CodegraphApi`. These are thin, binding-ready (§5.5)
  wrappers that move the logic of the IPC commands above into the lib (the IPC
  commands then call these, so there is one implementation):
  - `MemoryApi`: `search(query, limit)`, `overview()`, `list(filter)`,
    `get(id)`, `backlinks(id)`, `access_log(limit)`, `index_status()`,
    `cleanup()`, `rebuild_search()`, `rebuild_index()`.
  - `CodegraphApi`: `status()`, `progress()`, `refresh()`, `rebuild()`,
    `query(..)` (the same query surface the `codegraph` tool exposes).
  Return types are the existing serde DTOs.

### 5.7 Stats and telemetry (user follow-up 2026-10-11)

Facts. All of these are already computed:
- per LLM request: `AgentEvent::Usage { prompt_tokens, completion_tokens,
  reasoning_tokens, cached_tokens, ttft_ms, generation_ms }`
  (channels.rs:233). The doc there defines input tok/s = prompt/ttft and
  output tok/s = completion/generation.
- model switches: `AgentEvent::ModelChanged { model, provider,
  reasoning_effort }` (:316).
- context: `AgentEvent::ContextUsage { used, max, breakdown, quality }` (:249).
- savings: `SavingsEvent { kind, tokens_before, tokens_after, tokens_saved,
  measured }` and `SpendEvent { model, lane, tokens_in, tokens_out,
  cached_tokens, .. }` are **persisted** to the memory store
  (src/memory/types.rs:~560-640; written in turn.rs:3730/3787), but there is
  **no live event** for them. Aggregates come from `MemoryStore::savings_stats`
  (`SavingsStats { saved_tokens_total, per_kind, per_day, recent, cache:
  CacheEfficiency }`), `session_stats(session_id)` and `project_stats()`
  (exposed by IPC as `get_{savings,session,project}_stats`,
  src-tauri/src/ipc/agent.rs:912-975).
- trace: `LlmRequestLog::list() -> Vec<LlmRequestSummary>` (src/provider/
  trace.rs:327; `usage`, `ttft_ms`, `generation_ms`, `reasoning_ms`,
  `connect_ms`, `tools_ms`, `prep_ms`, `compact_ms`, `backoff_ms`, `stall_ms`,
  model, provider) and `get(id)`.
- steering: `SteeringStats::snapshot()` (src/agent/steering_stats.rs:692).

Design:
- **Live (push) on `events()`:** `Usage`, `ModelChanged` and `ContextUsage`
  pass through as-is. New `AgentEvent::Savings { kind, tokens_before,
  tokens_after, tokens_saved, measured }` is emitted from
  `record_savings_event`, and new `AgentEvent::Spend { model, lane,
  tokens_in, tokens_out, cached_tokens }` from `record_spend_row`, at the
  same points they are persisted. They are display-only and have no effect on
  the turn. The GUI forwarder ignores the new variants for now.
- **Derived per-request fields:** `SessionEvent` wraps `Usage` with a computed
  `UsageRates { input_tok_per_s: Option<f64>, output_tok_per_s: Option<f64>,
  cache_hit_ratio: f64 }`, using the formulas already documented on the
  variant and `LlmUsage::cache_hit_ratio`. This is computed in the headless
  layer, not the runtime.
- **Pull (query):** `AgentSession::stats() -> SessionStats` (running totals
  kept in the session from the live events: requests, tokens in / out /
  reasoning / cached, saved tokens per kind, model-switch count, current
  model + effort, last context usage, mean TTFT, mean output tok/s), plus
  `AgentSession::stored_stats()` (= `session_stats(session_id)`).
  `Engine::stats() -> EngineStats` (`project_stats`, `savings_stats`,
  `steering_snapshot`). `Engine::trace() -> TraceApi` with `list()`, `get(id)`
  and `clear()`.
- Everything is serde and binding-ready (§5.5). The IPC `get_*_stats`
  commands are rewired to call the same lib functions (one implementation).

### 5.8 Phase 4: standalone setup and config API (U8)

A standalone install has no GUI Settings, no project picker and no first-run
screens, so everything those do must be reachable from code. Most of the
logic is in the app crate's IPC adapters today. It moves into the lib, and the
IPC commands become thin callers (one implementation each).

Facts (where it lives today):
- Config is split across `~/.mnemo/{config,endpoints,keys,mcp,projects}.toml`
  (src/config/mod.rs:5-15). `global_config_dir()` (:485) and
  `migrate_legacy_config_dir()` (:501) are in the lib. Endpoint validation and
  patching are in src/config/patch.rs (`validate_endpoint`, `apply_endpoints`).
  Settings DTOs are in src/config/settings_dto.rs. Keys are in
  src/config/keys.rs (`KeyStore`, plaintext with a user-only ACL).
- Saves are applied live by the IPC adapters `save_settings` / `save_endpoints`
  (src-tauri/src/ipc/settings.rs) plus `rewire.rs` (rebuilds vision, embedder,
  classifier, resolver). **Both are in the app crate.**
- Semantic search: the catalog is `BundledEmbedder::available_models()`
  (src/memory/embedder.rs:183) and `is_model_installed` (:310) is in the lib.
  The download is `download_bundled_model` → `spawn_download`
  (src-tauri/src/ipc/embeddings.rs:219, **app crate**; fastembed pulls on
  init, and progress is sampled from the cache-dir size). On first run with a
  missing model, startup uses the hash embedder (`EmbedderStatus::Fallback`),
  downloads in the background, then hot-installs and re-embeds every row.
  `EmbedderStatus { Ready, Checking, Fallback, Pulling, Downloading{model,
  progress}, Failed }` (:101).
- Optional Laya classifier: `laya_catalog` / `laya_setup` (ipc/laya.rs, a
  managed sidecar download). Disabled by default.
- Models: `list_models` / `list_vision_models` (ipc/models.rs).
- MCP: `mcp_{list_servers,status,save_servers,oauth_start,test}` (ipc/mcp.rs).
- Projects: `create_project` / `switch_project` / `list_projects`
  (ipc/projects.rs); `Project::init` + scaffold (src/project/scaffold.rs).

API: `mnemo::headless::setup::Setup`. It works **without a running Engine**
(first run), and also against a live one (changes are applied live through
the moved rewire path, effective on the next request):
```rust
pub struct Setup { /* config_dir, optional live HostRuntime */ }
impl Setup {
    pub fn open(config_dir: Option<PathBuf>) -> Result<Setup, EngineError>; // creates dir + default files, runs legacy migration
    pub fn for_engine(engine: &Engine) -> Setup;
    pub async fn status(&self, project: Option<&Path>) -> SetupStatus;      // readiness report, below
    // Endpoints + keys
    pub fn endpoints(&self) -> Vec<EndpointDto>;
    pub async fn upsert_endpoint(&self, e: EndpointDto) -> Result<()>;     // validate_endpoint(_set)
    pub async fn remove_endpoint(&self, name: &str) -> Result<()>;
    pub async fn set_api_key(&self, endpoint: &str, key: Secret) -> Result<()>; // write-only
    pub fn has_api_key(&self, endpoint: &str) -> bool;                      // never returns the value
    pub async fn list_models(&self, endpoint: &str) -> Result<Vec<ModelInfoDto>>; // probes the endpoint
    pub async fn test_endpoint(&self, endpoint: &str) -> Result<EndpointCheck>;
    // Model routing + general settings
    pub fn settings(&self) -> SettingsDto;                                  // existing DTO
    pub async fn patch_settings(&self, patch: SettingsPatch) -> Result<SettingsDto>; // default model, [models.*] incl. free, effort, budget, optimizer, shell filter, verify, context, trace logging
    // Semantic search
    pub fn embedding_models(&self) -> Vec<BundledModelInfo>;               // catalog + installed flag + size
    pub async fn install_embedding_model(&self, id: &str) -> Result<EventReceiver<SetupEvent>>; // download with progress
    pub async fn install_embedding_model_from(&self, id: &str, dir: &Path) -> Result<()>; // offline / air-gapped
    pub async fn select_embedding_model(&self, id: &str) -> Result<()>;    // config + live swap + re-embed
    pub fn embedder_status(&self) -> EmbedderStatus;
    pub async fn reembed(&self) -> Result<EventReceiver<SetupEvent>>;
    // Optional pieces
    pub async fn vision_model(&self, m: Option<String>) -> Result<()>;
    pub fn mcp_servers(&self) -> Vec<McpServerDto>;
    pub async fn save_mcp_servers(&self, s: Vec<McpServerDto>) -> Result<()>;
    pub async fn test_mcp(&self, name: &str) -> Result<McpCheck>;
    pub async fn mcp_oauth_start(&self, name: &str) -> Result<OAuthStart>; // returns the URL; the caller opens it
    pub async fn classifier_setup(&self, enable: bool) -> Result<EventReceiver<SetupEvent>>; // Laya, optional
    // Project
    pub async fn init_project(&self, root: &Path, opts: InitProjectOptions) -> Result<ProjectInfo>; // Project::init + scaffold + agent.md template; register: bool
    pub fn agent_md(&self, scope: Scope) -> String;                         // Global | Project(path)
    pub async fn set_agent_md(&self, scope: Scope, text: String) -> Result<()>;
    pub async fn safety_rules(&self, project: &Path) -> SafetyRulesDto;     // used under ApproveViaApi
    pub async fn set_safety_rules(&self, project: &Path, r: SafetyRulesDto) -> Result<()>;
}
pub struct SetupStatus {
    pub config_dir: PathBuf,
    pub endpoints: Vec<EndpointReadiness>,   // name, has_key, reachable: Option<bool>
    pub default_model: Option<String>,
    pub ready_to_run: bool,                  // an endpoint + a key (if needed) + a default model
    pub embedder: EmbedderReadiness,         // configured id, installed, status, semantic: bool (false = hash fallback)
    pub classifier: ClassifierStatus,
    pub project: Option<ProjectReadiness>,   // is_git_repo, coding_dir, scaffolded, agent_md, codegraph_indexed
    pub missing: Vec<SetupStep>,             // ordered to-do list for a first-run flow
}
```
- **Install isolation:** `EngineOptions.config_dir`, `Setup::open(dir)`, and a
  `MNEMO_HOME` env override let a standalone install keep its own config
  instead of sharing `~/.mnemo` with a GUI install. The embedder cache dir
  derives from it (today `embedder_cache_dir()` is fixed in the app crate).
- **Secrets:** `Secret` is a zeroize-on-drop newtype with no `Debug`/`Serialize`
  of the value. Besides `keys.toml`, `KeySource::{File, Env(var), InMemory}`
  lets a host process inject keys without writing plaintext to disk (NEW;
  `KeyStore` only reads the file today, ASSUMPTION verified at implementation).
- **Engine::start preconditions:** if `SetupStatus.ready_to_run` is false,
  start returns `EngineError::NotConfigured { missing }` rather than failing
  deep inside the brain. A missing embedding model is not fatal: the engine
  starts on the hash fallback, `semantic: false` is reported, and the caller
  decides whether to install. Optional auto-install is
  `EngineOptions.auto_install_embedder: bool` (default false: no surprise
  ~100 MB download in a host process).
- `SetupEvent` = `{ Progress { step, label, fraction }, Done { step }, Failed
  { step, error } }`, binding-ready (§5.5).

### 5.9 Self-review against the goal (rev 5, 2026-10-11) and the resulting changes

**Goal restated (U9):** mnemo is an *engine* usable with any UI and any
workflow guardrails: cloud agents, a different UI, non-coding tasks. The UI and
the hardcoded workflow are separated from the engine.

**Verdict on rev 4:** the structure (host extraction, `AgentPolicy`, setup API)
is right, but the kernel still carried ten mnemo-app assumptions. Each is
listed with evidence and the change that removes it. These changes are
normative; where they conflict with §5.1-5.8, this section wins.

| # | Gap in rev 4 | Evidence | Change |
|---|---|---|---|
| G1 | Kernel prompt is a coding agent | prompt.rs:27 "You are a coding agent…"; `KERNEL_PREAMBLE` kept coding principles | `PromptSpec` (§5.9.1): the identity is caller-supplied; kernel blocks are a library selected by name. |
| G2 | Fixed tool set, no extension point | factory.rs:1194-1282 registers 68 tools unconditionally; `ToolRegistry::register` (tool/mod.rs:1191) is internal | `ToolSelection` + `Engine::register_tool` + host tools (§5.9.2). |
| G3 | Project = git + `.coding/` mandatory | project/mod.rs:96 `Project::resolve`; memory.db/codegraph.db under `.coding`; scaffold writes git files; prompts name `.coding/knowledge/**` | `Workspace { root, state_dir }` (§5.9.3); git optional; scaffold/registry/instance marker move to the app. |
| G4 | Process-global state | steering_stats.rs:449 `SteeringStats::shared()`; src/instance_marker.rs; `pending_project` in the global config dir | Engine-owned stats; marker app-only; two-engines test (§5.9.4). |
| G5 | No conversation persistence/resume | no export/restore in the lib; transcript lives in the GUI frontend | `history()`, `resume`, `snapshot()` (§5.9.5). |
| G6 | Custom guardrails Rust-only | `AgentPolicy` is a trait; `SpawnOptions.policy` a Rust-registered name | Three policy tiers: trait, declarative, external-over-channel (§5.9.6). |
| G7 | No per-session limits | `BudgetConfig` is global | `SessionLimits` (§5.9.7). |
| G8 | Memory on/off only | `memory_handle()` already returns `Arc<dyn MemoryStoreTrait>` | `MemoryBackend` + shared resources (§5.9.8). |
| G9 | Engine config mixed with UI prefs | general.rs:29 `ui: UiConfig`, :1075 theme/show_* | `EngineConfig` vs `UiConfig` split (§5.9.9). |
| G10 | Weak proof of separation | import-grep test; GUI IPC "calls lib functions" | Feature-gated kernel build; the mnemo app rebuilt on the public API; a second-consumer example (§5.9.10, phase 5). |

#### 5.9.1 Prompt composition (G1)
```rust
pub struct PromptSpec {
    pub identity: String,                  // REQUIRED for NoPolicy; e.g. "You are a support agent for Acme."
    pub blocks: Vec<PromptBlock>,          // kernel library, opt-in by name
    pub extra_stable: Option<String>,      // caller text appended to the stable head
    pub constitution: ConstitutionMode,    // None | Global | Project | Both (agent.md files)
}
#[derive(Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum PromptBlock { CoreCodingPrinciples, ToolCallDiscipline, ToolStrategy, MemoryRecords, FileToolRules }
```
The kernel stable head = identity + selected blocks + extra + constitution, in
that fixed order, byte-stable for the session. The volatile tail = recall +
CAPABILITIES + the policy's tail. `WorkflowPolicy` reproduces today's head
byte-for-byte from the same blocks (golden). `NoPolicy` with no `PromptSpec`
is an error (`EngineError::code "prompt_required"`), never a silent coding
default. A `PromptSpec::coding_default()` constructor exists for convenience
and is what the mnemo console/GUI free mode passes.

#### 5.9.2 Tools (G2)
- `ToolSelection { include: Vec<ToolGroup>, exclude: Vec<String> }` on
  `SpawnOptions`. Groups: `Read` (read_files, search, search_read, pattern…),
  `Write` (file_edit, multi_edit, file_write, file_append, convert_line_endings),
  `Shell`, `Git`, `Web` (web_fetch), `Browser`, `Codegraph`, `Memory`,
  `Images`, `Spawn`, `AskUser`, `Mcp`, `Host`. The factory registers only the
  selected groups: the tool array never advertises tools the caller excluded,
  and `list_models`/`load_tools` are registered only with `Spawn`/`Mcp`.
  `WorkflowPolicy` selects today's full set, so the app is unchanged.
- `Engine::register_tool(Arc<dyn Tool>)` (Rust) adds a custom tool to group
  `Host` for subsequent spawns.
- **Host tools** (any language): `SpawnOptions.host_tools: Vec<HostToolSpec
  { name, description, parameters: JsonSchema, safety: SafetyLevel }>`. The
  model calls them like any tool; the kernel emits
  `AgentEvent::HostToolCall { call_id, name, args }` and waits for
  `SessionCommand::HostToolResult { call_id, result | error }` with a
  per-tool timeout (default 120 s → error result to the model). Output
  compaction and the read cache apply to host-tool results like any other.
- MCP stays as the other extension path for out-of-process tools.

#### 5.9.3 Workspace instead of project (G3)
- Kernel: `Workspace { root: PathBuf, state_dir: PathBuf }`. `state_dir`
  defaults to `<root>/.coding` but can be anywhere (e.g. a per-tenant
  directory outside the tree). Memory db, codegraph db, trace logs, read
  cache and safety.toml all derive from `state_dir`. `Sandbox` roots at
  `root`. Prompt references to `.coding/knowledge/**` are rendered from
  `state_dir` (the protected-path rule becomes "the state dir").
- `require_git: bool` (default false). With no repo, the `Git` group is not
  registered and `git_read`-dependent features (diff cards, worktrees) are
  absent; the engine still starts.
- App: `Project` (`.coding` discovery, the projects registry, `pending_project`,
  `Project::init` + scaffold, the instance marker) stays in `mnemo::app` and
  the app crate. `Setup::init_project` becomes `Setup::init_workspace(root,
  state_dir, git_scaffold: bool)`; the app's `create_project` calls it with
  the mnemo conventions.
- `EngineOptions.project_root` becomes `workspace: Workspace`.

#### 5.9.4 No process-global mutable state (G4)
- `SteeringStats` becomes an engine-owned `Arc` injected into the factory and
  the dispatch layer; `SteeringStats::shared()` is removed (the app's
  `get_steering_stats` reads the engine's).
- The instance marker (`src/instance_marker.rs`) moves to the app crate.
- `global_config_dir()` is used only by `Setup::open(None)` and the app;
  every kernel path takes its directories from `EngineOptions`.
- The regex/BPE `OnceLock`s (context.rs:2301, output_compactor.rs, shell_filter.rs,
  trace.rs) are immutable caches and stay.
- Test: two `Engine`s in one process with different workspaces and config
  dirs run a session each concurrently; stats, memory, trace and events do not
  cross.

#### 5.9.5 Conversation persistence and resume (G5)
- `AgentSession::history() -> Transcript` (serde: messages with roles, tool
  calls/results, images as refs, the compaction summaries, and the kernel's
  layout metadata so a resumed request reproduces the same byte prefix).
- `SpawnOptions.resume: Option<Transcript>`; `AgentSession::snapshot() ->
  SessionSnapshot { transcript, context_usage, model, pending: Option<
  Approval | Question | HostToolCall>, stats }` for a reconnecting UI.
- Persistence itself is the host's job (the kernel hands out the bytes); the
  app layer keeps its own behaviour (nothing today) until it opts in.
- Cache note: resuming with the same transcript, same `PromptSpec`, same
  `ToolSelection` and same model must hit the provider prefix cache; a test
  asserts the request body bytes are identical to the pre-restart request.

#### 5.9.6 Three policy tiers (G6)
1. **Rust trait** `AgentPolicy` (§5.2), via `Engine::register_policy(name,
   Arc<dyn PolicyFactory>)` and `SpawnOptions.policy = Some(name)`.
2. **Declarative** `SpawnOptions.guardrails: Guardrails` (data, binding-ready):
   `prompt: PromptSpec`, `tools: ToolSelection`, `route: Vec<String>`,
   `limits: SessionLimits`, `gate_rules: Vec<GateRule { tool, arg_pattern,
   verdict: Allow | Deny { message } | RequireApproval }>`, `question_policy:
   QuestionPolicy { WaitForHost | AnswerConstant(String) | Deny }`, and
   `turn_end: Stop`. `NoPolicy` *is* `Guardrails` with defaults.
3. **External** `SpawnOptions.external_policy: Option<ExternalPolicySpec {
   hooks: Vec<HookKind>, timeout_ms, on_timeout: Allow | Deny | Stop }>`.
   For each selected hook (`ToolGate`, `PromptIntake`, `TurnEnd`,
   `AfterTool`, `SessionEnd`) the kernel emits `AgentEvent::PolicyHook { hook_id,
   kind, payload }` and waits for `SessionCommand::PolicyAnswer { hook_id,
   answer }` (`GateVerdict` / `PromptHook` / `Continuation`), with the timeout
   default applied. This is the same request/response shape as approvals and
   host tools, so an orchestrator in any language can implement a workflow
   (including auto-continue) outside the process.
   Rejected: letting the external policy change the tool *array* mid-session;
   it may only gate calls, because the array is the cache head (R1).

#### 5.9.7 Limits (G7)
`SessionLimits { max_turns, max_requests, max_prompt_tokens, max_completion_tokens,
max_cost_micros (uses the endpoint's price table when present), wall_clock_ms,
idle_ms }`, all `Option`. Hitting one ends the turn with
`Finished { reason: LimitReached { which } }` and parks the session. The
global `BudgetConfig` remains the default when a limit is `None`.

#### 5.9.8 Memory backend and shared resources (G8)
- `EngineOptions.memory: MemoryBackend::{ Sqlite { path: Option<PathBuf> },
  None }` (serde) plus Rust-only `Engine::builder().memory_store(Arc<dyn
  MemoryStoreTrait>)`.
- `SharedResources` (Rust-only builder): embedder, HTTP client pool, model
  catalog cache, shared across engines in one process so N tenants don't load
  N copies of the ONNX model.

#### 5.9.9 Config split (G9)
`Config` splits into `EngineConfig` (providers/endpoints, keys source, models
incl. `free`, memory, budget, optimizer, verify, shell filter, context, trace,
safety mode default) and `UiConfig` (`[ui]` theme/show_* — app-owned, read
only by src-tauri). On disk nothing moves (same files, same tables; the app
owns `[ui]`); `Setup` and `SettingsPatch` expose only `EngineConfig`.

#### 5.9.10 Proof (G10) — phase 5
- Cargo features: `kernel` (default) and `app`. `mnemo::app` is
  `#[cfg(feature = "app")]`. `cargo build --no-default-features --features
  kernel,headless` must build, and the second-consumer example must run on it.
- `examples/second_consumer.rs` (~60 lines, compile-tested in CI): a
  `NoPolicy` agent with a non-coding identity, `ToolSelection = [Read, Host]`,
  one host tool answered by the example, limits set, events printed as JSON.
- **The mnemo app is rebuilt on the public API:** `IpcState` holds an
  `Engine`; every IPC command and the console REPL go through `Engine` /
  `AgentSession` / `Setup` / `MemoryApi` / `CodegraphApi` / `TraceApi` with
  `WorkflowPolicy` attached. A test asserts src-tauri references no
  `mnemo::agent::`, `mnemo::runtime::` or `mnemo::host::` internals other than
  the re-exported public types. If the GUI needs a backdoor, the API is
  incomplete and the backdoor is a defect, not a shortcut.

## 6. Scope (files / regions)

| Job | Files / regions |
|-----|-----------------|
| 1 (steps 1.1-1.3) | new src/host/{mod,startup,maintenance,index_progress,notifier,brain,runtime}.rs; src/lib.rs (mod); src-tauri/src/{main,console,startup}.rs; src-tauri/src/ipc/{memory_maintenance,codegraph_cmds,error,spawn,events,state}.rs — re-exports at the old paths; see job-01 step file |
| 2a | new src/agent/policy.rs (trait, NoPolicy stub, KernelPromptBlocks); src/agent/{turn,dispatch,loop_impl,prompt,factory}.rs and src/runtime/agent.rs (route every coupling site through the policy); new src/app/workflow_policy.rs (wrapper); src/model_resolver.rs (RouteKey); golden tests |
| 2b | move src/workflow, src/backlog.rs, src/skill, src/tool/workflow, src/tool/agent/write_review_report.rs, src/memory/finish_capture.rs and the runtime auto-continue block under src/app/ (re-exports at the old paths for one release); src/agent/factory.rs (PolicyFactory, backlog wiring out); src-tauri spawn/run_all callers |
| 2c | src/agent/policy.rs (NoPolicy); src/agent/prompt.rs (PromptSpec + block library, KERNEL_RULES); src/agent/factory.rs (ToolSelection, Workspace, engine-owned SteeringStats); src/agent/steering_stats.rs (remove `shared()`); src/project/mod.rs → kernel `Workspace` + app `Project`; src/instance_marker.rs → src-tauri; src/config/general.rs + settings_dto.rs (models.free); src/agent/dispatch.rs (PermissionPolicy gate, :483-520); src/runtime/channels.rs (`ApprovalBypassed`) — G1-G4 |
| 3a | new src/headless/{mod,engine,session,types,error}.rs (engine, sessions, permissions, Transcript/snapshot/resume, SessionLimits); src/agent/context.rs (transcript export/import that preserves layout) — G5, G7 |
| 3b | new src/headless/{memory_api,codegraph_api,stats}.rs; src/runtime/channels.rs (`Savings`/`Spend`); src/agent/turn.rs (emit at :3730/:3787); src-tauri/src/ipc/{memory_debug,memory_maintenance,codegraph_cmds,agent,trace}.rs (thin callers); tests/integration/headless.rs |
| 3c | new src/headless/{host_tools,external_policy}.rs; src/runtime/channels.rs (`HostToolCall`, `PolicyHook`); src/agent/dispatch.rs (host-tool dispatch); src/agent/policy.rs (`DeclarativePolicy`, `ExternalPolicy`) — G2 (host tools), G6 |
| 4 | new src/headless/setup.rs + src/host/{rewire,embed_download}.rs (moved from src-tauri/src/ipc/{rewire,embeddings}.rs); src/config/keys.rs (KeySource, Secret); src/config/{mod,general}.rs (MNEMO_HOME, EngineConfig/UiConfig split, MemoryBackend); src/headless/engine.rs (builder: memory_store, SharedResources); src-tauri/src/ipc/{settings,embeddings,models,mcp,projects,laya}.rs (thin callers) — G8, G9 |
| 5 | Cargo.toml features `kernel`/`app`; examples/second_consumer.rs; src-tauri/** and src-tauri/src/console.rs rebuilt on Engine/AgentSession/Setup/*Api; tests/boundary.rs — G10 |

Order: 1 → 2a → 2b → 2c → 3a → 3b → 3c → 4 → 5, strictly sequential. Nine
jobs, each executed as deepseek steps per §14. 2c and 3a grew with §5.9; if either report shows a diff over ~3k lines,
the G-items split into their own follow-up jobs before review. Inside a job
there is no parallel split: the regions overlap. Each job runs in its own
worktree on `alf/<job>` and merges to `main` before the next starts. Real state
lives in `~/.mnemo` and `<project>/.coding`; tests must use temp dirs only.

## 7. Acceptance criteria

1. Phase 1: GUI and `-console` behave as before. The existing test suite is
   green, and `build_brain` timing output is unchanged in form.
2. Phase 1: `mnemo` (lib) builds without tauri. `cargo tree -p mnemo -i tauri`
   finds nothing.
3. Phase 1: every former `app_handle.emit` site in the brain maps to exactly
   one `HostEvent` variant, and the app's emitted event names and payloads are
   identical (contract fixture test).
4. Phase 2a/2b: the workflow (app) agent is byte-identical to before: stable
   head (golden), tool array per state, routing slot per (state, skill, plan
   kind, subagent) (golden table), and the existing auto-continue / unattended
   / plan-nudge / dispatch / backlog tests pass unmodified.
4b. Phase 2b: `kernel_does_not_import_app` passes. Kernel modules reference no
   `crate::workflow|backlog|skill|app|tool::workflow`.
5. Phase 2c: a `NoPolicy` (free) agent advertises no plan/backlog/skill/review tool. A call to
   `create_plan`/`complete_step`/`finish`/`start_skill` is rejected at dispatch
   with a clear error (unhappy path: model hallucinates a plan tool).
6. Phase 2: a Free agent's head contains no "plan-first" / "create_plan" /
   "WORKFLOW LIFECYCLE" text, and its tool array is byte-identical across 3
   turns, including after a mutating tool call.
7. Phase 2: `[models.free]` set → used. Unset → `[models.executing]`. Dangling
   ref → falls through. A forced model and the routing gate still win as today.
8. Phase 2: a Free agent writes nothing under `.coding/plans/`, gets recall
   injected, and runs consolidation at session end.
9. Phase 3: `Engine::start` with a non-project dir returns
   `EngineError::NoProject`; it never blocks and never shows a picker.
10. Phase 3: `run_to_completion` on a scripted mock provider returns the final
    text, and the event stream contains Started … Finished in order.
11. Phase 2/3: under `OverridePermissions` (the default), `git merge`,
    `git push`, `shell "git push origin main"` and a `NeedsApproval` file write
    all execute with **no** `ApprovalRequest`, and each emits exactly one
    `ApprovalBypassed`. Under `ApproveViaApi(ApproveEachAction)` the same calls
    emit `ApprovalRequest`; `Approve` proceeds, `Deny` does not execute, and a
    `SafetyRules` auto-approve match skips the prompt (except merge/push).
    GUI/console agents keep today's behaviour (existing dispatch tests green,
    unchanged).
12. Phase 3: two subscribers on `events()` both receive every event. Dropping
    one does not stall the agent.
13. Phase 3: every public type (`EngineOptions`, `SpawnOptions`,
    `SessionCommand`, `SessionEvent`, `HostEvent`, `PermissionPolicy`,
    `EngineError`, `TurnOutcome`) and every `AgentEvent` variant round-trips
    through `serde_json`. Malformed JSON for `SessionCommand` returns a serde
    error and does not panic. A compile-time test asserts the public types are
    `Send + Sync + 'static` (§5.5).
14. Phase 3: `ask_user` on a session with no event subscriber gets the constant
    freeform answer immediately; with a subscriber, it waits for `Answer`.
15. Phase 3: `Engine::start_blocking` works from a plain `fn main` with no
    Tokio runtime, and `send` on a closed session returns
    `EngineError` with `code() == "session_closed"`.
16. Phase 3: `shutdown()` ends all sessions with no leaked tasks (Tokio runtime
    exits).
17. Phase 3b: a free agent can call `memory_search` and the code-graph tool;
    `Engine::start` emits `EmbedderStatus` and `CodegraphProgress`/`Ready` on
    `host_events()`; `index_codegraph: false` starts no watcher; `memory: false`
    means no recall, no consolidation, and `Engine::memory()` returns
    `EngineError` with code `"memory_disabled"`.
18. Phase 3: on a scripted mock provider with usage + one model switch +
    one output-compaction saving, `events()` carries `Usage` (with
    `UsageRates` correct to the formula), `ModelChanged`, `Savings` and
    `Spend`. `AgentSession::stats()` totals equal the sum of those events, and
    `Engine::stats().savings.saved_tokens_total` includes the saving.
19. Phase 3: the GUI's `get_{savings,session,project}_stats`, memory and
    code-graph IPC commands return byte-identical JSON before and after the
    rewire (contract fixtures).
20. Phase 4: on an empty `MNEMO_HOME` temp dir, `Setup::open` creates the
    config files; `status().missing` lists endpoint, key and default model, in
    order; after `upsert_endpoint` + `set_api_key` + `patch_settings`,
    `ready_to_run` is true and `Engine::start` succeeds. Before that, it returns
    `NotConfigured { missing }`.
21. Phase 4: `install_embedding_model` streams `Progress` then `Done`.
    `select_embedding_model` on a live engine flips `embedder_status` to Ready
    and re-embeds; recall works with no restart. `install_embedding_model_from`
    works with the network disabled. Without a model the engine starts with
    `semantic: false`. (Network tests are `#[ignore]` + a local-fixture variant.)
22. Phase 4: `has_api_key` never exposes the value; `Secret` has no
    `Debug`/`Serialize` of the value (compile-fail test); `KeySource::Env`
    works with no keys.toml present.
23. Phase 4: `init_project` on a temp git repo writes the scaffold
    (.gitignore/.gitattributes entries, .coding/, agent.md template) and is
    idempotent on a second run.
24. Phase 4: every GUI Settings/projects/embeddings/MCP IPC command returns
    byte-identical JSON before and after the move (contract fixtures).
25. Phase 2c (G1): a `NoPolicy` spawn without `PromptSpec` fails with code
    `prompt_required`; with `identity = "You are a support agent."` and
    `blocks = []`, the stable head contains no coding text and no tool
    strategy; `WorkflowPolicy`'s head is still byte-identical (criterion 4).
26. Phase 2c (G2): `ToolSelection { include: [Read] }` advertises only the
    read group; a `shell` call is rejected at dispatch as unknown; adding
    `Host` + `register_tool` makes the custom tool callable.
27. Phase 2c (G3): an engine on a non-git temp dir with `state_dir` outside
    the tree starts, writes memory.db/codegraph.db/trace only under
    `state_dir`, and advertises no `git` tools; `.gitignore` is not created.
28. Phase 2c (G4): two engines in one process (different workspaces + config
    dirs) run one session each concurrently; steering stats, memory rows,
    trace entries and events are disjoint; no instance-marker file is written
    by the kernel.
29. Phase 3a (G5): `history()` after two turns, `resume` into a fresh engine,
    then a third turn: the provider request body bytes equal those of a
    never-restarted session (cache-prefix test on the mock provider);
    `snapshot()` during a pending approval reports it.
30. Phase 3a (G7): `max_turns = 2` ends with `LimitReached { which: "max_turns" }`
    on the third prompt; `wall_clock_ms` interrupts a stalled mock request.
31. Phase 3c (G2): a host tool is called by the scripted model, the host
    answers via `HostToolResult`, the result reaches the model; an unanswered
    call times out to an error result and the turn continues.
32. Phase 3c (G6): with `external_policy.hooks = [ToolGate, TurnEnd]`, a
    `Deny` answer blocks the call and a `Continue { note }` answer makes the
    kernel run another turn (auto-continue implemented outside the process);
    `on_timeout = Allow` proceeds when nobody answers.
33. Phase 4 (G8/G9): a custom `MemoryStoreTrait` receives the recall and
    consolidation calls; `SettingsPatch` cannot set `[ui]` fields (compile-time
    absent); the GUI still reads its `[ui]` section unchanged.
34. Phase 5 (G10): `cargo build --no-default-features --features kernel,headless`
    succeeds and `examples/second_consumer.rs` runs; src-tauri compiles with
    no reference to kernel internals (boundary test); the GUI and console
    behave as before on the public API (existing IPC contract fixtures green).
35. No phase changes the plan-first behaviour of a `Workflow` profile agent.

## 8. Verify commands (run each once; stop after 3 failed fix attempts)

```
cargo build -p mnemo --no-default-features --features headless
cargo test -p mnemo
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo tree -p mnemo -i tauri
```
Leak check (phase 1 and 3 touch spawn/worktree paths): `git worktree list`,
`git branch --list "alf/*"`, and `git status --short` identical before and
after the test run.

## 9. Tests to add (by name)

- host::brain::tests::build_brain_headless_without_notifier
- host::brain::tests::every_emit_site_maps_to_host_event
- tests/integration/contract_fixtures.rs::host_event_payloads_match_app_events
- prompt::tests::workflow_stable_head_bytes_unchanged (golden)
- model_resolver::tests::workflow_policy_routing_table_unchanged (golden)
- policy::tests::kernel_does_not_import_app
- policy::tests::no_policy_surface_is_constant
- app::workflow_policy::tests::auto_continue_moved_behaviour_identical
- tests/integration/headless_setup.rs::{first_run_status_lists_missing_steps, configure_then_start, start_not_configured_errors, embedder_install_progress_fixture, embedder_install_from_dir_offline, select_embedder_live_reembeds, engine_starts_on_hash_fallback, key_never_exposed, key_from_env, init_project_idempotent}
- tests/integration/contract_fixtures.rs::settings_projects_embeddings_mcp_ipc_unchanged
- prompt::tests::{no_policy_requires_identity, prompt_spec_blocks_compose_in_fixed_order}
- factory::tests::{tool_selection_registers_only_groups, register_tool_lands_in_host_group}
- tests/integration/headless_kernel.rs::{workspace_without_git_starts, state_dir_outside_tree, two_engines_isolated, history_resume_preserves_request_bytes, snapshot_reports_pending_approval, limits_max_turns, limits_wall_clock, host_tool_roundtrip, host_tool_timeout, external_policy_gate_and_continue, external_policy_timeout_default, custom_memory_store_receives_calls}
- tests/boundary.rs::{kernel_builds_without_app_feature, tauri_uses_only_public_api}
- examples/second_consumer.rs (compile-tested; run in CI against the mock provider)
- prompt::tests::kernel_head_has_no_workflow_text
- dispatch::tests::no_policy_rejects_app_tools
- model_resolver::tests::free_route_resolves_free_slot_then_default
- turn::tests::no_policy_never_auto_continues_or_nudges
- tests/integration/headless.rs::{run_to_completion_mock_provider, override_permissions_runs_merge_push_without_approval, approve_via_api_prompts_and_honours_decision, two_subscribers_both_receive, no_project_errors, public_types_serde_roundtrip, session_command_malformed_json_errors, public_types_are_send_sync_static, ask_user_without_subscriber_answers_constant, start_blocking_without_runtime, closed_session_error_code, shutdown_leaves_no_tasks, free_agent_writes_no_plans}
- tests/integration/headless.rs::{free_agent_uses_memory_and_codegraph_tools, engine_start_emits_index_host_events, memory_disabled_errors, stats_totals_match_events, usage_rates_formula}
- turn::tests::savings_and_spend_emitted_where_persisted
- tests/integration/contract_fixtures.rs::stats_memory_codegraph_ipc_unchanged
- dispatch::tests::{override_permissions_skips_never_auto_for, approve_via_api_matches_legacy_gate}

## 10. Risks / review checklist

- R1 (cache): any byte drift in the workflow head or tool array re-bills every
  session. Check the golden test, and diff the head with the profile = Workflow.
- R2 (refactor size): phase 1 moves ~1500 lines across crates. Review it for a
  move with no logic change (`git diff --color-moved`).
- R3 (safety): `OverridePermissions` lets the agent push and merge to main
  with no human in the loop (U5/U6, intended). Guardrails on that: it is
  reachable **only** through the headless API (`PermissionPolicy` is never read
  from config, and GUI/console agents hard-code `ApproveViaApi`), it is
  documented on `EngineOptions`, and every bypass is an `ApprovalBypassed`
  event. Review: grep that no GUI/IPC path constructs `OverridePermissions`.
- R4 (hang): under `ApproveViaApi` with no answerer, the turn waits on
  `ApprovalRequest` forever. That is by design (the caller chose prompts);
  documented on the variant. `ask_user` has its own no-subscriber escape
  (criterion 16).
- R8 (event volume): `Savings`/`Spend` add events per request. They must
  not add to the GUI's transcript; the forwarder drops them (criterion 19
  guards the IPC side).
- R7 (binding-readiness drift): a Rust-only type (`AgentId`, `ModelRef`,
  `tokio::sync::broadcast::Receiver`) leaking into a public signature breaks a
  future JS wrapper. Review the `pub` surface of `mnemo::headless` against §5.5.
- R5 (2a is the riskiest step): routing every coupling site through the
  trait in ~25k lines of loop code. Mitigation: 2a is a pure indirection
  with golden tests before any code moves (2b), and review checks that each
  site maps 1:1. If 2a's diff exceeds ~3k lines, split it by file
  (turn/dispatch first, runtime auto-continue second).
- R6: `Subagent` state panics without an allow-list (workflow/mod.rs:462).
  That stays app-side; kernel `spawn_agent` takes an explicit allow-list
  from `spawn_roles()` or the parent surface.
- R9 (setup side effects): Phase 4 tests must never touch the real
  `~/.mnemo` (`MNEMO_HOME` = temp dir in every test) or download for real in
  CI (fixture model dir).
- R11 (scope growth): rev 5 roughly doubles the surface. Mitigation: each
  G-item is a separately reviewable unit inside its job, criteria 25-34 are
  per item, and phase 5 is the single integration gate; no G-item is "done"
  until the mnemo app runs on it (G10).
- R12 (external policy latency): every hook is a round trip to the host.
  Hooks are opt-in per kind and the kernel batches nothing; a host that
  selects `AfterTool` on a chatty agent pays for it. Documented on the spec.
- R13 (transcript compatibility): `Transcript` carries a `version`; a resume
  with an older version is rejected with code `transcript_version`, never
  silently re-laid-out (that would also break the cache prefix).
- R10 (old paths): 2b keeps `pub use` re-exports at the old module paths so
  src-tauri and tests compile unchanged. Removing them is a later cleanup.
- Review: run the verify commands myself, read the diffs, and confirm the
  console REPL still works on a quick manual run.

## 11. Tiering (when delegated)

Superseded by §14.4 (U11): every step defaults to deepseek; glm only for a
plan pass or where a step file says so. Previous text, kept for the record —
all nine jobs (1, 2a, 2b, 2c, 3a, 3b, 3c, 4, 5) → glm: cross-module design with
judgement calls. 2b is the closest to mechanical (moves + re-exports), but it
follows 2a's seam, so glm stays the default; deepseek can be tried on it if the
2a report leaves no decisions open.

## 12. Report format (per phase)

What changed (files + one line each); verify commands run, with pass/fail;
deviations from this spec and why; alternatives ruled out; open questions.

## 13. Open questions for the user

None blocking. Resolved 2026-10-11: weak-worker method + step files (U11, §14); engine-for-any-UI review (U9, §5.9); kernel/app split (U7); standalone setup API (U8); memory + code graph + stats in scope (§5.6, §5.7); U5 (no approvals, merge/push included),
U6 (never-commit-to-main dropped; JS deferred, Rust-only, binding-ready core).

## 14. Implementation method for a weak worker (U11)

The user wants the work implementable by DeepSeek Flash. The ALF rule for that
tier is "purely mechanical and fully specified: exact files, exact changes or a
pattern to copy, explicit acceptance criteria, nothing left to decide". So the
design stays with the coordinator, and every job is turned into **step files**
that a worker can execute by matching text and typing code verbatim.

### 14.1 Step files
- Each of the nine jobs gets a step file `.coding/specs/headless-free-agent/
  job-<nn>-<slug>.md`, written by the coordinator **right before** that job is
  delegated (so line anchors and counts are fresh against `main`), after the
  previous job merged. A job is split into steps of roughly ≤ 400 changed
  lines; each step is one ALF job on deepseek, reviewed and merged before the
  next.
- A step file contains, in this order: goal (one sentence); the exact file
  list ("no other file may change"); moves as "from this anchor to that
  anchor, verbatim"; a **replacement table** of exact old → new strings with
  expected counts; new code written out in full, to be typed verbatim;
  call-site edits as file + quoted old line → new line; the single
  compiler-driven edit allowed (deleting `unused import` names, both crates
  are `#![deny(warnings)]`); the forbidden list (no renames beyond the table,
  no doc rewrites, no `#[allow]`, no test deletion or weakening, no edits
  outside the listed files); verify commands, run once each; mechanical
  acceptance checks (greps with expected counts, named tests that must pass);
  the stop rule; the report format.
- The stop rule is on every step: "If any quoted anchor is not found exactly
  once, or the code you find does not match this file, or a verify command
  still fails after 3 fix attempts, stop and report what differs. Don't
  redesign."
- `.coding/specs/headless-free-agent/job-01-host-extraction.md` is the model:
  job 1 as three steps (1.1 pure helpers, 1.2 notifier + brain, 1.3 spawn path
  + bookkeeping), with 30 + 15 tabled replacements and 6 + 8 + 5 greps.

### 14.2 How design-heavy jobs become mechanical
- **Decisions are made in the step file, never by the worker.** The test is:
  if the coordinator would have to write "adapt as needed", the step is not
  deepseek-ready. Then either the coordinator reads more code and enumerates,
  or a glm **plan pass** produces the step file (glm writes it, the
  coordinator reviews it, deepseek executes it): the ALF split "glm plans,
  deepseek types, coordinator reviews".
- **Types first.** For the seams (`AgentPolicy`, `HostNotifier`, `Guardrails`,
  `Engine`/`AgentSession`), the first step types the public types and trait
  definitions verbatim from the spec, with bodies that compile and do nothing
  (the default impls the spec gives). Only after that compiles do the call-site
  steps follow, each a 1:1 table of "this call → that policy method".
- **Characterisation tests before behaviour-adjacent steps.** Golden tests
  (stable-head bytes, tool-array bytes per state, routing table) are added as
  their own step before 2a touches anything: "add this test file verbatim; it
  must pass without changing non-test code; if it fails, stop and report".
  Every later step inherits them as acceptance checks.
- **Prefer additive moves over edits in place:** new module + `pub(crate) use`
  re-exports at the old path, so callers compile unchanged and the diff is a
  move (`git diff --color-moved=dimmed-zebra` shows it).
- **One concern per step.** A step either moves code, or adds a type, or
  rewires call sites — never two of these at once.

### 14.3 Review gates the coordinator runs on every step
1. `git diff --color-moved=dimmed-zebra`: moved regions appear as moves; the
   only in-region changes are the tabled ones.
2. The step's greps and named tests; all verify commands re-run locally.
3. The `#![deny(warnings)]` cleanups removed imports only (no `#[allow]`, no
   dead code deletions beyond the table).
4. For steps that touch startup or spawn paths: smoke-run the console and a
   GUI debug build.
5. A calibration row per step. A step that needed a decision is a defect in
   the step file, not in the worker; the fix is a better step file, recorded.

### 14.4 Tiering under this method
- Default tier for every step: **deepseek**.
- glm is used only for a plan pass (writing a step file the coordinator could
  not fully enumerate) or when a step file explicitly says "glm" (the
  coordinator decides that at writing time, e.g. a root-cause hunt).
- The coordinator writes the step files for jobs 1 and 2c-3b directly; the 2a
  seam enumeration (every `Workflow` read in turn/dispatch/loop_impl/runtime)
  is the most likely glm plan pass, since it needs a full audit of ~25k lines.

