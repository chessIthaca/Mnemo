# Review: git commit auto-stage (`git add -A`)

**Date:** 2026-04-04
**Scope:** All uncommitted changes in the working tree (`git status` / `git diff HEAD`).
**Reviewer:** read-only subagent

## Files changed

- `src/tool/agent/git.rs` — the feature change (commit now runs `git add -A` first)
- `.coding/plans/stack.json` — plan-stack bookkeeping (plan ID swap); expected, not a bug
- `.coding/plans/796af988-6a02-4e8a-8d15-dced724ae368.md` — new plan file (untracked); expected, not a bug

## What the change does

The `commit` subcommand previously ran only `git commit -m <message>`, which failed
with "no changes added to commit" when nothing was staged. The fix runs
`git add -A` first (via `run_git`), aborts (returns the failed `add` `ToolResult`)
if staging fails, then runs `git commit -m <message>`. The module doc comment
(lines 1–12) and schema description (lines 124–127) were updated to document the
new behavior. A new test `commit_stages_untracked_files` (lines 369–409) was added.

## Analysis by category

### Correctness

The commit handler (lines 178–192) is correct:

```rust
let add = self.run_git(&["add", "-A"]).await;
if !add.success {
    return add;
}
self.run_git(&["commit", "-m", &message]).await
```

- `run_git` sets `current_dir(&self.project_root)` (line 76), so `git add -A`
  operates on the whole working tree rooted at the project root — the intended
  scope. ✓
- `git add -A` stages new, modified, and deleted files and respects
  `.gitignore` (ignored files are not staged). ✓
- The abort-on-failure logic returns the failed `add` `ToolResult` (which carries
  `success: false` plus the git stderr/exit code), so the caller sees *why*
  staging failed and the commit is correctly skipped. ✓
- Edge case: if the tree is clean, `git add -A` exits 0 (success) and
  `git commit` then fails with "nothing to commit". This is a legitimate,
  expected failure (you cannot commit nothing) — not a regression. ✓
- The pre-existing message validation (`Some(m) if !m.is_empty()`, line 180)
  is unchanged and still runs before staging. ✓

### Bugs

No findings. The abort path, the staging, and the commit are all correct. The
`message` is passed as a discrete argv element after `-m` (line 191), so git
treats it as the message value regardless of leading `-` — no flag-injection
risk (and this is pre-existing behavior, not introduced here).

### Security

No findings.

- `git add -A` respects `.gitignore`, so ignored secrets/build artifacts are not
  staged. The risk of committing an *un-ignored* secret file is identical to any
  developer running `git add -A && git commit` — standard practice.
- This change does **not** alter the approval gating: `commit` remains
  `SafetyLevel::NeedsApproval` (line 152) and `never_auto_for` returns `false`
  for `commit` (only `merge`/`push` are core ops, line 61) — exactly as before.
  The diff adds no new code path that bypasses approval.
- The behavior (stage all changes, then commit) aligns with the constitution's
  standard closing sequence, which expects all working-tree changes to be
  committed to the feature branch.

### Constitution compliance

No findings.

- **Doc comments:** The module doc comment (lines 1–12) and schema description
  (lines 124–127) were updated to document the new staging behavior. No new
  public functions were added; existing public functions retain their doc
  comments. `run_git` is private, so it is not subject to the "all public
  functions must have doc comments" rule. ✓
- **Windows paths / syntax:** `git add -A` is platform-agnostic; `run_git`
  already applies `CREATE_NO_WINDOW` on Windows (lines 80–84). No Linux paths or
  bash syntax introduced. ✓
- **`cargo test`:** Stated to pass (438 tests). The new test
  `commit_stages_untracked_files` is a meaningful regression test — it would
  fail on the old code (untracked `c.txt` → "nothing added to commit") and passes
  on the new code. It correctly uses the existing `init_repo` helper (which sets
  `core.autocrlf false` for cross-platform consistency), gracefully skips when
  git is unavailable, and verifies both the outcome (file tracked via
  `git ls-files`) and the commit message (via `git log`). ✓
- **No commit to main:** The change does not touch branch/merge logic; it only
  affects the `commit` subcommand's staging step. ✓

## Verdict

**No findings.** The diff is clean, correct, well-documented, and
constitution-compliant. The abort-on-add-failure logic is sound, the security
posture is unchanged (approval gating untouched, `.gitignore` respected), and
the new test is a meaningful regression guard.
