# Deep Review — Maintainability & Refactoring (whole codebase, HEAD)

**Reviewer:** maintainability/refactoring deep reviewer (read-only)
**Goals ranked by user:** (1) reduce overall code size, (2) make future agent-initiated changes easier/safer.
**Method:** every duplication claim below was verified by reading the cited files/line ranges. LOC deltas are estimates for the *production+test code actually deleted net of the helper added*.

## Verdict

The codebase is disciplined in the ways that matter most for agent-initiated changes: one shared spawn path (`spawn_agent_shared`), one provider constructor (`build_openai_client`), typed wire structs pinned by golden fixtures, a facade'd frontend store (H3 already done), and a pure reducer layer. The remaining size/safety debt is concentrated in exactly three places:

1. **Three hand-copied "persist config → reload → swap live provider" pipelines** in the Tauri layer (`save_endpoints`, `save_settings`, `set_model`) that have *already drifted* (`set_model` clears each loop's `resolved_model`; `save_endpoints` does not).
2. **~28 copies of an AgentLoop test fixture** in `src/agent/tests.rs` (2,866 lines, mostly identical construction).
3. **Table-shaped frontend appearance code** (11 hand-written color actions + two near-identical resets) and a triplicated IPC fallback-state constructor in `main.rs`.

Estimated achievable reduction: **≈ −1,000 LOC (range −850…−1,200)** at low–medium risk, with the highest future-change payoff coming from the provider-swap and sandbox-write unifications (both remove ordering/security-sensitive duplication, which is where agent edits are most dangerous).

---

## Top-10 refactor table (ranked by LOC delta × future-change payoff)

| # | Refactor | Files | LOC Δ | Risk | Payoff for future changes |
|---|----------|-------|------:|------|---------------------------|
| 1 | `agent/tests.rs` fixture builder (`TestHarness::new(provider)`) | src/agent/tests.rs | **−400** | low | New turn-behavior tests become ~10 lines instead of ~25; constructor changes (AgentLoop gained params recently) stop touching 28 sites |
| 2 | One `persist_reload_config` + `swap_live_provider` helper for the 3 config-write commands | src-tauri/ipc/settings.rs, agent.rs, (new) ipc/config_io.rs | **−150** | med | Single place to change provider-swap semantics; kills the existing `resolved_model` drift; new config sections get save/reload/rewire for free |
| 3 | `IpcState` accessors + send-command/stats command helpers | src-tauri/ipc/agent.rs (+embeddings.rs, memory_debug.rs) | **−115** | low | New commands stop copy-pasting lock/error boilerplate; "brain failed to start" wording can't fork |
| 4 | Color-action table driven by a `COLOR_KEYS` const | frontend/src/hooks/useAgentStore.ts (+appearance.ts) | **−90** | low-med | Adding/removing a customizable color = 1 table row + 1 CSS var instead of 4 edits across interface/init/action/reset×2 |
| 5 | `Sandbox::validate_for_write()` shared ladder | src/tool/agent/file_write.rs, file_append.rs, file_edit.rs, sandbox.rs | **−60** | med | Protected-file + creation-gap rules change in ONE place — security-relevant dedup; also unifies file_append's missing mkdir |
| 6 | Delete `get_config` command; FE uses `get_settings` | src-tauri/ipc/settings.rs, main.rs, StatusBar.tsx, lib/tauri.ts | **−55** | med | One settings payload shape instead of two that must stay consistent (general/endpoints/pricing are a strict subset) |
| 7 | Shared `spawn_reembed_if_needed` (lib crate) | src-tauri/src/main.rs, src-tauri/src/ipc/rewire.rs → src/memory/ | **−30** | low | Fingerprint/re-embed policy (data-integrity logic!) lives in the lib crate, tested once, not copy-pasted in the shell |
| 8 | `fallback_ipc_state()` constructor for the 2 degraded arms | src-tauri/src/main.rs | **−30** | low | Adding an IpcState field no longer requires editing 3 near-identical struct literals (Ready/NeedsProject/Err) |
| 9 | In-file dedup: pricing map fn, mangled comment block, `AgentLoopMap` alias, `end_run` helper | src-tauri/ipc/settings.rs, state.rs, spawn.rs, events.rs, run_all.rs | **−55** | low | Noise removal; type alias makes the loop-map signature readable at a glance |
| 10 | `parse_models_with_vision` arm merge + u64 extractor in SSE parser | src/provider/openai.rs | **−28** | low | Provider-shape probes (`/models`) get one extension point |
| | **Total** | | **≈ −1,013** | | |

---

## Detailed proposals (top 5)

### 1. `agent/tests.rs` fixture builder — −400 LOC, low risk

**Evidence.** The identical construction block appears 28×: e.g. lines 102–123, 154–174, 382–405, 542–565, 1304–1308, 2336–2342, 2681–2688, 2918–2925… Each is `tempdir()` + `Arc<Mutex<Workflow>>` + `Arc<Sandbox>` + `make_registry(...)` + `AgentLoop::new(provider, registry, workflow, sandbox, Constitution::default(), SafetyMode::Autonomous, ContextManager::new(128_000, 0.5), None, None)` (~20 lines). Only the provider (and occasionally `with_agent_id` / context size) varies. `src/runtime/agent.rs` tests repeat the same shape again (e.g. around lines 1145–1160, 1485–1495, 1873–1885, 2111–2125) **plus 6 near-identical mock `LlmClient` impls** (~15 lines each).

**Proposal.** In a `#[cfg(test)] pub(crate) mod test_support` (shared via `src/agent/mod.rs` or a new `src/testutil.rs`): 
`fn test_agent(dir: &TempDir, provider: Arc<dyn LlmClient>) -> (AgentLoop, Arc<Mutex<Workflow>>)` for the common case, plus a `TestAgentBuilder` with `.context(128_000).agent_id(n).safety(mode)` for the ~6 tests that deviate. Move the noop/mock providers into one module.

**Migration (one line):** replace each 20-line block with `let (agent, workflow) = test_agent(&dir, provider);` — mechanical, compiler-checked, tests-only.

**Why it helps:** `AgentLoop::new` has gained parameters twice recently (memory/vision handles); each such change currently edits 28+ sites. After this, it edits one.

### 2. Unify the three config-write pipelines — −150 LOC, med risk, **highest payoff**

**Evidence (verified line ranges).**
- **Save→reload→swap-into-state duplicated verbatim:** `settings.rs` 310–321 (`save_all` → `Config::load` → `*guard = reloaded.clone()`) ≡ `settings.rs` 1216–1226.
- **Provider rebuild + fan-out to factory and every loop:** `settings.rs` 330–381 (`save_endpoints`) ≈ `agent.rs` 447–519 (`set_model`). Both do factory check → config lock → `build_openai_client(..., Some(state.trace.clone()))` → `factory.set_provider` → `context_manager_for` → loop `agent_loops.values()` → `set_provider` → `eprintln!` label. **They have drifted:** `set_model` also calls `agent_loop.set_resolved_model(None)` (agent.rs 514) so `list_agents` reports the new model; `save_endpoints` omits it, so after an endpoint save the toolbar can show the stale per-context override. That is a live inconsistency caused precisely by the duplication.
- **Safety write + re-evaluate duplicated:** `agent.rs` 321–333 (`set_safety_mode`) ≈ `settings.rs` 1229–1240.
- Both settings commands also repeat the `rewire_vision_and_embedder` + `sync_model_resolver` tail (settings.rs 385–392, 1244–1248) — this part is already factored into `rewire.rs`; the *rest* should follow the same pattern.

**Proposal.** New `ipc/config_io.rs`:
- `pub(super) async fn persist_and_reload(state, new_config: Config) -> Result<Config, IpcError>` (save_all + load + swap into `state.project.config`).
- `pub(super) fn swap_live_provider(state, endpoint: &Endpoint, model: &str, reasoning: Option<String>, clear_resolved: bool) -> bool` (the factory/context-manager/loop fan-out).
- `pub(super) fn set_runtime_safety(state, mode: SafetyMode)` (lock write + `approvals.re_evaluate` + log).

`save_endpoints`, `save_settings`, and `set_model` then each become validate + build-config + two helper calls. While unifying, **decide the `resolved_model` semantics once** (almost certainly: clear on every swap — port `set_model`'s behavior into the shared helper).

**Migration:** extract helpers → rewire three call sites → run `cargo test` (the settings contract fixtures in `ipc/contract_fixtures.rs` pin the wire shapes, so regressions surface).

**Why:** this is the app's most ordering-sensitive code (locks held, live provider mutation, in-flight turns). One copy means the next provider-related feature (e.g. a per-agent model pin) is a one-site change, and divergences like the `resolved_model` bug class become structurally impossible.

### 3. `IpcState` accessors + command-body helpers — −115 LOC, low risk

**Evidence.**
- Six identical send-command bodies in `agent.rs` 194–278 (`send_prompt`, `send_suggestion`, `interrupt`, `cancel`, `compact`, `clear_conversation`): each locks the manager and maps `send(agent_id, X)` to `format!("failed to …: {e:?}")`.
- Three identical stats commands `agent.rs` 704–770 (`get_session_stats`, `get_project_stats`, `get_session_list`): factory → `memory_handle` → await a store method → `serde_json::to_value` with the same two error strings.
- Repeated `Option` unwraps with identical messages: `"agent factory unavailable (brain failed to start)"` ×5 (agent.rs 450, 636, 719, 740, 760; spawn.rs 39), `"no memory store configured"` ×3 (agent.rs 722, 743, 763), `"safety rules unavailable (brain failed to start)"` ×5 (agent.rs 29, 45, 65, 92, 115), `"safety_mode lock poisoned"` ×4 (agent.rs 325, 340; settings.rs 713, 1232), `"embedder status lock poisoned"` ×9 across 5 files.

**Proposal.** On `IpcState`: `fn factory(&self) -> Result<&Arc<AgentLoopFactory>, IpcError>`, `fn memory_store(&self) -> Result<&Arc<MemoryStore>, IpcError>`, `fn safety_rules(&self) -> Result<&Arc<SafetyRules>, IpcError>`, `fn set_safety(&self, mode)`, `fn embedder_status(&self) -> EmbedderStatus`. Plus one `send_cmd(state, agent_id, cmd, label)` helper. Each command body becomes 3–5 lines.

**Migration:** add accessors → rewrite command bodies mechanically (compiler + existing tests cover them).

**Why:** new IPC commands (a frequent agent task) stop re-deriving lock-poisoning and fallback wording; message drift between modules disappears.

### 4. Frontend color actions as a table — −90 LOC, low-med risk

**Evidence.** `useAgentStore.ts` 700–833: `setAccentColor`/`setBorderColor`/`setTextPrimaryColor`/`setTextMutedColor` (700–719, each re-passing the other three colors to `applyColors`), seven `setCode*Color` one-liners delegating to `setCodeColor` (720–740), then `resetColors` (741–781) and `resetAppearance` (782–833) — two ~40-line blocks that are the same 14 `writeLs` calls + two `apply*` calls + a 14-key `set({...})`, differing only in the theme/font additions.

**Proposal.** In `appearance.ts` export a single source of truth:
```ts
export const COLOR_PREFS = [
  { stateKey: "accentColor", ls: LS_ACCENT_COLOR, def: DEFAULT_ACCENT_COLOR, group: "ui" },
  /* …13 entries… */
] as const;
```
plus `applyColorPrefs(prefs)` (wraps `applyColors`+`applyCodeColors`) and `writeColorPrefs(prefs)`. The store keeps one `setUiColor(stateKey, c)` and one `resetColors()` implemented as loops over `COLOR_PREFS`; `resetAppearance` = `resetColors` + theme/font. The facade re-exports (lines 116–160) stay untouched so no consumer changes.

**Migration:** add table in appearance.ts → collapse actions → `npm test` (appearance has existing coverage) + `npm run build`.

**Why:** the 14-color surface is the app's most frequently extended appearance feature; today each new color touches 6 places (constant, interface field, init, action, resetColors, resetAppearance) and forgetting one produces the exact partial-reset bugs this table eliminates.

### 5. `Sandbox::validate_for_write()` shared ladder — −60 LOC, med risk (security-positive)

**Evidence.** The "validate → creation-fallback → protected-target check → mkdir → revalidate" ladder is hand-rolled three times with drift:
- `file_write.rs` 86–134: full ladder, protected-check duplicated **twice** (86–93 existing-path check; 113–119 creation-gap check) with a 4-line refusal message duplicated verbatim, plus again in `prepare_for_approval` (175–186).
- `file_append.rs` 89–106: validate → creation-fallback → protected-check — but **no parent-dir creation** (so `file_append` to `new/dir/a.txt` fails where `file_write` succeeds — an inconsistency, not a documented choice).
- `file_edit.rs` 534+ and 586: same validate + protected patterns again.
- The refusal message string appears 5× across the two files.

**Proposal.** `Sandbox::validate_for_write(&self, path: &Path) -> std::result::Result<PathBuf, String>` implementing the canonical order (validate → creation fallback → protected check → mkdir → revalidate), returning one shared refusal message; the three tools call it inside their existing `spawn_blocking` closures. Explicitly decide (and test) whether append creates parent dirs — recommend yes for consistency.

**Migration:** add method + unit tests in sandbox.rs (the M5 tests at file_write.rs 342–411 move to sandbox.rs unchanged) → replace the three ladders → `cargo test`.

**Why:** protected-target policy (`is_protected_write_target`) is the app's write-safety core; today a new rule must be honored in 4 code paths, and the M5 "creation gap" fix had to be applied twice because of this duplication. One method makes the next protected-path rule a one-site change with one test suite.

---

## Duplication evidence index (line refs)

- settings.rs **401–416**: comment header `// ── Full Settings read/write …` duplicated 3× and an orphaned paragraph whose first line was lost in a merge (starts mid-sentence at 405).
- settings.rs **59–67** ≡ **767–775**: pricing `map` closure duplicated in `get_config` and `get_settings` (PricingWire field-by-field copy twice).
- settings.rs **310–321** ≡ **1216–1226**: save/reload/swap pipeline (see proposal 2).
- settings.rs **330–381** ≈ agent.rs **447–519**: provider rebuild + fan-out (see proposal 2) — with the `set_resolved_model` drift.
- agent.rs **194–278**: 6 identical send-command bodies.
- agent.rs **704–770**: 3 identical stats-command bodies.
- agent.rs **29/45/65/92/115**, **450/636/719/740/760**, **722/743/763**: triple/quintuple-repeated `ok_or_else` messages.
- main.rs **741–771** ≡ rewire.rs **58–92**: the fingerprint-check + background `reembed_all` task copied verbatim (different only in which embedder handle is used) — includes identical comments.
- main.rs **250–308** ≈ **310–368**: NeedsProject and Err arms construct the same fallback `IpcState` (~38 lines each; only `startup_error`/`needs_project`/config source differ).
- spawn.rs/events.rs/state.rs/main.rs: `Arc<Mutex<HashMap<AgentId, Arc<AgentLoop>>>>` spelled out 8×.
- run_all.rs **466–468, 486–488, 507–509, 626–628, 705–706**: five copies of "clear run_all → emit_backlog_changed" exit sequence; plus 7 `crate::ipc::run_all::` fully-qualified self-references inside its own module (474, 484, 511, 566, 583, 596, 608).
- openai.rs **356–380** ≡ **381–405**: `parse_models_with_vision`'s two arms duplicate map + empty-vs-malformed classification (only the id key differs); **1066–1085**: `usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0) as u32` ×4.
- openai.rs **263–264**: doc-comment line duplicated verbatim.
- agent/tests.rs: 28× AgentLoop fixture block (see proposal 1); runtime/agent.rs: 6× mock provider impl.
- StatusBar.tsx **213–222, 225–234, 237–246, 249–258**: four identical outside-click-close `useEffect`s (only ref + state differ) — a `useClickAway(ref, open, close)` hook would cut ~30 lines (fold into proposal 4's PR or do standalone).

## Dead / stale code list

1. **settings.rs 401–416** — triplicated + orphaned comment block (delete ~6 lines, restore the lost opening sentence of the wire-struct rationale at 405–416).
2. **openai.rs 263–264** — duplicated doc line.
3. **files.rs `save_conversation`/`load_conversation`** — `_agent_id` parameters unused on the Rust side (kept for FE call-shape compat; removable only with a coordinated FE change — low value, note only).
4. **spawn.rs 39** — `IpcError::msg(...)` where the `From<&str>` impl already provides `.into()` (style-only).
5. No dead types/fns found in the sampled modules — `truncate_for_display`, `fetch_models_with_vision`, `ModelWithVision`, `EndpointDto` etc. all have live callers. The `#![deny(warnings)]` build keeps the true dead-code rate near zero; the "dead" mass here is *stale comments and duplicated logic*, not unused symbols.

## Non-findings (checked; deliberately NOT proposed)

- **AgentManager single-mutex, serde derives on channel types, AgentLoop field count** — excluded per instructions.
- **`VisionModelWire` vs `EmbeddingModelWire` (structurally identical)** — the file's own comment justifies distinct types to prevent cross-surface confusion; keep.
- **tauri.ts wrapper-per-command (957 lines)** — do **not** convert to a generic typed command map. The bulk is per-command JSDoc + arg-name mapping (`agentId`→`agent_id`), which a map cannot infer; wrappers are the type boundary. The file is long but each line earns its place.
- **Full Zustand-slice rewrite of useAgentStore.ts** — the store is already a facade over `agentState.ts`/`agentEventReducer.ts`/`appearance.ts` (per its header, H3). The remaining size is the `AppState` interface + init, which is genuine shape, not duplication. Proposal 4 (color table) is the only high-value change here; slicing would add combinator boilerplate for no consumer benefit.
- **`agentEventReducer.ts` per-event reducers / co-location** — the switch-dispatch + `Effects` design is the module's strength (pure, table-testable, single write path in `applyAgentEvent`). No change recommended.
- **backlog.rs, files.rs, error.rs, state.rs, spawn.rs, events.rs** — read in full; no material duplication beyond the items ranked above. `spawn_agent_shared` is exemplary (one path for UI/tool/startup).
- **`OpenAiClient::complete` (~300 lines)** — long but one linear stream loop; the trace-mirroring arms (640–655) could extract a `mirror_to_trace` closure (~−25) but the function reads top-to-bottom and is heavily commented for good reason. Low priority; not in top-10.
- **`agent/turn.rs run_turn`** — not fully audited within this pass's budget (only its signature/structure sampled); recommend a dedicated follow-up read before proposing extraction boundaries. Not counted in any estimate above.
- **`memory/mod.rs`, `workflow/mod.rs`** — structure sampled via symbol maps: cohesive single-responsibility methods, heavy but legitimate test sections; no cross-file duplication found.

## Suggested sequencing

Do #1 (tests, zero prod risk) and #3/#8/#9 (mechanical IPC) first to shrink noise, then #2 and #5 (the two semantic unifications, each deserving its own plan + review), then #4/#6/#7/#10. Every step is independently shippable and `cargo test`-verifiable (green implies warning-free under `#![deny(warnings)]`), with the settings contract fixtures guarding the wire shapes in #2/#6.
