# Job 3b — memory/codegraph/stats: the session builders' data APIs

Master-spec anchors: §5.6 (:404-437 the memory/codegraph design), §5.7 (:439-486 stats and telemetry), §6's 3b row (:764), §7 rows 17/18/19 (:828-840), §9's 3b test list (:930-932), R8 (:951-953 the GUI-forwarder drop).

## A. Job map

Job 3a closed the engine + session surface (review PASS at c38e7b2; master 7/11). Job 3b is the ENGINE's data surface: consume the typed-but-dormant `EngineOptions.memory` + `EngineOptions.index_codegraph` (G4-consistent — no globals), expose `Engine::memory() -> MemoryApi` + `Engine::codegraph() -> CodegraphApi` (the IPC commands become thin callers — ONE implementation), surface the startup index/embedder progress on `host_events()` (HostEvent grows), and land the stats/telemetry contract (§5.7): the live `Savings`/`Spend` events at the persistence points, the derived `UsageRates`, and the pull surfaces (`AgentSession::stats()`/`stored_stats()`, `Engine::stats()`, `Engine::trace()`). The free agent's memory/codegraph tool access (row 17's first clause) is verified, not built — the 2c free gate already allows the tools.

## B. Census (verified 2027-01-18, tree = c38e7b2 on wt/mnemo)

- src/headless/types.rs :47-92: `MemoryBackend { Sqlite { path: Option<PathBuf> }, Disabled }` + manual Default (Sqlite{None}) + `EngineOptions` FULLY TYPED per §5.3 (workspace/config_dir/permissions/memory/index_codegraph/require_git/auto_install_embedder) — the memory-backend/index/embedder option CONSUMPTION is 3b's work (the engine.rs module doc already says so).
- src/agent/turn.rs: the persistence points — `record_stats_row` :3711 (RequestStats → store.record_request_stats), `record_spend_row` :3772-3821 (SpendEvent built :3799, store.record_spend_event, fire-and-forget), `record_savings_event` :3829+ (the savings ledger; store.record_savings_event). §5.7's design: the NEW live `AgentEvent::Savings`/`AgentEvent::Spend` emit FROM these two methods, at the same points they persist. Both are `&self` loop methods WITHOUT fanin access — probe the loop's manager/agent-id access at 3b.4 execution (the loop holds a manager handle per from_config's comment; AgentManager::fanin_sender() is the emit route; agent id from build_with_id state).
- src/runtime/channels.rs :233/:316/:249 (spec's facts): `AgentEvent::Usage { prompt_tokens, completion_tokens, reasoning_tokens, cached_tokens, ttft_ms, generation_ms }`, `ModelChanged`, `ContextUsage` — all EXIST and ride the pump's forwarder; `Savings`/`Spend` variants are NEW (+ Serializable mirror entries + the forwarders' R8 drop).
- src/host/notifier.rs :22-27: `HostEvent { Reconcile, IndexProgress }` — NO EmbedderStatus variant. Row 17's `EmbedderStatus` + `CodegraphProgress`/`Ready` = new variants/payloads (IndexProgressEvent covers Started/Progress/Done — Ready may be Done's alias or a distinct variant; probe §5.1's webview-channel mapping at 3b.3 execution).
- src/memory/mod.rs: the aggregates exist — `savings_stats` :2286, `session_stats` + `project_stats` (the IPC reads at ipc/agent.rs :924/:953/:973 are thin store→serde_json reads; `get_session_stats` joins the agent loop's session_id).
- src/memory/types.rs :562-592: `SavingsEvent` (the persisted ledger row) + SpendEvent nearby; src/provider/trace.rs :327 `LlmRequestLog::list()/get(id)` — TraceApi wraps.
- The IPC surfaces to become thin callers: src-tauri/src/ipc/{memory_debug,memory_maintenance,codegraph_cmds,agent(:924-985),trace}.rs.

## C. The picks (one per step)

- **D1 — the EngineOptions consumption.** `wire_brain`'s start path consumes `memory` (Disabled → NO store, NO recall, NO consolidation, and `Engine::memory()` errors `EngineError` code `"memory_disabled"`; Sqlite{path} → the store at path or `<state_dir>/memory.db`) + `index_codegraph` (false → NO watcher, NO index pass). Probe build_brain_inner's store/graph construction sites at execution; the flags thread through the existing start_inner → wire_brain seam (the 3a.4 config_dir/switched_open pattern).
- **D2 — MemoryApi + CodegraphApi.** New src/headless/{memory_api,codegraph_api}.rs: the thin, binding-ready wrappers moving the IPC command logic into the lib (§5.6's method lists verbatim: MemoryApi search/overview/list/get/backlinks/access_log/index_status/cleanup/rebuild_search/rebuild_index; CodegraphApi status/progress/refresh/rebuild/query). The IPC commands become thin callers (ONE implementation). Return types = the existing serde DTOs.
- **D3 — the startup host events.** HostEvent grows `EmbedderStatus` (+ the codegraph Ready shape); `Engine::start` starts the deferred embedder load + the code-graph watcher (what the GUI setup hook + console do today, main.rs:1045's path) and their progress reaches `host_events()`. `index_codegraph: false` starts no watcher; `memory: false` starts no embedder/index memory work.
- **D4 — the live Savings/Spend events.** New `AgentEvent::Savings { kind, tokens_before, tokens_after, tokens_saved, measured }` + `AgentEvent::Spend { model, lane, tokens_in, tokens_out, cached_tokens }`, emitted from record_savings_event/record_spend_row at the persistence points; Serializable mirror entries; display-only, no turn effect; the GUI forwarder DROPS them (R8 — they never hit the transcript).
- **D5 — the stats pull + rates.** The headless layer computes `UsageRates { input_tok_per_s, output_tok_per_s, cache_hit_ratio }` (the documented formulas: input = prompt/ttft, output = completion/generation, `LlmUsage::cache_hit_ratio`) wrapping Usage; `AgentSession::stats() -> SessionStats` (running totals from live events), `stored_stats()` (= session_stats(session_id)), `Engine::stats() -> EngineStats { project_stats, savings_stats, steering_snapshot }`, `Engine::trace() -> TraceApi { list, get, clear }`.

## D. Tests (§9 :930-932, rows 17-19)

- tests/integration/headless.rs::{free_agent_uses_memory_and_codegraph_tools, engine_start_emits_index_host_events, memory_disabled_errors, stats_totals_match_events, usage_rates_formula}
- turn::tests::savings_and_spend_emitted_where_persisted
- tests/integration/contract_fixtures.rs::stats_memory_codegraph_ipc_unchanged (row 19: the get_{savings,session,project}_stats + memory + code-graph IPC JSON byte-identical before/after the rewire)

## E. Battery (per step + close)

`cargo test -p mnemo`; `-p mnemo --features browser,embeddings`; `cargo test --workspace`; `cargo build --workspace`; the goldens trio; clippy: ZERO new sites in own hunks (the standing storm sites stay documented, untouched). Boundary commit `feat(3b.N): …` --no-verify per step; deviations → §G amendment, never silent.

## F. The close

3b.6: the acceptance check-offs (row → test → result) for rows 17/18/19 + the §9 list; the report per the 3a close's format; then the review rounds to PASS; then the master tick.

## G. Amendments

(none yet)
### 3b.1 (D1) — EngineOptions consumption: memory + index_codegraph — landed 2027-01-18

- wire_brain (src/host/brain.rs) takes a NEW BrainMemoryOptions struct
  { memory_enabled, memory_path, index_codegraph } — the engine's
  decisions RESOLVED at the callers. The first shape (three plain
  params) pushed the fn to 9 args and TRIPPED too_many_arguments
  (a genuinely NEW clippy site — the fn sat at 6 before); the struct
  collapses it to 7, under the threshold. No #[allow]; root-cause fix.
- The app wrapper (build_brain) passes memory_enabled: true +
  memory_path: None + the config's codegraph flag — behavior
  byte-preserved. The headless Engine passes opts.memory/opts.
  index_codegraph resolved (the caller's explicit EngineOptions govern;
  the GUI threads the config values into EngineOptions at job 5).
- Disabled consumption (memory: false): the store is Option — NO
  MemoryStore at all (no memory.db, no recall/consolidation/capture);
  the gated consumers: store_trait→factory (None), the kNN overlay
  SKIPPED (its embedder getter is store-backed; the failure-triage
  flag mirror still works), set_memory_search_config, the derived-index
  reconcile, the startup re-embed, Brain.memory_store (None).
- index_codegraph: false → no graph open, no watcher (the config's
  flag is now only the app path's source).
- Sqlite{path} resolves path-or-<state_dir>/memory.db.
- Test: memory_disabled_consumes_no_store (headless_kernel.rs — the
  §9 canonical memory_disabled_errors rides 3b.2 with Engine::memory()).
  In-flight fixes: Option<Arc<Concrete>>→Option<Arc<dyn>> needed an
  explicit map-cast (coercion doesn't ride through Option); a duplicate
  MemoryBackend import in the test file.
- Battery: kernel 3006+46, features 3033+46, workspace + build green;
  clippy zero NEW sites in own hunks (brain.rs:116 BrainOutcome stays
  the documented pre-existing).
### 3b.2 (D2) — MemoryApi + CodegraphApi + the IPC rewire — landed 2027-01-18

- The row-19 pins captured FIRST via the app's EXISTING golden-fixture
  system (contract_fixtures.rs → frontend/src/lib/ipc-fixtures/) — NOT a
  new inline-literal test file: my first attempt clobbered the committed
  ipc/contract_fixtures.rs + duplicated its mod registration (E0428) —
  RECOVERED via git restore, then added the §9-named test
  `stats_memory_codegraph_ipc_unchanged` INTO the existing machinery
  (assert_fixture two-pass: capture → assert). New pins:
  dto-memory-debug-wire, dto-scored-memory-wire, dto-memory-counts,
  dto-memory-debug-overview, dto-savings-stats, dto-session-stats,
  dto-project-stats, dto-memory-index-status (+ the codegraph shapes
  re-asserted, already pinned by dto_fixtures_match_serde).
- src/headless/memory_api.rs: the 4 debug wire types + MemoryIndexStatus
  + to_wire + memory_counts moved VERBATIM; MemoryApi::from_parts
  (store Option, embedder_status, configured_model) with overview/list/
  search/resolve_link/backlinks/access_log/index_status + the maintenance
  trio as delegates (cleanup/rebuild_search to mnemo::memory::
  maintenance; rebuild_index to indexer::rebuild_derived — its report
  type is IndexReport). Fidelity fixes en route: list is FOUR-TIER when
  unfiltered (per-tier limit, matching the IPC), backlinks degrades to
  Ok(empty) on no-store, access_log is sync (Vec return).
- src/headless/codegraph_api.rs: CodegraphStatus (from_parts/unavailable
  now pub) + CodegraphGraph moved; CodegraphApi::status (Result,
  blocking-pool stats + the indexing flag) / progress
  (startup_index_snapshot → Option<(usize, usize)>) / refresh (the sync
  flag-set F1 contract preserved, tokio spawn) / rebuild (awaited full
  pass) / query (slice; None graph → Ok(empty), unknown from_symbol →
  empty).
- Engine accessors: `_brain` renamed `brain` (memory_store + embedder_
  status + factory.codegraph_handle() read); Engine gains
  `configured_embedding_model` (resolved in start_inner BEFORE wire_brain
  consumes the config, the hash-sentinel filter applied);
  Engine::memory() errors code `memory_disabled` (error.rs constructor)
  on a Disabled store; Engine::codegraph() → the unavailable shape.
- The IPC rewire: memory_debug.rs is now thin (a pub(crate) memory_api
  helper + forward); codegraph_cmds.rs lost the type defs +
  stats_via_blocking/log_refresh (status/refresh/graph forward through
  the API; the unavailable-refresh/graph error-on-None stays IPC-side);
  memory_maintenance's memory_index_status forwards (its type def
  deleted; the trio commands stay as-is — already thin over
  mnemo::memory::maintenance); contract_fixtures imports from
  mnemo::headless.
- Tests: memory_disabled_errors + free_agent_uses_memory_and_codegraph_
  tools (headless_kernel.rs, both canonical) — the free-agent test
  scripts g5_tool_turn("memory_search") + g5_tool_turn("graph_search")
  + a text final, then asserts both tool_use entries rode the transcript
  (session.history()); .coding is created so the store/graph open.
- In-flight fixes: Error::msg → the thiserror variants (Error::Memory/
  Error::Runtime with to_string); Option<Arc<Concrete>>→Option<Arc<dyn>>
  map-cast; the backlinks Result-vs-IntoIter E0631 (the un-unwrapped
  backlinks_for Result); expect_err needed Debug (restructured with
  match); stats()/refresh() Result-threaded like the IPC.
- Battery: kernel 3006+48, features 3033+48, workspace (3033+48+331+app
  tests), build green; clippy ZERO findings in all seven 3b.2 hunks.
### 3b.3 (D3) — the startup host events — landed 2027-01-18

- HostEvent gains EmbedderStatus (notifier.rs) — §5.1's sketch also
  named CodegraphProgress/CodegraphReady, but the sketch is job-1's
  PRE-landing design ("one variant per existing emit site"); job 1
  landed that stream as HostEvent::IndexProgress (the 1:1
  codegraph://index-progress mapping) and wire_brain's startup pass
  already emits it on the engine's bus. The doc note records the
  naming resolution; my initially-added duplicate variants were
  REMOVED (a second stream shape = drift risk).
- The engine does NOT spawn its own graph pass: wire_brain ALREADY
  spawns the startup index (brain.rs :882-1022, with the shared
  notifier). A tail-spawned second pass would double-index AND go
  tick-silent (the mtime fast path runs 0 waves on an already-indexed
  tree) while racing the real pass on the SQLite lock — discovered
  live; the tail carries the explanatory note instead.
- The startup events ride QUEUED tasks (the initial EmbedderStatus
  snapshot) so a subscriber connecting during/after `start` still sees
  them — the bus is lossy by design (BusNotifier: "no subscribers yet
  → the event drops"), and wire_brain's pass polls at start_inner's
  internal awaits, BEFORE a caller could subscribe; its IndexProgress
  Done therefore reaches mid-pass subscribers only (the GUI's
  forwarder — connected from construction — sees everything).
- Brain::spawn_startup_embedder_load (brain.rs, the FIRST impl Brain
  block): the kernel twin of the app's ipc::embeddings startup load —
  pending_model_load + memory_store checks moved into the one
  implementation; BundledEmbedder::new on the blocking pool →
  Ready/Failed transitions (emit callback) → reembed_if_needed +
  set_embedder (rewire.rs's order); the model cache derives from
  global_config_dir().join("models") in the HOST layer (the headless
  G4 rule stays intact — the engine just passes the forwarding emit).
  cfg(embeddings) + a no-op twin (the 3a.1 every-combination rule).
- The app's TauriNotifier (main.rs) gains the EmbedderStatus arm →
  emits on embedder://status, payload-identical to the IPC paths
  (E0004 non-exhaustive match — the app compiled green after).
- Test: engine_start_emits_index_host_events (headless_kernel.rs) —
  the shared opts() helper keeps index_codegraph FALSE (hermetic engine
  tests); this fixture opts IN explicitly. Asserts (a) EmbedderStatus
  on host_events() (the queued re-emission) and (b) the pass's RESULT:
  Engine::codegraph().status() reports available + last_indexed_at set
  (poll-bounded 5s) — row 17's "ready" observable for a post-start
  subscriber (the Done event drops on a zero-receiver bus for a
  tiny-tree instant pass). Debug trail: the temp diagnostic (listing +
  status) exposed the helper's false default — the root cause behind
  all three failing runs.
- Battery: module 25/25, kernel 3006+49, features 3033+49, workspace
  3033+49+331+app suites, build green; clippy ZERO new sites in the
  3b.3 hunks (brain.rs:116 stays the documented pre-existing).