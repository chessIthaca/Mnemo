# Batch E Review — moves + startup snapshot + frontend table

**Branch:** `feat/deep-review-e-moves`
**Scope:** E1 (pure moves: backlog → `myharness::backlog`, git_ops → `myharness::project::git_ops`), E2 (dedup `reembed_if_needed`), E3 (`startup_snapshot` IPC + App.tsx consolidation), E4 (COLOR_PREFS table), E5 (delete `get_config`).
**Diff:** 21 files, +258 / −1297 (net −1039 incl. deleted backlog.rs; source net ≈ −170 as planned).

## Summary

The moves (E1) are **pure and faithful** — `src/backlog.rs` is byte-identical to the deleted `ipc/backlog.rs` (all tests traveled), and `src/project/git_ops.rs` is identical to the deleted run_all.rs git ops except the one documented `tauri::async_runtime::spawn_blocking` → `tokio::task::spawn_blocking` swap (correct — the lib crate doesn't depend on tauri, and `BundledEmbedder::embed` already uses `tokio::task::spawn_blocking`, so no new dependency). The reembed dedup (E2) preserves the exact predicate, logging, and error handling. The COLOR_PREFS table (E4) produces identical CSS-var writes (verified field-by-field). Deleting `get_config` (E5) exposes no secrets (`get_settings` uses the same `endpoint_wire`/`PricingWire`, never serializes the KeyStore).

However, there are **6 findings** — one lock-ordering violation + spec deviation, one duplicate command registration, one missed plan-required fix (A9), and three smaller items.

---

## MEDIUM

### M1 — `startup_snapshot` nests `agent_loops` → `manager` (lock-ordering violation + spec deviation)
**Where:** `src-tauri/src/ipc/startup.rs:50-54`

```rust
let agent_loops = state.runtime.agent_loops.lock().await;   // :50
let agents: Vec<AgentInfo> = {
    let mgr = state.runtime.manager.lock().await;            // :54  ← nested INSIDE agent_loops
    ...
};
```

`state.rs:38-43` documents the invariant explicitly: *"always acquire `manager` first, then `agent_loops`… Never acquire `agent_loops` then `manager` — that ordering is not used anywhere today and would risk a deadlock if a future command did so while another task held `manager` then waited on `agent_loops`."* The snapshot introduces exactly the forbidden ordering.

The E3 spec (`.coding/reviews/2026-04-18-batch-e-spec.md:418`) explicitly instructed: *"The `list_agents` pattern (agent.rs:358-389) already snapshots under manager lock then drops before agent_loops — mirror it."* The implementation does NOT mirror it — it holds `agent_loops` across the `manager.lock().await`.

**Deadlock today?** No — I audited every `manager.lock().await` / `agent_loops.lock().await` site. No production code nests `manager`→`agent_loops` (all drop manager before acquiring agent_loops: `list_agents` agent.rs:333-347, `spawn_agent` spawn.rs:165-202, `dispatch_item` backlog_cmds.rs:249-264, `main_agent_workflow_state` run_all.rs:97-101). So there is no reverse-nesting partner to deadlock with today. **But** it is a latent deadlock: any future contributor who writes the *documented preferred* `manager`→`agent_loops` ordering would deadlock with this command. It also needlessly extends the `agent_loops` hold across manager-lock contention, blocking `spawn`/`events`/`get_workflow_state`/`set_model`.

**Fix:** mirror `list_agents` — snapshot the handle list `(id, name, running, parent_id)` under `manager` (drop), then read models + context_caps + workflow_states under `agent_loops`. The per-agent `workflow.lock().await` nesting inside `agent_loops` (lines 79-83) is fine — it matches the existing `get_workflow_state` (agent.rs:498-503) ordering.

### M2 — Duplicate `get_settings` registration in `invoke_handler`
**Where:** `src-tauri/src/main.rs:400-401`

The E5 migration replaced `ipc::settings::get_config,` with `ipc::settings::get_settings,`, but `get_settings` was **already** registered on the next line. The diff:
```
-            ipc::settings::get_config,
+            ipc::settings::get_settings,
             ipc::settings::get_settings,
```
There are now two consecutive `ipc::settings::get_settings,` entries. This is an unintended edit artifact (the `get_config` line should simply have been deleted, not replaced). Tauri's `generate_handler!` accepts the duplicate (the second shadows the first at runtime), so it is not a build failure, but it is redundant dead code and confusing. **Fix:** delete one of the two `ipc::settings::get_settings,` lines.

### M3 — A9 swallowed-dispatch-error fix was NOT implemented (E1 scope required it)
**Where:** `src-tauri/src/ipc/run_all.rs:370` and `:390`

The parent plan (e342c141 step 5) explicitly scoped E1 as: *"fix A9 while there (run_all.rs:632 swallowed dispatch error → log + mark item)"*. The E1 spec (`.coding/reviews/2026-04-18-batch-e-spec.md:173-179`) specified replacing `let _ = run_all_dispatch_next(app, &state).await;` with `if let Err(e) = … { eprintln!(…); }` and marking the item `Failed` on the no-main-agent path.

Both swallow sites are still `let _ =`:
```rust
:370   let _ = run_all_dispatch_next(app, &state).await;
:390   let _ = dispatch_next_impl(app, &state).await;
```
The A9 fix was dropped. The pre-existing bug (a dispatch failure leaves the run cleared and the item stuck `InFlight` with no note) remains. **Fix:** apply the spec's `if let Err(e)` logging at both sites (and the no-main-agent `Failed` marking if a harness permits).

---

## LOW

### L1 — `setCodeColor` is now dead code with a misleading doc comment
**Where:** `frontend/src/hooks/appearance.ts:310-331` (and re-exported via the useAgentStore facade `useAgentStore.ts:163`)

After E4, all 7 `setCode*Color` setters dispatch through `setColor` (the new table-driven helper), not `setCodeColor`. A search for `setCodeColor(` (call sites) returns zero matches in `.ts`/`.tsx` — only the definition, the facade re-export, and comments. So `setCodeColor` is exported but uncalled.

The `setColor` doc comment (appearance.ts:337-338) is also self-contradictory: *"Replaces the separate `setCodeColor` path for the code colors (kept for back-compat — `setColor` delegates to the table)."* — `setColor` does not delegate to `setCodeColor`; they are independent implementations, and `setCodeColor` has no callers to be "back-compat" for.

The E4 spec (batch-e-spec.md:420) anticipated this: *"if `setColor` replaces its role, keep `setCodeColor` or update the one internal caller."* There is no internal caller. **Fix:** either delete `setCodeColor` + its facade re-export (preferred — constitution forbids dead code), or fix the doc comment to stop claiming `setColor` delegates to it.

### L2 — `startup_snapshot` returns `backlog` but App.tsx discards it (spec required seeding)
**Where:** `frontend/src/App.tsx:161-182` (snapshot consumption), `src-tauri/src/ipc/startup.rs:33` (field), `frontend/src/lib/tauri.ts:218` (TS field)

The E3 spec (batch-e-spec.md:269) explicitly required: *"seed backlog into the store"* from `snap.backlog`. The snapshot serializes `backlog: Vec<BacklogItem>` and the TS interface types it, but App.tsx never calls `setBacklog(snap.backlog)` — the field is fetched, serialized, and discarded. The backlog still loads via the separate `backlog_changed` event / `backlogList()` path (useAgentEvents.ts:302), so this is **not a functional regression** (pre-E3 startup also didn't seed backlog), but it is a spec deviation and wasted payload (backlog items can carry large base64 image data URLs). **Fix:** either `useAgentStore.getState().setBacklog(snap.backlog)` after the snapshot (per spec), or drop `backlog` from the snapshot if eager seeding is unwanted.

### L3 — `rewire.rs` reembed log line gained a `; stored fingerprints: {fps:?}` suffix
**Where:** `src/memory/mod.rs:65-68` (the unified `reembed_if_needed`)

The original `rewire.rs` reembed path logged `"info: re-embedding memories for model '{expected_model}' (dim {expected_dim})"` (no fingerprint dump). The original `main.rs` path logged the same line **with** `; stored fingerprints: {fps:?}`. The unified `reembed_if_needed` always includes the fingerprint suffix, so the rewire (post-config-save) path now logs more verbosely than before. This is harmless (arguably an improvement — more diagnostics), but it is a behavior change in the dedup, not a byte-for-byte preservation. No action required; noted for completeness.

---

## Verified clean (no findings)

- **E1 moves are pure.** `src/backlog.rs` is byte-identical to the deleted `ipc/backlog.rs` (structs, `BacklogFile`, `default_next_id`, `now_secs`, all 9 tests). `src/project/git_ops.rs` is identical to the deleted run_all.rs git ops (`git`/`checkpoint`/`commit_success`/`rollback`/`first_line` + `TestRepo` + all 5 tests) except the documented `spawn_blocking` swap. All importers updated to `myharness::backlog::` / `myharness::project::git_ops::`. `pub mod backlog;` removed from `ipc/mod.rs`, added to `src/lib.rs`; `pub mod git_ops;` added to `src/project/mod.rs`. Unused imports correctly cleaned (Path/PathBuf/Command/BacklogItem from run_all.rs; HashMap from state.rs).
- **`tokio::task::spawn_blocking` ≡ `tauri::async_runtime::spawn_blocking` here.** Tauri v2's async runtime is tokio; `tauri::async_runtime::spawn_blocking` delegates to `tokio::task::spawn_blocking`. Same blocking pool, same `JoinHandle` semantics. The lib crate already uses `tokio::task::spawn_blocking` (`BundledEmbedder::embed`, embedder.rs:206), so no new dependency.
- **E2 reembed dedup is faithful.** The predicate (`fps.len() != 1 || fps[0].0 != expected_model || fps[0].1 != expected_dim`), the `stored_model_fingerprints` error handling, the `reembed_all` call + result logging, and the "do not overwrite status" semantics are all preserved verbatim. `embedder.clone()` (Arc clone) is equivalent to the original by-value move. Both call sites still wrap in their existing `tauri::async_runtime::spawn` + their own Ready/hash-opt-out/pending-download guards. `MemoryStoreTrait` import correctly removed from main.rs + rewire.rs (the trait is in scope in `memory/mod.rs` where `reembed_if_needed` is defined).
- **E3 snapshot aggregation is correct** (modulo M1). `agents` mirrors `list_agents`, `context_caps` mirrors `context_caps`, `workflow_states` returns every agent's `WorkflowState` (closes the stale-non-active gap), `backlog` mirrors `backlog_list`, `embedder_status` mirrors `get_embedder_status`. `WorkflowState` serializes lowercase (`#[serde(rename_all = "lowercase")]`) matching the frontend's `WorkflowState` union; the `as WorkflowState` cast is valid. `agent_loops` is dropped (line 84) before the backlog lock (line 87) — no backlog-inside-agent_loops nesting. `startup_snapshot` is registered in `invoke_handler` (main.rs:394).
- **E4 COLOR_PREFS produces identical CSS-var writes.** `setColor` for a code color writes the same 7 code CSS vars as the old `setCodeColor` (via `applyColorPrefs` → `applyCodeColors`) plus redundant-but-harmless UI-var re-writes (unchanged values). `setColor` for a UI color writes the 4 UI vars (via `applyColors`) plus the 7 code vars (unchanged). `resetColors`/`resetAppearance` write the same 11 LS keys + apply + set via `defaultColorPrefs()`/`writeColorPrefs`/`applyColorPrefs`. The `AppState` interface + facade re-exports are unchanged (80 consumers unaffected). The 11 rows (4 UI + 7 code) match the spec's correction (not 14).
- **E5 `get_config` deletion is safe.** `get_settings` is a strict superset: `general.default_provider`/`default_model` (same `Option<String>`), `endpoints` (same `endpoint_wire` type + source), `pricing` (same `PricingWire`). The one semantic difference (`safety`: get_config=on-disk, get_settings=live runtime) is non-observable in the only caller that reads it (StatusBar.resyncFromBackend fires only post-save when disk==runtime); the startup path already prefers live safety via `getSafetyMode()`. All 4 frontend callers migrated (App.tsx, StatusBar ×2, ProvidersSection). `GetConfigResponse`/`GetConfigGeneral` structs, the `get_config_response_renders_legacy_shape` test, the `dto-get-config.json` fixture + its contract test, and the `getConfig()`/`AppConfig` TS wrapper are all deleted with no dangling references (search confirms zero remaining `GetConfigResponse`/`GetConfigGeneral`/`get_config`/`AppConfig`/`getConfig` in source). `contract_fixtures.rs` import cleanup is correct (`EndpointWire`/`PricingWire` no longer referenced — the get_settings fixture uses empty vecs).
- **Security.** Deleting `get_config` exposes no secrets — `get_settings` never serializes the KeyStore (same `endpoint_wire`). The snapshot leaks nothing new — every field mirrors an existing command's payload returned to the same webview. COLOR_PREFS doesn't touch the facade surface.
- **Constitution.** Doc comments present on all new pub items (`reembed_if_needed`, `StartupSnapshot` + fields, `startup_snapshot`, `ColorPref`/`ColorStateKey`/`COLOR_PREFS`/`applyColorPrefs`/`writeColorPrefs`/`defaultColorPrefs`/`setColor`, moved `BacklogStore`/`BacklogItem`/`BacklogStatus`/`checkpoint`/`commit_success`/`rollback`). No `#[allow]`. No new dead code in Rust (the `now_secs` helper in backlog is still used by `add`; `first_line` still used by the commit fns). Line endings: only the plan `.md` triggered a CRLF warning (not source).

---

## Tests note

- The moved tests (backlog: 9 tests, git_ops: 5 tests + `first_line`) **do** exercise the moved code via the new `tokio::task::spawn_blocking` path — they verify the move + spawn_blocking swap work. ✓
- `reembed_if_needed` has **no test** (the original inline logic was also untested; the function needs a `MemoryStore` + `Embedder` mock, so this is a pre-existing coverage gap, not a regression).
- `startup_snapshot` has **no test** (needs a full `IpcState` harness). The lock-ordering issue (M1) would not be caught by a unit test anyway.
- No test verifies the COLOR_PREFS table produces identical CSS vars, but the existing `useAgentStore.test.ts` setter tests (if any touch color setters) would catch a gross regression.

---

## Verdict

E1 (moves), E2 (reembed dedup), E4 (COLOR_PREFS), and E5 (delete get_config) are correct and behavior-preserving. E3 (startup_snapshot) is functionally correct but has a lock-ordering violation (M1) that should be fixed before merge. M2 (duplicate `get_settings`) and M3 (missed A9 fix) are clear bugs/omissions to address. L1–L3 are cleanups. Recommend fixing M1–M3 + L1 before commit; L2–L3 can be folded in or deferred.
