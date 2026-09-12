## Verdict: PASS

Verification re-review of commit `52a72a3` on `wt/agenticcoding` ("Mock git in
tests — zero git dependency, 9.5s suite"). All 6 prior low findings (report
`.coding/reviews/2026-09-19-git-mocking-review.md`, "FINDINGS 0 high/6 low") are
resolved. No new issues were introduced by the fixes. The core git-mocking
architecture (GitRunner trait, MockGit, `_impl` extraction, CreatePlanTool
injection) remains correct and was not disturbed by the fix commit.

---

### Finding-by-finding verification

**LOW-1 — Broken no-op assertion — RESOLVED.**
`src/project/git_ops.rs:1072` now reads:
```rust
assert!(git.commits().len() <= 1, "no new commit after rollback");
```
The bitwise-NOT no-op (`!len > 1`, which on a `usize` is always true) is replaced
with a correct `<= 1` comparison. The test's flow (checkpoint on a clean MockGit
tree → no commit, then rollback → no commit) leaves `commits` at length 1, so the
assertion passes; a regression where `rollback_impl` erroneously added a commit
would push the length to 2 and the assertion would now correctly fail.

**LOW-2 — `branch_hint_impl` untested — RESOLVED.**
Three unit tests added at `src/project/git_ops.rs:1517-1551`, all using `MockGit`
(no real git):
- `branch_hint_impl_formats_non_main_branch` — sets branch `feat/x`, asserts the
  hint contains `"branch feat/x @ "` and `"(unmerged — exists only on this
  branch)"`. Exercises the format string + the sha lookup (`rev-parse --short
  HEAD`).
- `branch_hint_impl_returns_none_on_main` — `MockGit::new()` defaults to `main`;
  asserts `None`. Exercises the `main` guard.
- `branch_hint_impl_returns_none_on_master` — `set_branch("master")`; asserts
  `None`. Exercises the `master` guard.

The success path (format string + main/master/empty guards) is now covered in the
default suite, closing the coverage gap the prior review identified.

**LOW-3 — Last git-spawning test not ignored — RESOLVED.**
`src/memory/finish_capture.rs:662`:
```rust
#[ignore = "integration: spawns real git (branch hint probe); run with `cargo test -- --ignored`"]
async fn capture_outside_a_git_repo_has_no_branch_hint() {
```
All four finish_capture branch-hint tests now carry the identical `#[ignore]`
attribute (`capture_on_a_feature_branch_carries_the_unmerged_hint`,
`capture_on_main_has_no_branch_hint`,
`capture_bug_digest_on_a_feature_branch_carries_the_unmerged_hint`,
`capture_outside_a_git_repo_has_no_branch_hint`). The default suite no longer
spawns any git subprocess — the "zero git dependency" claim is now accurate.

**LOW-4 — Stale comment `prepare_branch_sync` — RESOLVED.**
`src/project/git_ops.rs:610` (inside `ensure_work_branch_impl`):
```rust
// On main: fork (or resume) the stable per-directory working branch from
// main. prepare_branch_impl carries dirty `.coding/` bookkeeping and
```
The renamed function is now correctly referenced as `prepare_branch_impl`.

**LOW-5 — Duplicate/orphaned doc comment above `MockRepo` — RESOLVED.**
`src/project/git_ops.rs:929-932` now has a single, clean doc-comment block:
```rust
/// A temp dir + MockGit combo for testing the git-orchestration logic
/// without spawning a real `git` process. The temp dir holds real files
/// (the carry mechanism in `prepare_branch_impl` does real file I/O); the
/// MockGit simulates the git commands.
struct MockRepo {
```
The orphaned one-line duplicate is gone.

**LOW-6 — Stale timing figure in nextest.toml — RESOLVED.**
`.config/nextest.toml:7`:
```
# `cargo test --lib` (9.52s, already parallel across 32 cores) is the optimal
```
Updated from `13.97s` to `9.52s`, matching the post-optimization measured result.

---

### Core changes — still correct (re-confirmed)

The fix commit touched only the six finding sites plus the new tests; the core
git-mocking architecture is unchanged from the state the prior review confirmed
clean:

- **GitRunner trait + RealGit + MockGit** — `GitRunner` (`pub trait …: Send +
  Sync`, line 63), `RealGit` (`pub(crate)`, delegates to `git_raw` with its
  `#[cfg(windows)]` `CREATE_NO_WINDOW` gate, line 71), and `MockGit` in a
  dedicated `#[cfg(test)] pub(crate) mod mock_git` (line 680) — all intact.
- **6 `_impl` functions** — `checkpoint_impl`, `commit_success_impl`,
  `rollback_impl`, `prepare_branch_impl`, `ensure_work_branch_impl`,
  `branch_hint_impl` all present and logic-identical to the originals (same git
  commands, order, error/restore paths); the public async wrappers construct
  `RealGit` inline and delegate.
- **CreatePlanTool injection** — holds `Arc<dyn GitRunner + Send + Sync>`;
  `new()` defaults to `Arc::new(RealGit)`; `with_git()` is the additive test
  injection point; `execute` clones the `Arc` into `prepare_branch_with` /
  `ensure_work_branch_with`.
- **Test conversion** — 24 original git_ops tests + 3 new branch_hint tests (27
  total) use `MockGit`; 6 plan.rs tests use `mock_git_project()`; 4
  finish_capture branch-hint tests are `#[ignore]`'d.
- **Test-count consistency** — prior baseline 1650 passed / 15 ignored → +3 new
  passing (LOW-2) −1 newly-ignored (LOW-3) = 1652 passed / 16 ignored, matching
  the commit message exactly.
- **Warning-free** — the fixes introduce no new `#[allow(...)]`, no unused
  imports, no dead code. (The two pre-existing `#[allow(dead_code)]` on the
  `MockGit::set_sha` / `calls` test helpers were already present in the original
  commit and accepted by the prior review as test-only mock-API surface; they are
  not touched by the fixes and are not a new issue.)

### Items verified clean (no new findings)

- **Multi-platform neutrality** — the fixes are pure-Rust test/comment/config
  changes; no platform-specific code added. `git_raw` retains its
  `#[cfg(windows)]` gate.
- **Documentation sync** — the stale comment (LOW-4) and stale timing figure
  (LOW-6) are the doc-sync items, both fixed. No other docs reference the
  renamed function or the old timing.
- **Security** — no new injection surface; the mock remains `#[cfg(test)]
  pub(crate)]` and never ships.
- **Test-success gate** — as a read-only reviewer I cannot run `cargo test`, but
  the code structure is sound, the one previously-broken assertion (LOW-1) is
  now a correct comparison that passes, and the test-count arithmetic is
  internally consistent with the commit's claimed 1652/0/16.
