# Batch B Review — structural correctness (per-agent plan dirs, config-write unification, sandbox write ladder)

**Branch:** `feat/deep-review-b-structure`
**Reviewed:** all uncommitted changes (`git diff HEAD` + untracked `src-tauri/src/ipc/config_io.rs`)
**Verdict:** ✅ Approve with one LOW finding (behavior change in a "pure refactor") + minor observations. All three structural fixes are correct; the C6 mismatch is fully closed; the config_io extraction is behavior-identical on the three contract points (lock ordering, ModelChanged emit, re_evaluate); `validate_for_write` refuses protected BEFORE mkdir; `file_append` now creates parents.

---

## B1 — per-agent plan dirs ✅

### Correctness — all verified
- **`build_inner` flows `plans_dir` to BOTH paths:** `Workflow::new(plans_dir.clone())` (factory.rs:360) AND `.with_plans_dir(plans_dir)` (factory.rs:400). The override reaches the workflow AND the loop's session-end consolidation handle. ✓
- **C6 fix is complete, not just for `FinishTool`:** `FinishTool::new(workflow, self.reviews_dir())` (factory.rs:523-526) uses the factory's main-derived `reviews_dir()` (factory.rs:467-472, which reads `self.plans_dir` — the factory's main field, NOT the per-agent override). Critically, `WriteReviewReportTool::new(self.reviews_dir())` (factory.rs:495) uses the SAME main-derived path. So the reviewer WRITES to `.coding/reviews/` and `finish` READS from `.coding/reviews/` — both main-derived, no mismatch. The old `plans_dir().parent().join("reviews")` derivation in `FinishTool::execute` is gone (plan.rs:548 now uses `self.reviews_dir.clone()`). ✓ C6 fully closed.
- **Isolation works:** each UI spawn gets `factory.plans_dir().join(format!("agents/{agent_id}"))` (spawn.rs:113) — a unique dir per agent id. `Workflow::persist_stack` does `create_dir_all(&self.plans_dir)` (workflow/mod.rs:660), so the per-agent dir is created on first plan save. `load_latest` for a fresh side agent starts empty (the `let _ = wf.load_latest()` at factory.rs:361 swallows the no-plan case). ✓

### The 3 `spawn_agent_shared` callers — all correct
| Caller | `own_plans_dir` | Correct? |
|---|---|---|
| `spawn_agent` (UI button, spawn.rs:50) | `true` | ✓ UI spawns get `.coding/plans/agents/<id>/` |
| `IpcSpawner` (tool-spawned subagent, spawn.rs:434) | `false` | ✓ subagents share main dir |
| `main.rs` main agent (main.rs:163) | `false` | ✓ main keeps `.coding/plans/` |

### Security — subagent plan mutation (observation, NOT a finding)
Subagents pass `own_plans_dir: false` → `build_with_id` → main plans dir. So a subagent's `create_plan`/`complete_step` tools write to the MAIN `.coding/plans/`. This is **pre-existing behavior** (B1 does not change subagent paths — it only adds isolation for UI-spawned parentless agents). The plan's "subagents already can't mutate" phrasing is slightly loose, but the file tools (`file_write`/`file_edit`/`file_append`) are still blocked from `.coding/plans/` by `is_protected_write_target` (sandbox.rs:186). No regression introduced by B1.

### Test quality
`build_with_id_and_plans_dir_uses_override` (factory.rs:1124) verifies the workflow uses the per-agent dir AND `reviews_dir()` stays main-derived. It transitively covers the C6 fix (since `register_workflow_tools` passes `self.reviews_dir()` to `FinishTool`). **Minor gap:** no test constructs two UI-spawned agents and verifies their stacks don't clobber (the plan mentioned this). Isolation is structurally guaranteed by the unique `agent_id` in the dir name, so risk is low — but a two-agent isolation test would directly exercise the fix's intent.

---

## B2 — config-write unification ✅

### Behavior-identical on the three contract points
- **Lock ordering:** `persist_and_reload` takes `config.lock().await`, swaps, drops (no await across). `swap_live_provider` takes `agent_loops.lock().await`, does `set_provider` + `set_resolved_model(None)` per loop, collects ids, drops the lock, THEN emits `ModelChanged` per id (no lock held across emit). `set_runtime_safety` is synchronous (`safety_mode.write()` is a std `RwLock`, no await). ✓ No lock held across await anywhere.
- **ModelChanged emit:** `swap_live_provider` emits `SerializableAgentEvent::ModelChanged { model }` per live agent id (config_io.rs:84-92), best-effort. Identical to both original sites. ✓
- **re_evaluate:** `set_runtime_safety` calls `state.approvals.re_evaluate(mode, &state.project.sandbox)` (config_io.rs:110-112), same as both originals. ✓
- **ALWAYS clears `resolved_model`:** `swap_live_provider` calls `agent_loop.set_resolved_model(None)` unconditionally (config_io.rs:76). ✓ This is the key behavior the spec required.

### `set_model` path (agent.rs:496-499)
`set_model` already errors out at line 440-445 if no factory, so `swap_live_provider`'s `return false` for no-factory is unreachable from this caller. The `let _ =` discards the bool (set_model doesn't use it). Original returned `Ok(())` after the swap; new does the same. ✓ No behavior change.

### `save_endpoints` path (settings.rs:320-366)
The `provider_swapped` bool was `true` in the swap arm; now it's `swap_live_provider(...)`'s return, which is `true` (factory presence guaranteed by the outer `if state.runtime.factory.is_some()`). Same value. The factory-access refactor (`is_some()` + `as_ref().unwrap()` at line 347 instead of cloning into a binding) is safe — `factory` is a plain `Option<Arc<...>>` field, not behind a lock. ✓ No borrow-after-move, no double-lock.

