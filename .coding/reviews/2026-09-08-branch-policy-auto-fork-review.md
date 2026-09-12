## Verdict: FINDINGS (0 high, 1 low)

Review of all uncommitted changes on `wt/branch-policy-auto-fork` (`git diff HEAD` + untracked `.coding/` records). The change enforces "one wt/* branch per agent directory": `create_plan` auto-forks a stable per-directory `wt/*` branch from `main` when on `main` (no `branch` arg) and reuses the current branch otherwise; the `branch` arg becomes explicit-user-only; `run_all` stops steering agents to pass per-item branch names.

The core logic is sound, the sub-plan safety is correct, the tests genuinely pin the new behavior, the docs are synced, and the build is warning-free. One low-severity finding on a detached-HEAD edge case + comment inaccuracy.

---

### L1 (low) — Detached HEAD is mishandled + the comment is inaccurate

**File:** `src/project/git_ops.rs:519-535` (`ensure_work_branch_sync`)

The probe uses `git rev-parse --abbrev-ref HEAD`:
```rust
// Probe the current branch. A failure (not a git repo, a detached HEAD
// that prints nothing, etc.) means we cannot safely auto-fork — return
// None so `create_plan` proceeds on the current branch with no note ...
let current = match git(root, &["rev-parse", "--abbrev-ref", "HEAD"]) {
    Ok(b) => b,
    Err(_) => return Ok(None),
};
if current != "main" && current != "master" {
    return Ok(Some(PrepareBranchOutcome::AlreadyOnBranch { branch: current }));
}
```

The comment claims "a detached HEAD that prints nothing" hits the `Err(_) => Ok(None)` skip path. That is wrong for this command: `git rev-parse --abbrev-ref HEAD` on a **detached HEAD succeeds and prints the literal string `HEAD`** (exit 0) — it does *not* fail and does *not* print nothing. (The "prints nothing on detached HEAD" property belongs to `git branch --show-current`, which the sibling `branch_hint` function at line 571 correctly uses with an `is_empty()` guard.) So on a detached HEAD, `current == "HEAD"`, which passes the `!= "main" && != "master"` check, and the function returns `AlreadyOnBranch { branch: "HEAD" }` — reusing the detached HEAD rather than skipping or forking.

Consequence: on a detached HEAD, `create_plan` notes "git: already on branch 'HEAD' — nothing to do" and the plan (and subsequent commits) land on the detached HEAD, where work is easily lost on the next checkout. The scenario is unusual for an agent directory, so severity is low, but the comment actively misleads a future maintainer and the behavior is the opposite of safe.

**Fix (minimal — correct the comment):** drop the "detached HEAD that prints nothing" clause from the comment, since only a genuine git failure (not a repo) reaches `Err(_)`.

**Fix (better — treat detached HEAD like `main`):** also fork from `main` when detached, so work never commits to a detached HEAD:
```rust
if current != "main" && current != "master" && current != "HEAD" {
    return Ok(Some(PrepareBranchOutcome::AlreadyOnBranch { branch: current }));
}
```
On a detached HEAD this falls through to `prepare_branch_sync(root, &branch, "main")`, which checks out `main` then forks `wt/<slug>` (its `current != base` arm handles `HEAD != main`). A regression test (`ensure_work_branch_forks_from_main_when_detached`) should pin this.

---

### Review-focus checklist

**1. Correctness of the auto-fork logic.** Sound. `ensure_work_branch_sync` (git_ops.rs:519-542): (a) not-a-repo → `git rev-parse` fails → `Ok(None)` ✓ (planning never blocked); (b) non-main branch → `AlreadyOnBranch`, no mutation ✓; (c) on main → `prepare_branch_sync(root, &work_branch_name(root), "main")` forks/resumes ✓. `work_branch_name` (git_ops.rs:166-189) sanitization is correct: lowercase, non-alphanumeric runs collapse to one `-`, `trim_matches('-')`, `wt/` prefix, `wt/work` fallback — all pinned by `work_branch_name_sanitizes_the_root_basename`. The derived name always passes `valid_branch_name` (`wt/...`, no leading `-`, no whitespace). The `AlreadyOnBranch` outcome is unreachable from the on-main path (branch is never `main`), consistent with the two on-main tests expecting `Created`/`SwitchedExisting`.

