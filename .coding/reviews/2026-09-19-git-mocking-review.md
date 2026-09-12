## Verdict: FINDINGS (0 high, 6 low)

Review of all uncommitted changes on `wt/agenticcoding` mocking git in tests
(`src/project/git_ops.rs`, `src/tool/workflow/plan.rs`, `src/memory/finish_capture.rs`,
`.config/nextest.toml`) + toolchain update to rustc 1.98.

**Production code is correct and safe.** All six `_impl` functions are logic-identical
to the originals (same git commands, same order, same error handling — verified
command-by-command); the public async wrappers default to `RealGit` (production
behavior unchanged); `CreatePlanTool::new()` defaults to `Arc::new(RealGit)` with
`with_git()` as test-only injection (no direct struct-literal constructions exist
elsewhere); the mock is `#[cfg(test)] pub(crate)]` so it never ships. Security is
sound (no new injection surface; mock is test-only). Multi-platform neutral (mock is
pure Rust; `git_raw` retains its existing `#[cfg(windows)]` CREATE_NO_WINDOW gate).

The findings below are all LOW — test-code quality, an inaccurate coverage claim, and
minor doc staleness. None affect shipped behavior.

---

### LOW-1: Broken no-op assertion in `checkpoint_then_rollback_restores_pre_edit_state`
**File:** `src/project/git_ops.rs:1074`

```rust
assert!(!git.commits().len() > 1, "no new commit after rollback");
```

`git.commits().len()` is a `usize`. In Rust, prefix `!` on an integer is **bitwise**
NOT (not logical NOT), and unary `!` binds tighter than `>`. So this parses as
`(!len) > 1`. For any small `len` (0, 1, 2, …), `!len` is a huge number
(`usize::MAX - len`), which is always `> 1`. **The assertion always evaluates to
`true` and can never fail**, regardless of how many commits exist. It is a no-op
that gives false confidence: if `rollback_impl` were ever broken to add a commit,
this test would still pass.

**Fix:** `assert!(git.commits().len() <= 1, "no new commit after rollback");`

This is not caught by `cargo test` (it compiles cleanly and passes) nor by
`#![deny(warnings)]` (no rustc warning for this pattern).

---

### LOW-2: `branch_hint_impl` has no unit test — plan's claim is inaccurate
**File:** `src/project/git_ops.rs:653`

The plan states **twice** that "the branch_hint logic is still tested by the mocked
`branch_hint_impl` tests in git_ops.rs." **No such tests exist.** A search for
`branch_hint_impl(` finds only the definition (line 653) and the call from the
async wrapper `branch_hint` (line 670) — there is no test that exercises it.

`branch_hint_impl` was extracted specifically so it could be mocked, but no mock
test was written. The only tests that exercised its **success path** (the
`"branch {name} @ {sha} (unmerged — …)"` format string + the main/master/empty
checks) were the 3 `finish_capture` tests, which are now `#[ignore]`'d. The only
default-suite coverage of `branch_hint` at all is `capture_outside_a_git_repo_has_no_branch_hint`
(see LOW-3), which exercises only the **failure** path (git errors → `None`).

So the success-path logic (format string, the `main`/`master`/empty guards) is
**untested in the default suite**. A regression there would only be caught by the
ignored integration tests.

**Fix:** Add a trivial unit test using `MockGit`:
```rust
#[test]
fn branch_hint_impl_formats_non_main_branch() {
    let git = MockGit::new();
    git.set_branch("feat/x");
    let root = Path::new("/tmp/mock");
    let hint = branch_hint_impl(&git, root);
    assert!(hint.as_deref().unwrap().contains("branch feat/x @ "));
    assert!(hint.unwrap().contains("(unmerged — exists only on this branch)"));
}

#[test]
fn branch_hint_impl_returns_none_on_main() {
    let git = MockGit::new(); // defaults to "main"
    assert!(branch_hint_impl(&git, Path::new("/tmp/mock")).is_none());
}
```

---

### LOW-3: `capture_outside_a_git_repo_has_no_branch_hint` spawns real git — contradicts "zero git subprocesses" claim
**File:** `src/memory/finish_capture.rs:662`

