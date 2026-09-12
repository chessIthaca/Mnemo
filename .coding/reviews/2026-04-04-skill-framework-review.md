# Review: General skill framework + reworked merge-to-main

**Date:** 2026-04-04
**Scope:** All uncommitted changes in the working tree (`git status` / `git diff HEAD`).
**Reviewer:** read-only subagent

The change set introduces a general skill framework (file-based registry in
`.coding/skills/*.toml`, `skill_start`/`skill_end`/`abandon_skill` tools, a
`Skill` workflow state with an allow-list tool filter + prompt overlay) and
reworks merge-to-main onto it as the first skill — agent-driven, with conflict
resolution, returning to Planning on success. Core git operations (merge/push)
are always approval-gated via a new `Tool::never_auto_for(args)` trait method.

Findings are listed by severity below. Line numbers cite the current (modified)
files.

---

## Correctness

### C1 — Low — `skill_start` is hidden in Planning by the filter, contradicting the design + the skill's `available_in`

**Files:** `src/tool/mod.rs:183`, `src/tool/mod.rs:566-583`, `.coding/skills/merge_to_main.toml:12`

The design (and the skill file) declare `merge_to_main` as `available_in =
["complete", "planning"]`, and the `SkillStartTool::execute` validates
`available_in` against the current state (so it *would* accept a start from
Planning). But the `ToolFilter::Planning` arm only allows `create_plan` for the
`Workflow` category:

```rust
ToolFilter::Planning => match category {
    ...
    ToolCategory::Workflow => name == "create_plan",   // line 183
```

So `skill_start` is **never visible** in Planning — the agent cannot call
`skill_start("merge_to_main")` from Planning even though the registry says it's
allowed there. The `skill_start_gating` test (`src/tool/mod.rs:580`) even
asserts `!visible(ToolFilter::Planning)`, codifying the contradiction, and its
doc comment (`src/tool/mod.rs:566-570`) hand-waves that "Planning is gated at
the registry level" — but the registry *permits* Planning, so the filter is the
thing actually blocking it.

This is internally inconsistent: the registry's `available_in` and the
`ToolFilter` disagree about Planning. Either:
- the filter should expose `skill_start` in Planning too (`name == "create_plan"
  || name == "skill_start"`), matching the skill file + the tool's own
  validation; or
- the skill file's `available_in` should drop `"planning"` and the
  `SkillStartTool`/`StatusBar` should reflect that skills start only from
  Complete.

The UI button (`StatusBar.tsx:519`) *does* show in Planning, and `enter_skill`
validates via `is_available_in` (which permits Planning) — so the **UI path**
works from Planning, but the **agent-driven `skill_start` path** does not (the
tool is invisible to the model in Planning). The two entry paths are
inconsistent.

**Recommendation:** add `|| name == "skill_start"` to the `ToolFilter::Planning`
`Workflow` arm (line 183) so the agent can start a skill from Planning, matching
the registry + the UI. Update the `skill_start_gating` test to assert visible in
Planning.

### C2 — Low — `git` tool is invisible in Planning/Complete (reads included), so the merge skill can't run `git status` from those states

**Files:** `src/tool/agent/git.rs:142-149`, `src/tool/mod.rs:179`, `src/tool/mod.rs:210`

`GitTool::safety()` returns `SafetyLevel::NeedsApproval` for **all** subcommands
(including read-only `status`/`diff`/`log`). The `ToolFilter::Planning` and
`ToolFilter::Complete` arms use `ToolCategory::Agent => safety ==
SafetyLevel::AutoRun`, so the `git` tool is **hidden entirely** in Planning and
Complete.

This is pre-existing (the git tool always returned `NeedsApproval`), so it's not
a regression introduced by this change. But it now matters for the skill
framework: the `merge_to_main` skill's prompt (`.coding/skills/merge_to_main.toml:27`)
tells the agent to "Check `git status`" as step 1, and the skill's `tools` list
includes `"git"`. Once the skill is active the `Skill` filter exposes `git`
(regardless of safety), so this works *inside* the skill. The only gap is that
the agent cannot probe `git status` *before* starting the skill from Planning/
Complete — but that's an exploration limitation, not a correctness bug, and the
skill itself is unaffected. Noting for completeness; no action required unless
the team wants `git status/diff/log` to be `AutoRun`.

