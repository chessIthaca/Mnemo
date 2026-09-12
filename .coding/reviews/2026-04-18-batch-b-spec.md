# Batch B — Structural Correctness: VERIFIED IMPLEMENTATION SPEC

**Scope:** Verified spec for Batch B (B1 per-agent plan dirs, B2 config-write unification, B3 `validate_for_write` ladder). READ-ONLY investigation; no source edited. All file:line refs re-verified against current HEAD (2026-04-18 plan refs have drifted — corrected below). A prior draft exists at `.coding/reviews/2026-04-18-batch-b-spec.md`; this report supersedes it and adds one **required edit** that draft only flagged as "verify" (FinishTool reviews-dir derivation — see C6/R1).

---

## 0. CRITICAL CORRECTIONS TO THE PLAN (read first)

### C1. B2's "live drift" bug is ALREADY FIXED — B2 is a pure refactor, not a bug fix
The plan (step 2, B2) claims `swap_live_provider` "ALWAYS clears resolved_model" to fix "the live drift where `save_endpoints` (settings.rs:371-373) doesn't clear" it. **This is stale.** `save_endpoints` ALREADY clears `resolved_model` for every live loop:

- `src-tauri/src/ipc/settings.rs:381-388` — the provider-swap loop:
  ```rust
  for agent_loop in agent_loops.values() {
      agent_loop.set_provider(provider.clone(), context_manager.clone());
      agent_loop.set_resolved_model(None);   // ← line 385: ALREADY CLEARS
  }
  ```
