# Review: Note commit requires message in git tool description

**Plan:** "Note commit requires message in git tool description"
**Scope:** Prompt-clarity change only — add a note to the git tool's top-level
schema description stating that `commit` requires a `message` argument and that
a message-less commit errors. Runtime already enforced this.

## Diff reviewed

- `src/tool/agent/git.rs` (lines 141–147): the `GitTool::schema` description
  string gained the clause
  `and REQUIRES a \`message\` argument (the commit message) — a commit call
  without \`message\` errors.`, spliced into the existing sentence after
  "before committing".
- `.coding/plans/stack.json` + untracked `.coding/plans/b1e5d20f-….md`:
  plan-system bookkeeping (active-plan stack id + plan file). Not source code;
  expected churn, not a concern.

## Correctness

The description accurately matches runtime behavior. The `commit` arm
(`git.rs:198-202`) requires `args.message` to be `Some(m) if !m.is_empty()`,
else returns `ToolResult::error("commit requires a 'message' field")`. The new
description text — "REQUIRES a `message` argument (the commit message) — a
commit call without `message` errors" — correctly states that a commit without
`message` errors. (The runtime also rejects an *empty* message string; the
description does not spell this out, but that level of precision is
appropriate for a tool description and the plan explicitly scoped this as a
prompt-clarity note, not a full spec. Not a finding.)

Tone is consistent with the existing description: "REQUIRES" mirrors the
existing "ALWAYS require approval" emphasis style; backticks around `message`
match the field-naming convention; the em-dash and line-continuation (`\`)
style are preserved.

## Bugs

No unintended changes beyond the description string. No test asserts on the
git schema description text. The one nearby assertion,
`commit_requires_message` (`git.rs:386`), asserts on the *runtime error
output* (`result.output.contains("requires a 'message'")`), and the runtime
error string ("commit requires a 'message' field") is unchanged, so it still
passes. The only `schema()` test calls in the crate are in `describe_image.rs`
and `ask_user.rs` — none read the git tool's description. Nothing breaks.

## Security

Description-only change. No new mutation surfaces, no new argument handling,
no change to `execute`, `safety`, or `never_auto_for`. The `message` field was
already an existing `Option<String>` passed as discrete argv (`-m`, &message`)
— never a shell string — so no injection surface introduced or altered.

## Constitution compliance

- **Warning-free build:** The edit is a pure string-literal modification inside
  an already-consumed description (passed to `ToolSchema::new`). No new
  imports, no unused items, no `mut`/dead-code changes, and no `#[allow(...)]`
  suppressions added. Cannot introduce a warning under `#![deny(warnings)]`.
- **Doc comments:** No function signatures or doc comments touched. Existing
  doc comments on `GitTool::new`, `is_core_operation`, `valid_branch_name`,
  and `run_git` are intact.
- **Code style:** Existing line-continuation and indentation style followed.

## Findings

**No findings.** The diff is clean, correct, and consistent with the plan's
stated intent (a prompt-clarity note matching already-enforced runtime
behavior).
