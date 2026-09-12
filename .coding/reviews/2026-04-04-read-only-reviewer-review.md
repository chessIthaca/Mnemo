# Review: Enforced read-only reviewer subagent

**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD` + untracked new files).
**Goal:** Make the reviewer subagent truly read-only by construction via a `role: "reviewer"` arg on `spawn_agent` that constrains the spawned agent to a read-only tool allow-list (`ToolFilter::Skill`), plus two new tools (`git_diff`, `write_review_report`).

## Summary

The change is **correct and secure**. The allow-list override is honored in both enforcement paths (schema building in `turn.rs`, re-check in `dispatch.rs`), is in-memory only (not persisted in `StackSidecar`, so it cannot clobber the shared on-disk plan state), and genuinely denies the reviewer every write/exec/spawn/finish tool. The path-traversal guard in `write_review_report` is sound. Unknown roles error before spawning. All 8 `AgentSpawner`/`ParentAwareSpawner` implementors were updated with the new `role` parameter — no signature mismatches.

One **low-severity** bug found (incomplete sub-path support in `write_review_report`).

---

## Correctness — no findings

### C1 — Allow-list overrides the state-derived filter in all paths ✓

- **`Workflow::allowed_tools()`** (`src/workflow/mod.rs:259-281`): the `tool_allowlist` check is the first branch, returning `ToolFilter::Skill(list.clone())` before the `match (&self.state, &self.active_skill)`. So the constraint holds regardless of the reviewer's (shared) `Reviewing` state.
- **Schema building** (`src/agent/turn.rs:292-301, 320`): reads `wf.allowed_tools()` into `tool_filter`, then `self.tools.schemas(caps, &tool_filter)`. `ToolRegistry::schemas` (`src/tool/mod.rs:329-349`) calls `filter.allows(category, safety, name)` per tool. For `ToolFilter::Skill`, `allows` (`src/tool/mod.rs:281-293`) returns `true` only for `ToolCategory::Memory`, `ask_user`, `current_plan`, or names in the allow-list. So the reviewer's visible tool set is exactly its allow-list + memory + ask_user + current_plan.
- **Dispatch re-check** (`src/agent/dispatch.rs:94-107`): re-reads `wf.allowed_tools()` and denies any tool `!filter.allows(...)`. A hallucinated write tool name is denied here even if it somehow appeared. ✓

### C2 — Allow-list is in-memory only (not persisted) ✓

`StackSidecar` (`src/workflow/mod.rs:100-109`) has only `stack`, `skill`, `reviewed` — no `tool_allowlist`. `persist_stack()` (`src/workflow/mod.rs:590-600`) constructs the sidecar from those three fields and never touches `tool_allowlist`. So the allow-list cannot leak onto the shared on-disk plan state. It is set on the reviewer's *own* `Workflow` instance (`agent_loop.workflow_handle()` at `src-tauri/src/ipc/spawn.rs:140`), which is a fresh per-agent instance built by `factory.build_with_id` — not the main agent's workflow. ✓

### C3 — Reviewer is denied every write/exec/spawn/finish tool ✓

The allow-list (`src-tauri/src/ipc/spawn.rs:143-152`) is `[file_read, read_files, search, search_read, describe_image, list_models, git_diff, write_review_report]`. Under `ToolFilter::Skill`:
- `file_edit`, `file_write`, `file_append`, `shell`, `git`, `spawn_agent` — `ToolCategory::Agent`, not in the list → denied.
- `finish` — `ToolCategory::Workflow`; the Skill arm allows only `ask_user`/`current_plan`/allow-listed names; `finish` is none of these → denied. This is important: the reviewer shares the plan on disk, so denying `finish` prevents it from flipping the shared `reviewed` flag and transitioning the plan to `Complete`. ✓
- `create_plan`/`update_plan`/`complete_step`/`abandon_plan` — `ToolCategory::Workflow`, not `ask_user`/`current_plan`/allow-listed → denied. (Also redundantly blocked by the `plan_mutations_allowed=false` gate at `dispatch.rs:113-125`, since the reviewer is a sub-agent with a parent.)

### C4 — No race between allow-list setup and first turn ✓

In `spawn_agent_shared` (`src-tauri/src/ipc/spawn.rs`): `build_with_id` (line 109) → `set_tool_allowlist` (lines 139-157) → register + spawn task (lines 159-170) → send initial prompt (lines 178-184). The task's `run` blocks on `cmd_rx` until the prompt arrives, which is after the allow-list is set. No window where the reviewer could run with an unrestricted filter. ✓

### C5 — All spawn-trait implementors + callers updated ✓

All 8 implementors of `AgentSpawner`/`ParentAwareSpawner` now carry the `role: Option<String>` parameter:
- `IpcSpawner::spawn` / `spawn_with_parent` (`src-tauri/src/ipc/spawn.rs:224, 250`)
- `MockSpawner` (`src/agent/factory.rs:742`, `src/tool/agent/spawn_agent.rs:264`)
- `ParentAwareMock` (`src/tool/agent/spawn_agent.rs:347, 364`)
- `ModelRecordingSpawner` (`src/tool/agent/spawn_agent.rs:536, 551`)

All callers of `spawn_agent_shared` pass `role`: the UI `spawn_agent` command (`spawn.rs:47`, `None`), `main.rs` (`None`), and the `spawn_agent` tool (`role.clone()`, `spawn_agent.rs:209, 224`). No missed call sites. ✓

---

## Bugs

### B1 (low) — `write_review_report` sub-path support is incomplete; `create_dir_all(parent)` is dead code

**File:** `src/tool/agent/write_review_report.rs:51-96, 158-167`

The `path` arg doc (`:23-25`) states sub-paths are supported ("Must be a bare filename or a relative sub-path"), and the comment at `:158-159` explicitly intends to handle them: *"a sub-path like \"2026/04/report.md\" may have intermediate dirs that canonicalize couldn't create."* The `create_dir_all(parent)` at `:160-167` was added to create those intermediate dirs.

But this is unreachable for the case it targets. For a sub-path `2026/04/report.md` where `2026/` does not yet exist under `reviews_dir`:
1. `resolve_under_reviews` joins `target = reviews_dir/2026/04/report.md` (`:61`).
2. `target.canonicalize()` fails (file doesn't exist) → falls to the `or_else` branch (`:68`).
3. `parent = target.parent()` = `reviews_dir/2026/04` (`:72-74`).
4. `parent.canonicalize()` **fails** because `reviews_dir/2026/04` doesn't exist — `canonicalize` requires the path to exist on the filesystem (`:75`).
5. `resolve_under_reviews` returns `Err` → `execute` returns the error at `:155`, **before** reaching `create_dir_all(parent)` at `:160`.

So `create_dir_all(parent)` at `:160-167` is dead code for the sub-path case (and a no-op for the bare-filename case, where the parent is `reviews_dir`, already created at `:64`). A sub-path with a non-existent intermediate directory fails with "failed to resolve reviews dir parent …" rather than creating the dirs and writing.

**Impact:** Low. The primary/intended use is a bare filename (the schema example is `2026-04-04-my-feature-review.md`), which works correctly. The reviewer is instructed to use bare filenames. The failure is graceful (error result, no wrong write). But the documented sub-path support does not actually work.

**Suggested fix:** In `resolve_under_reviews`, before canonicalizing the parent, call `std::fs::create_dir_all(parent)` so the parent exists and `canonicalize` succeeds. (Or create all intermediate dirs up front, then canonicalize the target directly.) Alternatively, narrow the doc to "bare filename only" if sub-paths aren't needed.

---

## Security — no findings

### S1 — Cannot escape `.coding/reviews/` via `write_review_report` ✓

`resolve_under_reviews` (`src/tool/agent/write_review_report.rs:51-96`):
- Absolute paths rejected early (`:56-60`).
- `..` traversal: `target = reviews_dir/../evil.md` canonicalizes (via the parent-fallback) to `<temp>/evil.md`, which `starts_with(canon_reviews)` is false → rejected. Test `rejects_path_traversal` (`:212-229`) confirms, and asserts nothing was written outside.
- Symlinks: `canonicalize` resolves symlinks, so a symlink inside `reviews_dir` pointing outside would canonicalize outside and fail the `starts_with` check.
- The guard mirrors `FinishTool`'s validation (`src/tool/workflow/plan.rs:521-526`), which prior reviews confirmed is the standard Rust path-containment pattern.

A minor TOCTOU exists between the canonicalize check and the write (same as FinishTool), but this is a local single-user tool and the reviewer is already constrained; not exploitable in this threat model.

### S2 — Unknown role cannot bypass the constraint ✓

`SpawnAgentTool::execute` (`src/tool/agent/spawn_agent.rs:162-173`): any non-empty role that isn't `"reviewer"` returns `ToolResult::error("unknown role …")` before any spawn occurs. Test `unknown_role_errors_without_spawning` (`:419-435`) confirms no spawn happens. `None` (no role arg) spawns an unrestricted sub-agent — the intended default. ✓

### S3 — `git_diff` is read-only and degrades gracefully ✓

`GitDiffTool` (`src/tool/agent/git_diff.rs`) runs only `git diff HEAD --stat`, `git diff HEAD`, `git status --short` — all read-only, discrete argv (no shell, no injection surface). `SafetyLevel::AutoRun`. In a non-git directory, git exits non-zero; the tool returns `success: false` with the git error message in `output` (no panic). In a repo with no uncommitted changes, all three exit 0 and `success: true`. ✓

---

## Constitution compliance — no findings

- **Windows paths/PowerShell:** New tools use `std::path::Path`/`PathBuf` (cross-platform) and `Command::new("git")` with discrete argv. The `rejects_absolute_path` test (`write_review_report.rs:232-247`) handles both `C:\Windows\…` and `/etc/…`. No Linux-only paths or bash syntax. ✓
- **Doc comments on public functions:** `GitDiffTool::new`, `WriteReviewReportTool::new`, `Workflow::tool_allowlist`/`set_tool_allowlist`, `ToolResult::with_data`, `AgentSpawner::spawn`, `ParentAwareSpawner::spawn_with_parent`, and the `IpcSpawner` impls all have doc comments. ✓
- **Line-ending preservation:** New files; the file tools normalize line endings. No mixed `\r\n`/`\n` introduced. ✓
- **No commit to main:** No commits in this diff; changes are uncommitted on the working tree. ✓
- **`cargo test` before step complete:** Not verifiable by this read-only reviewer, but the new code includes unit tests for the allow-list override (`workflow/mod.rs:999`), role forwarding (`spawn_agent.rs:399, 419, 447`), git_diff (`git_diff.rs:167`), and write_review_report (`write_review_report.rs:195`).

---

## Verdict

**Approve with one low-severity bug to address (B1).** The core security property — the reviewer is read-only by construction, enforced at both the schema and dispatch layers, in-memory only, with no path to a write/exec/spawn/finish tool — holds in all paths. The `write_review_report` sub-path issue is minor and does not affect the security or the primary use case.