- `src-tauri/src/ipc/agent.rs:516` — `set_model` also clears it (the plan's "port set_model's behavior agent.rs:514" — now line 516).

Both paths clear it. The drift was fixed in the ModelChanged-event work (semantic memory "ModelChanged event — 2026-11-22"). **`[CHOICE: confirmed]`** that `swap_live_provider` should ALWAYS clear `resolved_model` — but this is preserving existing correct behavior, not fixing a live bug. The commit message + reviewer framing must say "refactor: extract duplicated config-write logic" NOT "fix live drift" (a reviewer told it's a bug fix will look for the bug, find nothing, and flag the false premise).

### C2. The factory is at `src/agent/factory.rs`, NOT `src-tauri/src/ipc/factory.rs`
`src-tauri/src/ipc/factory.rs` does not exist. The plan's "factory.rs:326-330" = **`src/agent/factory.rs:325-335`** (`build_inner`). `spawn.rs` imports `myharness::agent::factory::AgentLoopFactory` (spawn.rs:12). The factory is a **lib-crate** type; the IPC layer holds `Arc<AgentLoopFactory>` in `IpcState.runtime.factory` (main.rs:192).

### C3. B1 breaks `reviews_dir()` derivation — must be addressed
`AgentLoopFactory::reviews_dir()` (`src/agent/factory.rs:438-443`) = `plans_dir.parent().join("reviews")`. With the main `plans_dir` = `.coding/plans/`, this yields `.coding/reviews/` ✓. But a per-agent `plans_dir` = `.coding/plans/agents/<id>/` makes `parent()` = `.coding/plans/agents/`, so `reviews_dir()` → `.coding/plans/agents/reviews/` ✗ (wrong; reviewer reports must stay at `.coding/reviews/`).

`reviews_dir()` is used to construct `WriteReviewReportTool` (factory.rs:466, called from `register_agent_tools`). **Fix:** derive `reviews_dir` from the MAIN plans dir (the factory's `self.plans_dir` field), not a per-build override. The new `build_with_id_and_plans_dir` must pass the override ONLY to `Workflow::new` + `with_plans_dir`, NOT to `reviews_dir()`. See §B1 edit plan.

### C4. `frontend/src/__tests__/ipc-contract.test.ts` does not exist
The actual contract test is **`frontend/src/lib/ipc-contract.test.ts`** (verified). The Rust half is `src-tauri/src/ipc/contract_fixtures.rs` (registered as `#[cfg(test)] mod contract_fixtures` in `src-tauri/src/ipc/mod.rs:36-37`). Both import the same fixtures under `frontend/src/lib/ipc-fixtures/`.

### C5. `main_agent_id` documentation is ALREADY present
The plan says "document main_agent_id (= min parentless id) backlog-target semantics in a comment at src/runtime/mod.rs:127-133." That doc comment already exists at `src/runtime/mod.rs:118-133` (the doc comment on `main_agent_id()`). **This sub-task is already done** — verify only, no edit needed.

### C6. NEW (not in prior draft) — FinishTool derives reviews from the PER-AGENT plans_dir → breaks under B1
`FinishTool::execute` (`src/tool/workflow/plan.rs:530-534`) derives its reviews dir from the workflow's own `plans_dir()`:
```rust
let reviews_dir = wf
    .plans_dir()           // ← per-agent workflow's plans_dir
    .parent()
    .map(|p| p.join("reviews"))
    .unwrap_or_else(|| std::path::PathBuf::from(".coding/reviews"));
```
Under B1, a UI-spawned side agent's workflow `plans_dir` = `.coding/plans/agents/<id>/`, so `parent()` = `.coding/plans/agents/` and `finish` looks for the review report under `.coding/plans/agents/reviews/` ✗. But the reviewer subagent (spawned by that side agent) writes its report via `WriteReviewReportTool`, which is constructed with `factory.reviews_dir()` = `.coding/reviews/` (after the C3 fix). **Mismatch: the side agent's `finish` can never find the report the reviewer wrote.**

The prior draft (R1) only said "Verify `finish` (tool/workflow/plan.rs:791) derives reviews consistently" — but :791 is a *test comment*, not the code. The actual code is at :530-534. **This is a REQUIRED EDIT, not a verification.** Fix: give `FinishTool` an explicit `reviews_dir: PathBuf` field (mirroring `WriteReviewReportTool`), constructed by the factory from `self.reviews_dir()` (main-derived). See §B1 edit plan step 3b.

---

## 1. PER-ITEM CURRENT-CODE VERIFICATION (corrected file:line)

### B1 — per-agent plan dirs

| Plan ref | Actual location | Status |
|---|---|---|
| factory build path ~:326-330 | `src/agent/factory.rs:325-335` (`build_inner`) | ✓ creates `Workflow::new(self.plans_dir.clone())` (:332) + `load_latest()` (:333) |
| factory `plans_dir` field | `src/agent/factory.rs:92` (field), `:144` (ctor param), `:165` (ctor body) | single `PathBuf`, no per-build override |
| factory construction | `src-tauri/src/main.rs:836-858` (passes `project.plans_dir.clone()` at :845) | `project.plans_dir` = `coding_dir.join("plans")` (`src/project/mod.rs:52`) = `.coding/plans/` |
| `build_with_id` | `src/agent/factory.rs:319-321` → `build_inner(Some(id))` | no plans_dir param |
| `build_inner` | `src/agent/factory.rs:325-403` | `Workflow::new(self.plans_dir.clone())` (:332); `with_plans_dir(self.plans_dir.clone())` (:371) |
| spawn_agent_shared | `src-tauri/src/ipc/spawn.rs:82-205` | calls `factory.build_with_id(agent_id)` at :111; id allocated at :95-98; parent_id block at :121-131 |
| main agent spawn | `src-tauri/src/main.rs:153-166` | `spawn_agent_shared(..., None /* parent */, ...)` — parentless |
| UI spawn | `src-tauri/src/ipc/spawn.rs:33-70` (`spawn_agent` cmd) | `spawn_agent_shared(..., None /* parent */, ...)` at :41-51 — parentless |
| tool spawn (IpcSpawner) | `src-tauri/src/ipc/spawn.rs:407-433` (`spawn_with_parent`) | passes `parent_id` through at :421 |
| `Workflow::load_latest` | `src/workflow/mod.rs:683-779` | reads `self.plans_dir`; if dir absent → Planning + empty (:686-689) |
| `Workflow::plans_dir()` | `src/workflow/mod.rs:237-239` | exposed for IPC |
| `get_plan` | `src-tauri/src/ipc/agent.rs:587-626` | reads `workflow.plans_dir()` at :616 → `plans_dir.join("{plan_id}.md")` at :619 |
| `get_workflow_state` | `src-tauri/src/ipc/agent.rs:546-572` | reads per-agent workflow's plan at :563 |
| `main_agent_id` | `src/runtime/mod.rs:127-133` | min parentless id; doc comment already at :118-126 |
| `reviews_dir()` (factory) | `src/agent/factory.rs:438-443` | `plans_dir.parent().join("reviews")` — **BREAKS** under per-agent dir (see C3) |
| `FinishTool` reviews derivation | `src/tool/workflow/plan.rs:530-534` | `wf.plans_dir().parent().join("reviews")` — **BREAKS** under per-agent dir (see C6) |
| `is_protected_write_target` | `src/tool/agent/sandbox.rs:170-187` | protects `.coding/plans/` (whole dir, :186 `starts_with(".coding/plans/")`) — per-agent subdirs stay protected ✓ |
| `with_plans_dir` (AgentLoop) | `src/agent/loop_impl.rs:347-349` | sets loop's plans_dir for session-end consolidation |
| corpus_digest | `src/memory/consolidation.rs:57`, used at `src/runtime/agent.rs:455-459` | digests the loop's plans_dir — per-agent dir → per-agent corpus (acceptable; main keeps full corpus) |

**Key fact:** `get_plan`/`get_workflow_state` ALREADY read the per-agent workflow's `plans_dir` (agent.rs:616, :554). If each agent's `Workflow` is built with its own `plans_dir`, both resolve per-agent **with no cross-dir scan needed**. The plan's "resolve plan ids across dirs" is satisfied by the per-agent workflow `plans_dir` — a linear scan is NOT required.

### B2 — config-write unification

| Plan ref | Actual location | Status |
|---|---|---|
| save_endpoints persist+reload | `src-tauri/src/ipc/settings.rs:314-325` | `save_all` (:315-317) + `load` (:320-321) + swap into `state.project.config` (:322-325) |
| save_endpoints provider swap | `src-tauri/src/ipc/settings.rs:334-407` | factory.set_provider (:375) + loop(set_provider+set_resolved_model(None)) (:383-385) + ModelChanged emit (:393-399) |
| save_endpoints rewire | `src-tauri/src/ipc/settings.rs:411-418` | rewire_vision_and_embedder + sync_model_resolver |
| save_settings persist+reload | `src-tauri/src/ipc/settings.rs:1242-1252` | same shape as save_endpoints |
| save_settings safety sync | `src-tauri/src/ipc/settings.rs:1255-1266` | `*safety_mode.write() = mode` + `approvals.re_evaluate` |
| set_model | `src-tauri/src/ipc/agent.rs:437-538` | validate+build provider (:457-499) + factory.set_provider (:503) + loop swap+clear (:508-519) + ModelChanged emit (:528-534) |
| set_resolved_model(None) in set_model | `src-tauri/src/ipc/agent.rs:516` | ✓ present |
| set_resolved_model(None) in save_endpoints | `src-tauri/src/ipc/settings.rs:385` | ✓ ALREADY present (plan's "doesn't clear" is wrong — see C1) |
| set_safety_mode | `src-tauri/src/ipc/agent.rs:321-333` | `*safety_mode.write() = parsed` (:323-325) + `re_evaluate` (:328) — duplicates save_settings safety block |
| wire structs | `src-tauri/src/ipc/settings.rs:444-658` | `EndpointWire`, `GetConfigResponse`, `GetSettingsResponse`, `SaveEndpointsResponse`, `SaveSettingsResponse`, etc. — STAY in settings.rs |
| contract fixtures (Rust) | `src-tauri/src/ipc/contract_fixtures.rs:85` (`dto_fixtures_match_serde`); save_endpoints at :265, save_settings at :272 | build `SaveEndpointsResponse`/`SaveSettingsResponse` + assert_fixture — unchanged by B2 (no struct changes) |
| contract fixtures (TS) | `frontend/src/lib/ipc-contract.test.ts:363-373` | dto-save-endpoints (:363) + dto-save-settings (:369) field-shape asserts — unchanged |
| `resolved_model()` readers | `src/agent/loop_impl.rs:471` (def), `src-tauri/src/ipc/agent.rs:377` (list_agents: `resolved_model().unwrap_or_else(provider model)`) | list_agents PREFERS resolved, falls back to provider — clearing → shows default model (intended post-swap). No caller depends on it NOT being cleared. ✓ |
| `set_resolved_model` internal callers | `src/agent/loop_impl.rs:548,557,568,578` | turn driver sets/clears per turn — unaffected by swap clearing ✓ |

**Risk surface for B2 (callers depending on resolved_model NOT being cleared):** NONE. The only reader is `list_agents` (agent.rs:376-378), which treats `None` as "fall back to provider model" — exactly the desired post-swap behavior. The turn driver re-resolves on the next turn (loop_impl.rs:548/578). Clearing is safe. ✓

### B3 — `Sandbox::validate_for_write` ladder

| Plan ref | Actual location | Current behavior |
|---|---|---|
| file_write execute ladder | `src/tool/agent/file_write.rs:79-158` (spawn_blocking) | Pre-check protected (:86-94) + ladder validate→creation-fallback→protected→mkdir→revalidate (:100-134). TWO protected checks, same message text. |
| file_write refusal msg (execute) | `src/tool/agent/file_write.rs:88-92` + `:114-118` | `"refused: '{}' is a protected .coding state/bookkeeping file — use the dedicated tools (memory/plan/safety) instead of file_write"` (×2, identical) |
| file_write prepare_for_approval | `src/tool/agent/file_write.rs:175-210` | validate→creation-fallback→protected (:177-186), NO mkdir (preview). Msg: `"refused: '{}' is a protected .coding state/bookkeeping file"` (no suffix) — **DIFFERENT** |
| file_append execute | `src/tool/agent/file_append.rs:85-149` | validate→creation-fallback (:89-98)→protected (:101-106). **NO mkdir parents** (the inconsistency). OpenOptions create+append at :121-128. |
| file_append refusal msg | `src/tool/agent/file_append.rs:102-105` | `"refused: '{}' is a protected .coding state/bookkeeping file"` (no suffix) — **DIFFERENT** |
| file_edit execute | `src/tool/agent/file_edit.rs:533-547` | validate→protected (:534-547). NO creation ladder (edits existing files). Msg: `"...instead of file_edit"` — **DIFFERENT** |
| file_edit prepare_for_approval | `src/tool/agent/file_edit.rs:582-610` | validate→protected (:586-592). Msg: `"refused: '{}' is a protected .coding state/bookkeeping file"` (no suffix) |
| `Sandbox::validate` | `src/tool/agent/sandbox.rs:78-109` | canonicalize existing; parent-must-exist for non-existent |
| `Sandbox::validate_for_creation` | `src/tool/agent/sandbox.rs:121-134` | lexical normalize + starts_with(root) |
| `Sandbox::is_protected_write_target` | `src/tool/agent/sandbox.rs:170-187` | strip_prefix(root) + lowercase + compare |
| M5 creation-gap tests (sandbox.rs) | `src/tool/agent/sandbox.rs:297-421` | `protected_nonexistent_path_caught_via_creation` (:341-363) already tests the creation gap at sandbox level |
| M5 creation-gap tests (file_write.rs) | `src/tool/agent/file_write.rs:342-427` | `refuses_to_write_memory_db` (:342), `refuses_to_write_bookkeeping_files` (:362), `refuses_to_create_nonexistent_protected_path` (:398), `allows_writing_review_reports` (:414) — tool-level |

**Message inconsistency confirmed:** 4 distinct refusal messages across the 3 tools (with/without tool-name suffix). B3 unifies to ONE shared message.

**Test assertion lines (must keep passing):** file_write.rs:353,384,408 + file_append.rs:219 all assert `.contains("protected")`. The shared message must contain "protected". ✓

---

## 2. CONFIRMED/REFINED DESIGN (choices marked)

### B1 — per-agent plan dirs  `[CHOICE: confirmed with refinements]`

**Confirmed:** Non-main parentless (UI-spawned) agents get `.coding/plans/agents/<id>/`; main keeps `.coding/plans/`; subagents (tool-spawned, with parent) keep sharing the main dir (they can't mutate — `plan_mutations_allowed=false`, spawn.rs:123). `load_latest` for a fresh side agent starts empty (dir absent → Planning, workflow/mod.rs:686-689). `main_agent_id` = min parentless id (already documented at runtime/mod.rs:118-126).

**Refinement 1 — distinguishing main from UI spawn:** The caller passes an explicit flag, NOT an id-based heuristic. Add a param to `spawn_agent_shared` (e.g. `own_plans_dir: bool`):
- `main.rs:154` (startup) → `false` (main keeps `.coding/plans/`).
- `spawn_agent` Tauri cmd (spawn.rs:33) → `true` (UI spawn gets own dir).
- `IpcSpawner::spawn_with_parent` (spawn.rs:407, tool spawn) → `false` (subagent shares; can't mutate).

This is more robust than "id == main_agent_id()" (avoids a manager lock + handles main-exit edge cases).

**Refinement 2 — NO cross-dir scan in get_plan/get_workflow_state.** Both already read the per-agent workflow's `plans_dir` (agent.rs:616, :554). With each workflow built against its own dir, resolution is automatic. `[CHOICE: skip the linear scan]` — it adds complexity for no current use case (plan staircase is per-agent; ancestors live in the same agent's dir). If a cross-agent plan fetch is ever needed, add it then. **The plan's "resolve plan ids across dirs" is satisfied by per-agent workflow `plans_dir`.** This CORRECTS the user's pre-resolved Choice 1 (the linear scan is unnecessary).

**Refinement 3 — fix `reviews_dir()` derivation (C3).** The factory must derive `.coding/reviews/` from the MAIN plans dir, not the per-agent override. Keep `plans_dir` as the main dir (`.coding/plans/`); the per-agent override flows ONLY into `Workflow::new` + `with_plans_dir`, never into `reviews_dir()`.

**Refinement 4 (NEW, C6) — fix `FinishTool` reviews derivation.** `FinishTool` must NOT derive reviews from the per-agent workflow's `plans_dir()`. Give it an explicit `reviews_dir: PathBuf` field (mirroring `WriteReviewReportTool`), constructed by the factory from `self.reviews_dir()` (main-derived). This closes the mismatch where a side agent's `finish` would look in `.coding/plans/agents/reviews/` while the reviewer writes to `.coding/reviews/`.

### B2 — config-write unification  `[CHOICE: confirmed; NOT a bug fix]`

**Confirmed:** New `src-tauri/src/ipc/config_io.rs` with:
- `persist_and_reload(state, new_config) -> Result<Config>` — `save_all` + `load` + swap into `state.project.config`. Extracts settings.rs:314-325 and :1242-1252.
- `swap_live_provider(state, app, provider, context_manager, model) -> bool` — `factory.set_provider` + loop(`set_provider` + `set_resolved_model(None)`) + `ModelChanged` emit per agent. Extracts settings.rs:372-401 and agent.rs:503-534. **ALWAYS clears `resolved_model`** (preserves existing correct behavior at settings.rs:385 + agent.rs:516 — see C1: this is NOT a new fix).
- `set_runtime_safety(state, mode) -> usize` — `*safety_mode.write() = mode` + `approvals.re_evaluate` → returns resolved count. Extracts agent.rs:321-332 and settings.rs:1255-1265.

**Wire structs STAY in settings.rs** (EndpointWire, GetConfigResponse, etc.). config_io.rs imports them. **Fixtures byte-identical** — no struct changes, so contract_fixtures.rs + ipc-contract.test.ts are untouched. ✓

**`[CHOICE: confirmed]` swap_live_provider ALWAYS clears resolved_model** — verified safe (no caller depends on it NOT being cleared; list_agents falls back to provider model). This CORRECTS the user's pre-resolved Choice 2 premise: the "live drift" is already fixed; B2 is a pure refactor.

### B3 — `Sandbox::validate_for_write` ladder  `[CHOICE: confirmed with file_edit carve-out]`

**Confirmed:** New `Sandbox::validate_for_write(&self, path: &Path) -> Result<PathBuf>` implementing the full ladder: validate → creation-fallback → `is_protected_write_target` → mkdir parents → revalidate, with ONE shared refusal `Error::InvalidInput`. Rewire file_write.execute + file_append.execute to call it. **`[CHOICE: confirmed]` file_append now creates parent dirs** (consistency — it currently doesn't, file_append.rs:89-98 has no mkdir). This matches the user's pre-resolved Choice 3.

**Refinement — file_edit does NOT use the full ladder.** file_edit edits EXISTING files (reads first, file_edit.rs:549). The creation ladder would mkdir parents for a non-existent file, then `read_to_string` fails — a confusing side effect. file_edit uses a thin `validate` + `is_protected_write_target` (or a `refuse_if_protected` helper) with the SHARED message, no creation. The plan's "rewire file_edit" = use the shared refusal message + the protected check, NOT the creation ladder.

**Shared refusal message:** ONE string, e.g. `"refused: '{path}' is a protected .coding state/bookkeeping file — use the dedicated tools (memory/plan/safety) instead of the file tools"`. All existing tests assert `.contains("protected")` (file_write.rs:353,384,408; file_append.rs:219) — pass. ✓

**`[CHOICE: confirmed]` move/adopt M5 creation-gap tests to sandbox.rs** — add `validate_for_write` tests covering the full ladder (mkdir + revalidate + protected refusal + non-existent protected caught before mkdir). Keep thin tool-level smoke tests in file_write/file_append (refusal + parent-dir creation) or remove the redundant ones.

---

## 3. EXACT EDIT PLAN (add/change what, where)

### B1 edits

**`src/agent/factory.rs`** (lib crate — the factory):
1. Add a public accessor `pub fn plans_dir(&self) -> &Path` (factory.rs:~92 area, near the field) so `spawn_agent_shared` can compute the per-agent dir from the main dir.
2. `reviews_dir()` (factory.rs:438-443) — **NO change needed** as long as it keeps using `self.plans_dir` (the main dir). Just ensure the per-agent override does NOT flow into it. (It already uses `self.plans_dir`, so this is verify-only — but the new `build_with_id_and_plans_dir` must NOT touch `reviews_dir()`.)
3a. Add `pub fn build_with_id_and_plans_dir(&self, id: AgentId, plans_dir: PathBuf) -> Arc<AgentLoop>` (factory.rs:~319 area) — like `build_with_id` but the `Workflow::new` (factory.rs:332), `with_plans_dir` (factory.rs:371), and the loop's plans_dir use the OVERRIDE; `reviews_dir()` (factory.rs:439) keeps using `self.plans_dir` (main). Add a doc comment explaining main vs per-agent.
3b. **(NEW, C6)** `FinishTool` construction (factory.rs:492): change `FinishTool::new(workflow.clone())` → `FinishTool::new(workflow.clone(), self.reviews_dir())` so FinishTool uses the main-derived reviews dir, not the per-agent workflow's `plans_dir().parent()`.
4. `build_with_id` (factory.rs:319-321) stays as-is (delegates to `build_inner` with `self.plans_dir`).

**`src/tool/workflow/plan.rs`** (FinishTool — C6 fix):
3c. Add a `reviews_dir: PathBuf` field to `FinishTool` + a `new(workflow, reviews_dir)` constructor (mirror `WriteReviewReportTool` at `src/tool/agent/write_review_report.rs:37-46`).
3d. In `execute` (plan.rs:530-534): replace the `wf.plans_dir().parent().join("reviews")` derivation with `self.reviews_dir.clone()`. Keep the canonicalize + `starts_with` + non-empty checks (:535-543) unchanged.

**`src-tauri/src/ipc/spawn.rs`**:
5. Add `own_plans_dir: bool` param to `spawn_agent_shared` (spawn.rs:82 signature). After allocating `agent_id` (spawn.rs:95-98), if `own_plans_dir`, compute `let plans_dir = factory.plans_dir().join(format!("agents/{agent_id}"));` and call `factory.build_with_id_and_plans_dir(agent_id, plans_dir)`; else `factory.build_with_id(agent_id)` (current spawn.rs:111). The dir need NOT be pre-created: `load_latest` returns Planning when the dir is absent (workflow/mod.rs:686-689), and `persist_stack` does `create_dir_all` on first plan (workflow/mod.rs:660).
6. `spawn_agent` Tauri cmd (spawn.rs:33-70): pass `own_plans_dir=true` at the spawn_agent_shared call (spawn.rs:41-51).
7. `main.rs:153-166`: pass `own_plans_dir=false` (main keeps `.coding/plans/`).
8. `IpcSpawner::spawn_with_parent` (spawn.rs:407-433): pass `own_plans_dir=false` at spawn.rs:415 (subagent shares main dir; can't mutate).

**`src-tauri/src/ipc/agent.rs`**:
9. `get_plan` (agent.rs:587-626) + `get_workflow_state` (agent.rs:546-572): **NO change** — they already read the per-agent workflow's `plans_dir` (agent.rs:616, :554). Verify only.

**`src/runtime/mod.rs`**:
10. `main_agent_id` (runtime/mod.rs:118-133): **NO change** — doc comment already present. Verify only.

### B2 edits

**NEW FILE `src-tauri/src/ipc/config_io.rs`**:
1. Create with three free functions (or a small impl block taking `&IpcState`):
   - `pub(crate) async fn persist_and_reload(state: &IpcState, new_config: Config) -> Result<Config, IpcError>` — `save_all(&config_dir)` + `Config::load(&config_dir)` + swap into `state.project.config`. (Extract settings.rs:314-325 / :1242-1252.)
   - `pub(crate) async fn swap_live_provider(state: &IpcState, app: &tauri::AppHandle, provider: Arc<dyn LlmClient>, context_manager: ContextManager, model: String) -> bool` — `factory.set_provider` + loop(`set_provider` + `set_resolved_model(None)`) + `ModelChanged` emit. Returns false if no factory. (Extract settings.rs:372-401 / agent.rs:503-534.) **ALWAYS clears resolved_model.**
   - `pub(crate) fn set_runtime_safety(state: &IpcState, mode: SafetyMode) -> usize` — `*safety_mode.write() = mode` + `approvals.re_evaluate` → resolved count. (Extract agent.rs:321-332 / settings.rs:1255-1265.)
2. Register in `src-tauri/src/ipc/mod.rs`: add `pub mod config_io;` (alphabetical — after `pub mod browser;` at mod.rs:19, before `pub mod embeddings;` at :20).

**`src-tauri/src/ipc/settings.rs`**:
3. `save_endpoints` (settings.rs:314-325): replace persist+reload with `let reloaded = config_io::persist_and_reload(&state, new_config).await?;`.
4. `save_endpoints` (settings.rs:334-407): replace the provider-swap block with `let provider_swapped = if let Some(factory) = ... { build provider...; config_io::swap_live_provider(&state, &app, provider, context_manager, model).await } else { false };`. Keep the no-endpoint `None` arm + rewire_vision_and_embedder + sync_model_resolver (settings.rs:411-418) in the caller.
5. `save_settings` (settings.rs:1242-1252): replace with `persist_and_reload`.
6. `save_settings` (settings.rs:1255-1266): replace safety block with `let resolved = config_io::set_runtime_safety(&state, mode);`.

**`src-tauri/src/ipc/agent.rs`**:
7. `set_model` (agent.rs:503-534): replace factory.set_provider + loop swap + ModelChanged emit with `config_io::swap_live_provider(&state, &app, provider, context_manager, model).await;` (drop the local `provider_swapped` bool — set_model returns `()`). Keep the validation + provider-build block (agent.rs:457-499).
8. `set_safety_mode` (agent.rs:321-332): replace with `let resolved = config_io::set_runtime_safety(&state, parsed);` + the eprintln.

**Fixtures:** NO change to `contract_fixtures.rs` or `ipc-contract.test.ts` (no struct changes). ✓

### B3 edits

**`src/tool/agent/sandbox.rs`**:
1. Add a shared refusal helper (module-level `fn protected_refusal(path: &Path) -> Error` or a `const`-templated message) — ONE message string containing "protected".
2. Add `pub fn validate_for_write(&self, path: &Path) -> Result<PathBuf>` (sandbox.rs:~134, after `validate_for_creation`):
   - `match self.validate(path)`:
     - `Ok(p)` → if `is_protected_write_target(&p)` return `Err(protected_refusal(path))`; else `Ok(p)`.
     - `Err(_)` → `let p = self.validate_for_creation(path)?;` → if protected → `Err(protected_refusal)` → `if let Some(parent) = p.parent() { if !parent.exists() { create_dir_all(parent)?; } }` → `self.validate(path)` again (revalidate) → `Ok` or propagate.
3. Add `pub fn refuse_if_protected(&self, validated: &Path) -> Result<()>` for file_edit / approval paths that don't need the creation ladder — returns `Err(protected_refusal)` if protected, else `Ok(())`.
4. Add `validate_for_write` tests in the `#[cfg(test)] mod tests` block (sandbox.rs:212+): full ladder (mkdir + revalidate), non-existent protected caught before mkdir, existing protected caught, normal file passes, parent-dir creation.

**`src/tool/agent/file_write.rs`**:
5. `execute` (file_write.rs:79-158): replace the pre-check (:86-94) + ladder (:100-134) with `let validated = sandbox.validate_for_write(path)?;` (one call). Remove the duplicate protected checks. Keep line-ending detection + write (:141-148).
6. `prepare_for_approval` (file_write.rs:175-186): replace validate+creation-fallback+protected with `let validated = match self.sandbox.validate(path) { Ok(p)=>p, Err(_)=>self.sandbox.validate_for_creation(path)? }; self.sandbox.refuse_if_protected(&validated)?;` (no mkdir — it's a preview).

**`src/tool/agent/file_append.rs`**:
7. `execute` (file_append.rs:85-149): replace validate+creation-fallback (:89-98) + protected (:101-106) with `let validated = sandbox.validate_for_write(path)?;`. This ADDS parent-dir creation (the consistency fix). Keep OpenOptions create+append (:121-128) + line-ending detection (:114-117) + metadata (:136-138).

**`src/tool/agent/file_edit.rs`**:
8. `execute` (file_edit.rs:533-547): replace validate (:534) + protected (:541-547) with `let validated = sandbox.validate(path)?; sandbox.refuse_if_protected(&validated)?;` (NO creation ladder — file_edit reads existing files). Keep read+prepare+write (:549-562).
9. `prepare_for_approval` (file_edit.rs:582-592): replace validate+protected with `let validated = self.sandbox.validate(path)?; self.sandbox.refuse_if_protected(&validated)?;`.

**Tests:**
10. Move/adopt M5 creation-gap tests from file_write.rs (:342-427) to sandbox.rs as `validate_for_write` tests. Keep a thin smoke test in file_write (`refuses_to_write_memory_db`) + add `file_append` parent-dir-creation test + `file_append` refuses-protected test.

---

## 4. TEST PLAN

**Rust (lib crate) — `cargo test` (unpiped, read `test result:` lines):**
- B1: `src/agent/factory.rs` tests (factory.rs:615+) — add a test that `build_with_id_and_plans_dir` produces a workflow whose `plans_dir()` differs from the factory's main `plans_dir`, and `load_latest` on a fresh per-agent dir returns Planning (empty). Add a spawn-level test (in spawn.rs tests, spawn.rs:450+) that two parentless agents built with `own_plans_dir=true` get distinct plans dirs and their `stack.json` sidecars don't clobber (create a plan in each → both persist independently). Add a FinishTool test (plan.rs tests, :780+) that `finish` finds a report under `.coding/reviews/` even when the workflow's `plans_dir` is a per-agent subdir (regression for C6).
- B3: `src/tool/agent/sandbox.rs` — `validate_for_write` ladder tests (mkdir, revalidate, protected-before-mkdir, non-existent protected, normal pass). `file_write.rs` — keep `refuses_to_write_memory_db`, `creates_parent_dirs`, `refuses_to_create_nonexistent_protected_path`. `file_append.rs` — add `creates_parent_dirs` (new) + `refuses_memory_db` (existing, :210). `file_edit.rs` — existing `path_traversal_rejected` still passes.

**Rust (app crate) — `src-tauri` `cargo test` + `cargo build`:**
- B2: `contract_fixtures.rs` `dto_fixtures_match_serde` (contract_fixtures.rs:85) — MUST still pass (byte-identical). `settings.rs` `endpoint_dto_tests` + `settings_dto_tests` (settings.rs:1288+) — unchanged, must pass. Add a config_io unit test if feasible (persist_and_reload against a tempdir; swap_live_provider with a mock factory — may be hard without a full IpcState; prefer keeping the extraction behavior-identical and rely on existing save_endpoints/set_model integration).

**Frontend — `npx vitest run` + `npm run build`:**
- `frontend/src/lib/ipc-contract.test.ts` — unchanged, must pass (no fixture changes). `useAgentStore.test.ts` — unchanged.

**Full verification matrix per the plan:** root `cargo test` + `src-tauri` `cargo test` + `cargo build` (root test does NOT compile the bin) + `npx vitest run` + `npm run build`. Windows/PowerShell, unpiped, read `test result:` / `$LASTEXITCODE`.

---

## 5. RISKS / BLOCKERS

### R1 (B1, HIGH — REQUIRED EDIT) — `reviews_dir()` + `FinishTool` derivation breaks under per-agent plans dir
`reviews_dir()` (factory.rs:438-443) = `plans_dir.parent().join("reviews")`. With per-agent `plans_dir` = `.coding/plans/agents/<id>/`, this yields `.coding/plans/agents/reviews/` ✗. **AND** `FinishTool::execute` (plan.rs:530-534) independently derives reviews the same way from the per-agent workflow's `plans_dir()`, so a side agent's `finish` looks in `.coding/plans/agents/reviews/` while the reviewer subagent writes to `.coding/reviews/` (via the factory's `reviews_dir()`). **Fix (two parts):** (a) `reviews_dir()` keeps using `self.plans_dir` (main) — verify-only since it already does, but the new `build_with_id_and_plans_dir` must NOT route the override into it; (b) **FinishTool must take an explicit `reviews_dir: PathBuf`** (mirror `WriteReviewReportTool`), constructed by the factory from `self.reviews_dir()`. The prior draft only said "verify finish" — this is a REQUIRED EDIT (C6).

### R2 (B1, MED) — `corpus_digest` per-agent scope change
Session-end consolidation (runtime/agent.rs:455-459) digests the loop's `plans_dir`. Side agents with per-agent dirs digest only their own (smaller) corpus; main keeps the full corpus. Likely acceptable (per-agent learning), but flag as a behavior change. No code change needed unless cross-agent corpus is desired.

### R3 (B2, HIGH — false-premise risk) — B2 is NOT a bug fix
The plan frames B2's `swap_live_provider` as fixing a live `resolved_model` drift. That drift is ALREADY fixed (settings.rs:385, agent.rs:516). The commit message + reviewer framing must say "refactor: extract duplicated config-write logic" NOT "fix live drift." If the reviewer is told it's a bug fix, it will look for the bug and find nothing (or flag the false premise). **Action:** the implementer must correct the framing; the spec confirms `[CHOICE: swap_live_provider ALWAYS clears resolved_model]` as preserving existing behavior. This CORRECTS the user's pre-resolved Choice 2 premise.

### R4 (B2, LOW) — `swap_live_provider` signature shape
`save_endpoints` (settings.rs:372-401) and `set_model` (agent.rs:503-534) differ: save_endpoints handles a `None` provider (no endpoints) and returns `provider_swapped: bool`; set_model always has a provider and returns `()`. The extracted `swap_live_provider` should take an already-built `provider` + `context_manager` + `model` (the common pieces) and return `bool`; the callers keep their provider-construction + the no-endpoint arm. The `ModelChanged` event needs the `model: String` + `app: &AppHandle` — pass both.

### R5 (B3, MED) — file_edit must NOT use the creation ladder
If `validate_for_write` (with mkdir) is naively wired into file_edit, it would create parent dirs for a non-existent file, then `read_to_string` (file_edit.rs:549) fails — a confusing side effect (dirs created, edit fails). **Fix:** file_edit uses `validate` + `refuse_if_protected` (no mkdir). The plan's "rewire file_edit" = shared message + protected check only.

### R6 (B3, LOW) — shared message text vs. existing test assertions
All existing tests assert `.contains("protected")` (file_write.rs:353,384,408; file_append.rs:219). The shared message must contain "protected." The current messages also say "use the dedicated tools (memory/plan/safety)" — keep that in the shared message for continuity. No test asserts the exact full string, so the unification is safe.

### R7 (B1, LOW) — `is_protected_write_target` already covers per-agent subdirs
`.coding/plans/agents/<id>/` is under `.coding/plans/`, so `is_protected_write_target` (sandbox.rs:186 `starts_with(".coding/plans/")`) still blocks file tools from writing there ✓. The workflow tools (create_plan etc.) bypass this (they write directly), which is correct. No change needed.

### R8 (B1, LOW) — plan_id validation in get_plan
`get_plan` (agent.rs:599-609) rejects plan_ids containing `.`/`/`/`\\`/`..`. Plan ids are UUIDs (no dots). Per-agent dirs don't change the plan_id format. ✓ No change.

### R9 (B1, LOW) — `build_with_id_and_plans_dir` must not break the `build()` (id-less) path
`build()` (factory.rs:309-311) and `build_with_id()` (factory.rs:319-321) both delegate to `build_inner` with `self.plans_dir`. The new `build_with_id_and_plans_dir` should share `build_inner`'s body but with the override passed in (e.g. add a `plans_dir: &Path` param to `build_inner`, or duplicate the small body). Ensure `build()`/`build_with_id()` still pass `self.plans_dir` (main). Watch for dead code under `#![deny(warnings)]` — if `build_inner` gains a param, both callers must use it.

### BLOCKERS: None identified. All three items are implementable as specified. The main watch-items are R1 (reviews_dir + FinishTool — REQUIRED EDIT, not just verify) and R3 (B2 framing — refactor, not bug fix).

---

## SUMMARY OF PRE-RESOLVED CHOICE VERIFICATION

| Choice | Verdict | Notes |
|---|---|---|
| 1. B1 lookup: linear scan main + agents dirs; load_latest empty for fresh parentless; main_agent_id = min parentless; document at runtime/mod.rs:127-133 | **PARTIALLY WRONG** | Linear scan NOT needed — get_plan/get_workflow_state already read per-agent workflow `plans_dir` (agent.rs:616, :554). `[CHOICE: skip the linear scan]`. load_latest empty ✓. main_agent_id = min parentless ✓. Documentation ALREADY present (runtime/mod.rs:118-126) — verify only. |
| 2. B2: swap_live_provider ALWAYS clears resolved_model; fixes live drift in save_endpoints; wire structs stay; fixtures byte-identical | **CORRECT in substance, WRONG premise** | swap_live_provider ALWAYS clears ✓ (safe — no caller depends on it NOT being cleared). But "fixes live drift" is STALE — save_endpoints ALREADY clears (settings.rs:385). B2 is a pure refactor, NOT a bug fix. Wire structs stay ✓. Fixtures byte-identical ✓. |
| 3. B3: file_append creates parent dirs; move/adopt M5 creation-gap tests to sandbox.rs | **CORRECT** | file_append currently has NO mkdir (file_append.rs:89-98); validate_for_write adds it (consistency). M5 tests at sandbox.rs:297-421 + file_write.rs:342-427 — adopt into sandbox.rs. ✓ |
