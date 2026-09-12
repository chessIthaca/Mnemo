# Review: Refined subagent permission model

**Date:** 2026-09-15
**Reviewer:** read-only subagent (spawned review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD`), focused on the refined subagent permission model.
**Branch:** feature branch (uncommitted working tree)

**Plan goal:** (1) all workflow states can spawn subagents (spawn_agent visible in Planning + Complete, staying NeedsApproval); (2) a subagent's tool surface is a subset of the spawning agent's CURRENT permissions — computed at spawn time in `compute_subagent_allowlist` (src-tauri/src/ipc/spawn.rs); roles restrict further (reviewer = read-only ∩ parent), with read-only role tools (git_diff, write_review_report) always kept since they're AutoRun/safe; (3) the agent cannot change workflow state while spawned subagents are running — a gate in `src/agent/dispatch.rs::execute_tool_call` blocks every state-transition tool when `DescendantTracker::has_running_descendants` returns true.

---

## Summary

The trait decoupling, the dispatch state-transition gate, the deadlock-avoidance, the ToolFilter visibility change, and the wiring are all **correct**. However, the core security property of goal (2) — "a subagent can never get a tool the parent doesn't currently have" — is **broken by a logic bug in `compute_subagent_allowlist`**: the `parent_allows` closure's first disjunct queries `ToolFilter::allows` with `ToolCategory::Memory`, whose arm is unconditionally `true` for *any* name. This short-circuits the entire filter check, so the parent's actual filter is never consulted — the subagent's allow-list collapses to "every tool the parent's registry registers", including mutations the parent's state hides. The accompanying tests pass only because they assert the *intended* outcome on inputs where the bug happens to coincide with the intended outcome; they do not actually exercise the filter.

One **high-severity** correctness/security finding (C1) and one **medium** finding (B1) below. Everything else verifies clean.

---

## Correctness

### C1 (HIGH / security) — `parent_allows` never consults the parent's filter; the "subagent ⊆ parent" property is violated

**File:** `src-tauri/src/ipc/spawn.rs:249-273` (the `parent_allows` closure inside `compute_subagent_allowlist`)

The closure is:
```rust
let parent_allows = |name: &str| -> bool {
    registered.iter().any(|r| r == name)
        && (filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, name)   // <-- ALWAYS TRUE
            || filter.allows(ToolCategory::Agent, SafetyLevel::AutoRun, name)
            || filter.allows(ToolCategory::Agent, SafetyLevel::NeedsApproval, name)
            || filter.allows(ToolCategory::Workflow, SafetyLevel::AutoRun, name)
            || filter.allows(ToolCategory::Workflow, SafetyLevel::NeedsApproval, name))
};
```

The first disjunct calls `filter.allows(ToolCategory::Memory, SafetyLevel::AutoRun, name)`. But **every** `ToolFilter` variant's `ToolCategory::Memory` arm returns `true` unconditionally, *regardless of `name`*:

- `ToolFilter::Planning` → `ToolCategory::Memory => true` (`src/tool/mod.rs:219`)
- `ToolFilter::Executing` → `ToolCategory::Memory => true` (`src/tool/mod.rs:242`)
- `ToolFilter::Reviewing` → `ToolCategory::Memory => true` (`src/tool/mod.rs:261`)
- `ToolFilter::Complete` → `ToolCategory::Memory => true` (`src/tool/mod.rs:285`)
- `ToolFilter::Skill(_)` → `ToolCategory::Memory => true` (`src/tool/mod.rs:293`)

So `filter.allows(ToolCategory::Memory, …, name)` is **always `true`**. Because it is the first term of the `||`, the whole parenthesized expression short-circuits to `true` for every name. The remaining four `filter.allows(...)` calls (the ones that *would* consult the Agent/Workflow arms and actually respect the parent's state) are dead — never evaluated. The closure therefore reduces to:

```rust
registered.iter().any(|r| r == name)
```

i.e. "is the tool registered in the parent's registry?" — the parent's `ToolFilter` (its current workflow-state permissions) is **never consulted**.

**Consequence — the "subagent ⊆ parent" property is violated.** For an **unrestricted** spawn (`role: None`) from a parent in **Planning** or **Complete**, the subagent's allow-list becomes *every tool the parent has registered*, including `file_write`, `file_edit`, `shell`, `git` — exactly the mutations Planning/Complete are supposed to hide. The subagent is then built with `wf.set_tool_allowlist(Some(list))`, which overrides its own state-derived filter with `ToolFilter::Skill(list)` (`src/workflow/mod.rs:272-274`), so the subagent *can* reach those mutation tools even though the parent (in Planning/Complete) cannot. This is the precise scenario the plan set out to prevent ("a subagent spawned from Planning can't reach file_edit, since Planning hides it").

The comment at `spawn.rs:259-266` even acknowledges the intent ("for Agent tools it's safety-only … allow iff the filter admits ANY Agent safety") — but the Memory term placed *first* defeats it.

**Why the tests pass anyway (and thus don't catch it):** the test `unrestricted_spawn_gets_parent_subset` (`spawn.rs:590-615`) asserts `file_write` and `git` are absent. They ARE absent — but for the wrong reason. The test's `parent_in_state` helper registers `file_write` and `git` in the parent's registry (`spawn.rs:462-465`), so with the bug `parent_allows("file_write")` returns `true` and `file_write` *should* appear in the list. It does not appear only because… it actually *does* get filtered out somewhere? No — tracing the `None` arm (`spawn.rs:302-310`): `out = registered.filter(parent_allows)`. With the bug, `parent_allows("file_write")` = `registered.contains("file_write") && true` = `true`. So `file_write` **is** in `out`. The test's `assert!(!list.contains("file_write"))` should therefore **fail**.

Let me re-check: the test registers `FileWriteTool` (name `"file_write"`, Agent, NeedsApproval) at `spawn.rs:462-464`. Parent state = Planning. `filter = ToolFilter::Planning`. `parent_allows("file_write")`:
- `registered.contains("file_write")` → `true`
- `filter.allows(Memory, AutoRun, "file_write")` → Planning's Memory arm → `true`
- short-circuits → `true`

So `file_write` IS in the returned list. **The test `unrestricted_spawn_gets_parent_subset` should be failing.** If `cargo test` was reported green, either the test was not actually run in this state, or the report of "all pass" is inaccurate. Either way, the bug is real and the test as written does not guard the property (it asserts the intended outcome, but the code produces the opposite; if it's passing, the test harness isn't exercising this path).

**Impact:** HIGH. The headline security property of the feature is not enforced. A subagent spawned (unrestricted) from Planning or Complete can be granted mutation tools the parent's state hides. (The reviewer role is less affected because its base list is read-only by construction, so even with the bug the reviewer still can't get `file_write`/`git` — they're not in `REVIEWER_BASE_TOOLS`. But the unrestricted-spawn path is wide open.)

**Suggested fix:** drop the `ToolCategory::Memory` term from the closure entirely (Memory tools are always-allowed by the *child's* `ToolFilter::Skill` arm at dispatch/schema time anyway — `src/tool/mod.rs:293` — so they don't need to be in the allow-list at all), and query the filter with the *actual* category+safety of each registered tool rather than guessing. The cleanest fix is to not use a name-only closure at all: iterate the parent's registered tools, read each tool's real `category()` + `safety()` (available on the `&dyn Tool`), and keep it iff `filter.allows(category, safety, name)`. That removes the guesswork and the Memory short-circuit in one step:

```rust
let parent_allowed: Vec<String> = {
    let loops = agent_loops.lock().await;
    let parent = loops.get(&parent_id)?.clone();
    drop(loops);
    let wf = parent.workflow_handle();
    let wf = wf.lock().await;
    let filter = wf.allowed_tools();
    parent.tools().iter()
        .filter(|t| filter.allows(t.category(), t.safety(), t.name()))
        .map(|t| t.name().to_string())
        .collect()
};
```
Then intersect `REVIEWER_BASE_TOOLS` (plus the always-safe role tools) with `parent_allowed` for the reviewer role, and use `parent_allowed` directly for the unrestricted role. Memory tools naturally drop out (they're always-allowed by the child's Skill filter, not by being listed).

---

## Bugs

### B1 (MEDIUM) — `out.dedup()` only removes *adjacent* duplicates; non-adjacent dups survive

**File:** `src-tauri/src/ipc/spawn.rs:289-291`

```rust
// De-dup (a role tool kept via the always-safe path may also be
// parent-allowed) while preserving order.
out.dedup();
```

`Vec::dedup` removes only *consecutive* equal elements. `REVIEWER_BASE_TOOLS` is a fixed literal with no internal duplicates, and the `filter_map` emits each name at most once, so today there are never adjacent (let alone non-adjacent) duplicates — the `dedup()` is a no-op and harmless. But the comment claims it de-dups "a role tool kept via the always-safe path [that] may also be parent-allowed", which can't actually happen here (each base-list entry is visited once). If `REVIEWER_BASE_TOOLS` ever gains duplicates or the construction changes, `dedup()` would silently miss non-adjacent dups. Low real risk, but the comment overstates what `dedup` does. Prefer a `HashSet`-based de-dup or just drop the `dedup()` (it's currently dead) and document that the base list has no duplicates.

---

## Security

### S1 — Role tools (git_diff, write_review_report) stay safe ✓ (with caveat)

`ALWAYS_SAFE_ROLE_TOOLS` (`spawn.rs:198`) = `["git_diff", "write_review_report"]`. In the reviewer arm (`spawn.rs:279-288`) a base-list entry is kept if `parent_allows(name) || ALWAYS_SAFE_ROLE_TOOLS.contains(name)`. So even if the parent lacks them, the reviewer keeps both. Both tools are `SafetyLevel::AutoRun` (verified: `git_diff` AutoRun per prior review `.coding/reviews/2026-04-04-read-only-reviewer-review.md` S3; `write_review_report` AutoRun per the same review + its path-traversal guard). Granting them can't make the subagent more permissive than the parent. ✓

**Caveat (ties to C1):** because `parent_allows` is broken, *every* `REVIEWER_BASE_TOOLS` entry passes `parent_allows` anyway (the Memory short-circuit), so the `ALWAYS_SAFE_ROLE_TOOLS` fallback is never the deciding factor in practice — but the *intent* is correct and would be correct once C1 is fixed.

### S2 — `spawn_agent` stays NeedsApproval; visibility ≠ auto-execution ✓

`SpawnAgentTool::safety()` returns `SafetyLevel::NeedsApproval` (`src/tool/agent/spawn_agent.rs:145-147`), unchanged. The ToolFilter change (`src/tool/mod.rs:207, 270`) makes `spawn_agent` *visible* in Planning/Complete (`name == "spawn_agent"`), but visibility only affects schema inclusion; execution still goes through the approval gate in `dispatch.rs:193-196` (`force_prompt = tool.never_auto() || tool.never_auto_for(...)`; `spawn_agent` is not `never_auto`, so it's governed by `approval::needs_approval` under the active safety mode). No auto-execution introduced. ✓

### S3 — Unknown role cannot spawn an unrestricted agent ✓

`SpawnAgentTool::execute` (`spawn_agent.rs:166-176`) rejects any non-empty role ≠ `"reviewer"` before spawning. Defensively, `compute_subagent_allowlist`'s `Some(other)` arm (`spawn.rs:294-301`) returns an empty tool list for an unknown role rather than the parent's full set — fail-closed. ✓ (Though note: the `Some(other)` arm is unreachable in practice because the tool validates first; it's belt-and-suspenders, which is fine.)

### S4 — Subagents still can't mutate plans ✓

`plan_mutations_allowed` is unchanged. `spawn_agent_shared` sets `agent_loop.set_plan_mutations_allowed(false)` + `wf.set_plan_mutations_allowed(false)` for any `parent_id.is_some()` spawn (`spawn.rs:120-130`), and `dispatch.rs:113-125` denies `create_plan/update_plan/complete_step/abandon_plan` when `!plan_mutations_allowed`. The new state-transition gate (`dispatch.rs:144-167`) is an *additional* restriction layered on top; it does not weaken this. ✓

---

## Correctness — the state-transition gate (goal 3)

### C2 — No deadlock: the workflow lock is dropped before the tracker await ✓

`dispatch.rs:94-107` acquires `self.workflow.lock().await` inside a `{ … }` block and the guard is dropped at the block's close (line 107), **before** the state-transition gate at lines 144-167. The gate's `tracker.has_running_descendants(id).await` (line 157) runs with no workflow lock held. The `IpcSpawner::has_running_descendants` impl (`spawn.rs:413-416`) acquires only `self.manager` (a separate `tokio::Mutex<AgentManager>`), calls the synchronous `AgentManager::has_running_descendants` (`src/runtime/mod.rs:140-144`, a pure in-memory walk over `self.agents` with no await), and releases it. No lock is held across an await. ✓

### C3 — The gate covers every state transition ✓

The `matches!` at `dispatch.rs:144-154` lists: `create_plan | update_plan | complete_step | abandon_plan | skill_start | skill_end | abandon_skill | finish` — all 8 state-transition tools. Cross-checking against `Workflow`'s transition methods (`src/workflow/mod.rs`): `create_plan` (→ Executing, :322), `complete_step` (→ Reviewing/Executing, :469/474), `abandon_plan` (→ Planning/Executing, :516-520), `start_skill`/`end_skill`/`abandon_skill` (:552/564/577), `finish` (→ Complete, :501), `update_plan` (state-preserving but mutates the active plan). Every method that mutates `self.state` or the plan stack is gated. `update_plan` doesn't change state but is correctly included (it mutates the active plan, which the gate's comment justifies). ✓

### C4 — Gate is a no-op when no tracker is wired (tests) ✓

`dispatch.rs:155-166`: the gate only fires `if let Some(tracker) = &self.descendant_tracker`. Loops built without a tracker (tests, or before IPC wiring) skip the gate — backward-compatible. The test `dispatch_no_tracker_allows_state_transition` (`src/agent/tests.rs`) confirms create_plan proceeds. ✓

### C5 — `agent_id()` is correctly consulted ✓

The gate uses `self.agent_id()` (`loop_impl.rs:448-450`), which the factory sets via `with_agent_id` at build time. For the main agent + tool-spawned subagents this is always `Some`. The `if let Some(id)` guard is defensive for test-built loops. ✓

---

## Correctness — trait decoupling (goal: clean brain/IPC seam)

### C6 — The trait decoupling is clean; no reverse dependency ✓

- `DescendantTracker` is defined in the brain crate (`src/runtime/mod.rs:244-264`), alongside `AgentSpawner`/`ParentAwareSpawner`. It depends only on `AgentId` (a brain type).
- The concrete impl is in the IPC layer (`src-tauri/src/ipc/spawn.rs:405-417`), backed by `AgentManager` (brain) via the spawner's own `manager` handle.
- The brain reaches it through `AgentLoop::descendant_tracker: Option<Arc<dyn crate::runtime::DescendantTracker>>` (`loop_impl.rs:121-126`) + `with_descendant_tracker` builder (`loop_impl.rs:257-267`) + `AgentLoopFactory::set_descendant_tracker`/`build_inner` wiring (`factory.rs:201,210,348-357`).
- `src-tauri/src/main.rs:91-99` builds one `IpcSpawner` and hands clones to both `set_spawner` (as `Arc<dyn AgentSpawner>`) and `set_descendant_tracker` (as `Arc<dyn DescendantTracker>`) via unsize coercion. No `src-tauri` type leaks into `src/`. ✓

---

## Constitution compliance

### CC1 — Warning-free under `#![deny(warnings)]`; no new `#[allow(...)]` ✓

