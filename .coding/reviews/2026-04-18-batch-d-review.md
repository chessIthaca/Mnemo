# Batch D Review — mechanical size reduction (D2 + D3)

**Branch:** `feat/deep-review-d-mechanical`
**Scope reviewed:** ALL uncommitted changes (`git diff HEAD`): `src-tauri/src/ipc/{agent,config_io,events,run_all,settings,spawn,state}.rs`, `src/provider/openai.rs`, plus `.coding/` bookkeeping (backlog.json, plan md).
**D1 (−400 LOC test-fixture builder) is DEFERRED** with written justification — out of scope for this review.

## Verdict

**Clean to merge.** All refactors are behavior-identical to the inline originals on every code path that matters. One LOW informational finding (a panic→error normalization in a poison-only path) and one estimation note (LOC delivered ≪ projected). No correctness bugs, no dead code, no `#[allow]`, no safety weakening, no lock-across-await, no dangling references. Doc comments present on all new `pub`/`pub(crate)` items; imports cleaned correctly.

---

## Correctness — verified behavior-identical

### `send_cmd` helper (agent.rs:23-37) — ✓ identical to the 6 originals
- **Same lock:** `state.runtime.manager.lock().await` (tokio Mutex), same as every original.
- **Same call:** `manager.send(agent_id, cmd)` — `send` is `pub fn send(&self, …) -> Result<(), AgentCommand>` (runtime/mod.rs:58), synchronous. No `.await` while the guard is held; guard drops at function return. Identical hold scope to the originals.
- **Same error mapping:** `.map_err(|e| format!("failed to {label}: {e:?}")).map_err(IpcError::from)`. With the passed labels (`"send prompt"`, `"send suggestion"`, `"interrupt"`, `"cancel"`, `"compact"`, `"clear conversation"`) the produced strings are byte-identical to each original's literal. `e: AgentCommand` formats via `{:?}` identically.
- All 6 call sites (send_prompt:208, send_suggestion:218, interrupt:227, cancel:236, compact:249, clear_conversation:260) pass `&state` (`&State<'_, IpcState>`) — matches the helper's `state: &State<'_, IpcState>` param. ✓

### `state.set_safety(mode)` (state.rs:188-199) — ✓ exact `map_err?` + `re_evaluate` preserved
- `*self.runtime.safety_mode.write().map_err(|e| format!("safety_mode lock poisoned: {e}"))? = mode;` — byte-identical to the removed `config_io::set_runtime_safety` (which already used `map_err?`, addressing the Batch B LOW finding). `safety_mode` is a std `RwLock` → `write()` is sync, no await.
- `let resolved = self.approvals.re_evaluate(mode, &self.project.sandbox);` — matches `re_evaluate(&self, mode: SafetyMode, sandbox: &Sandbox) -> usize` (approval.rs:124). Same args, same order.
- `eprintln!` on `resolved > 0` preserved. Returns `Ok(resolved)`.
- Both callers (`set_safety_mode` agent.rs:306, `save_settings` settings.rs:1201) drop the old `let resolved = …; if resolved > 0 { eprintln!(…) }` tail — but note `save_settings` **keeps** its own `if resolved > 0 { eprintln!("safety: settings save auto-resolved …") }` (settings.rs:1202-1205) with a *different* message ("settings save" vs "mode change"). This is intentional and matches the original two-site behavior (each site had its own log line). ✓ No drift.

### `end_run(app, state)` (run_all.rs:52-58) — ✓ exact clear+emit ordering
- Body: `*state.backlog.run_all.lock().await = None;` then `emit_backlog_changed(app, state).await;`. The lock guard is dropped at the end of the assignment statement (no binding held across the emit await) — identical to all 5 originals. `emit_backlog_changed` signature (`pub async fn emit_backlog_changed(app: &tauri::AppHandle, state: &IpcState)`, backlog_cmds.rs:62) matches.
- 5 sites rewired: `run_all_dispatch_next` ×3 (:508, :531, :551), `on_main_turn_resolved` (:669), `halt_run_all_for_approval` (:747). Each preserves the original `&state`/`state` borrowing form.

### `AgentLoopMap` alias (state.rs:68-69) — ✓ normalizes correctly
- Alias: `Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>`.
- Replaced spellings: state.rs `Arc<Mutex<HashMap<…>>>` (unqualified `Mutex`/`HashMap` imports), events.rs ×2 `Arc<Mutex<HashMap<…>>>`, spawn.rs ×3 `Arc<tokio::sync::Mutex<std::collections::HashMap<…>>>` (already fully qualified). All resolve to the same type. ✓

### Accessors — ✓ correct, no lock-across-await
- `factory()` (state.rs:169-174): `runtime.factory.as_ref().ok_or_else(|| "…".into())` → `Result<&Arc<AgentLoopFactory>, IpcError>`. Message identical to the 3 original inline `ok_or_else` strings. No lock. 3 callers (agent.rs stats bodies :681, :701, :719). ✓
- `safety_rules()` (state.rs:177-182): same pattern, same message as the 5 originals. 5 callers (agent.rs :57, :58, :76, :101, :122). ✓
- `embedder_status()` (state.rs:202-211): see LOW finding below.

---

## Bugs — none

- **No lock held across await** in any accessor or helper: `safety_mode.write()` and `embedder_status.read()` are std `RwLock` (sync); `manager.lock().await` in `send_cmd` is held only across the sync `send()`; `run_all.lock().await` in `end_run` is released before `emit_backlog_changed().await`. ✓
- **No borrow-after-move:** accessors return `&Arc<…>` borrows of `self.runtime` fields; callers use them immediately (`.memory_handle()`, `.read_raw()`, etc.) — no move of `self`. ✓
- **No dangling `set_runtime_safety` reference:** `search` confirms zero matches in `src/` (only in `.coding/` docs/reviews). Both callers rewired to `state.set_safety(…)?`. The `SafetyMode` import was removed from config_io.rs (no longer used there). ✓
- **`end_run` in `halt_run_all_for_approval` — no deadlock:** the `run_all` guard is explicitly `drop(guard)` at run_all.rs:732, *before* `end_run` is called at :747. `end_run` re-acquires `run_all` lock fresh — same as the original inline clear. ✓
- **`config_io.rs` not orphaned:** still exports `persist_and_reload` + `swap_live_provider` (both `pub(crate)`, both with callers in settings.rs/agent.rs). Module doc updated to "two operations" + cross-ref to `IpcState::set_safety`. ✓

---

## Security — no safety weakening

- All accessors return the **same errors** as the inline originals on the brain-failed-to-start path (`factory`/`safety_rules`) — identical messages, same `IpcError` kind `"error"`. No approval/safety-mode logic touched.
- `set_safety` preserves the exact `map_err?` + `re_evaluate` + core-op-gating behavior — no approval that was previously gated is now auto-resolved, and vice-versa.
- `send_cmd` preserves the exact error surface to the frontend for all 6 send commands.
- The one behavior change (`embedder_status` panic→error, see LOW) is strictly *more* graceful and affects only a UI-status banner — no safety decision reads embedder status.

---

## Constitution compliance — ✓

- **Doc comments on all pub items:** `factory()` :168, `safety_rules()` :176, `set_safety()` :184, `embedder_status()` :201, `AgentLoopMap` :65 — all documented. Private helpers `send_cmd` (agent.rs:22) and `end_run` (run_all.rs:55) also documented (good practice; not required). ✓
- **No `#[allow(...)]` anywhere** in the diff. ✓
- **No dead code — every new item has callers:**
  - `factory()` → 3 stats bodies (agent.rs). `safety_rules()` → 5 lookups (agent.rs). `set_safety()` → `set_safety_mode` + `save_settings`. `embedder_status()` → `get_embedder_status`. `send_cmd` → 6 commands. `end_run` → 5 sites. `AgentLoopMap` → state.rs:79, events.rs:150/420, spawn.rs:86/383/392. ✓
