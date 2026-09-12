## Verdict: PASS

Verification review of the L1 fix from `.coding/reviews/2026-09-08-branch-policy-auto-fork-review.md` (detached-HEAD mishandled + misleading comment in `ensure_work_branch_sync`), applied in commit `decdf6b` on `wt/branch-policy-auto-fork`. `decdf6b` is HEAD and the working tree is clean (`git diff HEAD` empty), so the committed state is the state under review. The fix is correct and complete on all five verification points.

---

### 1. The guard — detached HEAD no longer returns `AlreadyOnBranch { "HEAD" }`

`src/project/git_ops.rs:536-540` (`ensure_work_branch_sync`):

```rust
if current != "main" && current != "master" && current != "HEAD" {
    return Ok(Some(PrepareBranchOutcome::AlreadyOnBranch {
        branch: current,
    }));
}
```

The `&& current != "HEAD"` clause is present. On a detached HEAD, `git rev-parse --abbrev-ref HEAD` succeeds with `current == "HEAD"` (line 527), so `current != "HEAD"` is **false**, the `&&` short-circuits to false, and the early return is skipped. Control falls through to lines 545-546:

```rust
let branch = work_branch_name(root);
prepare_branch_sync(root, &branch, "main").map(Some)
```

Tracing `prepare_branch_sync(root, "wt/<slug>", "main")` with `current == "HEAD"` (lines 324-473): name validation passes (`wt/...` is valid); the tree is clean so `carry` is empty; line 385 `"HEAD" == branch` is false (no `AlreadyOnBranch`); line 407 `"HEAD" != "main"` is true → `git checkout main` (line 408); line 425 `branch_exists` is false on a fresh repo → `git checkout -b wt/<slug>` (line 433) → `PrepareBranchOutcome::Created { branch, base: "main", carried: 0 }`. A detached HEAD is now forked from `main` rather than reused. ✓

(Error-path note: if the base checkout at line 408 failed, line 461 runs `git checkout &current` = `git checkout HEAD`, a no-op that keeps HEAD detached at the original commit — the correct "return to original state" for a detached HEAD. Best-effort and correct; not a concern.)

### 2. The comment — accurate, no misleading "prints nothing" claim

`src/project/git_ops.rs:520-535`. The probe comment now reads:

> "NOTE: a detached HEAD is NOT a failure here — `rev-parse --abbrev-ref HEAD` succeeds and prints the literal "HEAD" — so it is handled explicitly below (fork from main, never reuse a detached HEAD)."

And the guard comment (lines 531-535):

> "A detached HEAD ("HEAD") is NOT reused — work there is easily lost on the next checkout — so it falls through to the fork-from-main path below."

Both accurately describe the behavior. The misleading "detached HEAD that prints nothing" clause from the original finding is gone from `ensure_work_branch_sync`. (The "prints nothing" phrasing at line 570 lives in `branch_hint`'s doc comment, which correctly uses `git branch --show-current` — that command genuinely prints nothing on a detached HEAD, so it is accurate and is not the comment the finding targeted.) ✓

### 3. The regression test — genuinely fails old, passes new

`src/project/git_ops.rs:1261-1285` (`ensure_work_branch_forks_from_main_when_detached`):

- Creates a fresh repo on `main` (clean tree, one commit), then detaches HEAD via `git checkout <sha>` (line 1267).
- Sanity-asserts `repo.current_branch() == ""` (line 1268-1272) — `current_branch()` uses `branch --show-current`, which prints nothing on a detached HEAD, confirming HEAD is actually detached before the call.
- Calls `ensure_work_branch` and asserts `Some(Created { branch: <slug>, base: "main", carried: 0 })` (lines 1276-1283) plus `repo.current_branch() == expected` (line 1284).

**Fails on old code:** without `!= "HEAD"`, `current == "HEAD"` satisfies `!= "main" && != "master"` → returns `AlreadyOnBranch { branch: "HEAD" }`, which does not match `Created { .. }` (first assertion fails), and the repo stays detached (`current_branch() == ""` ≠ `expected`, second assertion fails). **Passes on new code:** the guard falls through to `prepare_branch_sync`, which checks out `main` and forks `wt/<slug>` → `Created { base: "main" }`, and the repo ends on the slug branch. The test pins the exact regression. ✓

### 4. No regressions in the other `ensure_work_branch` tests

The added `!= "HEAD"` clause does not alter behavior for any other case (all in `src/project/git_ops.rs`):

- `ensure_work_branch_forks_stable_branch_from_main` (1191): `current == "main"` → `!= "main"` false → falls through to fork → `Created`. ✓
- `ensure_work_branch_reuses_a_non_main_branch` (1211): `current == "feat/prev"` → all three `!=` true → `AlreadyOnBranch { "feat/prev" }`. ✓
- `ensure_work_branch_resumes_an_existing_stable_branch` (1233): second call `current == "main"` → falls through → branch exists → `SwitchedExisting`. ✓
- `ensure_work_branch_returns_none_outside_a_repo` (1290): `git rev-parse` fails → `Ok(None)`. ✓

The `prepare_branch` test suite is unchanged (only a cosmetic comment edit on `prepare_branch_reuses_existing_wt_branch`, lines 1005-1007). The `create_plan` auto-fork tests in `src/tool/workflow/plan.rs` are unaffected by the guard change. The diff relative to the original review's scope is exactly: the `!= "HEAD"` clause, the rewritten comment, and the new detached-HEAD test — nothing else. ✓

### 5. Constitution

- **No `#[allow(...)]`** anywhere in the diff (git_ops.rs additions: `work_branch_name`, `ensure_work_branch_sync`, `ensure_work_branch`, five tests).
- **Public fns documented:** `work_branch_name` (pub, doc lines 155-165), `ensure_work_branch` (pub, doc lines 549-558). `ensure_work_branch_sync` is private (`fn`, line 519) with a doc comment (lines 517-518). ✓
- **Warning-free build:** no unused imports (`ensure_work_branch` is imported and used in plan.rs:25; `work_branch_name` is used in `ensure_work_branch_sync` and tests), no dead code, no `mut`/shadowing issues. Implementer reported root 1509+0 / src-tauri 169+0 green (1509 = the prior 1508 + the new detached-HEAD test, consistent with the fix). I verified the logic and test design by inspection; the test count delta matches exactly one added regression test. ✓

---

### Notes (not findings)

- **Test-count staleness in the original review report.** The committed review report states "root 1508+0" while the committed code has 1509 tests (the report's own L1 finding says a regression test "should pin this", future tense). This reflects the review being authored against the pre-fix state (1508) and the fix's test landing alongside it (→1509) in the single commit `decdf6b` — internally consistent with the commit message ("regression test added"). The actual final state is 1509+0 green. Not a code issue.
- **Reviewer tool surface.** As a read-only reviewer I verified correctness by code inspection + tracing `prepare_branch_sync`'s control flow; I could not independently re-run `cargo test` (no shell). The implementer's reported green build (1509+0 / 169+0) is consistent with the code and test design analyzed above.
