# Review: Settings → Memory maintenance (cleanup + search rebuild)

**Branch:** `feat/memory-maintenance` · **Scope:** ALL uncommitted changes (`git diff HEAD` + 5 untracked source files + plan bookkeeping) · **Date:** 2026-08-18

## Summary

The feature is implemented as planned and is functionally correct in every area I could trace end-to-end. No blocking findings. One small real bug in the frontend subscription lifecycle (listener leak on a fast unmount) plus a handful of low-severity robustness/UX notes. Wire shapes, lock discipline, busy-guard lifecycle, and the store-layer additions are all verified clean, and test coverage pins the important contracts on both sides of the IPC boundary.

## Verified clean (with evidence)

**1. Busy-guard lifecycle — no permanent `BUSY=true` wedge.**
`start_op` (src-tauri/src/ipc/memory_maintenance.rs:132-146) resolves the store *before* claiming; after a successful `compare_exchange` (memory_maintenance.rs:50-61) every remaining step is infallible (construct `BusyGuard`, best-effort `emit`, return), so a claim always produces a guard. The guard is moved into the spawned task (`let _guard = guard;` :206, :286); `Drop` stores `false` with `Release` (:43-47), so normal completion, `Err` return, and panic-unwind all clear it. `tauri::async_runtime::spawn` returns a `JoinHandle` (no `Result`), so there is no spawn-failure leak path. Double invocation is rejected at the claim and surfaces as a command error → `startError` in the UI.

**2. Lock discipline — invariant respected.**
In `memory_cleanup` (:160-201): `agent_loops.lock().await` is held in a scoped block while calling `session_id()` (src/agent/loop_impl.rs:692-698, an internal std `Mutex`, no await) then dropped; `project.config.lock().await` is held in a scoped block and `build_openai_client` is a **sync** fn (src/provider/client_factory.rs:73-91 — constructs a client, no I/O), so no await under the lock; `project.root.lock().await.plans_dir.clone()` is a statement-scoped snapshot (`Project::plans_dir` exists, src/project/mod.rs:34). Never two of {agent_loops, config, root} held simultaneously; `manager` is never taken, so the state.rs manager→agent_loops ordering rule (src-tauri/src/ipc/state.rs:38-43) is trivially satisfied.

**3. Live-session skip list + engine grouping.**
`AgentLoop::session_id()` is read for every live loop and skipped in the engine (`filter(|s| !skip_sessions.contains(s))`, src/memory/maintenance.rs:89). Session grouping derives from the working rows' `source_session_ids` (`record_tool_event` tags exactly one session — src/memory/mod.rs:1191-1193), and `delete_working_for_session` (mod.rs:1030-1051) deletes whole rows whose JSON array contains the session, matching the count in `working_rows_removed` (each row counted once — maintenance.rs:99-102). The multi-session-tag case is correctly reasoned about and documented (maintenance.rs:73-75, 97-98); it cannot occur via `record_tool_event` today. Pinned by `cleanup_consolidates_stale_sessions_and_skips_live_ones` (the live session's row survives, exactly 2 episodic rows created).

**4. `reembed_all` signature change.**
Single impl (`impl MemoryStoreTrait for MemoryStore`, mod.rs:707 — no mocks/other impls exist) and both callers updated (mod.rs:73, src-tauri/src/ipc/embeddings.rs:233, both `None`). Progress fires after each row persist (`report(count, total)`, mod.rs:1117-1119), is monotonic by construction, and is pinned by `reembed_all_progress_is_monotonic_and_ends_complete` (mod.rs:1727+). The `Option<&(dyn Fn…)>` borrow is scoped within one call frame on both producer and consumer sides (`embed_tick` closure in maintenance.rs:134-136) — compiler-checked sound. (Plan step 1 said `Option<Arc<dyn Fn…>>`; the implemented borrowed form is strictly better — no allocation, lifetime-enforced. Not a finding.)

**5. `vacuum()` / `rebuild_fts()`.**
Both clone the `Arc<Mutex<Connection>>` and lock **inside** `spawn_blocking` (mod.rs:325-352) — std-mutex discipline correct, never held across an await; writers serialize on the same lock; WAL read connection unaffected (and in-memory stores have no second conn). `rebuild_fts` no-ops when `schema::fts_available` is false and uses the exact `'rebuild'` statement proven by the one-time migration (src/memory/schema.rs:145). `execute_batch` runs in autocommit, so VACUUM outside a transaction is valid. Pinned by `vacuum_and_fts_rebuild_succeed_on_a_fresh_store`.

