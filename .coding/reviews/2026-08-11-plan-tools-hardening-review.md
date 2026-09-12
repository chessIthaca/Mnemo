# Review: Plan-lifecycle hardening + clickable plan staircase

**Date:** 2026-08-11
**Scope:** All uncommitted changes in the working tree (`git diff HEAD`).
**Build status:** ✅ `cargo test` 588 passed / 0 failed; ✅ frontend `npm run build` ok; ✅ `vitest` 86 passed.

## Summary of changes reviewed

1. `current_plan` tool — read-only query of the active plan (id/title/counts/state). Registered in factory + ToolFilter (all states).
2. `complete_step` plan-name echo + optional `plan_id` guard.
3. Clickable staircase: `WorkflowStateInfo.parents` `Vec<String>` → `Vec<PlanAncestorInfo { id, title }>`; new `get_plan` Tauri command reads a plan by id; frontend ancestor view.
4. StatusBar dropdown header shows active plan title.

---

## Findings

### 🔴 Security — `get_plan` has no path-traversal validation (defense-in-depth violation)

**File:** `src-tauri/src/ipc/agent.rs:474`

```rust
let path = plans_dir.join(format!("{plan_id}.md"));
if !path.exists() {
    return Err(format!("plan '{plan_id}' not found in {}", plans_dir.display()));
}
PlanFile::read_from_file(&path).map_err(...)
```

`plan_id` is an arbitrary `String` arriving over the Tauri IPC boundary (`get_plan(agent_id, plan_id)`). It is joined directly into a filesystem path with **no validation, no canonicalization, and no containment check**. `Path::join` does not sanitize `..` segments, so a `plan_id` of `../../Cargo` resolves to `<repo>/Cargo.md`, and `..\..\some\readable\file` (Windows) escapes the plans dir entirely.

The rest of the codebase is disciplined about this: every file tool routes user-supplied paths through `src/tool/agent/sandbox.rs` (`Sandbox::validate` → `canonicalize` + `starts_with(root)`), with explicit `path_traversal_rejected` tests (file_read/file_write/file_edit/describe_image). `get_plan` bypasses that sandbox entirely.

**Why it's exploitable, not just theoretical:** `PlanFile::parse` (`src/workflow/plan_file.rs:199`) is *lenient* — it reads the whole file and scrapes whatever `# Plan:` / `## Goal` / `## Context` / `- [ ]` lines it can find, defaulting to empty strings for missing sections rather than erroring. So a traversal id pointing at an arbitrary readable file returns a `PlanFile` (success, not error), and any text in that file matching the plan-section patterns is exfiltrated via the returned `title`/`goal`/`context`/`steps` fields to the renderer. Even on a parse miss, `read_from_file` reads the full file content into memory first.

The realistic caller (the staircase) only passes UUIDs sourced from the backend's `parent_infos()`, so this is not reachable via the normal UI flow today. But `get_plan` is a public IPC command — any renderer code (or a compromised/XSS'd renderer) can invoke it with an arbitrary string. This is exactly the class of input the sandbox module exists to neutralize.

**Fix:** validate `plan_id` before joining. Either (a) parse it as a `Uuid` (`Uuid::parse_str(&plan_id).map_err(...)`), or (b) canonicalize the joined path and assert it `starts_with(&plans_dir)`, mirroring `Sandbox::validate`. Option (a) is simplest and matches the fact that plan ids are always UUIDs (`create_plan` generates `Uuid::new_v4()`). Add a `path_traversal_rejected` test.

---

### 🟡 Bug (UX) — ancestor-view state is not cleared on agent switch

**File:** `frontend/src/components/views/PlanProgress.tsx:68-77`, `102`

When the user is viewing an ancestor plan (`viewingAncestor !== null`) and switches the active agent, the `activeAgent` effect (lines 68-77) calls `refresh()` which updates `plan`/`parents`/`state` but does **not** clear `viewingAncestor` / `viewingTitle` / `viewError`. The component early-returns at line 102 (`if (viewingAncestor)`) and keeps rendering the *previous agent's* ancestor plan — stale and cross-agent. The user is stuck looking at agent A's ancestor while agent B is active, with only a "back to active plan" button (which still works, but the stale display is confusing).

**Fix:** in the `activeAgent` effect, also reset `setViewingAncestor(null); setViewingTitle(""); setViewError(null);`.

---

### 🟡 Bug (UX) — failed ancestor fetch shows no error to the user

