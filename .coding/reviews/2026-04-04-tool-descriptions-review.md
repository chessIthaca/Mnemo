# Review: Sharpen inner tool descriptions (git fields + memory tools)

**Date:** 2026-04-04
**Scope:** All uncommitted changes (`git diff HEAD`).
**Files changed:** `src/tool/agent/git.rs`, `src/tool/memory/mod.rs`
  (plus workflow bookkeeping in `.coding/plans/*.md` and `stack.json`, excluded per instructions).

The diff is description-string-only — no logic/behavior changes. Each new
description was checked against the corresponding `execute()` body.

---

## Correctness

### [LOW] `branch` field description understates its requirement scope
**File:** `src/tool/agent/git.rs:137`

The new description reads:
> "Required for merge and checkout (the branch to merge in / switch to); optional for push (the branch to push, omitted = the current branch)."

This is accurate for merge (requires `branch`, `git.rs:197-204`), checkout
(requires `branch`, `git.rs:217-225`), and push (optional, omitted → current
branch, `git.rs:314-324`). **However**, the `branch` field is *also* required
for the `branch` subcommand's `delete` (`git.rs:262-269`) and `create`
(`git.rs:278-285`) actions, which error with `"branch delete requires a
'branch' field"` / `"branch create requires a 'branch' field"` when omitted.

The plan's stated goal was "to state which subcommands require which fields."
Omitting `branch delete`/`create` from the `branch` field's requirement list
leaves a gap: an LLM reading only the `branch` field description would not
know it must pass `branch` for those two actions. The `action` field
description (`git.rs:138`) lists `list/delete/create` as valid branch actions
but does not state that `delete`/`create` require the `branch` field either,
so the requirement is unstated anywhere in the schema.

**Suggested fix:** extend the `branch` description, e.g.:
> "Required for merge, checkout, and branch delete/create (the branch to merge in / switch to / delete / create); optional for push (omitted = the current branch)."

---

## Bugs

No bugs introduced by this diff. (Description-only changes; no logic touched.)

One **pre-existing** code inconsistency noted for awareness (NOT introduced
by, and NOT misrepresented by, this diff): the `stash` action defaulting
(`git.rs:235-239`) uses `.map(|s| s.to_string())` without filtering empty
strings, so `action: ""` errors ("unknown stash action ''") rather than
defaulting to `"push"`. The `branch` action defaulting (`git.rs:254-259`)
correctly filters empties via `.filter(|s| !s.is_empty())`. The new
description "defaults to push" is accurate for the omitted (`None`) case and
does not claim anything about empty strings, so it is not itself inaccurate.

---

## Security

No security findings. The diff touches only JSON schema description strings;
no argument parsing, command construction, or access-control logic changed.
The git tool's flag-injection guard (`valid_branch_name`, `git.rs:70-72`) and
core-operation gating (`never_auto_for`, `git.rs:155-166`) are unchanged.

---

## Constitution compliance

No findings. The diff adds no new public functions (so the "public functions
must have doc comments" rule is not newly triggered), introduces no
commits to `main`, and uses no shell/path constructs. Existing public
constructors (`MemoryWriteTool::new`, `MemoryRecallTool::new`,
`MemoryConsolidateTool::new`) lack doc comments, but this is pre-existing and
outside this diff's scope.

---

## Summary

One low-severity correctness finding: the `branch` field description omits
that `branch` is required for the `branch delete`/`create` actions. All other
new descriptions are factually accurate against the `execute()` logic,
including the tier-enum semantics, the stash→push / branch→list defaults, the
push "omitted = current branch" behavior, and the merge "omitted = git's
default message" behavior.