**6. Wire-shape consistency — backend ↔ TS ↔ reducer ↔ render.**
Backend serde (tag `"type"`, kebab-case) produces exactly `{"type":"started"|"progress"|"done"|"failed", op:"cleanup"|"rebuild", …}`, pinned by `maintenance_event_wire_shape` (memory_maintenance.rs:334-379). The TS union (frontend/src/lib/tauri.ts:590-602) matches field-for-field; `applyMaintenanceEvent` is an exhaustive switch over the union; `MemorySection.renderOp` treats `total === 0` as indeterminate (full-width pulse bar, empty pct label) which is precisely the contract the backend emits for `reindexing`/`vacuuming` `(0,0)` ticks, and the reducer tests pin it (maintenanceUi.test.ts:28-39). Phase labels (`phase_label`) match the documented strings; `every_phase_has_a_label` guards empties. `UnlistenFn` is already imported (tauri.ts:4).

**7. Provider + corpus wiring mirrors existing call sites faithfully.**
Endpoint/model resolution (memory_maintenance.rs:172-198) is line-for-line the `main.rs` startup logic (src-tauri/src/main.rs:773-798) minus the dummy-provider fallback — intentional and documented; `build_openai_client` args identical to settings.rs:199-206. The reviews-dir derivation (`plans_dir.parent().join("reviews")` with `.coding/reviews` fallback, :210-215) is byte-identical to both other call sites (src/runtime/agent.rs:516-519, src/tool/memory/mod.rs:289-292), and the digest runs on the blocking pool with join-error degradation to an empty corpus — matching the reviewed agent.rs pattern.

**8. Frontend section integration.**
`active={open}` matches every neighboring section (SettingsDialog.tsx:300-354); nav entry, `SettingsSectionId`, `isSettingsSectionId`, icon, ref, `sectionRefs` all present. Section never calls `onDirtyChange(true)`; `save: async () => true`. Overview header fields (`counts.*`, `model_id`, `dim`, `fingerprint_matches`) match the `MemoryDebugOverview` TS type (tauri.ts:529-548). Remount-mid-op self-heals: any later tick flips `running` on, and a terminal `done`/`failed` lands correctly even from idle state.

**9. Constitution compliance.**
Doc comments on **all** new public items including enum variants and struct fields (maintenance.rs, memory_maintenance.rs, mod.rs additions, tauri.ts wrappers, maintenanceUi.ts, MemorySection); trait doc updated for the new parameter. No `#[allow(...)]` anywhere in the diff. New-feature tests present on both sides (4 lib engine tests + monotonic reembed test + 2 IPC wire tests + 6 vitest reducer tests); no defect fixes in this diff that would demand additional regression tests. The CRLF warnings from `git diff` are the repo's standard normalization notice on every touched file — no mixed-ending damage introduced. The `.coding/plans/stack.json` + old-plan checkbox edits are expected workflow bookkeeping.

## Findings

### Bugs

**B1 (low-medium) — Listener leak when MemorySection unmounts before `listen` resolves.**
*Where:* frontend/src/components/settings/sections/MemorySection.tsx:75-87.
The cleanup closure captures `unlisten` (null until the `onMaintenanceEvent(...)` promise resolves). If the dialog closes within that window — close/reopen cycles make this repeatable, and React StrictMode double-mounts in dev — the `.then` runs *after* the cleanup already fired with `unlisten === null`, so the listener is never removed: it leaks per occurrence and keeps invoking `setCleanup`/`setRebuild` on the dead component. Fix is the standard disposed-flag pattern:
```ts
let disposed = false; let unlisten: (() => void) | null = null;
onMaintenanceEvent(handler).then((fn) => { if (disposed) fn(); else unlisten = fn; });
return () => { disposed = true; unlisten?.(); };
```
Add a vitest regression test if feasible (mock `onMaintenanceEvent` with a controllable promise; unmount before resolve; assert the unlisten fn was called).

