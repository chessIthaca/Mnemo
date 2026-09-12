## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on wt/agenticcoding (plan 3e680de3, backlog 51dab4da — status-bar reasoning effort goes wire-first). The change is correct and complete end-to-end: every provider-resolution branch maintains the effort mirror, the ModelChanged emission condition is sound (no spurious loops), the display/wire vocabulary separation is exact ("off" never leaks the off-wire encoding), all swap/report sites record the effort, serde stays backward-compatible with pre-wire JSON, and the regression tests exercise the changed paths. Two LOW consistency gaps in the new frontend `agentWireEfforts` map handling.

### LOW 1 — `removeAgent` doesn't clean the exited agent's `agentWireEfforts` entry

`frontend/src/hooks/agentEventReducer.ts:1495-1515` — the cross-map removal block deletes `agentId` from `agents`, `agentNames`, `agentParents`, `agentModels`, `agentProviders`, `agentEfforts`, and `workflowStates`, but NOT from the new `agentWireEfforts` map (also absent from the returned `next`). Every sibling per-agent map is cleaned; the new map should follow the same pattern. Functionally harmless today (agent ids are never reused, and `activeAgent` falls back to the main agent whose own entry is untouched), but it's an inconsistency with the sibling maps and a small unbounded-ish leak that will confuse future readers.

Fix: mirror the `agentEfforts` lines directly above — `const agentWireEfforts = { ...s.agentWireEfforts }; delete agentWireEfforts[agentId];` and include `agentWireEfforts` in the returned `next` object.

### LOW 2 — `resetStore()` doesn't reset `agentWireEfforts`

`frontend/src/hooks/useAgentStore.test.ts:24-49` — the helper's doc says "Reset the store slices the reducers touch before each test", but the new `agentWireEfforts` slice (written by `reduceModelChanged`) is missing from the `setState` partial. Zustand's `setState` merges, so the map survives across tests. No flakiness today (only the new regression test touches the map, and it ends on the clear assertion, self-cleaning), but any future test that dispatches an effort-bearing `model_changed` before another test reads the map would leak state — exactly the class of cross-test pollution `resetStore` exists to prevent.

Fix: add `agentWireEfforts: {}` to the reset object (next to `agentEfforts: {}` at line 30).


## Verification detail (all five requested checks)

### 1. Correctness / bugs / security — VERIFIED

- **Effort mirror at every resolution branch** — read the full `resolve_turn_provider` (`src/agent/loop_impl.rs:1037-1275`): all eight sites set `resolved_effort` alongside `resolved_model` — skill override (:1069), pin 429-sticky-alt (:1161), pin serve (:1171 via `pinned_display_effort`), forced-with-resolver (:1200), no-resolver (:1213 → None), default 429-sticky-alt (:1243), default no-override (:1254 via `default_display_effort`), state/subagent chain (:1271). No missed branch; the pin-doesn't-hold and forced-without-resolver fall-throughs correctly leave the set to the branch that actually serves.
- **Emission condition** (`src/agent/turn.rs:1375-1417`) — prev/now effort snapshots bracket the resolution; fires on model OR effort change. Steady state emits nothing (no event loop). The new first-turn emission on factory-stamped loops (effort None→Some) is informative, carries the correct default, and is idempotent in the reducer — intended behavior, not spurious.
- **Display vs wire vocabulary** — `display_reasoning_effort_for` / `display_normalize_reasoning_effort_for` (`src/config/endpoints.rs:458-508`) mirror the wire twins (:366-456) exactly in chain/gate/clamp; the `"off"` early-out precedes the allow-list clamp; the DeepSeek unit test asserts the pair differs only in vocabulary (`"off"` vs `"none"`). `resolve_display_effort` (`src/provider/client_factory.rs:227-236`) mirrors `resolve_effort`'s Some/None shape; `resolve_model_provider`'s `display_effort` (`src-tauri/src/ipc/config_io.rs:174-178`) mirrors `resolve_reasoning_effort`'s requested/unset shape.
- **Swap paths all record the effort** — console `/model` (`console.rs:991-998`), `set_model` both paths (`agent.rs:615`, `:672`), `save_endpoints` (`settings.rs:285-318`), `swap_provider_into_loops` (factory + every loop, `config_io.rs:216-238`), `swap_provider_into_loop` (pin + default + clear, `config_io.rs:291-304`), `swap_live_provider`'s ModelChanged carries it (`config_io.rs:353-357`). The only factory-level `set_provider` caller is inside `swap_provider_into_loops` — no bypass that could desync the factory stamp. `set_explicit_provider`'s other callers are test-only (`runtime/agent.rs:2462` is test code; `agent/tests.rs`) — they report None and the UI falls back, by design.
- **main.rs stamping** (:1045-1053, :1662-1664) — computed at provider build (endpoint branch → display chain; dummy branch → "max"), stamped after construction and before any `build()` (the intervening code is pure builder chaining); the `with_codegraph` rebind (:1671) uses `mut self` move semantics, so the stamp survives.
- **AgentInfo or-fallback** — `resolved_effort().or(default_display_effort())` at all three sites (`agent.rs:480-483`, `spawn.rs:70`, `startup.rs:83-86`). Pre-first-turn the factory stamp serves; post-swap the cleared mirror falls back to the freshly-set default. Consistent with `resolved_model`'s documented last-turn semantics.
- **Serde backward-compat** — `reasoning_effort` carries `#[serde(default, skip_serializing_if = "Option::is_none")]` mirroring `provider` on `SerializableAgentEvent::ModelChanged`; the roundtrip test asserts Some round-trips, None is skipped, and an absent key deserializes to None — old persisted events / mock swaps keep the pre-wire JSON. `AgentInfo.reasoning_effort` mirrors `provider`'s skip pattern; the None-valued dto-agent-info fixture serializes byte-identically (no fixture churn — correct).
- **Frontend** — precedence `agentWireEfforts ?? agentEfforts ?? effortForEndpoint` (`StatusBar.tsx:168-173`, and `effortForEndpoint` returns "off" for missing/unsupported, so the chain is well-formed); `reduceModelChanged` upserts/clears (`agentEventReducer.ts:1179-1188`) reusing `mergeAgentProviders`' tested upsert/clear-on-null semantics; `registerAgents` presence-wins (`useAgentStore.ts:727-732`). `selectModel`'s supported→supported case now carries the wire effort — WYSIWYG-consistent (the dropdown displays `effectiveEffort`), and the label still clamps via `clampEffortToModel` (`StatusBar.tsx:436`).
- **Security** — no new untrusted input; config-derived strings rendered as React labels. No issue.