### C3 — Info — `enter_skill` does not check whether the agent is busy

**File:** `src-tauri/src/ipc/commands.rs:534-598`

`enter_skill` locks the workflow, calls `start_skill`, then sends an
`AgentCommand::Prompt`. If the agent is mid-turn (streaming), the `Prompt` is
buffered as a `(new prompt) ...` suggestion (`src/agent/turn.rs:364-369`) and
injected as a system message at the next turn boundary — so it won't be lost,
but it also won't *drive* the skill until the current turn finishes. The
workflow state has already flipped to `Skill` (and the tool filter with it), so
the in-flight turn's tool schemas were already built from the prior filter —
the agent could keep calling now-hidden tools until the turn ends.

This is a benign race (the skill overlay applies on the *next* turn; the
in-flight turn completes with its already-built schema). The design brief asked
specifically about the busy case: it is handled (no crash, no lost prompt), just
not preemptive. Acceptable; noting for completeness.

---

## Bugs

### B1 — Low — `SkillRegistry::insert` is `#[cfg(test)]` but `load_dir` bypasses it (inconsistent, not a bug at runtime)

**File:** `src/skill/mod.rs:64-69`, `src/skill/mod.rs:88`

`SkillRegistry::insert` is gated `#[cfg(test)]` with a doc comment "used by tests
+ the loader", but `load_dir` (line 88) inserts directly via
`registry.skills.insert(...)`, not via the `insert` method. So the loader does
not actually use `insert`. The doc comment is misleading. Not a runtime bug
(behavior is correct), just a stale comment + a slightly odd cfg gate. The
`insert` method is only used by tests (`src/tool/workflow/skill.rs:259`,
`src/skill/mod.rs` tests). Either fix the doc comment ("used by tests only") or
have `load_dir` call `insert` (which would require dropping the `#[cfg(test)]`).

### B2 — Info — `enter_skill` comment block is stale/misleading

**File:** `src-tauri/src/ipc/commands.rs:546-551`

The inline comment says "The factory doesn't expose the registry directly, so we
re-derive it from the project's skills dir — but that's wasteful per call.
Instead, reach the agent's loop + its workflow, and validate via the registry
stored on the factory. The factory exposes the registry through a helper." This
narrates a discarded approach (re-deriving from the dir) that the code does not
do. The code correctly uses `factory.skills_handle()`. The comment should be
trimmed to just "Look up the skill in the registry held by the factory."

### B3 — Info — `git` tool schema description is stale

**File:** `src/tool/agent/git.rs:122-123`

The schema `description` still says "Supports: status, diff, log (read-only,
auto-run), and commit (requires approval)." — it does not mention the new
merge/checkout/stash/branch/push subcommands. The `enum` (line 129) is correct;
only the prose is stale. Low impact (the model sees the enum), but worth fixing
for accuracy.

---

## Security

### S1 — Info — `valid_branch_name` allows refspecs that git would reject, but rejects the dangerous ones

**File:** `src/tool/agent/git.rs:62-70`

