# Review — Configurable Git Core-Operations List

**Scope:** All uncommitted changes (16 modified + 2 new). Goal: replace the
hardcoded git core-operations list (`merge`/`push`) with a configurable,
runtime-mutable list exposed in Settings → Git, plus an agent.md rule
requiring a regression test for every defect fixed.

**Verdict:** The implementation is correct and well-tested end-to-end. The
configurable list genuinely gates approval through the full path
(config → factory handle → `GitTool::never_auto_for` → `dispatch.rs
force_prompt`), the live-rewire path is sound, and the default
`["merge","push"]` preserves the old behavior exactly. One
security/constitution-compliance tension worth a conscious decision, plus two
minor nits. No correctness bugs found.

---

## Findings

### MEDIUM — Security / Constitution compliance: empty list un-gates merge/push

**Files:** `src/tool/agent/git.rs:214-218`, `src/config/general.rs:307-320`,
`frontend/src/components/settings/sections/GitSection.tsx:60-63`

The global APP RULE states: *"Core operations (git merge, git push) are always
approval-gated, regardless of safety mode or active skill. Nothing bypasses
this security check."* This feature makes that list user-configurable, and an
**empty list** (or a list with `merge`/`push` removed) means
`never_auto_for` returns `false` for those subcommands — so in `Autonomous`
mode, or under a permissive safety rule, `git merge` / `git push` could be
auto-run or auto-approved without a contemporaneous user sanction.