- **Imports cleaned, none dangling:** `AgentLoop` removed from events.rs (only in comments now — verified no `: AgentLoop`/`<AgentLoop` type use remains); `HashMap` removed from state.rs (only fully-qualified in the alias + a doc comment); `HashMap` correctly **retained** in events.rs (`TurnResolveLatch` :61, `prev_workflow_state` :161-162). `SafetyMode` removed from config_io.rs. Under `#![deny(warnings)]` any unused import would fail the build. ✓
- **Line endings:** source files are LF-consistent; the CRLF warnings are only on `.coding/backlog.json` + the plan md (git autocrlf on bookkeeping files, not introduced by this diff). ✓
- **Warning-free build:** could not run `cargo test` (read-only reviewer), but by inspection there is no dead code, no unused import, no missing doc, no `#[allow]`. The main agent's `cargo test` in the closing sequence will confirm under `#![deny(warnings)]`.

---

## Findings

### LOW (informational) — `embedder_status()` changes panic-on-poison to error-on-poison
**Where:** `src-tauri/src/ipc/state.rs:202-211` (accessor) → `src-tauri/src/ipc/settings.rs:25` (`get_embedder_status` caller).

The original inline `get_embedder_status` used `.read().expect("embedder status lock poisoned").clone()` — a **panic** on lock poison. The new `embedder_status()` accessor uses `.read().map_err(|e| format!("embedder status lock poisoned: {e}"))?` — returns a structured `IpcError` instead.

