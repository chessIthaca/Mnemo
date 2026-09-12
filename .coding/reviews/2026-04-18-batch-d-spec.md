# Batch D — Mechanical Size Reduction: IMPLEMENTATION SPEC

**Scope:** Verified against current HEAD. All file:line refs corrected from the
2026-04-18 plan (which was approximate/stale). READ-ONLY spec — no source edits.
Target ≈ −570 LOC across D1 (−400), D2 (−115), D3 (−55/−30/−28).

**Pre-resolved choices verified below:** all three CONFIRMED with refinements
(see §2). One correction: the plan's "28 construction sites" is actually **32
in `src/agent/tests.rs`** + **14 in `src/runtime/agent.rs`** = 46 total test
construction sites (production sites in `factory.rs`/`spawn.rs` are OUT of
scope). The "6 mock LlmClient impls" is correct for each file (12 total).

---

## 1. Per-item current-code verification (corrected file:line)

### D1 — agent/tests.rs fixture builder

**Construction-site search** (`AgentLoop::new` / `with_constitution_source`):

| File | Count | Lines |
|------|-------|-------|
| `src/agent/tests.rs` | **32** | 113, 164, 253, 319, 405, 471, 565, 652, 754, 841, 958, 1137, 1219, 1308, 1434, 1493, 1566, 1660, 1765, 1825, 1861, 1877, 1906, 1988, 2048, 2254, 2342, 2504, 2582, 2688, 2925, 3051 |
| `src/runtime/agent.rs` | **14** | 648, 705, 768, 837, 971, 1063, 1212, 1321, 1403, 1561, 1654, 1802, 1919, 2148 |
| `src/agent/factory.rs:350` | 1 | PRODUCTION (`with_constitution_source`) — **EXCLUDE** |
| `src-tauri/src/ipc/spawn.rs:523` | 1 | PRODUCTION — **EXCLUDE** |
| `tests/workflow_integration.rs:181,263` | 2 | Integration tests — **OPTIONAL** (separate test binary; include if the builder is exported at crate root, else leave) |

**Mock `LlmClient` impls (12 total, 6 per file as the plan states):**

`src/agent/tests.rs` (6):
1. `MockProvider` (tests.rs:26-82) — canned event queue, `single()`/`sequence()`
2. `CapturingProvider` (tests.rs:1029-1114) — captures system prompt + tails, configurable `kind`
3. `FailingProvider` (tests.rs:1389-1416) — always errors, counts calls
4. `PausingProvider` (tests.rs:2442-2489, inline in `mid_turn_steer_soft_stops_and_carries_pending_steer`) — pauses on `Notify`
5. `FixedModelProvider` (tests.rs:2846-2881, inline in `resolve_turn_provider_forced_model_beats_state_override`) — fixed `model()`, `unreachable!()` on complete
6. `RecordingProvider` (tests.rs:3005-3032, inline in `run_turn_uses_resolved_override_provider`) — fixed `model()`, canned text

`src/runtime/agent.rs` (6):
1. `MockProvider` (agent.rs:599-628) — simple "Done!" text
2. `PausingMockProvider` (agent.rs:1141-1185) — pauses on gate, call-counted
3. `ToolCallProvider` (agent.rs:1478-1541) — emits create_plan then file_write
4. `CapturingProvider` (agent.rs:1753-1783, inline in `prompt_with_images_builds_multipart_message`) — captures user messages
5. `CapturingProvider` (agent.rs:1866-1895) — shared capturing provider (used by `spawn_capturing_task`)
6. `AlwaysFailProvider` (agent.rs:2103-2129) — always fails, captures full message list

