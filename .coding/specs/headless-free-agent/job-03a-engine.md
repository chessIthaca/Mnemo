# Job 3a — the Engine + HostRuntime + `mnemo::headless` begins

Master-spec anchors: §5.3 (:281-355 — the public API sketch, quoted in §C below), §5.1 :165-168 (D1/D2 deferred
into this job: "D1 `host::brain` is feature-gated on `browser`+`embeddings` until 3a un-gates it; D2 the
`HostRuntime` struct (and `ConsoleRuntime` as a thin wrapper) is built in 3a where the `Engine` needs it; D3 the
Laya manager's Tauri-free core moves in job 4."), §6's 3a scope row (:763), §5.9.4 (G4 :671-682), §5.9.5 (G5
:684-695), §5.9.7 (G7 :718-723), §7 rows 9/10/11/12/13/14/15 (:801-825), §9's test list (:922/:929), §6 :769-771
(the >3k-lines G-split rule).

## A. Job map

Job 2c closed the kernel gaps (G1-G4 kernel-form, §5.4 mechanism, the free slot). Job 3a opens phase 3: the
PUBLIC API begins. Seven deliverables, one per design pick (§C): the brain un-gate (D1), the HostRuntime
consolidation (D2), the `src/headless/` module skeleton (types + error), `Engine::start` (G4: no picker, no
global state), `spawn` + `AgentSession` (the session surface + the §5.4 exposure), the G5 transcript
(export/import/resume), the G7 limits. The G6 tiers (Guardrails/ExternalPolicy) and the host-tool channel are
3c; memory/codegraph/stats APIs are 3b; the builder extras + setup are job 4. The engine-form re-verifies of
rows 5/6/8 (free agent at the ENGINE level) land with 3a.5.

THE SIZE TRIGGER (§6 :769-771, verbatim): "2c and 3a grew with §5.9; if either report shows a diff over ~3k
lines, the G-items split into their own follow-up jobs before review." The steps below keep G5 and G7 LAST so
the split is a clean truncation: if 3a.1-3a.5 land >3k lines, G5+G7 become job 3a-bis BEFORE the review.

## B. Census (verified 2027-01-15, tree = 5376edc on wt/mnemo)

- src/host/mod.rs :11-12 (verbatim):
  `#[cfg(all(feature = "browser", feature = "embeddings"))]`
  `pub mod brain;`
  — D1's un-gate target: the WHOLE brain module compiles only under BOTH features today. Inside src/host/**
  there are NO other cfg(feature) sites (verified by grep — the kernel's feature sites live in
  agent/factory.rs :351/:496/:943/:951/:1378/:1628/:3101/:3527, memory/embedder.rs, provider/client_factory.rs,
  tool/mod.rs :17, lib.rs :15). The brain's feature coupling is therefore at its IMPORTS: build_brain wires
  the browser tool family and the embedder through the factory; the un-gate moves the gate from the module to
  those internal sites (probe brain.rs's factory/embedder/browser call sites at 3a.1 execution and cfg them
  item-level).
- src/host/brain.rs: `build_brain` :145-158 (async, returns `BrainOutcome`), `build_brain_inner` :162-1081,
  `Brain` struct (ends :104 — holds workspace/factory/manager-side state incl. `_graph_watcher`), `BrainOutcome
  { Ready(Brain) | NeedsProject(Config) }` :112-119 (verbatim doc: "no project could be resolved and the app
  should show the project picker"). §7 row 9's engine form: `Engine::start` maps `NeedsProject` to
  `EngineError::NoProject` — NEVER the picker.
- src/host/runtime.rs (1194 lines): the shared spawn path + lifecycle bookkeeping — `pub type AgentLoopMap`
  :23, `WorktreeBinding` :30, `pub async fn spawn_agent_shared` :73, `cleanup_inactive_subagents` :477,
  `HostSpawner` :528-615 (the AgentSpawner + ParentAwareSpawner impls; carries the permission param since 2c.5),
  `LifecycleBookkeeping` :633. D2's HostRuntime absorbs the ASSEMBLY that today lives in src-tauri (next item).
- src-tauri/src/console.rs :509-630 (verbatim comment at :509): "Headless core — ConsoleRuntime + HostSpawner"
  — the console's runtime assembly: `manager: Arc<tokio::sync::Mutex<AgentManager>>` :593, `AgentManager::new
  (256)` :616, one `HostSpawner` serving both spawner traits :626-628, fed by `crate::build_brain` :1210-1212
  (`BrainOutcome::NeedsProject` → the console's own no-project path :1212). THIS is the assembly that becomes
  `HostRuntime` in the lib; the console keeps a thin `ConsoleRuntime` wrapper over it (D2's letter).
- src/runtime/mod.rs: `AgentManager` (the session/task registry — `AgentManager::new(cap)`), the
  `AgentSpawner`/`ParentAwareSpawner` traits (the `permission` param, 2c.5), `DescendantTracker`.
- src/runtime/channels.rs: `AgentCommand` (the existing command enum — Prompt/Steer/CancelSteer/Interrupt/
  Compact/Clear/Approve/Answer…; `SessionCommand` :329-338 of the spec is the PUBLIC mirror), `AgentHandle`,
  `AgentId`, `SerializableEvent` (SessionEvent's inner), the forwarders (:783/:846 patterns).
- src/agent/context.rs: the conversation layout (messages, tool calls/results, compaction summaries) — G5's
  export/import preserves the byte prefix; probe the layout types at 3a.6 execution.
- src/config/mod.rs :485: `global_config_dir()` — G4 :676-677 (verbatim): "used only by `Setup::open(None)` and
  the app; every kernel path takes its directories from `EngineOptions`." In-scope check at 3a.4: the Engine
  passes config_dir explicitly; global_config_dir stays for the app (its 16 callers today are app-side + config
  internals — verified via graph, no kernel-path callers beyond config itself).
- src/lib.rs :13-33: the module list (no `headless` yet — 3a.3 adds it; `workspace` :33 exists from 2c.3).
- tests/integration/: main.rs (the runner), contract_fixtures.rs, ipc_bridge.rs, workflow_integration.rs,
  encoding_guard.rs, ci_workflow.rs. 3a adds headless.rs + headless_kernel.rs (§9's names).
- The battery greps (2c's §D): SteeringStats::shared() absent ✓ (2c.4), no kernel .gitignore write ✓,
  src/instance_marker.rs absent ✓ (2c.3) — unchanged by 3a, re-checked at the close.

## C. Design picks (D1-D9; each supersedes nothing in 2c — this is the job's own census)

- **D1 (the brain un-gate).** src/host/mod.rs :11-12's module gate lifts to `pub mod brain;` UNGATED. The
  feature coupling moves INSIDE brain.rs at the exact sites that touch gated items (probe at execution: the
  embedder init, the browser family registration, any embeddings-gated provider bits) — item-level
  `#[cfg(feature = ...)]` with runtime-optional wiring where the brain must still build without them (the
  embedder becomes `Option`/absent exactly as the no-features factory already handles it — the factory already
  compiles bare, so the brain's references must go through the same cfg shape). Acceptance: `cargo check -p
  mnemo` (NO features) compiles the brain; `cargo test -p mnemo --features browser,embeddings` stays green; the
  §9 test `host::brain::tests::build_brain_headless_without_notifier` (landed job 1) stays green. NOTE: the
  default `cargo test -p mnemo` run has NEVER compiled the brain (it compiles only under both features) — after
  the un-gate the DEFAULT battery covers the brain for the first time; any test that needed features gets
  feature-gated or mocked at the seam.
- **D2 (HostRuntime).** New `pub struct HostRuntime` in src/host/runtime.rs: `{ manager: Arc<Mutex<AgentManager
  >>, agent_loops: AgentLoopMap, spawner: Arc<HostSpawner>, bookkeeping: LifecycleBookkeeping, events: the host
  event bus }` — the assembly console.rs :593-630 builds inline today, hoisted into the lib beside HostSpawner.
  Constructor `HostRuntime::new(factory: Arc<AgentLoopFactory>, capacity)` mirroring console.rs :616-628 (one
  HostSpawner serving both spawner traits). The console's struct at :509+ keeps its role as the thin wrapper:
  its fields become `runtime: HostRuntime` + the console-only bits (the initial selection state :542-545); its
  spawn/forward sites delegate. NO behavior change — the console tests (console.rs :2108-2440, the HostSpawner
  + manager fixtures) stay green through the move (they construct the pieces directly; only the assembly site
  changes). The ENGINE wraps HostRuntime (§5.3 :295 verbatim: `pub struct Engine { /* HostRuntime */ }`).
- **D3 (the headless module).** `src/headless/` with mod.rs re-exporting the public surface: engine.rs,
  session.rs, types.rs, error.rs (§6's row, verbatim file list). lib.rs gains `pub mod headless;`. The public
  types are NEW names over EXISTING kernel types where §5.3 says so: `SessionEvent` = newtype over
  `runtime::channels::SerializableEvent` + the emitting `AgentId` (:345-348); `SessionCommand` = the public
  mirror of `AgentCommand` (serde `tag = "type"`, rename_all snake_case, :327-338 verbatim) — converted at the
  session boundary, ONE mapping, malformed JSON errors at the boundary (row 13); `EventReceiver<T>` = the
  multi-subscriber bus (broadcast channel; `RecvError::{Closed, Lagged(u64)}`, :339-343); `SessionId`,
  `TurnOutcome`, `SessionStats` (§5.7's totals — 3b completes them), `HostEvent` (job 1's host notifier events,
  re-exported).
- **D4 (EngineOptions + start).** §5.3 :284-302 verbatim: EngineOptions { workspace, config_dir, permissions,
  memory, index_codegraph, require_git, auto_install_embedder } + `Engine::start(opts) -> Result<Engine,
  EngineError>`. The start path: build the config from config_dir (MNEMO_HOME/~/.mnemo default), resolve the
  workspace (2c.3's `Workspace`), call the brain builder with a NO-PICKER contract — `NeedsProject` maps to
  `EngineError::NoProject` (row 9: "it never blocks and never shows a picker"), `require_git` honored
  (workspace_without_git_starts + the gate from 2c.3). `Engine::shutdown(self)` ends sessions + runs the
  session-end consolidation (2c's kernel path). `start_blocking` (:823-825 row 15) = a static tokio runtime
  wrapper. The Rust-only builder extras (:294: memory_store/shared/register_policy/register_tool) START in 3a
  (register_policy's map) and complete in job 4 — typed now, wired where the spec's §5.9.6 tier machinery lands
  (3c), stubbed with `unimplemented`-free passthroughs where 3c owns the semantics. NO — the builder methods
  that 3c/4 own are NOT in 3a's surface at all: 3a's Engine exposes start/spawn/session/host_events/shutdown/
  start_blocking only (§5.3 :296-302 minus the builder line).
- **D5 (spawn + the session surface).** `SpawnOptions` (:303-314) — 3a's subset: name, model, worktree,
  permissions (the per-session override; the 2c.5 kernel field consumes it — the SpawnOptions→factory→loop
  threading, the 3a Engine forward the 2c review enumerated), resume (3a.6), policy: Option<String> (the
  registered-name map — 3a builds the map + the default "free"/NoPolicy path; "mnemo.workflow" registers when
  the app policy factory is passed). Guardrails/external_policy/host_tools fields are 3c — NOT in 3a's struct
  yet (additive variants land with their owners; row 13's serde test covers what exists).
  `AgentSession` (:315-326): id/send/events/run_to_completion/history/snapshot/stats/close. `send` on a closed
  session → EngineError code "session_closed" (row 15). `events()` multi-subscriber: two subscribers both
  receive everything; dropping one never stalls (row 12 — the broadcast D3 bus). run_to_completion on the mock
  provider returns the final text; the stream shows Started…Finished in order (row 10). ask_user with ZERO
  subscribers at question time → the immediate constant Freeform answer, logged as an event (:350-354 verbatim
  + row 14).
- **D6 (G5 — the transcript).** §5.9.5 :684-695 verbatim: history() → Transcript (serde: messages with roles,
  tool calls/results, images as refs, compaction summaries, THE LAYOUT METADATA so a resumed request
  reproduces the same byte prefix); SpawnOptions.resume: Option<Transcript>; snapshot() → SessionSnapshot
  { transcript, context_usage, model, pending: Option<Approval|Question|HostToolCall>, stats }. The
  export/import lives in src/agent/context.rs (§6's row: "transcript export/import that preserves layout").
  The cache contract (:693-695): same transcript + same PromptSpec + same ToolSelection + same model ⇒ the
  request body bytes are IDENTICAL to pre-restart (the provider prefix-cache test —
  history_resume_preserves_request_bytes, the mock provider's captured bodies compared byte-for-byte).
- **D7 (G7 — the limits).** §5.9.7 :718-723 verbatim: SessionLimits { max_turns, max_requests,
  max_prompt_tokens, max_completion_tokens, max_cost_micros, wall_clock_ms, idle_ms } all Option; hitting one
  ends the turn with Finished { reason: LimitReached { which } } and parks the session; BudgetConfig remains
  the default when None. 3a implements max_turns + wall_clock_ms (the two §9 tests: limits_max_turns,
  limits_wall_clock) + the LimitReached Finished reason variant (additive — the app event fixtures stay
  green); the token/cost/idle fields are typed in SessionLimits but land with 3b (the spend accounting) —
  documented as the 3b forward.
- **D8 (G4 — isolation).** two_engines_isolated (§5.9.4 :680-682): two Engines, different workspaces + config
  dirs, one session each, concurrent; stats/memory/trace/events never cross. The Engine holds NO process
  globals: config from EngineOptions, workspace from EngineOptions, the steering stats Arc (2c.4) per-engine,
  the instance marker stays app-side (2c.3). The OnceLock caches (:678-679) stay (immutable).
- **D9 (the permission exposure).** §5.4's phase-3 exposure at the engine form: EngineOptions.permissions (the
  default) + SpawnOptions.permissions (the override) thread to the loop's kernel field (2c.5). The engine-form
  rows: override_permissions_runs_merge_push_without_approval + approve_via_api_prompts_and_honours_decision
  (headless.rs) — the §7 row 11 Engine half. GUI/console behavior UNTOUCHED (their factories keep wiring
  ApproveViaApi — the 2c.5 factory line stays the app path).

## D. Steps (in order; each = a boundary commit --no-verify; the goldens + the battery gate each step's complete)

- **3a.1 (D1)** — the brain un-gate: lift mod.rs :11-12, cfg the internal feature sites, first-ever default-
  battery coverage of the brain. Tests: the brain's existing suite green under BOTH configs; `cargo check -p
  mnemo` bare green.
- **3a.2 (D2)** — HostRuntime in src/host/runtime.rs + the console's thin-wrapper refactor. Tests: the console
  suite green unchanged (the assembly-site move is behavior-neutral).
- **3a.3 (D3)** — src/headless/{mod,error,types}.rs: the public types + the serde/Send+Sync contract tests
  (public_types_serde_roundtrip, session_command_malformed_json_errors, public_types_are_send_sync_static) —
  green on types alone.
- **3a.4 (D4)** — Engine::start/start_blocking/shutdown/host_events + EngineOptions; NoProject; require_git;
  config_dir. Tests: no_project_errors, start_blocking_without_runtime, workspace_without_git_starts,
  state_dir_outside_tree (headless_kernel.rs).
- **3a.5 (D5+D9)** — spawn + AgentSession + the event bus + the permission threading. Tests:
  run_to_completion_mock_provider, two_subscribers_both_receive, closed_session_error_code,
  shutdown_leaves_no_tasks, ask_user_without_subscriber_answers_constant,
  override_permissions_runs_merge_push_without_approval, approve_via_api_prompts_and_honours_decision,
  free_agent_writes_no_plans (the engine-form row 8).
- **3a.6 (D6)** — G5: context.rs export/import + history/snapshot/resume. Tests:
  history_resume_preserves_request_bytes, snapshot_reports_pending_approval.
- **3a.7 (D7)** — G7: SessionLimits (typed full, implemented max_turns/wall_clock) + LimitReached. Tests:
  limits_max_turns, limits_wall_clock.
- **3a.8 (D8 + the close)** — two_engines_isolated + the acceptance close (the §F report + review rounds +
  finish + the master tick).


## E. Verification (the battery — every step; unpiped on PowerShell, $LASTEXITCODE read directly)

- `cargo build --workspace`; `cargo test --workspace`; `cargo test -p mnemo`; `cargo test -p mnemo
  --features browser,embeddings` — plus from 3a.1 on, `cargo check -p mnemo` BARE is part of every step's
  battery (the brain now compiles default; a bare-check regression is a step failure).
- The goldens trio (workflow_stable_head_bytes_unchanged, workflow_policy_routing_table_unchanged,
  volatile_tail_section_order_pinned) green at EVERY step (the 2a.0 contract).
- Compile-enforced greps at the close: kernel sources carry no `crate::app` token (the lib.rs boundary test
  runs in-suite); SteeringStats::shared() call sites absent; src/instance_marker.rs absent; NEW for 3a:
  `global_config_dir` has NO caller under src/headless/** (G4 :676-677 — the engine passes config_dir
  explicitly; the grep rides the 3a.8 close).
- Both crates #![deny(warnings)] — a green cargo test proves zero warnings; never #[allow]. The clippy
  habit-leg: the rustc-1.95 storm stays out of scope (backlog a439d8c2) EXCEPT new sites in 3a's own hunks —
  zero new sites per step (the 2c rule).

## F. Report format (the close)

The 2c close-out shape: Landed (the per-step commits) / the test results (the battery at the final tree) / the
acceptance check-offs (row → test → result: rows 9, 10, 11-engine, 12, 13, 14, 15 + the engine-form rows
5/6/8 + G4/G5/G7's own tests) / the deviations (each + why) / the remaining risk + the 3b/3c/4 forwards.

## G. Amendments (the running record — append one per step at its boundary commit)


Amendment (2027-01-15, step 3a.1 — the landed D1 picks):
- The lift (src/host/mod.rs :11-12): the `cfg(all(feature = "browser", feature =
  "embeddings"))` module gate is GONE — `pub mod brain;` compiles on every
  feature combination, with the D1 rationale comment above it.
- The coupling map (compiler-enumerated after the lift — exactly 3 sites, ALL
  browser, NONE embeddings): the Brain struct's `browser` field, the
  BrowserManager::new + set_webview_enabled statements, and the factory
  chain's `.with_browser(...)` tail. Each is now item-level
  `#[cfg(feature = "browser")]`: the field, the two creation/inspection
  statements, a shadowing rebind (`let factory = factory.with_browser(...)`
  after the chain ends at `.with_reflex(...)`), and the construction's
  `browser` field. The embeddings side needed NOTHING: Embedder /
  HashEmbedder / EmbedderStatus are ungated (client_factory.rs :23's bare
  import proves it) and build_brain_inner's embedder startup already runs
  the hash fallback bare — the brain was feature-ready except for the
  browser three.
- First-ever bare coverage: `cargo check -p mnemo` (NO features) green — the
  brain had never compiled without both features before this step; the
  default `cargo test -p mnemo` now compiles + runs the brain's tests
  featureless.
- Clippy: the un-gate surfaced 4 pre-existing brain sites to the DEFAULT
  clippy run (the storm never compiled brain.rs bare): 3 needless borrows
  (:689/:695/:703 — the `&workspace.*.join(...)` drops, FIXED in-step) + 1
  large_enum_variant (:116 — BrainOutcome's Ready(Brain) vs
  NeedsProject(Config); NOT mechanical, a Box-ing that ripples through every
  consumer's match — left to the a439d8c2 sweep with this pointer). Net: the
  3a.1 delta adds ZERO new storm sites to any build configuration.
- The battery at the final tree: check -p mnemo bare (check_exit=0) + test
  -p mnemo (test_exit=0) + test -p mnemo --features browser,embeddings
  (feat_exit=0) + test --workspace (ws_exit=0) + build --workspace
  (build_exit=0); the goldens trio 3/3 in-suite;
  build_brain_headless_without_notifier green under features, unchanged.

Amendment (2027-01-15, step 3a.2 — the landed D2 picks):
- HostRuntime (src/host/runtime.rs, inserted between HostSpawner's impl and
  LifecycleBookkeeping): `pub struct { factory, manager, agent_loops,
  spawner, fanin_rx, main_id, bookkeeping }` + `pub async fn start(factory)
  -> Result<Self, String>` — the EXACT assembly the console built inline
  (AgentManager::new(256), the fan-in receiver taken before the wrap, one
  HostSpawner serving both spawner traits set on the factory BEFORE the
  main spawn, the main agent via spawn_agent_shared with the GUI's exact
  args, LifecycleBookkeeping over the shared maps). String errors — the
  kernel stays anyhow-free; the Engine layer maps into EngineError.
- The console thin-wrapper: ConsoleRuntime = { _brain, runtime:
  HostRuntime, config, project, trace, browser, selection } — 12
  delegation sites rewritten (fanin_rx/main_id/manager/factory/
  agent_loops/bookkeeping → self.runtime.X); `start` delegates to
  HostRuntime::start + keeps the console-only bits (selection computed
  after the host core — same ordering as the old inline code); the four
  host-core type imports moved into the tests module so BOTH
  configurations stay warning-free (plain check AND all-targets under
  deny(warnings)).
- NO behavior change: the console suite green unchanged (ws_exit=0 — the
  fixtures construct HostSpawner/AgentLoopMap directly and exercise the
  delegation paths).
- The battery at the final tree: check --workspace (check_exit=0) + test
  -p mnemo (test_exit=0) + test --workspace (ws_exit=0) + test -p mnemo
  --features browser,embeddings (feat_exit=0) + build --workspace
  (build_exit=0); the goldens trio 3/3 in-suite; clippy: host/runtime.rs
  EXACTLY its 5 pre-existing storm sites (zero new from the ~100-line
  insert, verified by count), console.rs 0 (never a storm file; still 0).

Amendment (2027-01-15, step 3a.3 — the landed D3 picks):
- src/headless/{mod,types,error}.rs + `pub mod headless;` in lib.rs.
  types.rs: SessionId (newtype over AgentId = u64, private field),
  MemoryBackend { Sqlite { path: Option<PathBuf> } with a MANUAL Default
  (the derive cannot express a non-unit variant), Disabled },
  EngineOptions (§5.3 :284-293 fields; no Default — a workspace isn't
  defaultable), SpawnOptions (the 3a subset: name / model / worktree /
  permissions / policy / resume; guardrails / host_tools /
  external_policy are 3c's additive land), SessionCommand (tag = "type",
  snake_case; Prompt/Steer/CancelSteer/Interrupt/Compact/Clear/Approve/
  Answer — HostToolResult + PolicyAnswer are 3c), SessionEvent { agent,
  event: SerializableAgentEvent }, EventReceiver<T> + RecvError::{
  Closed, Lagged(u64) } (the broadcast From), SessionLimits (all seven
  §5.9.7 fields typed; 3a.7 implements max_turns/wall_clock), Transcript
  (placeholder messages: Vec<Value>; 3a.6 fills the layout).
- error.rs: EngineError { code, message } + code()/message() + Display +
  std::error::Error, serde-able. The kind constructors (no_project /
  session_closed / spawn_failed / engine_shutdown) were written
  ahead-of-use and hit deny(warnings) dead_code — TRIMMED; each lands
  with its 3a.4/3a.5 caller (same for EventReceiver::from_broadcast).
- Workspace gained Serialize + Deserialize (EngineOptions carries it,
  §5.3 :286).
- The contract fixtures (tests/integration/headless.rs — the merged
  integration binary's new module): public_types_serde_roundtrip
  (serialize → deserialize → re-serialize, the JSON forms compared — no
  PartialEq needed on the types; the tag spellings pinned: {"type":
  "interrupt"} / {"type": "prompt", …}; the kebab-case permission shape
  pinned: {"mode": "ApproveViaApi", "safety_mode": "autonomous"}),
  session_command_malformed_json_errors (unknown tag / missing field /
  wrong tag key / non-object all reject), public_types_are_send_sync_static
  (11 types).
- In-flight repairs: MemoryBackend's #[default] on a non-unit variant
  (manual Default impl), EventReceiver's Clone derive (this tokio's
  broadcast::Receiver is NOT Clone — dropped; fresh subscribers come from
  AgentSession::events()), the dead-code trim above; EngineError's test
  construction goes through serde (the constructors are engine-internal).
- The battery at the final tree: test -p mnemo (test_exit=0, 2716 — the
  three contract tests ride the integration target) +
  --features browser,embeddings (feat_exit=0, 2743) + --workspace
  (ws_exit=0, 3063) + build --workspace (build_exit=0); the goldens trio
  3/3 in-suite; clippy: ZERO sites in the new files; workspace.rs:94
  ("items after a test module") is pre-existing 2c.3-era code outside
  this step's hunks — rides a439d8c2.

Amendment (2027-01-15, step 3a.4 — the landed D4 picks):
- The split (brain.rs): `build_brain_inner` keeps ONLY the app path — the
  global config load + the discovery block (pending marker → `--project`
  flag → cwd walk-up → NeedsProject) — then delegates to the NEW
  `pub(crate) wire_brain(config_dir, config, workspace, switched_open,
  notifier, rt) -> anyhow::Result<Brain>`: the whole wiring tail (trace,
  provider stack, memory, sandbox, safety, resolver, factory, watcher)
  with NO discovery and NO global read. The tail's
  `Ok(BrainOutcome::Ready(Brain {…}))` became `Ok(Brain {…})`; the app
  wraps via `.map(BrainOutcome::Ready)`. The trace creation moved INTO
  wire_brain (it was never used before the tail).
- The Engine (src/headless/engine.rs): `start(opts)` — the workspace
  gates FIRST (root missing, or require_git unmet → `EngineError`
  no_project — never a picker, row 9); BOTH require_git sources honored
  (`EngineOptions.require_git || workspace.require_git` — §5.9.3 :664 +
  the 2c.3 marker); the config from the explicit dir → $MNEMO_HOME →
  ~/.mnemo (`default_config_dir` — deliberately NOT global_config_dir,
  G4 :676-677); the `BusNotifier` (HostNotifier →
  broadcast::Sender<HostEvent>) — the engine ALWAYS has a host bus;
  wire_brain with `switched_open = false`; then HostRuntime::start over
  the factory. `host_events()` subscribes. `shutdown(self)` is the frame
  (3a.5 completes it — sessions + consolidation). `start_blocking`: a
  multi-thread Runtime OWNED by the Engine (dropped with it) +
  block_on(start) — works from a plain `fn main` (row 15).
- The not-yet-read fields are underscore-prefixed (_brain, _host,
  _default_permissions, _owned_runtime) — the Brain's `_graph_watcher`
  precedent under deny(warnings); 3a.5 renames _host/
  _default_permissions when their readers land. EngineOptions'
  unconsumed options (memory, index_codegraph, auto_install_embedder)
  are 3b's wiring — typed now, documented forward.
- error.rs: no_project + start_failed landed WITH their callers (the
  3a.3 trim rule: never ahead of use).
- Tests: headless.rs::{no_project_errors (both gates → code
  "no_project"), start_blocking_without_runtime (a plain #[test], the
  owned runtime)}; headless_kernel.rs::{workspace_without_git_starts (a
  fresh temp dir starts + shuts down clean), state_dir_outside_tree (the
  memory db lands at the OUT-of-tree state dir + no .coding appears
  under the root)} — 7/7 with the 3a.3 contract trio. In-flight
  repairs: expect_err demanded Engine: Debug → a match instead (Engine
  holds non-Debug internals); a doc-comment line lost its /// prefix in
  the write (fixed).
- The battery at the final tree: check -p mnemo bare (check_exit=0) +
  test -p mnemo (test_exit=0, 2719) + --features browser,embeddings
  (feat_exit=0, 2746) + --workspace (ws_exit=0, 3066) + build
  --workspace (build_exit=0); the goldens trio 3/3 in-suite; clippy:
  ZERO sites in src/headless/** + the tests; brain.rs:116 (the
  3a.1-documented large_enum_variant) is that file's only site, outside
  this step's hunks.

Amendment (2027-01-15, step 3a.5 — the landed D5+D9 picks):
- The session surface: src/headless/session.rs — EngineInner (the host
  core + the live-session registry + the pending approval/question
  responder maps), pump_task (fan-in → the lifecycle bookkeeping at the
  same per-event points as the console's run loop → responder stashing →
  the zero-subscriber constant Freeform answer → the broadcast),
  AgentSession { id, events(), send(), run_to_completion(), close() }.
  send maps the public SessionCommand → AgentCommand (Suggestion carries
  SteerPayload{text, images: []}); Approve/Answer resolve through the
  pump's responder maps; every send on a closed session → code
  "session_closed". The event bus is ENGINE-WIDE (capacity 1024) with
  the agent tag — lineage-scoped buses are a later refinement; the
  run_to_completion loop skips other agents' events and breaks on the
  session's own Finished/Exited.
- The engine: EngineBuilder (the job-4 builder's FIRST slice, landed for
  testability: .provider(Arc<dyn LlmClient>) → factory.set_provider
  BEFORE the main-agent build); Engine::spawn resolves the permission
  (SpawnOptions override | the engine default) and passes it through
  spawn_agent_shared's NEW permission param; model = "endpoint/model"
  parsed to ModelRef; policy: Some → spawn_failed (the registry is job
  4); worktree option typed, wired with 3c; shutdown cancels every live
  session + aborts the pump; HostRuntime::take_fanin_rx moves the
  receiver into the pump (a closed channel takes its place).
- The kernel plumbing: AgentLoop.permission is RwLock-backed with
  permission()/set_permission() (the set_provider precedent — the
  per-session swap is applied post-build, race-free before the task
  starts); spawn_agent_shared's 11th param permission: Option<
  PermissionPolicy> (None = the factory default �� every pre-3a.5
  caller); HostSpawner::spawn_with_parent FORWARDS its 2c.5 permission
  param (the discarded-param review forward is closed).
- THE FREE GATE (the step's real discovery): the engine's git fixtures
  exposed that NoPolicy's gate:None falls back to the workflow read
  (dispatch.rs :255-258), which refuses mutation tools in Complete —
  and UnwiredWorkflow.allowed_tools ALSO returns Complete. First
  attempt (NoPolicy answers ToolFilter::Free unconditionally) broke 5
  kernel tests that model WORKFLOW agents riding that fallback. The
  landed design: NoPolicy gains `ungated: bool` (default false — the
  old semantics everywhere) + NoPolicy::ungated(); the new
  FreePolicyFactory builds NoPolicy::with_spec(spec).ungated() and the
  ENGINE sets it on its factory (the app keeps its wiring). SUBTLETY:
  factory.rs :1194's build_policy fallback attaches the NON-ungated
  coding default on error — the factory therefore falls back to the
  coding-default spec ITSELF (never errors). ToolFilter::Free (allows()
  early-return true after the write_review_report hard rule) is the
  gate; the ADVERTISE side still falls back to UnwiredWorkflow's
  Complete schema (the mock ignores schemas; real-provider free
  advertisement is a 3b polish — documented forward).
- Tests (headless.rs, the ScriptedLlm mock + 8): run_to_completion_
  mock_provider, two_subscribers_both_receive, closed_session_error_
  code, shutdown_leaves_no_tasks (the session handle holds a bus-sender
  clone — dropped before the Closed assertion), ask_user_without_
  subscriber_answers_constant (zero subscribers proven via the mock's
  call counter — no subscriber ever created), override_permissions_
  runs_merge_push_without_approval (git merge → ApprovalBypassed{git,
  core_op}, NO ApprovalRequest), approve_via_api_prompts_and_honours_
  decision (ApproveViaApi+ApproveEachAction → ApprovalRequest →
  Approve resolves → executed), free_agent_writes_no_plans (the
  sandbox refusal + nothing lands under .coding/plans).
- In-flight repairs: expect_err needed Engine: Debug → match; the mock
  returns crate::error::Result + Box::pin (the factory MockProvider's
  exact shape); a killed run's zombie exe locked the link (Stop-Process
  between runs); a shutdown test hung on the session's sender clone.
- The battery at the final tree: the module 15/15 + test -p mnemo
  (test_exit=0, 2726) + --features browser,embeddings (feat_exit=0,
  2753) + --workspace (ws_exit=0, 3073) + build --workspace
  (build_exit=0); the goldens trio in-suite; clippy: ZERO sites in
  src/headless/** and zero NEW sites (dispatch :1312/:1317/:1353 +
  loop_impl :279/:371 are pre-existing storm, outside this step's
  hunks).
- Forwards: history/snapshot/stats + the TurnOutcome extensions = 3a.6;
  the worktree option + HostToolResult/PolicyAnswer arms = 3c; the
  policy registry + the builder's full surface = job 4; the free
  ADVERTISE surface + memory/index/embedder consumption = 3b.

### 3a.6 (D6) — G5: the transcript export/import + resume — landed 2026-01-13

- The ExportHistory reply rides a 1-cap mpsc (NOT oneshot — the Sender
  isn't Clone and the mid-request arm in turn.rs answers from a
  pre-request snapshot cloned before the pin, needing a second send
  site); `spawn_agent_shared` grew its 12th param
  `resume: Option<Vec<Message>>` (every app caller passes None —
  ipc/spawn.rs ×3 + main.rs ×1).
- Mid-block semantics (the hang fix): during a pending approval the
  turn blocks mid-dispatch and NOBODY drains the command channel, so a
  live export there would wait forever. `export_conversation` now
  bounds the wait at 2s — history() errors `session_busy`, snapshot()
  degrades its transcript to empty (pending stays engine-local and
  ALWAYS accurate). Verified: snapshot_reports_pending_approval asserts
  pending Some mid-block + None + a FULL transcript after the answer.
- The byte-prefix test's contract is shifted-position, not
  index-aligned: the volatile tail + context footer ride AFTER the
  history by design (build_volatile_tail — the stable head stays
  cache-immune), so the resumed layout is [head, history(+reply, +new
  prompt), tail, footer] and the original messages sit byte-identical
  at i<=1→i, i>1→i+2. Spec §5.9.5 :693-695's "identical bytes" = every
  carried message serializes identically (verified) + the shared cache
  prefix [stable head, user turn] hits.
- Clippy: turn.rs :4133/:4135 needless_borrow (complete_with_retry's
  &provider/&tool_schemas) were this step's and are FIXED; runtime.rs
  :73 too_many_arguments is pre-existing (11 params at 73d5322,
  git-shown — the count moved 11/7→12/7 with the resume param;
  struct-ification is sweep material); channels.rs :169 (RecallHit doc
  list) + agent.rs :379 + loop_impl :279/:371 are pre-existing storm,
  outside this step's hunks.
- In-flight repairs: the naive positional assert first failed at
  message 2 (ENGINE1 tail vs ENGINE2 carried reply — the position
  shift); the Option juggling in snapshot() needed a single destructure
  (partial-move E0382 ×3 rounds).
- The battery at the final tree: the module 17/17 (15 headless + 2 new
  G5) + test -p mnemo (kernel_exit=0, 3005+41) + --features
  browser,embeddings (feat_exit=0, 3032+41) + --workspace (ws_exit=0)
  + build --workspace (build_exit=0); clippy zero NEW sites in own
  hunks.

### 3a.7 (D7) — G7: SessionLimits + LimitReached — landed 2026-01-13

- SessionLimits MOVED from headless/types.rs (where 3a.3 pre-landed it)
  to runtime/channels.rs — the kernel-neutral home so the host spawn
  path carries it without layer-inverting into `headless`; the public
  surface stays `mnemo::headless::SessionLimits` via re-export (the
  HostEvent precedent). Typed FULL (all seven §5.9.7 fields); 3a.7
  enforces max_turns + wall_clock_ms, the rest ride 3b.
- LimitKind (7 variants) + FinishReason::LimitReached { which } live in
  provider/mod.rs — additive; sse_util's finish_reason_label gained the
  "limit_reached" arm (runtime-constructed, never off the wire). The
  frontend FinishReason mirror gained the limit_reached member.
- The enforcement: SessionLimiter (loop_impl, Mutex on the loop — the
  set_permission precedent) checks at the TOP of run_turn's request
  loop, after the deferred-swap early exit, mirroring the routing
  iteration count. Counting semantics: max_turns counts PROVIDER
  REQUESTS (tool-call chain iterations + provider-error retries — every
  request burns budget); the wall-clock epoch = set_limits time (the
  engine's spawn). On a hit: Finished{LimitReached{which}} + the turn
  returns TurnOutcome{finish_reason: LimitReached}.
- RUST GOTCHA fixed in-flight: an if-let scrutinee temporary (the
  MutexGuard) lives through the block — pinning the future to the
  thread (Send error). Bind to a `let` first, drop the guard before
  the await.
- spawn_agent_shared grew its 13th param limits (all 6 app/in-file
  callers pass None) — runtime.rs:73 too_many_arguments stays
  PRE-EXISTING (11→12→13 with the 3a.5/3a.6/3a.7 params;
  struct-ification is sweep material).
- Parking is ENGINE-side (§5.9.7 "parks the session"): AgentSession
  watches its own stream — a Finished{LimitReached} sets the parked
  flag; later Prompts error `session_parked` (a new EngineError code,
  asserted via the public code() accessor). TurnOutcome gained
  finish_reason (additive, serde default) so headless callers see WHY a
  run ended; the headless.rs SpawnOptions fixture gained limits.
- The battery at the final tree: the module 19/19 (15 headless + 2 G5
  + 2 new G7) + test -p mnemo (kernel_exit=0, 3005+43) + --features
  browser,embeddings (feat_exit=0, 3032+43) + --workspace (ws_exit=0)
  + build --workspace (build_exit=0); clippy zero NEW sites in own
  hunks (the listed turn.rs/loop_impl/provider sites are pre-existing
  storm outside this step's diff).
