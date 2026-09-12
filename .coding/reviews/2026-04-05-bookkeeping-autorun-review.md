# Review: Bookkeeping tools → always AutoRun

**Date:** 2026-04-05
**Reviewer:** read-only review subagent
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`)

## Changes reviewed

| File | Change |
|------|--------|
| `src/tool/memory/mod.rs` | `MemoryWriteTool::safety()` and `MemoryConsolidateTool::safety()` → `SafetyLevel::AutoRun`; module doc comment rewritten |
| `src/tool/workflow/plan.rs` | `CompleteStepTool::safety()` and `AbandonPlanTool::safety()` → `SafetyLevel::AutoRun`; module doc comment rewritten |
| `.coding/safety.toml` | Removed now-redundant `complete_step` and `memory_write` regex `[[rule]]` blocks |
| `agent.md` | Added "Bookkeeping tools never prompt" constitution note |
| `.coding/backlog.json`, `.coding/plans/stack.json`, `.coding/plans/d10066a6-....md` | Workflow bookkeeping state (backlog item → in_flight, new plan on the stack) |

## Summary verdict

The core change is **correct and safe**: the four tools flipped to `AutoRun`
(`memory_write`, `memory_consolidate`, `complete_step`, `abandon_plan`) only
touch the project's own sandboxed `.coding/` state and memory store — verified
by reading their `execute()` bodies. The approval gate for real mutations is
**not** weakened: `file_edit`/`file_write`/`file_append`/`shell`/`git`/
`spawn_agent` all remain `SafetyLevel::NeedsApproval`
(`src/tool/agent/*.rs`), and the git `merge`/`push` `never_auto_for`
always-prompt gate is intact and untouched (`src/tool/agent/git.rs:167`,
`src/agent/dispatch.rs:83`).

**However, the plan's own step 3 ("update any test-fixture expectations and
stale doc comments") was incompletely carried out** — two user-facing / doc
references still claim `abandon_plan` needs approval, contradicting the new
behavior. Both should have been updated by this plan and were missed.

---

## Findings

### Major

**M1 — Stale comment in `src/tool/mod.rs:194` still says `abandon_plan` is approval-gated.**

```
// (preserving completed steps); abandon_plan (approval-gated,
// last resort) pops a stale/wrong plan and resumes the parent;
```

`AbandonPlanTool::safety()` is now `SafetyLevel::AutoRun`
(`src/tool/workflow/plan.rs:354-358`). This comment directly contradicts the
new behavior. It is a maintenance hazard: a future reader trusting this
comment will mis-reason about the approval model. The plan's step 3 explicitly
scoped "update stale comments referencing these tools' approval status," so
this is an in-scope miss, not an out-of-scope observation. The fix is a
one-word deletion ("approval-gated, ").

**M2 — Stale system-prompt text in `src/agent/prompt.rs:137` tells the agent `abandon_plan` "needs approval".**

```
remaining ones. Only call abandon_plan (which needs approval) as a \
last resort to discard a fundamentally wrong plan and drop back to the \
parent.
```

This string is injected into the live system prompt for every Executing-state
turn. It is now factually wrong — `abandon_plan` is `AutoRun` and will not
prompt. Consequences: (a) the model is told an approval gate exists that
doesn't, which can alter its behavior (it may avoid or over-justify
`abandon_plan`); (b) the prompt is inconsistent with the new `agent.md`
constitution note stating plan-lifecycle tools "never ask for approval."
Keeping the "last resort" guidance is fine, but the parenthetical "(which
needs approval)" must be removed. Same scope miss as M1.

### Minor

**m1 — `agent.md` "Bookkeeping tools never prompt" note lists `memory_recall` among the tools that "never ask for approval," but this plan did not change `memory_recall`.**

`memory_recall` was already `AutoRun` before this change, so the note is
factually accurate — this is only a clarity nit about attribution, not an
error. No action required; noting so the author is aware the sentence
describes the end state, not the delta.

### Nit

**N1 — Working tree is currently on the `main` branch (`git branch --show-current` → `main`).**

The active plan's step 7 says "Commit the changes to the current feature
branch," and the constitution forbids committing to `main` outside the
explicit merge skill. The current branch is `main`. This is a state flag for
the main agent (the commit step has not run yet), not a defect in this diff —
but the main agent must create/switch to a feature branch before committing,
or the commit will land on `main` in violation of the constitution.

---

## Security / constitution checklist

- Approval gate for real mutations (file writes, shell, git commit/merge/push,
  spawn_agent): **intact** — all remain `NeedsApproval`.
- Git `merge`/`push` `never_auto_for` always-prompt gate: **intact and
  untouched** (`src/tool/agent/git.rs:167-172`, consulted at
  `src/agent/dispatch.rs:83`).
- `never_auto()` blanket gate: unchanged (`src/tool/mod.rs:112`).
- `.coding/safety.toml` still valid TOML; removed rules were redundant (the
  tools are now `AutoRun`, so a rule auto-approving them is dead config). The
  `spawn_agent` auto-approve rule (needed for the reviewer subagent) is
  untouched.
- No test asserts `NeedsApproval` for the four flipped tools (searched
  `src/**/*.rs` for `assert.*NeedsApproval` — no matches for these tools), so
  no test should break from the flip.
- `is_project_scoped` (`src/agent/approval.rs:100-115`) returns `false` for
  `memory_write` (unknown-tool branch). This is now dead code for these tools
  (they're `AutoRun`, so project-scoping is never consulted) — not a bug, just
  no longer reachable for them.
- Public-function doc-comment rule: satisfied — all changed `safety()`
  impls carry explanatory comments.
- Line endings: `git diff` reported an LF→CRLF normalization warning on
  `.coding/backlog.json` only (pre-existing, Git-managed). No mixed endings
  introduced in the edited source files.

## Required actions (Major findings)

1. **M1** — remove "approval-gated, " from the comment at `src/tool/mod.rs:194`.
2. **M2** — remove "(which needs approval) " from the prompt string at
   `src/agent/prompt.rs:137` (keep the "last resort" guidance).

Both are required before this plan's closing sequence completes, per the
constitution's "never defer review findings" rule. N1 must also be heeded by
the main agent at commit time (use a feature branch, not `main`).
