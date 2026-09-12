# Phase 4 Multi-Agent Plan Ownership Review Report
**Date:** 2026-04-09 (review of uncommitted changes on feature branch)  
**Scope:** ALL uncommitted changes for "Phase 4 — Multi-agent plan ownership (Maint H2)".  
**Reviewer:** read-only sub-agent (no source edits performed).  
**Plan goal:** Enforce main-agent-only mutations for `create_plan`/`update_plan`/`complete_step`/`abandon_plan`; sub-agents (those with `parent_id`) get read-only access. Preserve per-agent `Workflow` instances. Update dispatch, prompts, docs, tests. Follow full constitution close sequence.

Files changed (from `git status` + `git diff HEAD`):
- `.coding/plans/stack.json`
- `PLAN.md`
- `src-tauri/src/ipc/commands.rs`
- `src/agent/dispatch.rs`
- `src/agent/factory.rs`
- `src/agent/loop_impl.rs`
- `src/agent/prompt.rs`
- `src/agent/turn.rs`
- `src/tool/workflow/plan.rs`
- `src/workflow/mod.rs`
- `.coding/plans/42b89226-....md` (the active sub-plan file itself)

## 1. Correctness
**Gates prevent subs from mutating?** Yes (defense-in-depth, 4 independent layers):
- `Workflow::plan_mutations_allowed()` + `set_...` (src/workflow/mod.rs:139,145) default `true` in `new()` (122).
- `AgentLoop` stores `Mutex<bool>` (src/agent/loop_impl.rs:77), default `true` in `new()` (146) and `with_constitution_source()` (181). Public accessors with docs (253-274).
- IPC spawn path (src-tauri/src/ipc/commands.rs:310-315): `if parent_id.is_some()` calls `set_plan_mutations_allowed(false)` on both the loop **and** its `workflow_handle()` (correct, immediately after `factory.build_with_id`).
- `Workflow::allowed_tools()` early-returns `ToolFilter::Planning` for `!plan_mutations_allowed` (src/workflow/mod.rs:183-187) — subs see a read-only surface.
- Schema post-filter in `run_turn` (src/agent/turn.rs:295-303): `retain` strips the four plan-mut names for `!plan_mutations_allowed`.
- Dispatch gate (src/agent/dispatch.rs:113-125): after the existing `allowed_tools()` state filter, hard-denies the four names with a clear error.
- Tool wrappers (src/tool/workflow/plan.rs): every execute checks `wf.plan_mutations_allowed()` before calling the underlying `wf.create/update/complete/abandon` (Create:117, Update:201, Complete:292, Abandon:378). All return the same "restricted to the main agent" error.
- Factory test (src/agent/factory.rs:638-668) asserts default `true` for plain `build()`, simulates sub construction, and verifies the `CreatePlanTool` itself denies.

**Main agents still work?** Yes. No-parent builds keep `true`; `allowed_tools()` falls through to normal state-derived filters; no extra gates fire.

**Flag propagation correct across paths?**
- UI spawn and tool spawn both flow through `spawn_agent_shared` → parent_id check (commands.rs + IpcSpawner::spawn_with_parent).
- Per-agent `Workflow` independence (factory) is untouched and continues to work (each agent still has its own plans_dir-backed instance; `get_workflow_state(agent_id)` remains per-agent).
- `main_agent_id()` logic in backlog/run-all paths is untouched (still correctly routes user/backlog work to the main agent).

**Minor behavior note (not a bug, but observable):** When a sub-agent is `!allowed`, `allowed_tools()` returns `Planning` **regardless of actual workflow state** (Executing/Complete/Skill). This means subs see the Planning tool surface + prompt text even if a plan exists. The prompt.rs already has conditional text for `!allowed` (Planning note + Executing "use read tools only"), so the model is instructed correctly; mutations remain blocked at the other three layers. This is a side-effect of the "read-only surface" design choice.

