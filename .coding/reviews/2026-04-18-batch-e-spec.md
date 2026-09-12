# Batch E — Implementation Spec (verified, read-only)

Scope: produce a precise, verified IMPLEMENTATION SPEC for Batch E of plan
`e342c141-002c-4705-92ed-42dfbb5478e0.md` (step 5). All file:line refs below
were re-verified against current code (the 2026-04-18 refs drifted). No source
edited.

Branch: `feat/deep-review-e-moves`. Target ≈ −170 LOC net. Windows/PowerShell;
`cargo test` UNPIPED (read `test result:` lines + `$LASTEXITCODE`); `#![deny(warnings)]`
— no `#[allow]`, no dead code; doc comments on all public items; preserve line
endings. Run root `cargo test` AND `cargo test` in `src-tauri/` (root test does
NOT compile the bin crate) AND `cargo build` in `src-tauri/` + frontend
`npx vitest run` + `npm run build`.

---

## 1. Per-item current-code verification (corrected file:line)

### E1 — pure moves (BacklogStore + run-all git ops) + A9 fix

**BacklogStore** — lives at `src-tauri/src/ipc/backlog.rs` (NOT a separate
file). Verified contents:
- `BacklogItem` struct — `backlog.rs:16-29` (id, text, images, status, created_at, note).
- `BacklogStatus` enum — `backlog.rs:32-45` (`#[serde(rename_all="snake_case")]`: Pending/InFlight/Done/Failed/CantResolve).
- `BacklogStore` struct — `backlog.rs:52-56` (items, next_id, path).
- `BacklogFile` (on-disk repr) — `backlog.rs:59-64`; `default_next_id` — `:66-68`.
- `impl BacklogStore` — `:70-268` (open, add, remove, reorder, set_status, edit, next_pending, pending_item, clear_finished, items, empty, persist).
- `now_secs` helper — `:271-276` (private).
- `#[cfg(test)] mod tests` — `:278-527` (TestDir + 10 tests).
- **No Tauri deps** — only `std::path::PathBuf`, `serde`, `std::fs`, `std::time`. Pure domain code. ✓ move-safe.

**Importers of BacklogStore / BacklogItem / BacklogStatus** (must update after move):
- `src-tauri/src/ipc/state.rs:29` — `use crate::ipc::backlog::BacklogStore;` + `:133` `pub store: Arc<Mutex<BacklogStore>>`.
- `src-tauri/src/main.rs:102` — `ipc::backlog::BacklogStore::open(`.
- `src-tauri/src/ipc/run_all.rs:47` — `use crate::ipc::backlog::{BacklogItem, BacklogStatus};`.
- `src-tauri/src/ipc/backlog_cmds.rs` — imports BacklogItem/BacklogStatus (the command module; emits `backlog_list` etc.).
- `src-tauri/src/ipc/contract_fixtures.rs` — references BacklogItem (test-only).
- Frontend types are separate (TS) — unaffected.

**run-all git ops** — `src-tauri/src/ipc/run_all.rs`:
- `git(root, args)` — `:107-129` (std::process::Command + Windows CREATE_NO_WINDOW).
- `checkpoint(root, item)` — `:135-149` (takes `&BacklogItem`).
- `commit_success(root, item)` — `:152-166` (takes `&BacklogItem`).
- `rollback(root, sha)` — `:169-171`.
- `first_line(text)` — `:174-184` (private helper).
- `#[cfg(test)] mod tests` — `:186-396` (TestRepo + 4 tests: checkpoint_then_commit_success, checkpoint_commits_dirty_tree, checkpoint_then_rollback, commit_success_on_clean_tree).
- **No Tauri deps** — only `std::path::Path`, `std::process::Command`. ✓ move-safe.
- NOTE: `checkpoint`/`commit_success` take `&BacklogItem` → git_ops depends on `myharness::backlog::BacklogItem`. Both move to lib, so the dep is intra-lib. ✓.

**What STAYS in run_all.rs** (orchestration, not git ops — NOT moved):
- `RUN_ALL_STEER` const (`:55-61`), `strict_success_allows_commit` (`:77-79`, pure fn over WorkflowState), `main_agent_workflow_state` (`:89-101`), `extract_checkpoint_sha` + its `extract_tests` (`:404-453`), `run_all_dispatch_next` (`:461-521`), `on_main_turn_resolved` (`:531-654`), `halt_run_all_for_approval` (`:672-708`).

**A9 fix** — `run_all.rs:632`:
```rust
632:        let _ = run_all_dispatch_next(app, &state).await;
```
This discards the `Result<(), String>` from `run_all_dispatch_next`. Two Err paths
inside dispatch_next:
1. Checkpoint failure (`:478-489`) → next item already marked `Failed` + run_all cleared + emit + `return Err`. Item IS marked.
2. No main agent (`:506-510`) → the item was set `InFlight` at `:499-502` but never dispatched; run_all cleared + emit + `return Err("no main agent registered")`. **Item is left stuck `InFlight`** — this is the real A9 bug.