**2. Sub-plan safety.** Correct. `(None, WorkflowState::Executing | WorkflowState::Skill) => None` (plan.rs:337) returns no note and never calls `ensure_work_branch` — a sub-plan never moves branches and reuses the parent's branch. The explicit-`(Some, Executing|Skill)` arm still ignores with a note (plan.rs:333-336). ✓

**3. Dirty-tree behavior.** Accepted G2 residual, handled safely. On main with dirty paths outside `.coding/`, `prepare_branch_sync` returns `Err` → `ensure_work_branch` returns `Err` → `create_plan`'s `(None, _)` arm (plan.rs:380-383) surfaces "git: auto-branch skipped — {reason}; the plan is created on the current branch (create the feature branch manually)" and still creates the plan. This is the documented best-effort posture (hard-erroring planning was explicitly rejected); the note is the mitigation. Not a finding.

**4. run_all steer.** Correct. `RUN_ALL_STEER` (run_all.rs:94-106) no longer mentions `branch: "fix/<short-slug>"`; it now says `create_plan auto-forks a per-directory working branch ... do NOT pass a branch arg unless the user explicitly asked` and still carries `never commit work to main`. The renamed test `run_all_prompt_relies_on_auto_fork_not_a_branch_arg` (run_all.rs:218-238) asserts the slug hint is absent, the never-main rule is present, and `auto-forks` is present — it fails against the old text. ✓

**5. Docs sync.** Clean. `agent.md:51-52`, `README.md:45`, `PLAN.md:141`, `.coding/skills/merge_to_main.toml:11-13` all describe the new "one wt/* per agent directory, auto-fork, branch arg is explicit-user-only, merge_to_main deletes the branch" policy. The only remaining `develop` mentions are explanatory ("The old `develop` integration tier was removed (2026-08-24)") — not stale topology claims. No stale references. (Historical `.coding/plans/` + `.coding/knowledge/` records mentioning `develop` are immutable snapshots — not findings, per scope.)

**6. Multi-platform neutrality.** Neutral. No Windows-only APIs/paths/shell syntax in the new library or app code. The `#[cfg(windows)]` `CREATE_NO_WINDOW` block (git_ops.rs:32-37) is pre-existing and sanctioned. `work_branch_name` uses cross-platform `Path::file_name()`. ✓

**7. Constitution.** Satisfied. `work_branch_name`, `ensure_work_branch` are `pub` with doc comments; `ensure_work_branch_sync` is private with a doc comment. No `#[allow(...)]` anywhere in the diff. No unused imports (`ensure_work_branch` + `prepare_branch` + `valid_branch_name` all used in plan.rs) and no dead code. Tests genuinely pin the new behavior: `ensure_work_branch_forks_stable_branch_from_main` (asserts `Created` — fails if the function returned `None`), `create_plan_auto_forks_when_on_main_and_no_branch_given` (asserts the branch switched to the slug + "created branch" in output — fails under the old `(None,_) => None` which left HEAD on `main`), and `run_all_prompt_relies_on_auto_fork_not_a_branch_arg` (fails against the old slug-hint text). The reuse test (`create_plan_auto_reuses_current_branch_when_not_on_main`) is a characterization test (same observable branch as the old no-op), but the on-main test is the real distinguishing pin. Build reported green (root 1508+0, src-tauri 169+0); I verified no warning sources (no unused imports/dead code/`#[allow]`).

---

### Observations (not findings — accepted design decisions, safe degradation)

- **Basename uniqueness (review-focus Q1c).** `work_branch_name` derives the branch purely from the directory basename. Two agent directories sharing a basename (e.g. sibling checkouts both named `AgenticCoder`) would derive the same `wt/<slug>`; git refuses to check out one branch in two worktrees, so the second fork fails → "auto-branch skipped" note → plan on `main`. This is the documented assumption ("the basename must be unique per directory") and the degradation is safe (no corruption), but the code provides no uniqueness guarantee or collision detection — it relies entirely on the external assumption. Acceptable given the documented decision; noted for completeness.

- **Untracked `.coding/` files.** The decision record, how record, and three plan files are records (not source) and consistent with the new policy. Not findings.