This test is **not** `#[ignore]`'d, yet it passes `Some(dir.path().to_path_buf())`
as the repo-root (line 673). `capture_finish` then calls `branch_hint(root)`
(line 88), which runs through `RealGit` and spawns `git branch --show-current`.
The command fails (not a repo) and `branch_hint` collapses to `None` (best-effort),
so the test **passes regardless of whether git is installed** — the suite does not
hard-depend on git. But the plan's claim "Zero git subprocesses in the default test
suite (git is fully mocked or `#[ignore]`'d)" is **inaccurate**: this test spawns
one real git subprocess when git is present (as it is on this Windows machine).

**Fix (pick one):**
- Mark it `#[ignore]` with the same reason as the other 3 (simplest; the failure
  path is low-value and the success path is what matters), **or**
- Refactor `capture_finish` to accept an injectable `GitRunner` and mock it (more
  work, but keeps the failure-path test in the default suite with zero subprocesses).

---

### LOW-4: Stale comment references the renamed function `prepare_branch_sync`
**File:** `src/project/git_ops.rs:610`

```rust
// On main: fork (or resume) the stable per-directory working branch from
// main. prepare_branch_sync carries dirty `.coding/` bookkeeping and
```

The function was renamed `prepare_branch_sync` → `prepare_branch_impl` in this very
change, but this comment inside `ensure_work_branch_impl` still names the old one.

**Fix:** `prepare_branch_sync` → `prepare_branch_impl`.

---

### LOW-5: Duplicate / orphaned doc comment above `MockRepo`
**File:** `src/project/git_ops.rs:929-931`

```rust
    /// A temp dir + MockGit combo for testing the git-orchestration logic

    /// A temp dir + MockGit combo for testing the git-orchestration logic
    /// without spawning a real `git` process. The temp dir holds real files
```

A one-line doc comment, a blank line, then the full doc comment — a copy-paste
artifact (the first line was rewritten into the full block but not deleted). Harmless
(no warning under `#![deny(warnings)]`), but cosmetic noise.

**Fix:** Delete the first (orphaned) one-line block.

---

### LOW-6: Stale timing figure in `.config/nextest.toml` comment
**File:** `.config/nextest.toml:7`

The comment says `cargo test --lib (13.97s, …)`, but the plan's measured result
after this change is **9.52s**. The comment predates the git-mocking optimization
and wasn't updated.

**Fix:** Update `13.97s` → `9.52s` (or remove the specific figure).

---

### Items verified clean (no findings)

- **`_impl` logic identity** — `checkpoint_impl`, `commit_success_impl`,
  `rollback_impl`, `prepare_branch_impl`, `ensure_work_branch_impl`,
  `branch_hint_impl` are byte-for-byte equivalent to the originals modulo
  `git(`/`git_raw(` → `git_run`/`git_run_raw` and the added `git: &dyn GitRunner`
  parameter. Same commands, same order, same error/restore paths.
- **Production wrappers unchanged** — `checkpoint`/`commit_success`/`rollback`/
  `branch_hint` construct `RealGit` inline; `prepare_branch`/`ensure_work_branch`
  delegate to `_with(Arc::new(RealGit), …)`. API signatures preserved.
- **`CreatePlanTool` injection** — `new()` sets `Arc::new(RealGit)`; `with_git()`
  is additive; `execute` clones the `Arc` (cheap) into the `_with` calls. No
  direct struct-literal construction exists elsewhere (only `new()`).
- **MockGit fidelity** — handles every command the `_impl` functions issue
  (`status --porcelain`, `add -A`, `commit -m`, `rev-parse HEAD`/`--short`/
  `--abbrev-ref`/`--verify --quiet`, `reset --hard`, `restore …`, `checkout`/
  `checkout -b`, `branch --show-current`). The carry mechanism (real file I/O via
  `restore_carries`) works correctly with the mock's no-op `restore` because the
  end-state assertions check re-written file content, not intermediate git state.
- **Security** — mock is `#[cfg(test)] pub(crate)]`; production uses `RealGit`
  (unchanged `git_raw` with `CREATE_NO_WINDOW` + env handling). No new injection
  surface; the `GitRunner` trait is `pub` but `RealGit`/`MockGit` are `pub(crate)`.
- **Multi-platform neutrality** — mock is pure Rust with no platform-specific code;
  `git_raw` retains its `#[cfg(windows)]` gate. No `cfg(windows)`-only additions.
- **Doc comments** — present on `GitRunner`, `RealGit`, `git_run`, `git_run_raw`,
  all `_impl` functions, `with_git`, and the `mock_git` module.
- **No `rust-toolchain.toml`** pins a version (CI uses `dtolnay/rust-toolchain@stable`),
  so the 1.89→1.98 update is an environment-level `rustup update` with no docs-sync
  obligation.
- **Test-success gate** — I cannot run `cargo test` (read-only reviewer), but the
  code structure is sound and the one broken assertion (LOW-1) is a no-op that
  *passes*, so it would not cause a failure. The plan's claim of 1650 passed /
  0 failed / 15 ignored is consistent with the structure observed.