Searched `src/` for `#[allow(`: the only matches are the **pre-existing** `#[allow(clippy::too_many_arguments)]` on the two `AgentLoop` constructors (`src/agent/loop_impl.rs:174, 211`) — not added by this diff, and not a warning-suppression (it's a clippy style lint, predates this change). No new `#[allow(...)]` was introduced to silence warnings. The new code (trait, impl, gate, factory wiring, tests) introduces no unused imports/dead code. ✓ (Caveat: I cannot run `cargo test` myself; the main agent's step-10 instruction is to confirm `cargo test` passes with zero warnings. The C1 bug does not cause a compile error or warning — it's a logic bug — so a green `cargo test` is *consistent* with C1 being present, which is exactly why the test suite didn't catch it.)

### CC2 — Windows paths / PowerShell ✓

No shell commands introduced by this diff. The new code is pure Rust (traits, async fns, tests). The `IpcSpawner` uses `tokio::sync::Mutex` and `std::collections::HashMap` — cross-platform. No Linux paths or bash syntax. ✓

### CC3 — Doc comments on public functions ✓

`DescendantTracker::has_running_descendants` (`runtime/mod.rs:260-263`), `AgentLoop::with_descendant_tracker` (`loop_impl.rs:256-262`), `AgentLoop::tools` (`loop_impl.rs:548-553`), `AgentLoopFactory::set_descendant_tracker` (`factory.rs:201-212`), and the `IpcSpawner` impl (`spawn.rs:407-412`) all have doc comments. ✓