This is a **deliberate product decision** (backlog item #36, now resolved,
explicitly asked for "a config page to determine whether and which git commands
require approval"), and the behavior is documented in the test
`never_auto_for_empty_list_forces_nothing` (git.rs:562-573). So it is not a bug
— the code does exactly what it was designed to do. But it does contradict the
"Nothing bypasses this security check" wording of the global rule, and there is
no floor enforcement or UI warning to prevent a user from accidentally clearing
the field.

Note: under `ApproveEachAction` / `AutoReadApproveWrites` / `AutoApproveProject`
modes, merge/push still require approval (the git tool's `safety()` returns
`NeedsApproval`), so the relaxation only bites in `Autonomous` mode or under a
matching safety rule — but that is precisely the scenario the core-ops gate
existed to protect.

**Recommendation (pick one):**
1. Enforce a non-removable floor: always union `["merge","push"]` into the
   effective list in `never_auto_for` (or reject saves that remove them), so the
   user can *add* ops but never un-gate the historical set. This preserves the
   constitution invariant while still allowing additions (e.g. `checkout`).
2. Add a confirmation warning in `GitSection` when the user saves a list that
   omits `merge` or `push` (e.g. "Removing merge/push from core operations
   allows them to run without approval in Autonomous mode — continue?").
3. Consciously accept the relaxation and update the global rule's wording to
   reflect that core operations are now configurable (default `merge,push`).

If the team's intent is (3), this finding can be dismissed with that written
justification. Otherwise (1) is the smallest change that keeps the invariant.

### LOW — Test coverage: no IPC-level integration test for the live-rewire path

**Files:** `src-tauri/src/ipc/settings.rs:1087-1092`, `src/tool/agent/git.rs:575-597`

The live-rewire mechanism is well unit-tested in isolation
(`never_auto_for_live_update_takes_effect_without_rebuild` mutates the shared
`Arc<RwLock>` directly, mirroring `factory.set_core_operations`). But there is
no integration test that exercises the full wired path: `save_settings` with a
`core_operations` patch → `persist_and_reload` →
`factory.set_core_operations(reloaded.general.git.core_operations)` → an
already-built `GitTool` observes the new list. A regression in the wiring
(e.g. a future refactor that drops the `set_core_operations` call, or reads from
the wrong config field) would not be caught by the current unit tests.

**Recommendation:** Optional — add an IPC-level test (or a factory-level test)
that calls `factory.set_core_operations(...)` and asserts a `GitTool` built
*before* the call observes the new list on its next `never_auto_for` check.
The existing `factory_shares_safety_mode_across_builds` test is a good template
for the shared-handle pattern.

### LOW — UX nit: GitSection doesn't reflect backend normalization after save

**File:** `frontend/src/components/settings/sections/GitSection.tsx:59-66`

`handleSave` sends the raw comma-split input and then calls
`setCoreOpsSnap(coreOps)` — the *raw* (possibly mixed-case) string, not the
backend-normalized form. So after saving `"Merge, PUSH"`, the input still
displays `"Merge, PUSH"` until the section is reloaded (next open), at which
point it becomes `"merge, push"` (the backend trims + lowercases). This is a
minor visual inconsistency, not a bug — the stored value is correct either way.

**Recommendation:** Optional — after a successful save, re-derive the display
from the normalized form (e.g. `setCoreOps(patch.core_operations.join(", "))`
and match the snapshot), or reload via `getSettings()`. Low priority.

---

## Areas verified clean

**CORRECTNESS — end-to-end gating:** The configurable list gates approval
through the full path. `GitConfig::default()` → `["merge","push"]`;
`build_brain()` (main.rs:811-813) seeds the shared `Arc<RwLock<Vec<String>>>`
from `config.general.git.core_operations` and wires it via
`.with_core_operations()`; `register_agent_tools` (factory.rs:522-525) passes
the shared handle to every `GitTool`; `never_auto_for` (git.rs:210-218) reads
the handle and matches case-insensitively; `dispatch.rs:195` sets `force_prompt`
from `never_auto_for`, and `:196` forces the approval prompt, while `:203`
(`auto_approved = !force_prompt && ...`) correctly ensures a safety rule cannot
auto-approve a core operation. ✓

**CORRECTNESS — live-rewire:** `save_settings` applies the patch
(trim+lowercase+drop-empties, settings.rs:1052-1058), `persist_and_reload`
returns the reloaded config (:1069), and `f.set_core_operations(reloaded...)`
(:1090-1092) pushes into the shared handle. Because every `GitTool` holds the
same `Arc`, the mutation is observed on the next `never_auto_for` check with no
registry rebuild. The `if let Some(f) = state.runtime.factory.as_ref()` guard
correctly no-ops when no factory is wired (tests). ✓

**CORRECTNESS — default preserves old behavior:** `GitConfig::default()` =
`["merge","push"]`; `git_section_defaults_merge_push` confirms an empty/absent
`[git]` section parses to the default. The old `is_core_operation` was
case-sensitive `matches!("merge"|"push")`; the new `eq_ignore_ascii_case` is
case-insensitive, but since the git tool's `subcommand` enum is lowercase and
`execute` matches case-sensitively (a mixed-case subcommand errors as "unknown"),
the effective behavior for real (lowercase) subcommands is identical. The
case-insensitivity is a harmless robustness improvement, documented by
`never_auto_for_matches_case_insensitively`. ✓

**BUGS — none found:**
- No lock-ordering issues: `never_auto_for` acquires and drops the read lock
  synchronously within the function (not held across any `await`); the write
  lock in `set_core_operations` is a fast `Vec` assignment. This matches the
  codebase's existing `std::sync::RwLock` pattern (`set_provider`, `set_spawner`).
- No panics on missing fields: `GitConfig` has `#[serde(default)]` + a `Default`
  impl, so a missing `[git]` section or missing `core_operations` field parses
  to the default list. `state.runtime.factory.as_ref()` is guarded.
- No serde round-trip gaps: `GitConfig` derives `Serialize/Deserialize` with
  `#[serde(default)]`; `git_section_round_trips` confirms the cycle. A partial
  `[git]` table with no `core_operations` key resolves to the default.
- No fixture mismatches: `contract_fixtures.rs:227-229` builds `GitWire` from
  `GitConfig::default().core_operations` (`["merge","push"]`), matching the
  frontend fixture `dto-get-settings.json` (`["merge","push"]`) and the
  `ipc-contract.test.ts` assertion. Both `settings_dto_tests` samples updated. ✓

**SECURITY — case-insensitive match has no bypass:** `eq_ignore_ascii_case`
cannot be bypassed by casing. The only way to un-gate an op is to remove it
from the list (the intended configurability) — see the MEDIUM finding above for
the empty-list footgun. No flag/whitespace injection vector: `never_auto_for`
only decides whether to prompt; `execute` independently validates branch/ref
args via `valid_branch_name` (rejects leading `-` + whitespace) and passes
args as discrete argv (no shell). ✓

**CONSTITUTION COMPLIANCE:**
- Doc comments on all public fns: `GitTool::new`, `with_core_operations`;
  `AgentLoopFactory::with_core_operations`, `set_core_operations`; `GitConfig`;
  `GitWire` — all documented. ✓
- Warning-free build: the `spawn.rs` change moves `use AgentLoop` into the
  `#[cfg(test)]` block (genuine warning fix, not `#[allow]`); the dead
  `is_core_operation` was removed (not silenced). ✓
- Regression tests for defects: 4 new tests in `git.rs` + 3 in `general.rs`
  cover the configurable behavior, live-update, case-insensitivity, and
  round-trip. The agent.md rule was added verbatim. ✓
- Windows/PowerShell: no shell commands in the diff; `build_brain` uses
  `std::sync::RwLock` (not platform-specific). ✓