Plus 2 `ImageDescriber` mocks in agent.rs (`CannedDescriber`:1967, `FailingDescriber`:1980) — these are NOT `LlmClient` impls; leave in place (or optionally move to test_support, but the plan's "6 mocks" refers to LlmClient only).

**Existing helper `make_registry`** (tests.rs:86-99): builds file_read + file_write + create_plan + complete_step + memory tools (with a fresh in-memory store). Used by ~20 of the 32 sites. The other ~12 sites build custom registries (GitTool only, AskUserTool, shared store, no memory tools, etc.).

**Existing helper `make_dispatch_fixture`** (tests.rs:1893-1920): builds an agent + channels for direct `execute_tool_call` tests. Internally calls `make_registry` + `AgentLoop::new`. This should be refactored to use the new builder.

**`AgentLoop::new` signature** (loop_impl.rs:264-298): 9 positional args — `(provider, tools, workflow, sandbox, constitution, safety_mode, context_manager, memory, vision)`. `with_constitution_source` (304-340) swaps the 5th arg for a `ConstitutionSource`. Builder chaining methods: `with_safety_rules` (364), `with_agent_id` (680), `with_descendant_tracker` (374), `with_model_resolver` (397), `with_fill_rate` (505), `with_plans_dir` (347), `with_safety_mode_handle` (388).

### D2 — IpcState accessors

**`IpcState`** (state.rs:45-64) is a plain struct with **no `impl` block** — adding methods is collision-free. Fields are on nested `AgentRuntimeContext` (state.rs:67-116): `factory`, `memory_store`, `safety_rules`, `safety_mode`, `embedder_status`, etc.

**Collision check — CONFIRMED SAFE:** Accessors as methods on `IpcState` (e.g. `state.factory()`) do NOT collide with the fields `state.runtime.factory` (different types: method on `IpcState` vs field on `AgentRuntimeContext`). ⚠️ Do NOT put the accessors on `AgentRuntimeContext` — `runtime.factory()` would collide with the `factory` field (field-then-call is a compile error). **[CHOICE: put accessors on `IpcState`, not on `AgentRuntimeContext`]**

**Repeated `ok_or_else` messages (verified):**
- `"safety rules unavailable (brain failed to start)"` — **5×** in agent.rs (29, 45, 65, 92, 115)
- `"agent factory unavailable (brain failed to start)"` — **4×** in agent.rs (651, 734, 755, 775) + 1× in spawn.rs:37 (different shape — `match` not `ok_or_else`)
- `"no memory store configured"` — **3×** in agent.rs (739, 760, 780)
- `"no skill registry configured"` — **1×** in agent.rs (657)
- `memory_debug.rs:140` — uses `let Some(store) = state.runtime.memory_store.as_ref() else { return Ok(default) }` (NOT an error path — returns a default overview). The `memory_store()` accessor would need a `?`-returning form AND the call site keeps its `let-else` since it returns `Ok`, not `Err`. **Flag: this site does NOT fit a `Result`-returning accessor directly** — either add a `memory_store_or_default()` variant or leave this one site as-is.

**6 send-command bodies** (agent.rs:194-278): `send_prompt` (194), `send_suggestion` (210), `interrupt` (224), `cancel` (237), `compact` (254), `clear_conversation` (269). All 6 follow: `lock manager → send(agent_id, AgentCommand::X) → map_err(|e| format!("failed to {label}: {e:?}")) → map_err(IpcError::from)`. The `enter_skill` command (644) also sends a prompt but with extra logic (skill lookup, emit event) — only its final `manager.send(...)` (699-705) fits the helper.

**3 stats bodies** (agent.rs:704-770): `get_session_stats` (721), `get_project_stats` (752), `get_session_list` (772). All 3 follow: `factory? → store? → store.X().await.map_err? → serde_json::to_value.map_err?`. The `get_session_stats` has an extra `agent_loops` lock + `session_id` lookup before the factory/store pattern.

### D3 — misc in-file dedup

**D3a — `fallback_ipc_state()` for main.rs:**
- NeedsProject arm: main.rs:250-308 (builds minimal IpcState with `needs_project=true`, `startup_error=None`, config from arg)
- Err arm: main.rs:310-368 (builds minimal IpcState with `needs_project=false`, `startup_error=Some(msg)`, `Config::default()`)
- Both share: cwd fallback project, fallback sandbox, fallback safety, fresh AgentManager, fresh BrowserManager, console forwarder, pending approvals/questions, fresh trace, backlog context. **Differences:** `startup_error` (None vs Some), `needs_project` (true vs false), `config` (passed `&Config` clone vs `Config::default()`), and the Ready arm (188-216) is the full state (NOT part of the fallback). A `fallback_ipc_state(app, config, startup_error, needs_project, pending_approvals, pending_questions, backlog)` helper captures the shared ~50 lines.

**D3b — `AgentLoopMap` alias in state.rs:** 8 spellings verified:
- `state.rs:74` — `Arc<Mutex<HashMap<AgentId, Arc<AgentLoop>>>>` (uses `tokio::sync::Mutex` + `std::collections::HashMap` via imports)
- `events.rs:151, 421` — `Arc<Mutex<HashMap<AgentId, Arc<AgentLoop>>>>`
- `spawn.rs:85, 265, 358, 367, 476` — `Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>` (fully qualified)
- Alias: `pub(crate) type AgentLoopMap = Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>;` in state.rs. Note: events.rs uses unqualified `Mutex`/`HashMap` (imported), spawn.rs uses fully-qualified — the alias normalizes both.

**D3c — `end_run` helper in run_all.rs:** 5 copies of `*state.backlog.run_all.lock().await = None; emit_backlog_changed(app, &state).await;`:
- run_all.rs:466-467 (in `run_all_dispatch_next`, `state: &IpcState`)
- run_all.rs:486-487 (same fn)
- run_all.rs:507-508 (same fn)
- run_all.rs:626-627 (in `on_main_turn_resolved`, `state` is local `IpcState` from `app.state()`)
- run_all.rs:705-706 (in `halt_run_all_for_approval`, `state` is local)
- Helper: `async fn end_run(app: &tauri::AppHandle, state: &IpcState) { *state.backlog.run_all.lock().await = None; emit_backlog_changed(app, state).await; }`. Note: at 467 the call is `emit_backlog_changed(app, state)` (state already `&IpcState`), at 627 it's `emit_backlog_changed(app, &state)` — the helper takes `&IpcState` either way.

**D3d — drop self-qualified `crate::ipc::run_all::` paths:** 6 in run_all.rs itself (474, 511, 566, 583, 596, 608) — these are in the same module so `crate::ipc::run_all::checkpoint` → just `checkpoint` (or `self::checkpoint`). Plus 5 in other files (events.rs:268, 298, 302, 373, 583; backlog_cmds.rs:19) — these are cross-module calls and MUST keep the `crate::ipc::run_all::` prefix (or import the functions). **The plan's "7" = the 6 in run_all.rs + 1 in backlog_cmds.rs:19 (`use crate::ipc::run_all::run_all_dispatch_next;`) which is already an import, not a self-qualified call.** Only the 6 in run_all.rs are true self-qualifications to drop.

**D3e — settings.rs:427-442 triplicated/orphaned comment block:** Verified — lines 427, 428, 430 each have `// ── Full Settings read/write (general / context / ui / vision / pricing) ─` (3×). Lines 431-442 are an orphaned comment referencing "`serde_json::Value` via `json!({ ... })`" — the code now uses typed structs (the `json!` bodies are gone). Fix: delete the 2 duplicate header lines (428, 430) + delete the orphaned comment block (431-442). Keep one header (427) if desired, or delete all three.

**D3f — dedup pricing map (settings.rs:62-71 ≡ 793-802):** Both map `config.pricing.iter()` → `PricingWire { model, input_per_1m, output_per_1m, cached_per_1m }`. Extract `fn pricing_wire(config: &Config) -> Vec<PricingWire>`.

**D3g — openai.rs:263-264 doc-line dup:** Line 263 and 264 are identical: `/// Whether a single `/models` entry explicitly reports image input modality.` Delete one.

**D3h — openai.rs:355-408 `parse_models_with_vision` arm merge:** The `data` arm (356-380) and `models` arm (381-406) are structurally identical: `filter_map` extracting an id string → `ModelWithVision` with caps + vision flag, then `if empty { Some(vec![]) } else if no ids { None } else { Some(vec) }`. The ONLY difference is the id-extraction: `m.get("id")` vs `m.get("name").or_else(|| m.get("model"))`. Merge into a helper `fn extract_models(entries: &[Value], id_fn: impl Fn(&Value) -> Option<&str>) -> Option<Vec<ModelWithVision>>` and call it for each arm.

**D3i — openai.rs:1066-1085 u64 usage extractor:** 4 fields each do `.get("X").and_then(|v| v.as_u64()).unwrap_or(0) as u32`. The `reasoning` (1073-1077) and `cached` (1081-1085) fields have an extra `.and_then(|d| d.get("Y"))` nesting. Helper: `fn u64_at<'a>(v: &'a Value, key: &str) -> u32` for the flat case + `fn nested_u64(v: &Value, parent: &str, child: &str) -> u32` for the nested case. Or one helper `fn u64_path(v: &Value, path: &[&str]) -> u32` that walks a dotted path. Net: ~8 lines saved (4 fields × ~4 lines → 4 one-liners + 1 helper).

**D3j — StatusBar useClickAway hook (StatusBar.tsx:212-258):** 4 identical `useEffect` blocks: pickerOpen/pickerRef (212-221), safetyOpen/safetyRef (224-233), modelOpen/modelRef (236-245), effortOpen/effortRef (248-257). Each: `if (!open) return; function handleClick(e) { if (ref.current && !ref.current.contains(e.target)) setOpen(false); } document.addEventListener("mousedown", handleClick); return () => document.removeEventListener("mousedown", handleClick);`. Extract `function useClickAway(open: boolean, ref: RefObject<HTMLElement>, onClose: () => void)`.

---

## 2. Confirmed/refined design with choices marked

### D1 — `test_support` module + `TestAgentBuilder` [CHOICE: confirmed + refined]

**Module location:** New `#[cfg(test)] pub(crate) mod test_support;` declared in `src/agent/mod.rs` (alongside the existing `#[cfg(test)] mod tests;` at mod.rs:44-45). This makes it accessible to both `src/agent/tests.rs` (via `super::test_support::...`) and `src/runtime/agent.rs` tests (via `crate::agent::test_support::...`).

**API (confirmed, with refinements for the variations found):**

```rust
// src/agent/test_support.rs
use std::sync::Arc;
use tempfile::TempDir;
use crate::agent::{AgentLoop, context::ContextManager};
use crate::config::SafetyMode;
use crate::memory::MemoryStoreTrait;
use crate::project::{Constitution, ConstitutionSource};
use crate::tool::ToolRegistry;
use crate::tool::agent::sandbox::Sandbox;
use crate::workflow::Workflow;

/// Convenience: build an AgentLoop with defaults (Autonomous, 128k ctx,
/// no memory, no vision, default registry). Returns (agent, dir) so the
/// caller keeps the TempDir alive.
pub fn test_agent(dir: &TempDir, provider: Arc<dyn LlmClient>) -> AgentLoop { ... }

/// Builder for the varying bits. `dir` + `provider` are required;
/// everything else has defaults.
pub struct TestAgentBuilder {
    dir: TempDir,
    provider: Arc<dyn LlmClient>,
    context: (usize, f64),           // default (128_000, 0.5)
    safety: SafetyMode,              // default Autonomous
    memory: Option<Arc<dyn MemoryStoreTrait>>,  // default None
    vision: Option<Arc<dyn ImageDescriber>>,   // default None
    registry: Option<Arc<ToolRegistry>>,       // default: make_registry()
    constitution_source: Option<ConstitutionSource>,  // default: Constitution::default()
    agent_id: Option<AgentId>,
    safety_rules: Option<Arc<SafetyRules>>,
    descendant_tracker: Option<Arc<dyn DescendantTracker>>,
    model_resolver: Option<Arc<dyn ModelResolver>>,
}

impl TestAgentBuilder {
    pub fn new(dir: TempDir, provider: Arc<dyn LlmClient>) -> Self { ... }
    pub fn context(mut self, max: usize, fill: f64) -> Self { self.context = (max, fill); self }
    pub fn safety(mut self, mode: SafetyMode) -> Self { self.safety = mode; self }
    pub fn memory(mut self, store: Arc<dyn MemoryStoreTrait>) -> Self { self.memory = Some(store); self }
    pub fn vision(mut self, client: Arc<dyn ImageDescriber>) -> Self { self.vision = Some(client); self }
    pub fn registry(mut self, reg: Arc<ToolRegistry>) -> Self { self.registry = Some(reg); self }
    pub fn constitution_source(mut self, src: ConstitutionSource) -> Self { self.constitution_source = Some(src); self }
    pub fn agent_id(mut self, id: AgentId) -> Self { self.agent_id = Some(id); self }
    pub fn safety_rules(mut self, rules: Arc<SafetyRules>) -> Self { self.safety_rules = Some(rules); self }
    pub fn descendant_tracker(mut self, t: Arc<dyn DescendantTracker>) -> Self { self.descendant_tracker = Some(t); self }
    pub fn model_resolver(mut self, r: Arc<dyn ModelResolver>) -> Self { self.model_resolver = Some(r); self }
    pub fn build(self) -> AgentLoop { ... }
}
```

**Mock consolidation:** Move the 6 mocks from tests.rs + the 6 from agent.rs into `test_support` (or a `test_support::mocks` submodule). Collapse near-identical ones:
- `MockProvider` (tests.rs) and `MockProvider` (agent.rs) → **1 shared** `MockProvider` with the `single()`/`sequence()` API (the richer one from tests.rs). The agent.rs "Done!" variant becomes `MockProvider::single(vec![TextDelta("Done!"), Finish::Stop])`.
- `CapturingProvider` (tests.rs:1029) and `CapturingProvider` (agent.rs:1866) → **1 shared** `CapturingProvider` with the `new()`/`new_with_tails()`/`new_local_with_tails()`/`build(kind)` API (the richer one from tests.rs). The agent.rs variant (captures user messages only) is a subset.
- `FailingProvider` (tests.rs) and `AlwaysFailProvider` (agent.rs) → **1 shared** `FailingProvider` (agent.rs's captures full messages; tests.rs's counts calls — merge into one with both features, or keep 2 if the merge is awkward). **[CHOICE: merge into 1 `FailingProvider` with optional call-count + message capture]**
- `PausingProvider` (tests.rs) and `PausingMockProvider` (agent.rs) → **1 shared** `PausingProvider` (both pause on a `Notify` gate; the agent.rs one uses `AtomicU32` call-count, the tests.rs one uses `StdMutex<bool>` — merge to the `AtomicU32` design).
- `FixedModelProvider` and `RecordingProvider` (both inline in tests.rs) → move to `test_support` as-is (they're small + test-specific). Could merge into 1 `FixedModelProvider` with an optional canned-text response, but they're different enough (one `unreachable!()`s, one returns text) — **[CHOICE: keep as 2 separate mocks in test_support, not merged]**.
- `ToolCallProvider` (agent.rs) → move to `test_support` as-is (no analog in tests.rs).

**Net mock count: 12 → ~7 shared mocks** (MockProvider, CapturingProvider, FailingProvider, PausingProvider, FixedModelProvider, RecordingProvider, ToolCallProvider).

### D2 — IpcState accessors + send_cmd helper [CHOICE: confirmed, accessors on IpcState]

```rust
// src-tauri/src/ipc/state.rs  (new `impl IpcState { ... }`)
impl IpcState {
    /// The agent factory, or an error if the brain failed to start.
    pub fn factory(&self) -> Result<&Arc<AgentLoopFactory>, IpcError> {
        self.runtime.factory.as_ref()
            .ok_or_else(|| "agent factory unavailable (brain failed to start)".into())
    }
    /// The concrete memory store, or an error if none is configured.
    pub fn memory_store(&self) -> Result<&Arc<MemoryStore>, IpcError> {
        self.runtime.memory_store.as_ref()
            .ok_or_else(|| "no memory store configured".into())
    }
    /// The safety-rules store, or an error if the brain failed to start.
    pub fn safety_rules(&self) -> Result<&Arc<SafetyRules>, IpcError> {
        self.runtime.safety_rules.as_ref()
            .ok_or_else(|| "safety rules unavailable (brain failed to start)".into())
    }
    /// Set the runtime safety mode + re-evaluate pending approvals.
    pub fn set_safety(&self, mode: SafetyMode) -> Result<(), IpcError> {
        *self.runtime.safety_mode.write()
            .map_err(|e| format!("safety_mode lock poisoned: {e}"))? = mode;
        let resolved = self.approvals.re_evaluate(mode, &self.project.sandbox);
        if resolved > 0 { eprintln!("safety: mode change auto-resolved {resolved} pending approval(s)"); }
        Ok(())
    }
    /// The live embedder status (cloned).
    pub fn embedder_status(&self) -> Result<EmbedderStatus, IpcError> {
        Ok(self.runtime.embedder_status.read()
            .map_err(|e| format!("embedder status lock poisoned: {e}"))?.clone())
    }
}

// src-tauri/src/ipc/agent.rs (or a shared helper module)
/// Lock the manager + send a command, mapping the error with `label`.
pub async fn send_cmd(
    state: &State<'_, IpcState>,
    agent_id: AgentId,
    cmd: AgentCommand,
    label: &str,
) -> Result<(), IpcError> {
    let manager = state.runtime.manager.lock().await;
    manager.send(agent_id, cmd)
        .map_err(|e| format!("failed to {label}: {e:?}"))
        .map_err(IpcError::from)
}
```

**`set_safety` consolidation:** Currently `set_safety_mode` (agent.rs:321) and `save_settings` (settings.rs:1255) both do the `*safety_mode.write()? = mode` + `approvals.re_evaluate(...)` dance. The `set_safety()` accessor centralizes the lock-write + re-evaluate. Both call sites use it. **Note:** `save_settings` only calls it when `parsed_safety.is_some()` — the accessor returns `Ok(())` and the caller gates it.

### D3 — misc dedup [CHOICE: confirmed]

All D3 items confirmed as described in §1. The `end_run` helper signature: `async fn end_run(app: &tauri::AppHandle, state: &IpcState)`.

---

## 3. Exact edit plan

### D1 edits
1. **Create `src/agent/test_support.rs`** — the `TestAgentBuilder`, `test_agent()`, shared mocks, and a shared `make_registry()` (moved from tests.rs:86-99). ~150 lines of new code, but it replaces ~600+ lines of duplicated construction + mock definitions.
2. **`src/agent/mod.rs`** — add `#[cfg(test)] pub(crate) mod test_support;` after line 44.
3. **`src/agent/tests.rs`** — delete the 6 mock structs (26-82, 1029-1114, 1389-1416, 2442-2489, 2846-2881, 3005-3032), delete `make_registry` (86-99), delete `make_dispatch_fixture` (1893-1920, or refactor to use the builder), and rewrite all 32 construction sites to use `TestAgentBuilder` / `test_agent`. Add `use super::test_support::*;` (or `use crate::agent::test_support::*;`).
4. **`src/runtime/agent.rs`** tests module — delete the 6 mock structs (599-628, 1141-1185, 1478-1541, 1753-1783, 1866-1895, 2103-2129), delete `spawn_capturing_task`/`wait_for_capture`/`CannedDescriber`/`FailingDescriber` (or move the latter two to test_support), and rewrite all 14 construction sites. Add `use crate::agent::test_support::*;`.

### D2 edits
5. **`src-tauri/src/ipc/state.rs`** — add `impl IpcState { factory(), memory_store(), safety_rules(), set_safety(), embedder_status() }`. Add `use crate::ipc::error::IpcError;` import.
6. **`src-tauri/src/ipc/agent.rs`** — add `send_cmd()` helper (module-private). Rewrite the 6 send-command bodies (194-278) to `send_cmd(state, agent_id, AgentCommand::X, "label")`. Rewrite the 3 stats bodies (721-787) to use `state.factory()?` + `state.memory_store()?`. Rewrite the 5 safety-rules bodies (26-122) to use `state.safety_rules()?`. Rewrite `set_safety_mode` (321) + `get_safety_mode` (337) to use accessors where applicable. Rewrite `enter_skill` (651) factory lookup.
7. **`src-tauri/src/ipc/settings.rs`** — rewrite `save_settings` safety block (1255-1266) to use `state.set_safety(mode)?`.
8. **`src-tauri/src/ipc/embeddings.rs`** — `get_embedder_status` (72) already reads `state.runtime.embedder_status` — optionally use `state.embedder_status()` accessor (minor).
9. **`src-tauri/src/ipc/memory_debug.rs:140`** — **FLAG: leave as-is** (it returns `Ok(default)`, not `Err`; the `Result`-returning accessor doesn't fit). Optionally add a `memory_store_or_none()` accessor that returns `Option<&Arc<MemoryStore>>`, but the `let-else` is already one line — not worth it.

### D3 edits
10. **`src-tauri/src/main.rs`** — extract `fallback_ipc_state(app, config, startup_error, needs_project, pending_approvals, pending_questions, backlog) -> IpcState` and use it in the NeedsProject (271) + Err (334) arms.
11. **`src-tauri/src/ipc/state.rs`** — add `pub(crate) type AgentLoopMap = Arc<tokio::sync::Mutex<std::collections::HashMap<AgentId, Arc<AgentLoop>>>>;`. Replace the 8 spellings in state.rs:74, events.rs:151/421, spawn.rs:85/265/358/367/476.
12. **`src-tauri/src/ipc/run_all.rs`** — add `async fn end_run(app, state)`; replace the 5 clear+emit pairs (466-467, 486-487, 507-508, 626-627, 705-706). Drop the 6 self-qualified `crate::ipc::run_all::` prefixes (474, 511, 566, 583, 596, 608) → bare `checkpoint`/`rollback`/`commit_success`/`extract_checkpoint_sha`/`RUN_ALL_STEER`.
13. **`src-tauri/src/ipc/settings.rs`** — delete the 2 duplicate header lines (428, 430) + orphaned comment block (431-442). Extract `fn pricing_wire(config: &Config) -> Vec<PricingWire>`; replace the 2 inline maps (62-71, 793-802).
14. **`src/provider/openai.rs`** — delete the duplicate doc line (264). Merge the two `parse_models_with_vision` arms (356-406) via a shared `extract_models` helper. Extract `fn u64_or_zero(v: &Value, key: &str) -> u32` + `fn nested_u64_or_zero(v: &Value, parent: &str, child: &str) -> u32`; replace the 4 usage-field extractions (1066-1085).
15. **`frontend/src/components/layout/StatusBar.tsx`** — extract `function useClickAway(open: boolean, ref: React.RefObject<HTMLElement | null>, onClose: () => void)`; replace the 4 identical `useEffect` blocks (212-258).

---

## 4. Full enumerated list of D1 construction sites + D2 call sites

### D1 construction sites (46 test sites — migrate all)

**`src/agent/tests.rs` (32 sites):**

| # | Line | Test fn | Variation (needs builder method) |
|---|------|---------|----------------------------------|
| 1 | 113 | `full_turn_text_response` | default |
| 2 | 164 | `context_usage_event_carries_cached_token_count` | default |
| 3 | 253 | `full_turn_with_tool_call` | default |
| 4 | 319 | `error_recovery_malformed_json` | default |
| 5 | 405 | `unknown_tool_error_recovery` | default |
| 6 | 471 | `max_retries_aborts_after_consecutive_tool_errors` | default |
| 7 | 565 | `create_plan_never_needs_approval` | `.safety(ApproveEachAction)` |
| 8 | 652 | `git_merge_prompts_even_in_autonomous_mode` | `.registry(custom: GitTool only)` |
| 9 | 754 | `run_turn_records_session_id_on_tool_events` | `.memory(store)` + custom registry |
| 10 | 841 | `run_turn_records_request_stats_on_usage` | `.memory(store)` |
| 11 | 958 | `cache_heuristic_skipped_after_summarization` | `.memory(store)`, `.context(100, 0.5)` |
| 12 | 1137 | `constitution_reread_after_agent_md_edit` | `.constitution_source(source)` (uses `with_constitution_source`) |
| 13 | 1219 | `volatile_tail_then_stable_footer_appended_and_popped` | default |
| 14 | 1308 | `local_provider_tail_folded_into_leading_system_message` | default (CapturingProvider Local kind) |
| 15 | 1434 | `complete_with_retry_returns_err_not_panic` | default (FailingProvider) |
| 16 | 1493 | `midstream_error_with_no_output_returns_err` | default |
| 17 | 1566 | `midstream_error_after_partial_output_surfaces_note_and_keeps_text` | default |
| 18 | 1660 | `safety_rule_auto_approves_in_approve_each_mode` | `.safety(ApproveEachAction)`, `.safety_rules(rules)` |
| 19 | 1765 | `safety_rule_non_match_still_prompts_in_approve_each_mode` | `.safety(ApproveEachAction)`, `.safety_rules(rules)` |
| 20 | 1825 | `describe_image_errors_when_no_vision_client` | default |
| 21 | 1861 | `is_multimodal_reflects_provider_caps` (non-multimodal) | default (inline MockProvider with custom caps) |
| 22 | 1877 | `is_multimodal_reflects_provider_caps` (multimodal) | default (inline MockProvider with custom caps) |
| 23 | 1906 | `make_dispatch_fixture` helper | default (this helper itself — refactor to use builder) |
| 24 | 1988 | `dispatch_blocks_state_transition_when_descendants_running` | `.agent_id(1)`, `.descendant_tracker(tracker)` |
| 25 | 2048 | `dispatch_allows_state_transition_when_no_descendants_running` | `.agent_id(1)`, `.descendant_tracker(tracker)` |
| 26 | 2254 | `dispatch_approval_request_includes_file_write_preview` | `.safety(ApproveEachAction)` |
| 27 | 2342 | `deny_all_latches_and_skips_remaining_tool_calls` | `.safety(ApproveEachAction)` |
| 28 | 2504 | `mid_turn_steer_soft_stops_and_carries_pending_steer` | `Arc::new(...)` wrap, inline PausingProvider |
| 29 | 2582 | `ask_user_two_questions_round_trip` | `Arc::new(...)` wrap, `.registry(custom: AskUserTool)` |
| 30 | 2688 | `ask_user_interrupt_returns_error` | `Arc::new(...)` wrap, `.registry(custom: AskUserTool)` |
| 31 | 2925 | `resolve_turn_provider_forced_model_beats_state_override` | `.model_resolver(resolver)` |
| 32 | 3051 | `run_turn_uses_resolved_override_provider` | `Arc::new(...)` wrap, `.model_resolver(resolver)` |

**`src/runtime/agent.rs` (14 sites):**

| # | Line | Test fn | Variation |
|---|------|---------|-----------|
| 33 | 648 | `agent_task_processes_prompt` | `Arc::new(...)`, custom registry (with store) |
| 34 | 705 | `correction_prompt_writes_user_correction_working_memory` | `Arc::new(...)`, `.memory(store)`, custom registry (FileRead only) |
| 35 | 768 | `normal_prompt_writes_no_correction_memory` | `Arc::new(...)`, `.memory(store)`, custom registry (FileRead only) |
| 36 | 837 | `agent_task_stays_alive_after_turn_and_processes_second_prompt` | `Arc::new(...)`, custom registry (with store) |
| 37 | 971 | `cancel_emits_exited_promptly_without_blocking_on_consolidation` | `Arc::new(...)`, custom registry (with store) |
| 38 | 1063 | `between_turn_steer_is_converted_to_user_message_and_runs_turn` | `Arc::new(...)`, custom registry (with store) |
| 39 | 1212 | `mid_turn_steer_soft_stops_and_runs_follow_up_turn` | `Arc::new(...)`, PausingMockProvider |
| 40 | 1321 | `cancel_during_streaming_exits_agent` | `Arc::new(...)`, PausingMockProvider |
| 41 | 1403 | `interrupt_during_streaming_keeps_agent_alive` | `Arc::new(...)`, PausingMockProvider |
| 42 | 1561 | `cancel_during_approval_exits_agent` | `Arc::new(...)`, `.safety(ApproveEachAction)`, ToolCallProvider |
| 43 | 1654 | `interrupt_during_approval_stops_turn_and_keeps_agent_alive` | `Arc::new(...)`, `.safety(ApproveEachAction)`, ToolCallProvider |
| 44 | 1802 | `prompt_with_images_builds_multipart_message` | `Arc::new(...)`, inline CapturingProvider |
| 45 | 1919 | `spawn_capturing_task` helper | `Arc::new(...)`, `.vision(client)`, CapturingProvider |
| 46 | 2148 | `terminal_provider_failure_is_pushed_in_context` | `Arc::new(...)`, AlwaysFailProvider |

**Sites that DON'T fit the builder cleanly (flagged):**
- **#12 (tests.rs:1137)** — uses `with_constitution_source`. **Needs `.constitution_source(source)` method.** ✓ Handled.
- **#8 (tests.rs:652)** — GitTool-only registry. **Needs `.registry(custom)`.** ✓ Handled.
- **#9 (tests.rs:754)** — custom registry with shared store (FileRead + FileWrite + CreatePlan + CompleteStep + MemoryWrite + MemoryRecall, all sharing one store). **Needs `.registry(custom)`.** ✓ Handled.
- **#29, #30 (tests.rs:2582, 2688)** — AskUserTool-only registry. **Needs `.registry(custom)`.** ✓ Handled.
- **#21, #22 (tests.rs:1861, 1877)** — inline `MockProvider { responses: empty, caps: custom }`. The builder takes `provider: Arc<dyn LlmClient>` — these pass a custom-caps MockProvider. ✓ Handled (provider is a parameter).
- **memory_debug.rs:140** (D2) — returns `Ok(default)`, not `Err`. **Doesn't fit `Result`-returning `memory_store()` accessor.** Leave as-is.

### D2 call sites (accessors + send_cmd)

**`send_cmd` call sites (6 + 1 partial):**
- agent.rs:194 `send_prompt` → `send_cmd(state, agent_id, AgentCommand::Prompt { text, images }, "send prompt")`
- agent.rs:210 `send_suggestion` → `send_cmd(state, agent_id, AgentCommand::Suggestion(text), "send suggestion")`
- agent.rs:224 `interrupt` → `send_cmd(state, agent_id, AgentCommand::Interrupt, "interrupt")`
- agent.rs:237 `cancel` → `send_cmd(state, agent_id, AgentCommand::Cancel, "cancel")`
- agent.rs:254 `compact` → `send_cmd(state, agent_id, AgentCommand::Compact, "compact")`
- agent.rs:269 `clear_conversation` → `send_cmd(state, agent_id, AgentCommand::Clear, "clear conversation")`
- agent.rs:699 `enter_skill` (partial — only the final `manager.send` after skill setup) → `send_cmd(state, agent_id, AgentCommand::Prompt { ... }, "send skill prompt")`

**`factory()` call sites (4):** agent.rs:651, 734, 755, 775.
**`memory_store()` call sites (3):** agent.rs:739, 760, 780.
**`safety_rules()` call sites (5):** agent.rs:27, 43, 63, 90, 115.
**`set_safety()` call sites (2):** agent.rs:323 (`set_safety_mode`), settings.rs:1256 (`save_settings`).
**`embedder_status()` call sites (1+):** embeddings.rs:72, settings.rs:27 (`get_embedder_status`), main.rs:229 (startup emit — reads directly, optional).

---

## 5. Test plan

**Rust (lib crate):** `cargo test` (unpiped, read `test result:` lines). The D1 migration is pure refactor — all 46 construction sites must compile + their tests pass unchanged. Key tests to watch:
- `full_turn_text_response`, `full_turn_with_tool_call` (basic loop)
- `create_plan_never_needs_approval`, `git_merge_prompts_even_in_autonomous_mode` (safety/approval)
- `run_turn_records_request_stats_on_usage`, `cache_heuristic_skipped_after_summarization` (memory + context)
- `constitution_reread_after_agent_md_edit` (constitution source path)
- `resolve_turn_provider_forced_model_beats_state_override` (model resolver)
- `mid_turn_steer_soft_stops_and_carries_pending_steer` (PausingProvider)
- `ask_user_two_questions_round_trip` (custom registry)
- All 14 `runtime/agent.rs` tests (AgentTask-level)

**Rust (bin crate):** `cd src-tauri && cargo test` — D2/D3 IPC changes. Key: `endpoint_dto_tests`, `settings_dto_tests`, `run_all` tests (checkpoint/rollback/strict_success), `extract_tests`.

**Rust build:** `cd src-tauri && cargo build` (root `cargo test` does NOT compile the bin crate). Must be warning-free (`#![deny(warnings)]`).

**Frontend:** `cd frontend && npx vitest run` + `npm run build`. The StatusBar `useClickAway` extraction must not change behavior — the 4 dropdowns still close on outside click.

**No new tests required** — this is mechanical dedup; existing tests are the regression net. Optionally add a `test_agent` smoke test if one doesn't exist, but the migrated tests already cover it.

---

## 6. Risks

1. **D1 — `test_support` visibility:** `pub(crate)` makes it visible to `src/runtime/agent.rs` tests. If `runtime` is a sibling module of `agent`, `crate::agent::test_support` resolves. **Verify:** `src/runtime/mod.rs` must re-export or `src/runtime/agent.rs` imports `crate::agent::test_support`. Low risk — standard Rust path.

2. **D1 — `TempDir` lifetime:** `test_agent(dir, provider)` takes `&TempDir` (borrows) so the caller keeps it alive. `TestAgentBuilder::new(dir, provider)` takes ownership of `TempDir` (moves it) — the builder (and thus the built `AgentLoop`) must NOT hold the `TempDir` (the `AgentLoop` only holds `Arc<Sandbox>` + `Arc<Mutex<Workflow>>` which borrow `dir.path()`). **The builder must drop `TempDir` after extracting paths, OR return `(AgentLoop, TempDir)` so the caller keeps it alive.** ⚠️ **CRITICAL:** if the builder owns `TempDir` and drops it on `.build()`, the temp dir is deleted and all sandbox/workflow paths dangle. **Fix: `build(self) -> (AgentLoop, TempDir)`** — return the dir alongside the agent. Or: `test_agent` takes `&TempDir` and `TestAgentBuilder` also takes `&TempDir` (borrow). **[CHOICE: borrow `&TempDir` in both — simpler, matches the existing pattern where every test does `let dir = tempdir().unwrap();` then passes `&dir` implicitly via `dir.path()`]**

3. **D1 — `make_registry` store coupling:** The current `make_registry` creates its own in-memory store. Tests that need a shared store (#9, #34-38) build custom registries. The builder's `.registry(custom)` override handles this, but the builder must NOT call `make_registry` when a custom registry is provided. Low risk.

4. **D2 — `send_cmd` borrow:** `send_cmd(state, agent_id, cmd, label)` takes `&State<'_, IpcState>`. The `State` guard is held for the duration of the lock+send (no await between lock and send except the implicit send). The current code drops the manager lock implicitly at the end of the expression. `send_cmd` does the same. Low risk.

5. **D2 — `set_safety` in `save_settings`:** The accessor re-evaluates pending approvals. `save_settings` currently does this only when `parsed_safety.is_some()`. The accessor always re-evaluates. **Fix: only call `state.set_safety(mode)` when `parsed_safety.is_some()`** — the caller gates it, not the accessor. Low risk.

6. **D3b — `AgentLoopMap` alias normalization:** events.rs uses unqualified `Mutex`/`HashMap` (from `use tokio::sync::Mutex; use std::collections::HashMap;`). The alias uses fully-qualified paths. Replacing `Arc<Mutex<HashMap<...>>>` with `AgentLoopMap` works regardless of imports. Low risk.

7. **D3c — `end_run` in `halt_run_all_for_approval`:** This fn holds `state.backlog.run_all.lock()` at line 674, drops it at 690 (`drop(guard)`), then re-locks at 705. The `end_run` helper locks `run_all` again. **Verify no deadlock:** the guard is dropped before `end_run` is called. ✓ Safe (the `drop(guard)` at 690 precedes the 705 call).

8. **D3h — `parse_models_with_vision` merge:** The two arms have a subtle difference: the `data` arm checks `data.is_empty()` (the raw array), the `models` arm checks `models.is_empty()`. The merged helper takes `entries: &[Value]` (already extracted) so `entries.is_empty()` works for both. ✓ Safe.

9. **D3j — `useClickAway` ref type:** The 4 refs are `useRef<HTMLDivElement>(null)` → type `RefObject<HTMLDivElement>`. The hook signature `RefObject<HTMLElement | null>` may need the divs typed as `HTMLElement` or the hook generic. **Verify:** React's `contains` takes `Node`, so `HTMLElement` works. The `null` in `RefObject<HTMLElement | null>` depends on React version (React 19 vs 18 `RefObject` nullability). **Check the React version in `package.json`** — if React 19, refs are `RefObject<T | null>`; if 18, `RefObject<T>`. Low risk but must match.

10. **`#![deny(warnings)]` — dead code:** The `test_support` module is `#[cfg(test)]` so it's only compiled in test builds — no dead-code warning in release. The shared mocks must ALL be used by at least one test (otherwise dead-code warning in test builds fails `cargo test`). **Verify every moved mock is referenced** — `FixedModelProvider` and `RecordingProvider` are each used by exactly 1 test; `ToolCallProvider` by 2. ✓ All used.

11. **LOC estimate reality check:** D1's −400 assumes ~20 lines per site × 46 sites ≈ 920 lines removed, minus ~150 lines of new builder/mocks = −770 gross, but many sites have non-boilerplate logic (custom registries, store setup) that stays. Realistic net ≈ −350 to −450. D2's −115 is plausible (6 send bodies × ~6 lines + 3 stats × ~8 lines + 5 safety × ~3 lines + accessor centralization). D3's −55/−30/−28 is plausible. **Total ≈ −500 to −600, close to the −570 target.**