### CC4 — Line-ending preservation ✓

The diff touches existing files with targeted edits (file_edit-style changes to `dispatch.rs`, `factory.rs`, `loop_impl.rs`, `main.rs`, `tool/mod.rs`, `runtime/mod.rs`); the file tools normalize to the file's detected style. No mixed `\r\n`/`\n` introduced. (The `git diff` CRLF warnings are on `.coding/*.json` bookkeeping files, not source.) ✓

### CC5 — No commit to main ✓

No commits in this diff; all changes are uncommitted on the working tree. ✓

---

## Verdict

**Request changes.** One **high-severity** correctness/security finding (C1): the `parent_allows` closure in `compute_subagent_allowlist` never actually consults the parent's `ToolFilter` because its first disjunct (`ToolCategory::Memory`, which is unconditionally `true`) short-circuits the check. This breaks the "subagent ⊆ parent" property for unrestricted spawns from Planning/Complete — the subagent can be granted mutation tools the parent's state hides. The fix is to query the filter with each registered tool's *actual* category+safety (read off the `&dyn Tool`) instead of the name-only closure, and drop the Memory term. One **medium** finding (B1: `dedup()` semantics/comment mismatch). Everything else — the dispatch gate, deadlock-avoidance, trait decoupling, spawn_agent visibility/NeedsApproval, role-tool safety, plan-mutation restriction, constitution compliance — verifies clean.
