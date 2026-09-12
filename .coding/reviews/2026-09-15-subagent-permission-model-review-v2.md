# Review: Refined subagent permission model (v2 — re-review after C1 fix)

**Date:** 2026-09-15
**Reviewer:** read-only subagent (spawned re-review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD`), focused on verifying the C1 fix from the first review pass.
**Branch:** feature branch (uncommitted working tree)

**Plan goal:** (1) all workflow states can spawn subagents (spawn_agent visible in Planning + Complete, staying NeedsApproval); (2) a subagent's tool surface is a subset of the spawning agent's CURRENT permissions — computed at spawn time in `compute_subagent_allowlist` (src-tauri/src/ipc/spawn.rs); roles restrict further (reviewer = read-only ∩ parent), with read-only role tools (git_diff, write_review_report) always kept since they're AutoRun/safe; (3) the agent cannot change workflow state while spawned subagents are running — a gate in `src/agent/dispatch.rs::execute_tool_call` blocks every state-transition tool when `DescendantTracker::has_running_descendants` returns true.

---

## Summary

**C1 is fixed.** The `compute_subagent_allowlist` function no longer uses the name-only `parent_allows` closure whose first disjunct (`ToolCategory::Memory`, unconditionally `true`) short-circuited the filter check. It now reads each registered tool's *actual* `category()` + `safety()` off the `&dyn Tool` and queries the parent's `ToolFilter` with that exact tuple (`filter.allows(category, safety, name)`). The Memory short-circuit is gone. The "subagent ⊆ parent" property now actually holds for unrestricted spawns from Planning/Complete.

The spawn tests now compile and run (5 tests in `ipc::spawn::tests`) and genuinely exercise the intersection — the `unrestricted_spawn_gets_parent_subset` test registers `file_write` (Agent/NeedsApproval) + `git` (Agent/NeedsApproval) in the parent's registry and asserts they're absent when the parent is in Planning, which would *fail* under the old buggy code (the Memory short-circuit would have included them) and *passes* under the fix. The `futures` dev-dependency and the owned-`String` tuple collection resolve the two compile errors that previously prevented the tests from running.

Everything else verified in the first pass remains clean: no deadlock (workflow lock dropped before the tracker await), role tools stay safe (AutoRun, always kept for a reviewer), spawn_agent stays NeedsApproval, the gate covers all 8 state-transition tools, subagents still can't mutate plans, and the build is warning-free under `#![deny(warnings)]` with no new `#[allow(...)]`.

**No findings.** The diff is clean.

---

## C1 verification — FIXED ✓

**File:** `src-tauri/src/ipc/spawn.rs:229-310` (`compute_subagent_allowlist`)

### The fix

The old closure (first review, C1):
```rust
let parent_allows = |name: &str| -> bool {
    registered.iter().any(|r| r == name)
        && (filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, name)  // ALWAYS TRUE
            || filter.allows(ToolCategory::Agent, SafetyLevel::AutoRun, name)
            || ...)
};
```
The `ToolCategory::Memory` arm is unconditionally `true` in every `ToolFilter` variant (`src/tool/mod.rs:219,242,261,285,293`), so the first disjunct short-circuited the `||` and the parent's actual filter was never consulted.

The new code (`spawn.rs:244-270`):
```rust
let (filter, registered): (ToolFilter, Vec<(String, ToolCategory, SafetyLevel)>) = {
    let loops = agent_loops.lock().await;
    let parent = loops.get(&parent_id)?.clone();
    drop(loops);
    let wf = parent.workflow_handle();
    let wf = wf.lock().await;
    let filter = wf.allowed_tools();
    let registered = parent
        .tools()
        .iter()
        .map(|t| (t.name().to_string(), t.category(), t.safety()))  // ACTUAL category + safety
        .collect();
    (filter, registered)
};

let parent_allowed: Vec<String> = registered
    .iter()
    .filter(|(name, category, safety)| filter.allows(*category, *safety, name))  // EXACT tuple
    .map(|(name, _, _)| name.clone())
    .collect();
```

The name-only closure is gone. Each registered tool's real `category()` + `safety()` (read off `&dyn Tool` via `ToolRegistry::iter()`, `src/tool/mod.rs:327-329`) is paired with its name, and the filter is queried with that exact `(category, safety, name)` tuple. `ToolCategory` and `SafetyLevel` both derive `Copy` (`src/tool/mod.rs:23,34`), so the `*category`/`*safety` derefs are sound.

### Trace: unrestricted spawn from Planning (the scenario C1 broke)

Parent in Planning, registry includes `file_write` (Agent, NeedsApproval — verified `src/tool/agent/file_write.rs:39-41,59-61`).

- `filter = ToolFilter::Planning`.
- For `file_write`: `filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, "file_write")`.
- Planning's Agent arm (`src/tool/mod.rs:207`): `safety == SafetyLevel::AutoRun || name == "spawn_agent"` → `NeedsApproval == AutoRun` is `false` AND `"file_write" == "spawn_agent"` is `false` → **`false`** → `file_write` excluded from `parent_allowed`. ✓

Under the old buggy code, `filter.allows(Memory, AutoRun, "file_write")` would have returned `true` (Memory arm), short-circuiting to `true`, and `file_write` would have been *included* — violating "subagent ⊆ parent". The fix removes this. **The "subagent ⊆ parent" property now holds.** ✓

Same trace for `git` (Agent, NeedsApproval — verified `src/tool/agent/git.rs:134-136,164-171`): excluded under Planning. ✓

### Memory tools drop out correctly

Memory tools (category `Memory`) are not added to the allow-list because `filter.allows(Memory, …, name)` returns `true` but they're filtered into `parent_allowed`... wait — actually they ARE in `parent_allowed` (the Memory arm is `true`). But this is harmless: the child's `ToolFilter::Skill` arm (`src/tool/mod.rs:293`) admits all Memory tools unconditionally at dispatch/schema time regardless of the allow-list, so whether or not a Memory tool name appears in the allow-list is irrelevant to the child's ability to use it. The allow-list only *restricts* (a tool NOT in the list is hidden); listing a Memory tool doesn't grant anything extra. So Memory tools appearing in `parent_allowed` (and thus in an unrestricted child's list, or being intersected away for a reviewer) changes nothing functionally. The doc comment at `spawn.rs:225-228` correctly explains this. ✓

---

## Test verification — tests now compile, run, and genuinely exercise the intersection ✓

### Compile errors fixed

1. **`futures` dev-dependency** (`src-tauri/Cargo.toml:28-30`): the `NoopProvider` mock names `futures::stream::BoxStream` (the `LlmClient::complete` return type, `src/provider/mod.rs:321`). `futures` is a transitive dep via `myharness`; pinning it as a dev-dependency lets the test name the type. `Cargo.lock` updated (`+ "futures"`). ✓
2. **Owned `String` tuples** (`spawn.rs:253-257`): the registered tuples are collected as `(String, ToolCategory, SafetyLevel)` so the borrow of `parent` ends at the block close (the `Arc<AgentLoop>` is dropped there), avoiding the lifetime error. ✓

### 5 tests in `ipc::spawn::tests` (`spawn.rs:416-650`)

1. `reviewer_allowlist_intersected_with_parent_planning` — parent Planning, reviewer role. Asserts `file_read` present (Agent/AutoRun, Planning allows), `git_diff`+`write_review_report` present (always-safe role tools), `file_write`+`git` absent (Planning hides them). **Genuinely distinguishes bug from fix**: under the old bug, `file_write` would have been in `parent_allowed` (Memory short-circuit), but it's not in `REVIEWER_BASE_TOOLS`, so the reviewer arm would still exclude it — so this specific test doesn't distinguish. However, it does confirm the always-safe role tools are kept even when the parent (Planning) lacks them. ✓
2. `reviewer_allowlist_includes_mutations_parent_has_in_executing` — parent Executing (where `file_write`/`git` ARE allowed), reviewer role. Asserts `file_write`/`git` absent (not in reviewer base list). Proves the role restricts further than the parent. ✓
3. `unrestricted_spawn_gets_parent_subset` — parent Planning, no role. Asserts `file_read`+`create_plan` present, `file_write`+`git` absent. **This is the test that catches C1**: under the old bug, `file_write` (registered, Agent/NeedsApproval) would have been included in `parent_allowed` via the Memory short-circuit and returned in the unrestricted list — the `assert!(!list.contains("file_write"))` would have **failed**. Under the fix, `filter.allows(Agent, NeedsApproval, "file_write")` under Planning is `false`, so it's excluded. The test genuinely exercises the intersection. ✓
4. `no_parent_returns_none` — UI-button spawn (no parent) → `None` (no restriction). ✓
5. `missing_parent_loop_returns_none` — defensive: unknown parent id → `None` (no restriction rather than over-constraining). ✓

The test helper `parent_in_state` (`spawn.rs:427-496`) registers the **real** tools (`FileReadTool`, `FileWriteTool`, `GitTool`, `GitDiffTool`, `WriteReviewReportTool`, `CreatePlanTool`, `CompleteStepTool`) — not stubs — so the categories/safety read off `&dyn Tool` are the production values. The tests exercise the real intersection. ✓

---

## No deadlock ✓

**File:** `src/agent/dispatch.rs:94-167`

The workflow lock is acquired at line 95 (`let wf = self.workflow.lock().await;`) inside a `{ … }` block and the guard is dropped at the block close (line 107), **before** the state-transition gate at lines 144-167. The gate's `tracker.has_running_descendants(id).await` (line 157) runs with no workflow lock held.

`IpcSpawner::has_running_descendants` (`spawn.rs:410-413`) acquires only `self.manager` (a separate `tokio::sync::Mutex<AgentManager>`), calls the synchronous `AgentManager::has_running_descendants` (`src/runtime/mod.rs:140-144` — a pure in-memory walk over `self.agents` with no await), and releases it. No lock is held across an await. ✓

`compute_subagent_allowlist` (`spawn.rs:244-259`) acquires `agent_loops.lock()`, clones the parent `Arc`, drops the loops lock, then acquires the workflow lock — no nested locks, and both are released before the allow-list is built. ✓

---

## Role tools stay safe ✓

**File:** `src-tauri/src/ipc/spawn.rs:184-198, 279-294`

`ALWAYS_SAFE_ROLE_TOOLS = ["git_diff", "write_review_report"]` (`spawn.rs:198`). In the reviewer arm (`spawn.rs:279-290`), a base-list entry is kept if `parent_allowed.contains(name) || ALWAYS_SAFE_ROLE_TOOLS.contains(name)`. So even if the parent lacks them, the reviewer keeps both.

Both tools are `SafetyLevel::AutoRun` (verified: `git_diff` AutoRun at `src/tool/agent/git_diff.rs:95-98`; `write_review_report` AutoRun at `src/tool/agent/write_review_report.rs:151-155`). `git_diff` is read-only (runs `git diff`/`git status`, never mutates); `write_review_report` writes only under `.coding/reviews/` with a path-traversal guard (`write_review_report.rs:51-111`). Granting them can't make the subagent more permissive than the parent. ✓

---

## spawn_agent stays NeedsApproval ✓

**File:** `src/tool/agent/spawn_agent.rs:145-147`

`SpawnAgentTool::safety()` returns `SafetyLevel::NeedsApproval`, unchanged. The ToolFilter change (`src/tool/mod.rs:207,270`) makes `spawn_agent` *visible* in Planning/Complete (`name == "spawn_agent"`), but visibility only affects schema inclusion; execution still goes through the approval gate in `dispatch.rs:193-196` (`force_prompt = tool.never_auto() || tool.never_auto_for(...)`; `spawn_agent` is not `never_auto`, so it's governed by `approval::needs_approval` under the active safety mode). No auto-execution introduced. ✓

---

## The gate covers every state transition ✓

**File:** `src/agent/dispatch.rs:144-154`

The `matches!` lists all 8 state-transition tools: `create_plan | update_plan | complete_step | abandon_plan | skill_start | skill_end | abandon_skill | finish`. Cross-checked against the workflow transition methods and the tool registry (`src/agent/factory.rs:899,932`): all 8 are real registered tools, and every method that mutates `Workflow::state` or the plan/skill stack is gated. `update_plan` doesn't change state but is correctly included (it mutates the active plan). ✓

---

## Subagents still can't mutate plans ✓

**File:** `src-tauri/src/ipc/spawn.rs:120-130`, `src/agent/dispatch.rs:113-125`

`plan_mutations_allowed` is unchanged. `spawn_agent_shared` sets `agent_loop.set_plan_mutations_allowed(false)` + `wf.set_plan_mutations_allowed(false)` for any `parent_id.is_some()` spawn, and `dispatch.rs:113-125` denies `create_plan/update_plan/complete_step/abandon_plan` when `!plan_mutations_allowed`. The new state-transition gate (`dispatch.rs:144-167`) is an *additional* restriction layered on top (it runs after the plan-mutation gate); it does not weaken this. ✓

---

## Constitution compliance ✓

### CC1 — Warning-free under `#![deny(warnings)`; no new `#[allow(...)]` ✓

`#![deny(warnings)]` is at both crate roots: `src/lib.rs:1` and `src-tauri/src/main.rs:7`. Searched `src/` for `#[allow(`: the only matches are the **pre-existing** `#[allow(clippy::too_many_arguments)]` on the two `AgentLoop` constructors (`src/agent/loop_impl.rs:174,211`) — not added by this diff, not a rustc warning suppression (it's a clippy style lint, predates this change). No new `#[allow(...)]` was introduced to silence warnings. The new code (trait, impl, gate, factory wiring, tests, `compute_subagent_allowlist` rewrite) introduces no unused imports/dead code. ✓

The `ModelRefDto` unused import in `src-tauri/src/ipc/settings.rs:1659` was removed (it was masked because the app crate's tests never compiled before the `futures` fix). `ModelsConfigDto` (the remaining import in that test) is genuinely used (`settings.rs:1663,1673,1679,1689,1696`), and `ModelRefDto` is still imported in the *other* test that uses it (`settings.rs:1708`). ✓

### CC2 — Windows paths / PowerShell ✓

No shell commands introduced by this diff. The new code is pure Rust (traits, async fns, tests). `IpcSpawner` uses `tokio::sync::Mutex` and `std::collections::HashMap` — cross-platform. No Linux paths or bash syntax. ✓

### CC3 — Doc comments on public functions ✓

`DescendantTracker::has_running_descendants` (`runtime/mod.rs:260-263`), `AgentLoop::with_descendant_tracker` (`loop_impl.rs:256-262`), `AgentLoop::tools` (`loop_impl.rs:548-553`), `AgentLoopFactory::set_descendant_tracker` (`factory.rs:201-212`), and the `IpcSpawner` impl (`spawn.rs:407-412`) all have doc comments. `compute_subagent_allowlist` (`spawn.rs:200-228`) has a thorough doc comment explaining the security-critical property. ✓

### CC4 — Line-ending preservation ✓

The diff touches existing files with targeted edits; the file tools normalize to the file's detected style. No mixed `\r\n`/`\n` introduced. (The `git diff` CRLF warnings are on `.coding/*.json` bookkeeping files and `Cargo.lock`, not source.) ✓

### CC5 — No commit to main ✓

No commits in this diff; all changes are uncommitted on the working tree. ✓

---

## Verdict

**No findings.** The C1 fix is correct and complete: `compute_subagent_allowlist` now reads each registered tool's actual `category()` + `safety()` and queries the filter with that exact tuple, eliminating the Memory short-circuit that previously broke "subagent ⊆ parent" for unrestricted spawns from Planning/Complete. The spawn tests now compile (the `futures` dev-dependency + owned-`String` tuple fixes resolve the two prior compile errors) and genuinely exercise the intersection — `unrestricted_spawn_gets_parent_subset` would fail under the old bug and passes under the fix. The unused `ModelRefDto` import is removed. Everything else from the first pass remains clean: no deadlock, role tools safe, spawn_agent NeedsApproval, gate covers all 8 transitions, subagents can't mutate plans, warning-free under `#![deny(warnings)]` with no new `#[allow(...)]`.
