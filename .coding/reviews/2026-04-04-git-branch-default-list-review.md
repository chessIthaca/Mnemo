# Review: git branch defaults to list

**Date:** 2026-04-04
**Reviewer:** read-only subagent
**Scope:** all uncommitted changes (`git diff HEAD`)

## Files changed
- `src/tool/agent/git.rs` — `branch` subcommand now defaults `action` to `"list"` when absent (was: error). New test `branch_defaults_to_list`.
- `.coding/plans/64e59c08-….md`, `.coding/plans/stack.json`, `.coding/plans/58a7ea32-….md` — plan-workflow bookkeeping (checkbox/stack state). No code impact; not flagged.

## Summary
The change replaces the `branch` handler's "action required" error with a default-to-`"list"` pattern that deliberately mirrors the existing `stash` default-to-`"push"` handler (lines 222-237). The implementation is correct, secure, and constitution-compliant.

## Findings

### Correctness
- **No findings.** Defaulting logic is correct: `None` → `"list"`; the `match` then dispatches to `self.run_git(&["branch"])` (line 249), which is exactly what the explicit `"list"` arm does. The new test `branch_defaults_to_list` (lines 605-618) asserts both success and that `feat`/`main` appear in the output, matching the assertions in the existing `branch_list_and_create` test. Sound.

### Bugs
- **Informational (low) — empty-string `action` produces a less precise error.** `src/tool/agent/git.rs:243-247`. The old code treated `action: ""` as "missing" via `Some(a) if !a.is_empty()` and returned `"branch requires an 'action' field…"`. The new `as_deref().map(…).unwrap_or_else(…)` lets `Some("")` pass through as `""`, which hits the `other =>` arm and yields `"unknown branch action ''. Use: list, delete, create"`. This is still an error (no silent success, no security impact) and is **identical to the existing `stash` handler's behavior** for `action: ""`, so it is consistent within the file. Noting only because the review brief asked specifically. No fix required; if desired, a `filter(|s| !s.is_empty())` would restore the old empty-string semantics, but matching `stash` is the better consistency argument.

### Security
- **No findings.** Flag-injection guards are intact:
  - `delete` (line 259) and `create` (line 275) still call `Self::valid_branch_name` (rejects leading `-` and whitespace) before passing the name to git.
  - `list` (the new default) takes no branch argument, so there is no injection surface.
  - Args are still passed as discrete `Command` argv (line 73-74), never a shell string.
  - `never_auto_for` (lines 152-163) is unchanged; `branch` remains a non-core subcommand, so defaulting to a read-only `list` is safe and does not weaken the merge/push approval gate.
  - Existing tests `branch_rejects_flag_injection` (line 643) and `branch_unknown_action_errors` (line 654) still cover the write/unknown paths.

### Constitution compliance
- **No findings.** No new public functions were added; existing public items (`GitTool`, `new`, `is_core_operation`, `valid_branch_name`) retain their doc comments. The new inline comment (lines 239-242) explains the defaulting rationale. Code style matches the adjacent `stash` handler exactly. The schema description (line 135) still lists valid actions and `action` is not in `required`, consistent with optional/defaulted behavior.

## Verdict
Diff is clean. The single informational note (empty-string action error wording) is consistent with the pre-existing `stash` handler and requires no change.
