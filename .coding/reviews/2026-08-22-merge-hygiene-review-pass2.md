## Verdict: PASS

# Re-review (pass 2): Merge hygiene overhaul (backlog #87), commit 7c79c10

Scope: the three Low findings from `.coding/reviews/2026-08-22-merge-hygiene-review.md` and their claimed fixes, committed as `7c79c10` on `feat/merge-hygiene`. Method note: the working tree is clean (`git status --short` empty, `git diff HEAD` empty), so HEAD == 7c79c10 and reading the current files reviews the commit's content. I have no shell, so `git show`/`cargo test` could not be re-run; all verification below is by reading the code and cross-checking the load-bearing helpers (`budgeted_digest`, `PlanKind` Display, the `git_ops` fixture style).

## Finding 1 — BUG-digest hint path — FIXED, assertion meaningful

`capture_bug_digest_on_a_feature_branch_carries_the_unmerged_hint` (`src/memory/finish_capture.rs:503-538`):

- Builds `PlanKind::BugFixing` via `make_plan`, so the BUG-digest arm (`finish_capture.rs:129-163`) — including the hint append at **lines 144-147** named by the finding — actually runs.
- Uses the existing `git_repo_on_branch("feat/hint-probe")` fixture (412-439: `init -q`, local user/gpgsign config, `branch -M main` normalization, then `checkout -q -b`) — same style as the other git fixtures, portable `Command::new("git").args(...)` with no shell.
- Passes `Some(repo.path().to_path_buf())`, so `branch_hint` really probes git.
- Asserts on the memory behind `report.bug_memory_id` (also pinning that the BUG digest exists): content contains `branch feat/hint-probe @ `, contains `(unmerged — exists only on this branch)`, and `chars().count() <= 600`.
- **Non-vacuous:** `budgeted_digest` (`src/memory/indexer.rs:365-373`) is pointer-first — it truncates only the gist (`chars().take(room)`), never the pointer, so the hint (pointer-resident) appears in `m.content` iff the code appended it. The bug gist (`symptom: the app crashes on open`) contains no `branch …` substring, so the assertion cannot pass by accident. Estimated digest size (~190 chars) sits well under 600, so the budget assert has room and pins the "hint grows the pointer, never the budget" invariant.

## Finding 2 — probe-failure arm — FIXED, correct arm exercised

`capture_outside_a_git_repo_has_no_branch_hint` (`finish_capture.rs:540-564`):

- Passes a plain `tempdir()` (no `git init`) as `Some(root)` — exactly the arm the finding named: `git branch --show-current` exits non-zero, `git()` returns `Err`, and `.ok()?` collapses to `None` (`src/project/git_ops.rs:486`; the `spawn_blocking(...).await.ok().flatten()` at 499-500 is also covered).
- Asserts the plan digest contains neither `unmerged` nor `branch`. **Safe negative assertions:** the content is gist (`Make the crash on open go away`) + pointer (`1/2 steps · implementation · path .coding/plans/plan-abc-123.md · regression test: crash_on_open_regression`); `PlanKind::Implementation` Displays as `implementation` (`src/workflow/plan_file.rs:65-78`) and no plan `.md` exists in the dir so no commit token is appended — no `branch` substring can legitimately appear, and any leaked hint would fail the test.

## Finding 3 — `git_ops.rs` module doc — FIXED, now accurate

`src/project/git_ops.rs:5-7` reads "Git operations shared by the workflow engine — Run-All checkpoint, commit-success, and rollback, plus plan branch preparation (`prepare_branch`) and the finish-digest branch probe (`branch_hint`)." The module's public items are `checkpoint`, `commit_success`, `rollback` (74/102/126), `prepare_branch` + its supporting `valid_branch_name`/`PrepareBranchOutcome` (153/161/467), and `branch_hint` (484) — all three groups named; the validator/outcome enum are reasonably subsumed under "plan branch preparation" (a module doc summarizes, not enumerates).

## Regression / sloppiness check

- The two pre-existing hint tests (`capture_on_a_feature_branch_carries_the_unmerged_hint`, `capture_on_main_has_no_branch_hint`) are intact; the new tests slot in after them with identical fixture/assert style (`{}`-message asserts, `chars().count()` budgets, `#[tokio::test]`).
- Production code untouched by the fixes (tests + one doc line only); `branch_hint`'s doc comment (477-483) still accurately lists every `None` arm.
- Each new test is a genuine pin: it fails without its corresponding production path (bug-hint append; `.ok()?` collapse).

## Constitution

- Public functions documented; new tests carry explanatory comments.
- No `#[allow(...)]` in either touched file (searched both — zero matches).
- Platform-neutral: args-array git invocations, `tempfile::tempdir`, no Windows-only paths/APIs added; the `CREATE_NO_WINDOW` cfg predates this change and is the sanctioned console-suppression gate.

## Notes (non-findings)

- The non-repo test implicitly relies on the OS temp dir not sitting inside a git worktree (git walks parents). Standard practice and shared by every tempdir git fixture in the repo; risk is negligible.
- `cargo test` green-run (1410 passed) is the author's claim; as a read-only reviewer I verified test names, paths exercised, and assertion soundness by reading the code instead.