### Dead imports — clean
`settings.rs` removed `SerializableAgentEvent`, `AgentId`, `emit_agent_event` (confirmed 0 remaining uses in settings.rs). `agent.rs` still uses all three (18 uses). No dead-import warning under `deny(warnings)`. ✓

### Contract fixtures — unchanged
No wire struct (`Serialize`/`Deserialize` type) was touched — only logic moved between functions. ✓

### Framing — correct
`config_io.rs` module doc (lines 8-10) explicitly says "This is a pure refactor — the 'live drift' ... was already fixed; this module preserves that correct behavior." ✓

---

## B3 — `Sandbox::validate_for_write` ladder ✅

### Refuses protected BEFORE mkdir — verified
`validate_for_write` (sandbox.rs:210-235):
1. validate → creation-fallback (steps 1-2)
2. `is_protected_write_target` check (step 3, line 217) — **BEFORE** any `create_dir_all`
3. `create_dir_all(parent)` (step 4, line 224) — only reached if NOT protected

The test `validate_for_write_refuses_protected_nonexistent` (sandbox.rs:455) asserts `.coding/plans` is NOT created after refusing `.coding/plans/stack.json`. ✓ This directly exercises the BEFORE-mkdir ordering.

### `file_append` now creates parents — verified
`file_append.rs` now calls `sandbox.validate_for_write(path)` (line 93) which includes step 4 (`create_dir_all`). Previously file_append did NOT create parents. The test `validate_for_write_creates_parent_dirs` (sandbox.rs:443) verifies `nested/deep` is created. ✓

### Security — no protected-path write via new mkdir
A protected path (e.g. `.coding/plans/stack.json`) is refused at step 3 before `create_dir_all` at step 4. The new mkdir cannot create dirs under a protected ancestor because the protected check runs first and returns early. ✓

### Shared refusal message — consistent
`protected_refusal` (sandbox.rs:252-258) is ONE function used by `validate_for_write` (file_write + file_append) AND `refuse_if_protected` (file_edit). All three file tools produce the identical message containing "protected". Existing tests assert `.contains("protected")` (file_write.rs:304,335,359; file_append.rs:210) — all still pass. ✓

### `file_edit` correctly does NOT use the creation ladder
`file_edit.rs` uses `sandbox.validate(path)` + `sandbox.refuse_if_protected(&validated)` (lines 543, 586) — no mkdir, since it edits existing files. ✓

### Tests — all exercise the fix
All 6 new tests directly exercise the ladder behavior (parent creation, before-mkdir refusal, existing-file refusal, normal pass, `refuse_if_protected` both arms). No no-op tests. ✓

---

## Findings

### LOW — `set_runtime_safety` panics on lock poison (behavior change in a "pure refactor")
**File:** `src-tauri/src/ipc/config_io.rs:103-107`

The original `set_safety_mode` (agent.rs) and the safety block in `save_settings` (settings.rs) used:
```rust
.map_err(|e| format!("safety_mode lock poisoned: {e}"))?  // → returned IpcError
```
The extracted `set_runtime_safety` uses:
```rust
.expect("safety_mode lock poisoned")  // → panics
```
This changes a graceful IPC error response into a panic on lock poisoning. The spec asked to "verify the extraction is behavior-identical."

**Mitigating factors:** lock poisoning only occurs after a panic in a thread holding the lock (already catastrophic), and `.expect("... lock poisoned")` is the codebase's dominant pattern (factory.rs:371, 375, 390 all use it) — so this aligns with existing style. The original `map_err?` was the outlier.

**Recommendation:** either (a) restore `map_err?` + return `Result<(), IpcError>` from `set_runtime_safety` to preserve exact behavior, or (b) accept the panic as a deliberate style normalization and note it in the commit message. Either is defensible; the current state is a silent behavior change in a refactor labeled "behavior-identical."

---

## Observations (not findings — no fix required)

1. **Pre-existing TOCTOU in `validate_for_write` step 5 fallback** (sandbox.rs:229-234): `Err(_) => Ok(validated)` returns the lexical (non-canonical) path if revalidation fails. This is pre-existing (old `file_write` had the same fallback) — not a B3 regression. A symlink race between mkdir and revalidate could theoretically return a non-canonical path, but the lexical `validated` was already root-checked by `validate_for_creation`. Low risk.

2. **No two-agent isolation test** (B1): the plan mentioned "two parentless agents create plans → stacks don't clobber" but only a single-agent factory test exists. Isolation is structurally guaranteed by the unique `agent_id` in `format!("agents/{agent_id}")`. Low risk; a two-agent test would be a nice-to-have.

3. **`save_endpoints` factory access** (settings.rs:347): `state.runtime.factory.as_ref().unwrap()` is guarded by the outer `is_some()`. Safe, though cloning the Arc (as the original did) would be marginally cleaner. Not a bug.

---

## Constitution compliance ✅
- **Doc comments:** all new pub items have doc comments (`build_with_id_and_plans_dir`, `plans_dir()`, `validate_for_write`, `refuse_if_protected`, `FinishTool::new`, `config_io` module + its 3 `pub(crate)` fns). The private `protected_refusal` fn is also documented. ✓
- **No `#[allow]`:** none added. ✓
- **No dead code:** `plans_dir()`, `build_with_id_and_plans_dir`, `validate_for_write`, `refuse_if_protected`, `protected_refusal`, `FinishTool::reviews_dir`, and all 3 `config_io` fns have callers. ✓
- **Warning-free:** removed imports in `settings.rs` confirmed unused; `agent.rs` imports still used; `let _ =` in `set_model` is intentional. ✓
- **Line endings:** source files preserve LF; the git LF→CRLF warning is only for the `.coding/plans/*.md` bookkeeping file, not source. ✓