This is a behavior change in a batch labeled "behavior-identical." However:
- It only triggers on lock poison, which requires a prior panic while holding the lock (already a fatal-ish state).
- It is **strictly more graceful** (structured error to the frontend vs. a command-thread panic) and does **not** weaken safety — embedder status drives only a UI banner, not any approval/safety decision.
- It is **consistent** with `set_safety`'s `map_err?` style and with the Batch B review's explicit recommendation (`.coding/reviews/2026-04-18-batch-b-review.md:85-100`) to prefer `map_err?` over panic for these locks.

**Recommendation:** accept as a deliberate normalization (the accessor pattern mandates `Result`-returning, so the panic path had to become an error). No fix required. Flagged only so the main agent is aware this accessor is not byte-for-byte behavior-identical to the original inline code on the poison path.

### INFORMATIONAL — LOC delivered ≪ D2+D3 projection
`git diff HEAD --stat` totals **+138 / −135 = +3 net** across all 10 files; subtracting the `.coding/` bookkeeping (+9 net backlog.json, 0 net plan md) yields **≈ −6 LOC net** for the 8 source files. The plan projected D2 ≈ −115 and D3 ≈ −113 (≈ −170 combined for D2+D3; −570 for the whole batch incl. D1).

The shortfall is structural, not a defect: the `impl IpcState` block (4 accessors, each with a constitution-mandated 3-4-line doc comment ≈ +50 LOC), `send_cmd` (+~15 with doc), `end_run` (+~7 with doc), and `AgentLoopMap` (+~4) together offset the call-site savings. Small-N dedup with required doc comments barely breaks even on raw LOC. The refactors are correct and do reduce *duplication* (the actual maintainability goal); they just don't move the raw-LOC needle much.

**Recommendation:** no code change. Recalibrate the batch-D −570 target: with D1 (−400) deferred and D2+D3 delivering ≈ −6, the realistic batch total is far below target. If hitting the −570 number matters, D1 (the 46-site test-fixture builder) is the only item large enough to move it — its deferral is well-justified on risk grounds, but it carries essentially all of the batch's LOC impact.

---

## Summary

D2 + D3 are correct, behavior-identical on every meaningful path, warning-free by inspection, and constitution-compliant. The single behavior change (`embedder_status` panic→error) is a defensible, safety-neutral normalization. Merge after the main agent confirms `cargo test` is green.