`valid_branch_name` rejects empty, leading-`-`, and whitespace-containing names.
This blocks the flag-injection vector (`--no-commit`, `-b`) and shell-splitting
issues. It does *not* block other git-ref-illegal characters (e.g. `..`, `~`,
`^`, `:`, `\`, control chars), but since args are passed as discrete `Command`
argv (no shell), the worst a weird name does is make git error out — no
injection. The guard is sufficient for the security goal (no flag injection).
No action required.

### S2 — Info — `git push` without a branch pushes the current branch to the remote

**File:** `src/tool/agent/git.rs:309`

When `branch` is omitted, `push` runs `git push <remote>` (no explicit refspec).
On some git configs this pushes the current branch to a same-named remote branch
(`push.default=upstream`/`current`). This is a core operation (always
approval-gated via `never_auto_for`), so the user sees and approves it — no
security gap. The skill prompt (`merge_to_main.toml:34`) tells the agent to use
`git push` to push main. Acceptable; noting that the behavior of a no-branch
push depends on the repo's `push.default`.

---

## Constitution compliance

### CC1 — Pass — Core operations (merge/push) are always approval-gated, even in Autonomous mode

**Files:** `src/tool/mod.rs:116-130`, `src/tool/agent/git.rs:151-162`, `src/agent/dispatch.rs:83-96`, `agent.md:38-44`

The hard rule ("core operations always gated") is **enforced correctly**:

- `Tool::never_auto_for` (default `false`) is overridden by `GitTool` to return
  `true` for `merge`/`push` (`is_core_operation`, `git.rs:58-60`).
- `dispatch.rs:83` computes `force_prompt = tool.never_auto() ||
  tool.never_auto_for(&parsed_call.arguments)`.
- The safety-rules shortcut is gated on `!force_prompt` (`dispatch.rs:91`), so a
  stored safety rule **cannot** auto-approve a `git merge`/`git push`.
- Under `SafetyMode::Autonomous`, `needs_approval` returns `false` for
  everything (`approval.rs:80`), but `force_prompt` still forces the prompt
  branch (`dispatch.rs:84`).
- Test `git_merge_prompts_even_in_autonomous_mode` (`src/agent/tests.rs:600`)
  asserts an `ApprovalRequest` is emitted for `git merge` under Autonomous mode.

The constitution's invariant holds. ✅

### CC2 — Pass — `never_auto()` (no-arg) is no longer overridden by any tool

**File:** `src/tool/mod.rs:100-114`

With `MergeToMainTool` deleted, no tool overrides `never_auto()` to `true`. The
trait method exists with a default `false`, and the dispatch still consults it
(`tool.never_auto()`). The doc comment on `never_auto` (lines 100-111) still
cites `merge_to_main` as "the one example today" — that's now stale (see B4
below), but the *mechanism* is correct and the `never_auto_for` path carries the
guarantee. No compliance issue.

### CC3 — Pass — Skill entry is not approval-gated, but performs no mutation

**Files:** `src/tool/workflow/skill.rs:102-108`, `src/workflow/mod.rs:391-413`

`skill_start`/`skill_end`/`abandon_skill` are all `AutoRun`. This is consistent
with the design ("protection is on the operations, not the entry"): starting a
skill only changes tool visibility + the prompt; it performs no repo mutation.
The dangerous operations inside the skill (`git merge`/`push`) are gated by
`never_auto_for`. The constitution's "never commit to main except via the
merge_to_main skill" rule is preserved — the only path to land commits on main
is inside the skill, and the merge itself is always approval-gated. ✅

### CC4 — Pass — `enter_skill` validates `available_in` before starting

**File:** `src-tauri/src/ipc/commands.rs:566-574`

`enter_skill` reads the current workflow state, checks
`registry.is_available_in(&skill, current)`, and returns an error if the skill
isn't available in that state. So the UI button can't start a skill from
`Executing` (where `merge_to_main` is not available). ✅ (Note: this is
consistent with the registry, though see C1 for the filter-level inconsistency
on the agent-driven path.)

---

## Additional notes (not findings)

### B4 — Low — Stale `never_auto` doc comment references deleted `merge_to_main`

**File:** `src/tool/mod.rs:105-111`

The doc comment on `never_auto` says "The one example today is `merge_to_main`"
and describes the old atomic tool. `MergeToMainTool` is deleted; no tool
overrides `never_auto()` to `true` anymore. The example should be updated to
reference `never_auto_for` (the git merge/push case) or removed. Cosmetic;
doesn't affect behavior.

### N1 — Info — `dispatch.rs:74` comment references `merge_to_main`

**File:** `src/agent/dispatch.rs:74`

The comment "A tool with `never_auto()` (e.g. merge_to_main)" cites the deleted
tool. Should say "e.g. `git merge`/`git push` via `never_auto_for`". Cosmetic.

### N2 — Pass — Persist/load round-trips the active skill correctly; legacy bare-array sidecar still readable

**Files:** `src/workflow/mod.rs:82-92` (`StackSidecar`), `src/workflow/mod.rs:446-455`
(`persist_stack`), `src/workflow/mod.rs:467-531` (`load_latest`)

- `persist_stack` writes `{"stack":[...],"skill":{...}|null}` (skill omitted when
  `None` via `skip_serializing_if`).
- `load_latest` tries `StackSidecar` first; on failure falls back to the legacy
  bare-array `serde_json::from_str(&text).unwrap_or_default()` (line 488).
- A persisted skill is restored: state set to `Skill`, overlay repopulated
  (lines 517-522), short-circuiting the plan-derived state.
- Test `skill_persists_across_load_latest` (line 1020) covers the round-trip;
  `load_latest_reads_legacy_bare_array_sidecar` (line 1064) covers legacy.
- `ActiveSkill` derives `Serialize`/`Deserialize`; `WorkflowState` is
  `#[serde(rename_all="lowercase")]` so the TOML/JSON forms (`"planning"`,
  `"skill"`) match the frontend `WorkflowState` union. ✅