**File:** `frontend/src/components/views/PlanProgress.tsx:46-57`, `126-130`

`viewAncestor` sets `viewError` on failure but leaves `viewingAncestor` as its previous value (`null` on the first failed click). The error banner is rendered **only inside** the `viewingAncestor` block (lines 126-130). So when a fetch fails with `viewingAncestor === null`, the early return at line 102 is skipped and the active-plan view is shown — but the error banner, which lives exclusively in the ancestor-view branch, is never displayed. The error is set in state and silently never shown.

**Fix:** either render the `viewError` banner in the active-plan view too (above the staircase), or set `viewingAncestor` to a sentinel / show the error unconditionally at the top of the component. Simplest: lift the `{viewError && (...)}` banner out of the `viewingAncestor` branch to the top of both return paths (or to a shared wrapper).

---

### 🟢 Doc — `enter_skill` lost the first line of its doc comment

**File:** `src-tauri/src/ipc/agent.rs:480-494`

The `get_plan` function was inserted by replacing the original first line of `enter_skill`'s doc comment (`/// Enter a skill on the given agent: lock its workflow, call `start_skill`,`). The remaining lines (480-494) now attach to `enter_skill` but begin mid-sentence:

```
/// and send an `AgentCommand::Prompt` to the agent with the skill's goal. This
/// is the UI-initiated entry path (the "Merge to main" button's confirm
/// dialog). ...
```

`get_plan`'s own doc (453-458) is clean. Only `enter_skill`'s summary is affected — its rendered docs now start with a fragment. No `deny(warnings)` so the build is unaffected, but the constitution requires doc comments on public functions and this one is now malformed.

**Fix:** restore the summary line before line 480: `/// Enter a skill on the given agent: lock its workflow, call `start_skill`,`.

---

## Verified correct (no findings)

- **`plan_id` guard logic** (`plan.rs:312-326`): `if active_id != Some(provided_id.as_str())` correctly catches both wrong-id and no-active-plan cases. `None != Some(x)` is `true`, so a `plan_id` supplied while in Planning (no active plan) errors — correct, though normally unreachable since `complete_step` isn't in the Planning ToolFilter. The `Some`-vs-`Some` string comparison is correct. Tests `complete_step_plan_id_mismatch_errors` + `complete_step_plan_id_match_succeeds` cover both branches.
- **`current_plan` return data** (`plan.rs:438-527`): returns id/title/completed/total/depth/state when a plan is active; null id/title + zeros when in Planning. `plan_id()` + `plan()` are read consistently from the same locked workflow. Tests `current_plan_returns_active_plan` + `current_plan_returns_no_active_plan_in_planning` cover both.
- **ToolFilter registration** (`tool/mod.rs`): `current_plan` added to the Workflow branch of Planning, Executing, Complete, and Skill filters. `CurrentPlanTool::category()` returns `Workflow`, so the `name == "current_plan"` checks match. Factory test asserts `current_plan` is registered.
- **`parents` shape change**: `PlanAncestorInfo { id, title }` is `Serialize`-only (no `Deserialize`) — correct, since it's an output type. Both the Rust contract fixture (`contract_fixtures.rs:145`) and the frontend fixture (`dto-workflow-state-info-skill.json`) were updated to the new shape, and `ipc-contract.test.ts` passes. `parent_infos()` correctly slices `stack[..len-1]` (excludes the active plan) and maps `(id, title)`.
- **`plans_dir()` accessor** (`workflow/mod.rs:169-174`): read-only `&Path` borrow; the IPC command clones it to a `PathBuf` before dropping the workflow lock — no lock-held-during-IO issue.
- **StatusBar dropdown title** (`StatusBar.tsx:575`): `plan.title` is gated behind `pickerOpen && hasPlan` where `hasPlan = plan !== null && plan.steps.length > 0`, so no null deref.
- **Sub-agent registration**: `current_plan` is read-only, so registering it for sub-agents (whose `plan_mutations_allowed = false`) is safe — it only reads the workflow it's bound to.
- **Tests + build**: 588 Rust tests pass (incl. 5 new plan-tool tests), 86 vitest tests pass, frontend build succeeds.

---

## Constitution compliance

- ✅ Public functions have doc comments (except the `enter_skill` malformation noted above).
- ✅ `cargo test` run before review; passes.
- ✅ Line-ending style preserved (file tools normalize automatically).
- ✅ No commit to main.