**B2 (low) — Panic inside the maintenance task wedges the UI's buttons with no terminal event.**
*Where:* src-tauri/src/ipc/memory_maintenance.rs:205-270 / :285-323.
`BusyGuard` correctly frees `BUSY` on panic, but no `Failed` event is emitted on unwind, so the frontend stays `running: true` → `busy` → **both buttons disabled** until the Settings dialog is closed and reopened (remount resets to idle; the backend accepts a new op). Plausible panic sources are the `.expect("conn lock poisoned")` sites downstream in `MemoryStore`. Cheap hardening: wrap the engine call in `std::panic::AssertUnwindSafe(fut).catch_unwind()` and emit `Failed { error: "maintenance task panicked" }` on `Err`, so the UI always receives a terminal event. (An LLM/provider hang has the same UI symptom, but that mirrors existing consolidation behavior and is out of scope here.)

**B3 (low) — Skip-list TOCTOU: a session started mid-cleanup can be consolidated prematurely.**
*Where:* src-tauri/src/ipc/memory_maintenance.rs:160-168 (skip snapshot) vs src/memory/maintenance.rs:85 (working snapshot, taken later inside the spawned task, after the corpus `spawn_blocking`).
An agent spawned in the window between the preamble and the task's `list_by_tier` has rows in the snapshot but its session is absent from `skip_sessions` — and `consolidate_session` re-lists rows live (consolidation.rs:110-114), so it sweeps that session's *current* rows (not just snapshot rows) into an early episodic summary and deletes them mid-session. No data loss (the signal is preserved in the episodic row) but the live session's working memory is drained early and its session-end consolidation produces a second, partial episodic row. Window is small (spawn + ≤20 file reads), so low likelihood. Suggested fix: clone the `Arc` agent-loops map into the task and re-snapshot `skip_sessions` immediately before calling `maintenance::cleanup`.

### Correctness / design notes (no action required to ship)

**C1 (low) — Session-less working rows are unreachable by cleanup.**
`record_tool_event(None, …)` leaves `source_session_ids` empty (mod.rs:1191-1193); such rows belong to no session, so `cleanup` never consolidates or deletes them and they aren't counted. Production paths thread a real session id (turn.rs:54, :1258), so this is a pre-existing edge (rows written before a session starts), not a regression — but it means "Clean up memory" can report success while `working` count stays > 0. Worth a doc-comment sentence (the maintenance.rs doc already covers the one-session-per-row invariant; extend it to note untagged rows are left as-is) or a future enhancement.

**C2 (nit) — `startError` is not cleared by lifecycle events.**
After a spurious double-click rejection ("already running"), the red error line persists next to the running bar until the next `start()` call. Clearing `startError` when a `started` event arrives (in the subscription handler) would tidy this. Similarly, the overview reload effect keys on `cleanup.summary`/`rebuild.summary` only (MemorySection.tsx:68-70), so counts don't refresh after a *failed* op — reactivating the section covers it.

### Security

Clean. No new attack surface: both commands take no parameters; `plans_dir`/reviews derivation comes from trusted app state (no user input reaches a path); the emit channel is app-scoped; the provider build under the config lock is the established shared-construction path (key resolution stays inside `client_factory`); progress events carry no sensitive data.

### Constitution compliance

Clean — see "Verified clean" §9. All public items documented; no `#[allow]`; tests added per feature; line-ending warnings are repo-standard.

### Performance notes (optional, low)

- `reembed_all` emits one IPC event **per row** (mod.rs:1117). A multi-thousand-memory store will push thousands of events/setState through the webview. Consider throttling in the IPC `report_progress` closure (forward only when `done % N == 0`, `done == total`, or ≥100 ms since the last emit).
- `sessions.contains(s)` in maintenance.rs:89/:101 is an O(sessions) `Vec` scan per row — fine at current scale; a `HashSet` would future-proof a large backlog.

## Test-status note

I could not execute the test suites (read-only reviewer). The claimed results — lib 1067 passed, app crate 69 passed, tsc clean, vitest 234 passed — are consistent with the code (the diff compiles against a single trait impl and two call sites; the new tests reference only APIs verified above). The main agent should re-run `cargo test` + frontend tests after fixing B1/B2/B3.