### N3 — Pass — `ToolFilter::Skill` allow-list is correct

**File:** `src/tool/mod.rs:221-226`

The `Skill(allowed)` arm exposes a tool iff `allowed.iter().any(|n| n == name)`
for non-Memory categories, and always exposes Memory tools. This matches the
design ("memory tools always available; only named tools visible"). The
`skill_filter_is_an_allow_list` test (line 586) confirms. `skill_end`/
`abandon_skill` appear only if the skill's `tools` list names them — the
`merge_to_main.toml` lists both. ✅

### N4 — Pass — `merge_to_main.toml` prompt + tools are sensible

**File:** `.coding/skills/merge_to_main.toml`

- `available_in = ["complete", "planning"]`, `target_state = "planning"` —
  correct (returns to Planning after the merge).
- `tools` includes `file_read`/`file_edit`/`file_write`/`file_append`/`shell`/
  `search`/`git`/`skill_end`/`abandon_skill` — covers stash, commit, merge,
  checkout, branch, conflict resolution via file_edit, and the exit tools.
  `memory_*` tools are always available (not listed, per the filter rule).
- The prompt walks the merge steps (stash → commit → checkout main → merge →
  resolve conflicts → delete branch → skill_end) and notes core ops always
  require approval. Sensible.
- One minor: the prompt says "delete the feature branch" but the `git` tool's
  `branch delete` uses `-d` (safe delete, refuses if not merged) — fine.

### N5 — Pass — Windows path handling

**Files:** `src/project/mod.rs:55`, `src/tool/agent/git.rs:76-82`

`skills_dir` is built via `coding_dir.join("skills")` (platform-correct).
`run_git` uses `#[cfg(windows)]` `CREATE_NO_WINDOW` (unchanged, correct). No
Linux paths or bash syntax introduced. ✅

### N6 — Pass — Doc comments on public functions

All new public functions (`SkillStartTool::new`, `SkillEndTool::new`,
`AbandonSkillTool::new`, `SkillRegistry::{new,load_dir,get,iter,is_available_in}`,
`Workflow::{start_skill,end_skill,abandon_skill,active_skill}`,
`AgentLoopFactory::{with_skills,skills_handle}`, `enter_skill`,
`ActiveSkillInfo`, `ActiveSkill`) have doc comments. ✅

---

## Summary

The core security invariant (core git operations always approval-gated via
`never_auto_for`, unbypassable by safety mode or safety rule) is **correctly
enforced** and tested. The skill lifecycle (start/end/abandon), persist/load
round-trip, legacy sidecar compat, and the `Skill` allow-list filter are all
correct.

The one **actionable correctness finding** is **C1**: the `ToolFilter::Planning`
arm hides `skill_start`, contradicting the skill's `available_in = [...,
"planning"]` and the UI path (which works from Planning). This makes the
agent-driven `skill_start` path inconsistent with the UI `enter_skill` path.
Fix: expose `skill_start` in the Planning filter arm.

The remaining items are cosmetic (stale comments referencing the deleted
`merge_to_main`/`MergeToMainTool` in `src/tool/mod.rs:105-111`,
`src/agent/dispatch.rs:74`, `src-tauri/src/ipc/commands.rs:546-551`,
`src/tool/agent/git.rs:122-123`) or informational.