So the swallowed error masks a stuck-InFlight item on the no-main-agent path.

### E2 — dedup maybe_reembed

**Duplicate confirmed** — near-verbatim:
- `src-tauri/src/main.rs:741-771` (startup cross-machine re-embed).
- `src-tauri/src/ipc/rewire.rs:58-92` (post-config-save re-embed).

Both: spawn a `tauri::async_runtime::spawn` task → `store.stored_model_fingerprints().await` → predicate `fps.len() != 1 || fps[0].0 != expected_model || fps[0].1 != expected_dim` → `store.reembed_all(embedder).await` → log Ok(n)/Err(e). Same comments.

**Differences (caller-specific, stay at call sites):**
- main.rs uses `store.embedder_handle()` (the store's current embedder) + guards on `configured_bundled.is_some() && !is_hash_opt_out && pending_download.is_none() && status == Ready`; logs stored fingerprints in the info line.
- rewire.rs uses `new_embedder` (just built by `build_embedder`) + guards on `!is_hash_opt_out && status == Ready` (rewire only fires on a config change, so a model is implicitly configured).

**Shared (the ~30 verbatim lines to extract):** the spawn + fingerprints fetch + needs_reembed predicate + reembed_all + result logging.

### E3 — startup_snapshot()

**Current App.tsx startup pulls** — `frontend/src/App.tsx:140-285` (the plan's
":166-233" range is the core, but the full sequence runs to :285):
1. `getNeedsProject()` — `:145` (gates everything; stays).
2. `getStartupError()` — `:155` (gates everything; stays).
3. `listAgents()` — `:168` → `registerAgents` + set activeAgent.
4. `getContextCaps()` — `:183` → `seedContextCaps`.
5. `getWorkflowState(id)` — `:193` → **active agent ONLY** (the stale-non-active-workflowStates gap).
6. `getGitBranch()` — `:200` (stays — not in snapshot).
7. `getConfig()` — `:206` (model/endpoints/pricing — E5 migrates to getSettings).
8. `getSettings()` — `:237` (UI prefs — stays).
9. `getSafetyMode()` — `:270` (live safety — stays).
10. `getEmbedderStatus()` — `:280` (in snapshot).

**Backend commands the snapshot aggregates** (all in `src-tauri/src/ipc/agent.rs` + backlog):
- `list_agents` — `agent.rs:353-390` (returns `Vec<AgentInfo>`).
- `context_caps` — `agent.rs:400-406` (returns `Vec<(AgentId, u32)>` — max only).
- ALL workflow states — NOT an existing command. Must iterate `state.runtime.agent_loops` and read each `workflow.state()` (`WorkflowState`, `#[serde(rename_all="lowercase")]` → "planning"/"executing"/"reviewing"/"complete"/"skill"). Mirror `main_agent_workflow_state` (run_all.rs:89-101) but for ALL agents.
- backlog — `state.backlog.store.lock().await.items()` → `Vec<BacklogItem>` (same as `backlog_list`, backlog_cmds.rs:98).
- embedder_status — `state.runtime.embedder_status.read()` → `EmbedderStatus` (same as `get_embedder_status`, settings.rs:27-34).

### E4 — frontend COLOR_PREFS table

**appearance.ts** — `frontend/src/hooks/appearance.ts` (NOT `frontend/src/appearance.ts` — plan path was wrong):
- LS_* color keys — `:15-25` (4 UI + 7 code = **11 color keys**, NOT 14).
- DEFAULT_* color constants — `:34-45` (11 defaults).
- `applyColors(accent, border, textPrimary, textMuted)` — `:101-113` (4-arg).
- `applyCodeColors(codeText, codeComment, codeKeyword, codeString, codeNumber, codeTitle, codeVariable)` — `:129-147` (7-arg).
- `CodeColorKey` type — `:213-220` (7 keys).
- `CODE_COLOR_ORDER` — `:223-231`.
- `setCodeColor(key, value, get, set)` — `:239-260` (already a shared helper for the 7 code colors).

**useAgentStore.ts:700-833** — `frontend/src/hooks/useAgentStore.ts`:
- `setAccentColor`/`setBorderColor`/`setTextPrimaryColor`/`setTextMutedColor` — `:700-719` (each writeLs + re-passes the other 3 to `applyColors` + set).
- 7 `setCode*Color` — `:720-740` (one-liners delegating to `setCodeColor`).
- `resetColors` — `:741-781` (11 writeLs + applyColors + applyCodeColors + 11-key set).
- `resetAppearance` — `:782-833` (theme/font + the same 11 writeLs + 2 apply + 14-key set).

**Count correction:** the plan says "× 14" but the actual color count is **11** (4 UI + 7 code). The plan's own "collapse the 11 hand-written actions" confirms 11. `[CHOICE: 11 rows, not 14]` — flag: the "× 14" in the plan text is wrong.

### E5 — delete get_config (CRITICAL VERIFICATION)

**get_config** — `src-tauri/src/ipc/settings.rs:44-73`, returns `GetConfigResponse` (`:545-553`):
```rust
GetConfigResponse {
    general: GetConfigGeneral {                          // :556-564
        default_provider: Option<String>,               // source: config.general.general.default_provider (ON-DISK)
        default_model: Option<String>,                  // source: config.general.general.default_model (ON-DISK)
        safety: SafetyMode,                             // source: config.general.general.safety (ON-DISK)
    },
    endpoints: Vec<EndpointWire>,                       // via endpoint_wire(e) — :694-706
    pricing: Vec<PricingWire>,                          // {model, input_per_1m, output_per_1m, cached_per_1m}
}
```

**get_settings** — `settings.rs:734-812`, returns `GetSettingsResponse` (`:567-587`):
```rust
GetSettingsResponse {
    config_dir: String,
    general: GetSettingsGeneral {                       // :591-608
        default_provider: Option<String>,               // source: config.general.general.default_provider (ON-DISK)  ← SAME
        default_model: Option<String>,                  // source: config.general.general.default_model (ON-DISK)     ← SAME
        safety: SafetyMode,                             // source: runtime_safety (LIVE runtime lock)               ← DIFFERENT SOURCE
        vision_model: Option<VisionModelWire>,          // (extra)
        embedding_model: Option<EmbeddingModelWire>,    // (extra)
        bundled_embedding_model: Option<String>,        // (extra)
    },
    context: GetSettingsContext { summarize_at_fill_rate: f64 },   // (extra)
    ui: GetSettingsUi { theme, show_token_usage, show_memory_activity },  // (extra)
    models: ModelsConfigWire,                           // (extra)
    markdown: MarkdownWire { skip_dirs },               // (extra)
    endpoints: Vec<EndpointWire>,                       // via endpoint_wire — SAME type, SAME source
    pricing: Vec<PricingWire>,                         // SAME type, SAME source
    projects: Vec<ProjectWire>,                        // (extra)
}
```

**Frontend callers of getConfig()** (all must migrate):
- `frontend/src/App.tsx:206` — reads `config.general.default_model`, `config.endpoints`, `config.pricing` (NOT safety).
- `frontend/src/components/layout/StatusBar.tsx:134` (`resyncFromBackend`) — reads `config.general` (default_model, default_provider, **safety**), `config.endpoints`.
- `frontend/src/components/layout/StatusBar.tsx:291` (mount) — reads `config.endpoints` only.
- `frontend/src/components/settings/sections/ProvidersSection.tsx:48` — reads config + api_keys (endpoints tab).
- `frontend/src/lib/tauri.ts:289` — `getConfig()` wrapper.

---

## 2. Confirmed / refined design (choices marked)

### E1 `[CHOICE: pure moves to lib; tests travel; A9 = log + mark item]` — CONFIRMED with refinement
- New lib module `myharness::backlog` (`src/backlog.rs` + `pub mod backlog;` in `src/lib.rs:14` area, alphabetical after `app`). Moves: `BacklogItem`, `BacklogStatus`, `BacklogStore`, `BacklogFile`, `default_next_id`, `now_secs`, and the `#[cfg(test)] mod tests` block — verbatim, tests travel.
- New lib module `myharness::project::git_ops` (`src/project/git_ops.rs` + `pub mod git_ops;` in `src/project/mod.rs:7` area). Moves: `git`, `checkpoint`, `commit_success`, `rollback`, `first_line`, and the `#[cfg(test)] mod tests` (TestRepo + 4 tests) — verbatim. `checkpoint`/`commit_success` take `&myharness::backlog::BacklogItem`.
- IPC modules re-import from lib: `state.rs`, `main.rs`, `run_all.rs`, `backlog_cmds.rs`, `contract_fixtures.rs` swap `crate::ipc::backlog::{...}` → `myharness::backlog::{...}`; `run_all.rs` swaps `crate::ipc::run_all::checkpoint/commit_success/rollback` → `myharness::project::git_ops::{...}` (also drops the 7 self-qualified `crate::ipc::run_all::` paths at :474,484,511,566,583,596,608 — use `super::` or direct since they're intra-module).
- `src-tauri/src/ipc/backlog.rs` is DELETED (its contents moved to lib); remove `pub mod backlog;` from `ipc/mod.rs:17`.
- **A9 fix (refined):** two edits.
  1. `run_all.rs:632` — replace `let _ = run_all_dispatch_next(app, &state).await;` with:
     ```rust
     if let Err(e) = run_all_dispatch_next(app, &state).await {
         eprintln!("backlog: run-all failed to dispatch next item: {e}");
     }
     ```
  2. `run_all.rs:506-510` (no-main-agent path inside `run_all_dispatch_next`) — the item was set `InFlight` at `:499-502` but never dispatched. Before returning `Err`, mark it `Failed` so it isn't stuck `InFlight`:
     ```rust
     let Some(main_id) = manager.main_agent_id() else {
         state.backlog.store.lock().await.set_status(
             item.id,
             BacklogStatus::Failed,
             Some("run-all dispatch failed: no main agent registered".to_string()),
         );
         *state.backlog.run_all.lock().await = None;
         emit_backlog_changed(app, state).await;
         return Err("no main agent registered".to_string());
     };
     ```
  - Rationale: the checkpoint-failure path (`:478-489`) already marks the item `Failed`; only the no-main-agent path left it stuck. Logging at `:632` surfaces the previously-swallowed error. `[CHOICE: log the dispatch error and mark the item failed/incomplete (do not silently swallow)]` — confirmed; the "mark item" is the no-main-agent revert.

### E2 `[CHOICE: dedup into one helper]` — CONFIRMED, placement refined to `src/memory/`
- New helper in the **lib** (`src/memory/mod.rs` or a new `src/memory/reembed.rs` + `pub mod reembed;` in `src/memory/mod.rs`), NOT `rewire.rs` — both call sites are in the adapter (`main.rs` + `rewire.rs`), so the helper must be lib-side to avoid an adapter→adapter import and to make the data-integrity logic unit-testable.
- Signature:
  ```rust
  /// Fire-and-forget: if the stored model fingerprints don't exactly match the
  /// embedder's (model_id, dim), re-embed all rows in the background. No-op
  /// (returns without spawning) when the fingerprint set already matches. The
  /// caller is responsible for the Ready-status + hash-opt-out + pending-download
  /// guards (they differ between startup and config-save paths).
  pub fn maybe_reembed(store: Arc<dyn MemoryStoreTrait>, embedder: Arc<dyn Embedder>) {
      let expected_model = embedder.model_id().to_string();
      let expected_dim = embedder.dim();
      tauri::async_runtime::spawn(async move {
          let fps = match store.stored_model_fingerprints().await {
              Ok(f) => f,
              Err(e) => { eprintln!("warning: stored_model_fingerprints failed: {e}"); return; }
          };
          let needs_reembed = fps.len() != 1 || fps[0].0 != expected_model || fps[0].1 != expected_dim;
          if needs_reembed {
              eprintln!("info: re-embedding memories for model '{expected_model}' (dim {expected_dim})");
              match store.reembed_all(embedder).await {
                  Ok(n) => eprintln!("info: re-embedded {n} memories"),
                  Err(e) => eprintln!("warning: reembed_all failed: {e}"),
              }
          }
      });
  }
  ```
  - NOTE: `tauri::async_runtime::spawn` is available in the lib only if the lib depends on tauri. **Verify**: if `myharness` (lib) does NOT depend on `tauri`, use `tokio::spawn` instead (the lib already uses tokio). Check `src/Cargo.toml` for the tokio dep + a multi-thread runtime. If the lib has no runtime handle, the helper must take a `tokio::runtime::Handle` or the caller spawns. **Simplest safe option**: keep the `spawn` at the call sites and extract only the synchronous predicate + the async body into a `pub async fn reembed_if_needed(store, embedder)` that the caller spawns. **Recommended final shape** (avoids runtime coupling):
    ```rust
    pub async fn reembed_if_needed(store: &Arc<dyn MemoryStoreTrait>, embedder: &Arc<dyn Embedder>) {
        // ... fingerprints fetch + predicate + reembed_all + logging (no spawn)
    }
    ```
    Each call site keeps its `tauri::async_runtime::spawn(async move { reembed_if_needed(&store, &embedder).await; })` wrapper + its own guards. This extracts the ~25 verbatim lines while keeping the spawn (which differs in embedder source) at the call sites. ~−30 LOC.
- main.rs:741-771 → guards + `tauri::async_runtime::spawn(async move { myharness::memory::reembed_if_needed(&store_for_reembed, &embedder_for_reembed).await; })`.
- rewire.rs:58-92 → guards + same spawn wrapper calling `reembed_if_needed`.
- Add a lib unit test for `reembed_if_needed` (mock MemoryStoreTrait + Embedder returning mismatched fingerprints → asserts reembed_all called; matching fingerprints → not called).

### E3 `[CHOICE: single serde struct, 5 fields, one invoke, typed wrapper]` — CONFIRMED
- New IPC module `src-tauri/src/ipc/startup.rs` (+ `pub mod startup;` in `ipc/mod.rs`). Command:
  ```rust
  #[derive(Debug, Clone, Serialize)]
  pub struct StartupSnapshot {
      /// All agents (same as list_agents).
      pub agents: Vec<AgentInfo>,
      /// Per-agent context-window max (same as context_caps).
      pub context_caps: Vec<(AgentId, u32)>,
      /// EVERY agent's workflow state (closes the stale-non-active gap).
      pub workflow_states: Vec<(AgentId, WorkflowState)>,
      /// The backlog (same as backlog_list).
      pub backlog: Vec<BacklogItem>,
      /// Embedder status (same as get_embedder_status).
      pub embedder_status: EmbedderStatus,
  }

  #[tauri::command]
  pub async fn startup_snapshot(state: State<'_, IpcState>) -> Result<StartupSnapshot, IpcError> { ... }
  ```
  - `workflow_states`: lock `agent_loops`, map each `(*id, loop.workflow_handle().lock().await.state())`. `WorkflowState` serializes lowercase (`#[serde(rename_all="lowercase")]`, workflow/mod.rs:21). Shape `[[id,"planning"],...]` matches `context_caps`' `[[id,max],...]`.
  - Reuses `AgentInfo` (agent.rs), `BacklogItem` (now `myharness::backlog`), `EmbedderStatus` (settings.rs import).
  - Register `ipc::startup::startup_snapshot` in `main.rs` invoke_handler (near `list_agents`).
- Frontend `frontend/src/lib/tauri.ts`: add typed wrapper:
  ```ts
  export interface StartupSnapshot {
    agents: AgentInfo[];
    context_caps: [number, number][];
    workflow_states: [number, string][];
    backlog: BacklogItem[];
    embedder_status: string;
  }
  export async function getStartupSnapshot(): Promise<StartupSnapshot> {
    return await invoke("startup_snapshot");
  }
  ```
- `App.tsx:166-285`: replace the `listAgents` + `getContextCaps` + `getWorkflowState(active)` + `getEmbedderStatus` pulls with ONE `getStartupSnapshot()` call. Seed: `registerAgents(snap.agents)`, `seedContextCaps(snap.context_caps)`, set ALL `workflowStates` (loop `snap.workflow_states` → `setWorkflowState(id, state)` — closes the gap), seed backlog into the store, `setEmbedderStatus(snap.embedder_status)`. `getGitBranch`, `getSettings` (post-E5, replaces getConfig), `getSafetyMode` stay as separate pulls. The embedder-status event subscription + 10s poll (App.tsx:286-326) stay (they catch post-startup transitions).

### E4 `[CHOICE: COLOR_PREFS const + applyColorPrefs/writeColorPrefs + thin dispatchers + facade unchanged]` — CONFIRMED, count corrected to 11
- In `frontend/src/hooks/appearance.ts`, add:
  ```ts
  export type ColorStateKey =
    | "accentColor" | "borderColor" | "textPrimaryColor" | "textMutedColor"
    | "codeTextColor" | "codeCommentColor" | "codeKeywordColor"
    | "codeStringColor" | "codeNumberColor" | "codeTitleColor" | "codeVariableColor";

  export const COLOR_PREFS: { stateKey: ColorStateKey; lsKey: string; def: string }[] = [
    { stateKey: "accentColor",      lsKey: LS_ACCENT_COLOR,        def: DEFAULT_ACCENT_COLOR },
    { stateKey: "borderColor",      lsKey: LS_BORDER_COLOR,        def: DEFAULT_BORDER_COLOR },
    { stateKey: "textPrimaryColor", lsKey: LS_TEXT_PRIMARY_COLOR, def: DEFAULT_TEXT_PRIMARY_COLOR },
    { stateKey: "textMutedColor",   lsKey: LS_TEXT_MUTED_COLOR,   def: DEFAULT_TEXT_MUTED_COLOR },
    { stateKey: "codeTextColor",    lsKey: LS_CODE_TEXT_COLOR,    def: DEFAULT_CODE_TEXT_COLOR },
    { stateKey: "codeCommentColor", lsKey: LS_CODE_COMMENT_COLOR, def: DEFAULT_CODE_COMMENT_COLOR },
    { stateKey: "codeKeywordColor", lsKey: LS_CODE_KEYWORD_COLOR,  def: DEFAULT_CODE_KEYWORD_COLOR },
    { stateKey: "codeStringColor",  lsKey: LS_CODE_STRING_COLOR,  def: DEFAULT_CODE_STRING_COLOR },
    { stateKey: "codeNumberColor",  lsKey: LS_CODE_NUMBER_COLOR,  def: DEFAULT_CODE_NUMBER_COLOR },
    { stateKey: "codeTitleColor",   lsKey: LS_CODE_TITLE_COLOR,   def: DEFAULT_CODE_TITLE_COLOR },
    { stateKey: "codeVariableColor", lsKey: LS_CODE_VARIABLE_COLOR, def: DEFAULT_CODE_VARIABLE_COLOR },
  ];

  /** Write all 11 color CSS vars from a state snapshot (UI 4 + code 7). */
  export function applyColorPrefs<S extends Record<ColorStateKey, string>>(s: S): void {
    applyColors(s.accentColor, s.borderColor, s.textPrimaryColor, s.textMutedColor);
    applyCodeColors(s.codeTextColor, s.codeCommentColor, s.codeKeywordColor,
      s.codeStringColor, s.codeNumberColor, s.codeTitleColor, s.codeVariableColor);
  }

  /** Persist all 11 color prefs to localStorage. */
  export function writeColorPrefs<S extends Record<ColorStateKey, string>>(s: S): void {
    for (const p of COLOR_PREFS) writeLs(p.lsKey, s[p.stateKey]);
  }
  ```
- In `useAgentStore.ts`: replace the 4 `setUi*Color` bodies + 7 `setCode*Color` bodies with ONE `setColor(stateKey, c)`:
  ```ts
  setColor: (key: ColorStateKey, c: string) => {
    const p = COLOR_PREFS.find(x => x.stateKey === key)!;
    writeLs(p.lsKey, c);
    const next = { ...get(), [key]: c } as AppState;
    applyColorPrefs(next);
    set({ [key]: c } as Partial<AppState>);
  },
  ```
  The 11 named actions become one-line dispatchers: `setAccentColor: (c) => get().setColor("accentColor", c)` etc. (Keep all 11 in the `AppState` interface so the facade re-exports stay identical — 80 consumers unchanged.)
- `resetColors`: `const d = Object.fromEntries(COLOR_PREFS.map(p => [p.stateKey, p.def])) as Record<ColorStateKey,string>; writeColorPrefs(d); applyColorPrefs(d); set(d);`
- `resetAppearance`: `resetColors()` + theme/font reset (writeLs + applyTheme + applyFontVars + set).
- `[CHOICE: 11 rows]` — the plan's "× 14" is incorrect; 4 UI + 7 code = 11.

### E5 — see §4 (conditional GO).

---

## 3. Exact edit plan

**E1:**
1. Create `src/backlog.rs`: move `BacklogItem`/`BacklogStatus`/`BacklogStore`/`BacklogFile`/`default_next_id`/`now_secs` + tests from `src-tauri/src/ipc/backlog.rs`. Update doc-comment module header. Make `now_secs` `pub(crate)` or keep private (only used internally). Add `pub mod backlog;` to `src/lib.rs` (alphabetical, after `app` at :9).
2. Create `src/project/git_ops.rs`: move `git`/`checkpoint`/`commit_success`/`rollback`/`first_line` + tests from `run_all.rs:107-396`. Imports: `use std::path::Path; use std::process::Command; use crate::backlog::BacklogItem;`. Add `pub mod git_ops;` to `src/project/mod.rs:7` (after `agent_md`).
3. Delete `src-tauri/src/ipc/backlog.rs`; remove `pub mod backlog;` from `ipc/mod.rs:17`.
4. Update importers: `state.rs:29` → `use myharness::backlog::BacklogStore;`; `main.rs:102` → `myharness::backlog::BacklogStore::open(`; `run_all.rs:47` → `use myharness::backlog::{BacklogItem, BacklogStatus};` + swap `crate::ipc::run_all::{checkpoint,commit_success,rollback}` → `myharness::project::git_ops::{checkpoint,commit_success,rollback}` at :474,583,596,608; `backlog_cmds.rs` → `use myharness::backlog::{BacklogItem, BacklogStatus};`; `contract_fixtures.rs` → `use myharness::backlog::BacklogItem;`.
5. A9 fix: edit `run_all.rs:632` (log) + `run_all.rs:506-510` (mark item Failed).

**E2:**
1. Add `pub async fn reembed_if_needed(store: &Arc<dyn MemoryStoreTrait>, embedder: &Arc<dyn Embedder>)` to `src/memory/mod.rs` (or new `src/memory/reembed.rs` + mod decl). Import `Embedder` trait.
2. `main.rs:741-771` → guards + `tauri::async_runtime::spawn(async move { myharness::memory::reembed_if_needed(&store_for_reembed, &embedder_for_reembed).await; })`.
3. `rewire.rs:58-92` → guards + same spawn wrapper.
4. Add lib unit test (mock store/embedder).

**E3:**
1. Create `src-tauri/src/ipc/startup.rs` with `StartupSnapshot` + `startup_snapshot` command. Add `pub mod startup;` to `ipc/mod.rs`.
2. Register `ipc::startup::startup_snapshot` in `main.rs` invoke_handler.
3. `frontend/src/lib/tauri.ts`: add `StartupSnapshot` interface + `getStartupSnapshot()`.
4. `frontend/src/App.tsx:166-285`: replace listAgents/getContextCaps/getWorkflowState(active)/getEmbedderStatus with one `getStartupSnapshot()`; seed all workflow states + backlog.

**E4:**
1. `frontend/src/hooks/appearance.ts`: add `ColorStateKey`, `COLOR_PREFS` (11 rows), `applyColorPrefs`, `writeColorPrefs`.
2. `frontend/src/hooks/useAgentStore.ts:700-833`: add `setColor`; collapse 11 setters to dispatchers; rewrite `resetColors`/`resetAppearance` over the table. Keep `AppState` interface + facade re-exports identical.

**E5 (conditional — see §4):**
1. Migrate 4 frontend callers: `App.tsx:206`, `StatusBar.tsx:134`, `StatusBar.tsx:291`, `ProvidersSection.tsx:48` from `getConfig()` → `getSettings()` (read `.general.default_model`, `.endpoints`, `.pricing`). Consolidate App.tsx's getConfig+getSettings into one getSettings call.
2. Delete `get_config` command (`settings.rs:44-73`), `GetConfigResponse` (`:545-553`), `GetConfigGeneral` (`:556-564`), the `get_config_response_renders_legacy_shape` test (`:1459-1512`).
3. Remove `get_config` from `main.rs` invoke_handler.
4. Delete `getConfig()` wrapper in `tauri.ts:289-291` + `AppConfig` interface (`:279-287`) if no longer referenced.
5. Delete `frontend/src/lib/ipc-fixtures/dto-get-config.json` + its reference in `src-tauri/src/ipc/contract_fixtures.rs`.

---

## 4. E5 field-by-field comparison + GO/NO-GO

| Section | Field | get_config type/source | get_settings type/source | Match? |
|---|---|---|---|---|
| general | default_provider | `Option<String>` / on-disk `config.general.general.default_provider` | `Option<String>` / on-disk `config.general.general.default_provider` | ✅ identical |
| general | default_model | `Option<String>` / on-disk `config.general.general.default_model` | `Option<String>` / on-disk `config.general.general.default_model` | ✅ identical |
| general | safety | `SafetyMode` / **on-disk** `config.general.general.safety` | `SafetyMode` / **live runtime** `state.runtime.safety_mode` | ⚠️ same type+shape, **DIFFERENT SOURCE** |
| endpoints | (whole vec) | `Vec<EndpointWire>` via `endpoint_wire(e)` | `Vec<EndpointWire>` via `endpoint_wire(e)` | ✅ identical (same fn, same source) |
| pricing | (whole vec) | `Vec<PricingWire>` from `config.pricing` | `Vec<PricingWire>` from `config.pricing` | ✅ identical |

**Shape/type verdict:** get_settings is a strict superset in shape — every get_config field exists in get_settings with the same type. endpoints/pricing use the identical `endpoint_wire`/`PricingWire` types.

**The ONE semantic difference — `general.safety`:**
- get_config reads the **on-disk** safety (`config.general.general.safety`).
- get_settings reads the **live runtime** safety (`state.runtime.safety_mode`).
- These diverge when the user toggles safety via `set_safety_mode` (agent.rs:321-333), which updates the runtime lock but does **NOT** write `config.toml`.

**Why the migration is still safe (conditional GO):**
1. The ONLY frontend caller that reads `getConfig().general.safety` is `StatusBar.tsx:149` inside `resyncFromBackend()`.
2. `resyncFromBackend` is gated on `configVersion` (StatusBar.tsx:303-307), which bumps ONLY on a config save (`save_endpoints`/`save_settings`). `set_safety_mode` does NOT bump `configVersion`, so `resyncFromBackend` is NOT called after a runtime-only safety toggle.
3. After any `save_settings` that patches safety, disk and runtime are synced (save_settings writes config.toml AND updates the runtime lock, settings.rs:1255-1258). So when `resyncFromBackend` fires, on-disk == live-runtime — the source difference is unobservable.
4. At startup, the frontend already reads the LIVE safety via `getSafetyMode()` (App.tsx:270) — so the live value is already the source of truth for the initial label. Migrating `resyncFromBackend` to live safety is consistent with that.
5. `App.tsx:206` getConfig does NOT read safety at all (only default_model/endpoints/pricing).

**GO/NO-GO: CONDITIONAL GO — proceed with deletion.**
- The shape/type is a clean strict superset (the user's stated criterion: "missing or differently-shaped/typed" — none are).
- The safety source difference is real but **not observable** in the only caller that reads it (resyncFromBackend fires only post-save when disk==runtime), and the startup path already prefers live safety.
- **Migration requirement (must follow exactly):** `StatusBar.resyncFromBackend` switches to `getSettings()`; its safety read now returns live-runtime (consistent with `getSafetyMode`). Do NOT add a separate on-disk safety read — the live value is correct here. Add a code comment at the migrated call site noting "safety is the live runtime value (matches getSafetyMode); resync only fires post-save when disk==runtime, so no on-disk fallback is needed."
- If, during implementation, ANY caller is found that genuinely needs the on-disk safety specifically (none found in this audit), STOP and flag — do not delete get_config.

`[CHOICE: confirm deletion if clean strict superset]` — confirmed: shape/type is a clean strict superset; the safety source difference is non-observable in practice. **Delete get_config.**

---

## 5. Test plan

**Rust (root `cargo test` + `src-tauri/ cargo test` + `src-tauri/ cargo build`):**
- E1: moved BacklogStore tests pass in lib (10 tests); moved git_ops tests pass (4 tests + TestRepo). `cargo build` in src-tauri confirms no dead code / missing imports. Verify `contract_fixtures.rs` still compiles (BacklogItem import updated).
- E1 A9: add/extend a test if feasible — `run_all_dispatch_next` no-main-agent path marks the item `Failed` (may require a harness; if the existing run_all tests can't construct an IpcState without a main agent, document the manual verification). At minimum, the `let _ =` → `if let Err` change is covered by existing dispatch tests.
- E2: new lib unit test `reembed_if_needed` — mock `MemoryStoreTrait` (returns mismatched fingerprints) + mock `Embedder` → assert `reembed_all` invoked; matching fingerprints → not invoked.
- E3: `startup_snapshot` command — if testable without a full Tauri AppHandle, add a unit test asserting the struct serializes with the 5 fields + `workflow_states` renders lowercase. Otherwise rely on the frontend vitest + manual.
- E5: `get_config_response_renders_legacy_shape` test removed; remaining `get_settings_response_renders_*` tests still pass. `contract_fixtures.rs` compiles without the get_config fixture.

**Frontend (`npx vitest run` + `npm run build`):**
- E3: add/extend App.tsx startup test — `getStartupSnapshot` seeds all agents' workflow states (not just active). Verify the store's `workflowStates` has an entry per agent after the snapshot.
- E4: add vitest for `COLOR_PREFS` — `applyColorPrefs` writes all 11 CSS vars; `writeColorPrefs` writes all 11 LS keys; `resetColors` restores all 11 defaults; `setColor("accentColor", "#fff")` updates only accent + reapplies. Verify the 11 named setters still exist + dispatch (facade unchanged → existing tests pass).
- E5: `npm run build` (tsc) confirms no `getConfig` references remain; `ProvidersSection`/`StatusBar`/`App` typecheck against `AppSettings`.

**Manual smoke (post-merge build):**
- Startup: agents list, ctx bars show "0/max", ALL agent tabs show correct workflow state, backlog populated, embedder banner appears if not ready.
- Toggle safety via StatusBar → label updates; save Settings → resync keeps label correct.
- Settings → Appearance: change a UI + a code color → applies live; Reset all appearance → all 11 colors + theme/font restore.
- Settings → Endpoints tab loads (ProvidersSection via getSettings).

---

## 6. Risks

1. **E2 runtime coupling** — if the lib has no tokio runtime handle, `tauri::async_runtime::spawn` can't live in the lib. Mitigation: the recommended `reembed_if_needed` is an `async fn` (no spawn) — callers keep their `tauri::async_runtime::spawn` wrapper. Verify `src/Cargo.toml` does not need a new dep.
2. **E1 circular dep** — `git_ops` (in `myharness::project`) depends on `myharness::backlog::BacklogItem`. Both are lib modules → intra-lib, no cycle. ✓.
3. **E3 lock ordering** — `startup_snapshot` reads `agent_loops` (for agents/caps/workflow_states) and `backlog.store` and `embedder_status`. Acquire in a consistent order (agent_loops → backlog → embedder_status) and never nest backlog inside agent_loops. The `list_agents` pattern (agent.rs:358-389) already snapshots under manager lock then drops before agent_loops — mirror it.
4. **E3 WorkflowState serialization** — `Vec<(AgentId, WorkflowState)>` serializes as `[[1,"planning"],...]`. The frontend `workflowStates` is `Record<number, string>`. The App.tsx seed loop must map the array → record. Verify the TS `WorkflowState` type accepts the lowercase string.
5. **E4 facade breakage** — 80 files import from `useAgentStore`. The 11 named setters MUST remain in the `AppState` interface + the store impl (as dispatchers). If any consumer imports `setCodeColor` directly (the internal helper), it breaks — verify only the 11 public names are imported. `setCodeColor` (appearance.ts:239) is currently exported; if `setColor` replaces its role, keep `setCodeColor` or update the one internal caller.
6. **E5 safety source** — covered in §4. The one semantic difference is non-observable in the only caller. If a future caller needs on-disk safety, get_settings won't provide it — document this in the `get_settings` doc comment.
7. **E5 fixture/test deletion** — removing `dto-get-config.json` + its `contract_fixtures.rs` reference must not leave a dangling `include!`/path. Grep `contract_fixtures.rs` for `get-config` and remove the block.
8. **deny(warnings)** — every moved public item needs a doc comment; the deleted `get_config` removes its doc too. No `#[allow]`. The `now_secs` helper in backlog must stay used (it is, by `add`).