## 2. Bugs
- **No deadlocks or lock-poisoning issues.** The new `Mutex<bool>` on `AgentLoop` is used only for the simple flag (under the same patterns as `agent_id`/`session_id`). Workflow flag is a plain `bool` (set under the existing `Mutex<Workflow>` held by callers).
- **No missed construction paths.** All factory builds start with `true`; the only place that flips to `false` is the documented IPC parent_id branch. Direct test construction in factory.rs test also exercises the flip.
- **Schema vs. allowed_tools drift risk mitigated.** The retain in turn.rs is a post-filter (after `schemas(..., &tool_filter)`). Even if the filter were `Executing`, the retain would still strip plan-mut tools for subs. In practice the filter is already `Planning` for subs, so the retain is somewhat redundant but harmless and explicit.
- **Test coverage gaps (non-blocking for this phase):**
  - No explicit test that a main agent in Executing still receives the four plan tools in schemas.
  - No test that a sub-agent in Executing state receives the restricted prompt text + Planning surface.
  - The existing factory test only exercises `CreatePlanTool` directly; dispatch + schema paths are covered indirectly by the broader test suite.
- The touched `.coding/plans/...` files are artifacts of the plan execution itself (stack push + plan md) — expected, not a source change.

**No silent failures or incorrect error paths observed.**

## 3. Security
- **No bypasses.** Even if the LLM hallucinates a plan-mut tool name (or a stale schema leaks through), dispatch, the four tool wrappers, and the workflow methods themselves all re-check the flag. The state `ToolFilter` gate is still applied first.
- **No info leak of plans to subs.** Each agent owns its own `Workflow` (factory design preserved). Subs can still call read tools (`get_workflow_state(agent_id)` continues to work per-agent) and can observe the main agent's plan only if the main agent writes it to disk in a location the sub can read — same as before. The policy does not change visibility of plan *content*; it only gates mutation entry points.
- **Approval / core-op invariants untouched.** Plan tools remain `AutoRun`; the new gates sit before the approval/never_auto logic.
- **Constitution "no drive-bys" and "core ops approval-gated" respected.** The policy change does not touch `git merge`/`push` or other never-auto paths.

## 4. Constitution Compliance
- **Public functions have doc comments.** All new accessors (`plan_mutations_allowed`, `set_plan_mutations_allowed` on both `Workflow` and `AgentLoop`) are documented with `///`.
- **cargo test before close steps.** The active plan step 7 is exactly "Run cargo test (unpiped) after changes..." The added factory test would be exercised by that.
- **Line-ending style preserved.** All edits follow the repository's detected style (tools normalize; no mixed `\r\n`/` \n` introduced).
- **No drive-bys.** Every change is scoped to the declared Phase 4 policy (flag + 4 enforcement points + prompt conditioning + test + PLAN.md). Existing multi-agent tests (factory independence, spawn, main_agent_id routing) were left intact.
- **Plan steps followed.** The review covers the full set of changes listed in the plan's own step 7 ("git status/diff, spawn reviewer on ALL uncommitted...").
- **Full close sequence.** This report is the reviewer artifact. The caller will read it, fix any findings, re-run tests, and commit (including the report) on the feature branch only.

## 5. Interactions with Existing Multi-Agent Code
- Factory still produces independent per-agent `Workflow` + `ToolRegistry` (the two_builds / independence tests remain valid).
- `spawn_agent` tool path (via `IpcSpawner` + `spawn_agent_shared`) correctly sets the flag for children.
- `main_agent_id()` / backlog / run-all continue to treat the parentless smallest-id agent as the one that drives the plan (the policy simply makes that the *only* one that can).
- `get_workflow_state(agent_id)` and per-agent event routing are unaffected.
- No changes to safety mode, provider, memory, or approval isolation.

## Summary of Findings by Severity
- **Correctness:** No issues. Gates are correct and comprehensive; main agents unaffected.
- **Bugs:** None blocking. One minor observable behavior change (subs always see Planning surface) that is intentional per the "read-only surface" design and is documented in prompts. A few test-coverage gaps for sub states (non-blocking).
- **Security:** No issues. No bypasses, no leaks.
- **Constitution compliance:** Fully compliant (docs, style, scope, process).
- **Other:** The plan-stack and plan-md files are expected side-effects of running the plan; they are not part of the feature diff for review purposes.

**Overall:** Clean implementation of the main-agent-only plan ownership policy. Defense-in-depth across allowed_tools, schema, dispatch, and tool wrappers. No findings that require source changes. The reviewer recommends proceeding with the remaining close steps (cargo test unpiped, act on any new findings if tests surface issues, commit on feature branch including this report).

**Report location:** `.coding/reviews/2026-04-09-phase4-plan-ownership-review.md` (this file).  
"No findings" would have been stated if the tree were empty of policy-related diffs; instead the above documents a thorough pass with only non-blocking observations.