### 2. Regression tests — VERIFIED

The frontend test exercises `reduceModelChanged`'s new effect and asserts both the upsert and the clear-on-absent (the stale-labeling hazard); the extended `mid_turn_skill_model_switch_emits_model_changed_events` asserts the effort rides both ModelChanged events (`Some("low")` skill flip, `None` default fallback) through the real turn-loop emission path; the three `endpoints.rs` units pin the display chain, the DeepSeek vocabulary split, and the off-before-clamp early-out. Root cause documented: BUG memory record (f66f5d46) + `.coding/knowledge/bug/2027-01-07-status-bar-reasoning-effort-stale-for-auto-selec.md` — both accurate against the code.

### 3. Documentation sync — VERIFIED

New SPEC `.coding/knowledge/spec/2027-01-07-status-bar-reasoning-effort-is-wire-first-modelc.md` is accurate (checked against the implementation). The stale StatusBar comment was rewritten in this diff. `README.md:126` ("the status-bar effort dropdown stays the runtime override on top of it") and `PLAN.md:947-949` remain true post-fix (dropdown semantics unchanged; wire-first strengthens the per-model claim). No stale claims found that the status bar reads the endpoint default.

### 4. Multi-platform neutrality — VERIFIED

No Windows-only APIs, paths, or shell syntax anywhere in the diff; the only platform text is pre-existing context (the WebView2 comment in main.rs). Clean.

### 5. Warning-free build — VERIFIED

Every new pub item has callers (display twins ← config_io/settings/main/client_factory; `resolve_display_effort` ← model_resolver; the trait method ← loop_impl + both impls; the accessors ← the IPC layer); no unused imports introduced; no cfg changes. Nothing in the diff plausibly breaks the reported green matrix (root 2042+16, src-tauri 233+4, frontend 1004 + build).

## Notes (non-findings)

- Git warns that the working copy of `frontend/src/lib/ipc-fixtures/event-model-changed.json` has CRLF; git normalizes it to LF at commit (the warning itself says so), so the committed blob matches the repo's LF style — no action needed.
- The deferred-swap window (`SwapOutcome::Deferred`) can transiently report the new effort alongside the old model id in `list_agents` until the turn loop completes the swap — but the pre-fix toolbar echo showed the same newly-requested value in that window, and no ModelChanged is emitted until the swap lands, so this is not a regression.
