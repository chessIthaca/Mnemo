# Review: create_plan `branch` param — fork feature branch inside the tool (feat/create-plan-branch)

Scope: ALL uncommitted changes (`git diff HEAD` + `git status --short`) —
`src/project/git_ops.rs`, `src/tool/workflow/plan.rs`, `src/workflow/mod.rs`,
`src/tool/agent/git.rs`, `src-tauri/src/ipc/run_all.rs`, plus the expected
`.coding` bookkeeping churn (backlog flip, stack.json, new plan file).

Static review only — as the read-only reviewer I could not execute `cargo
test`; both crates' green runs are taken from the author's report. All code
paths below were verified by reading the full files, not just the diff.

---

## Findings

### 1. [Bug — medium] Err after a successful `git checkout <base>` leaves HEAD on `base` (usually main) — the item then proceeds and commits to main

**Where:** `src/project/git_ops.rs:321-353` (`prepare_branch_sync`)

Once `git checkout base` succeeds (git_ops.rs:321-326), a subsequent failure
of `git checkout -b <branch>` / `git checkout <branch>` (git_ops.rs:332-353)
runs `restore_carries` (correct — no data lost) but **returns Err while HEAD
is still on `base`**, not on the original branch. The original branch name
(`current`, captured at :301) is never restored.

Consequences, concretely:
- `create_plan` reports "branch prep skipped — …; the plan is created on the
  **current branch**" (plan.rs:259-262) — but the current branch is now
  `base`, i.e. **main**, in direct contradiction of the new RUN_ALL_STEER
  rule "never commit work to main" (run_all.rs:94-95).
- The carried dirty bookkeeping (the backlog InFlight flip) was re-applied
  onto main's worktree, so even the *next* item's pre-item `checkpoint`
  (`git add -A && commit`) will commit it to main.
- The plan-loop's `commit_success` at item end commits the item's work to main.

This is reachable without exotic states: `valid_branch_name` (git_ops.rs:
147-149) only rejects empty/leading-`-`/whitespace, but `git checkout -b`
itself rejects many names that pass it — e.g. `fix/a..b` (consecutive dots),
`fix/.hidden` (component starting with `.`), `x.lock`, trailing `.` — and the
refs/heads namespace conflict case (`checkout -b fix/x` while branch `fix`
exists → "cannot lock ref"). In every such case the repro is: start on
`feat/prev`, call `prepare_branch(root, "fix/a..b", "main")` → Err, and
`current_branch()` is `main`, not `feat/prev`.

**Fix:** remember `current` (already captured at :301) and, in the Err arm at
:349-352, best-effort `git checkout <current>` before `restore_carries` (the
tree is clean-for-carried-paths at that point, so the checkout back is safe).
Add a regression test asserting the branch is unchanged after this failure
(e.g. `prepare_branch_failure_after_base_checkout_restores_original_branch`,
using `fix/a..b` as the repro name). Note the existing
`prepare_branch_missing_base_errors_and_restores_carry` test only covers the
checkout-base-**failure** path; the checkout-base-**success**-then-branch-
failure path is untested.

### 2. [Robustness — low] Snapshot/restore edge cases can silently lose carried bytes: `None` conflates "deleted" with "unreadable"; re-write skips parent-dir creation and swallows all errors on the success path

**Where:** `src/project/git_ops.rs:296` (snapshot), `:240-251`
(`restore_carries`)

- `let content = std::fs::read(root.join(&path)).ok();` records `None` both
  for a genuinely deleted path (intended: re-delete after the switch) and for
  a file that exists but can't be read (locked by another process on Windows,
  permissions). In the unreadable case the file is treated as deleted:
  `git restore` resets it, and `restore_carries` then *attempts to delete it*
  (:247). Original dirty bytes unrecoverable.
- `restore_carries` uses `let _ = std::fs::write(...)` (:244) with no
  parent-dir creation. A carried path whose parent dir doesn't exist on the
  target branch fails silently — **on the success path too**, where the
  outcome still reports `carried = N` (describe() at :184-190), so the agent
  is told the flip was carried when it wasn't.

Neither is reachable with today's fixed set of carried paths
(`.coding/backlog.json`, `.coding/plans/stack.json` — both parents always
exist and are plain files), but this is the transactional core of the
feature; a future carried path (e.g. a nested reviews file) walks into it.
**Suggested:** (a) distinguish "not found" from other read errors — only
`ErrorKind::NotFound` may become `None`, anything else should Err;
(b) `create_dir_all(parent)` before `fs::write`;
(c) at minimum on the success path, surface a restore failure (append to the
outcome / downgrade to Err) instead of discarding it.
(d) Add a test for the deletion carry (` M`-style ` D .coding/x` → file
absent again after the switch) — the `None` arm of `restore_carries`
(:246-248) currently has zero coverage.

### 3. [Docs/robustness — low] `Workflow::project_root()` doc is wrong for shallow *relative* plans dirs — returns `Some("")`, not `None`

**Where:** `src/workflow/mod.rs:167-179`

`Path::new("plans").parent()` is `Some("")` (not `None`) — `parent()` only
returns `None` for a root. So a relative shallow `plans_dir` yields
`Some(Path(""))`, and `git_raw` runs git with `current_dir("")`, which fails
at spawn → prepare_branch errs → the graceful skip note. Safe outcome, but
the doc comment's claim ("Returns `None` when the plans dir is too shallow to
imply a root (fewer than two parent components)") is inaccurate. Production
always passes an absolute `<root>/.coding/plans` (factory), so this is
doc-truth + hardening: filter out empty components (e.g. require the result
to be non-empty / absolute) or fix the doc. No test covers the `None` skip
path in plan.rs either.

### 4. [Nit] Formatting + direct unit-test gap for the untrimmed-output fix

- `src/project/git_ops.rs:396-397` — double blank line before `#[cfg(test)]`;
  `cargo fmt` would collapse it.
- The dev-time trim bug (old `git()` ate the leading status-flag space of the
  first porcelain line) is only covered *incidentally*: in every `diverged_repo`
  the first porcelain line is ` M .coding/backlog.json`, and with the old trim
  the path would lose its leading `.` → `is_bookkeeping` false → refuse → the
  happy-path tests fail. That's real coverage, but a one-line direct test
  (`git_raw` output starts with `' '` for a worktree-modified file, or a
  `parse_porcelain_line(" M x")` unit test) would pin the contract explicitly.

---

## Verified — no findings (answers to the review brief)

**Transactional carry ordering.** Snapshot → `git restore --source=HEAD
--staged --worktree --` → `checkout base` → `checkout -b/branch` → re-write
is sound: every failure path after the restore step calls `restore_carries`
before returning Err (git_ops.rs:315-318, 322-325, 344-352); before the
restore step nothing destructive has happened. `git restore --source=HEAD
--staged --worktree` is the correct clean for tracked M/D (resets index +
worktree to HEAD; staged modifications `M `/`D ` are correctly *carried*, not
rejected). Porcelain parsing is correct: XY at 0/1, path from offset 3;
renames' post-image after `" -> "` (and renames are always rejected upstream
by the `A|R|U|C` check, so the arrow handling is belt-and-braces); `str::lines()`
strips `\r\n`, so CRLF is safe. Quoted-path handling only strips surrounding
quotes without C-unescaping — unreachable for `.coding/`'s ASCII fixed names,
and a misparse degrades to a graceful Err (restore pathspec failure), not data
loss. Leaving `??` untracked files in place is correct — checkout never moves
them; a collision makes the checkout fail → restore → Err. The only residual
state issue is Finding 1.

**State-machine integration.** The workflow mutex (tokio `Mutex`, held at
plan.rs:222) is held across the `prepare_branch(...).await` (plan.rs:257) —
verified deadlock-free: `prepare_branch` never takes the workflow lock (or
any lock), so no lock-ordering cycle; other workflow tools serialize behind
it for the bounded duration of a few git spawns, which the in-code comment
documents as deliberate. `plan_mutations_allowed` gate runs **first**
(plan.rs:223-227) — sub-agents cannot trigger branch forks at all. The
`Executing | Skill` ignore-arm (plan.rs:236-239) is correct and importantly
`Planning`/`Complete` are *not* included — run-all items rest in `Complete`
between turns, and forking there is exactly the intended per-item flow.
`Reviewing` falls into the fork arm, but that is practically benign: mid-
Reviewing the tree holds uncommitted source (refused as "outside .coding/") or
is already committed (switch harmless); the stack-clear on create_plan from a
non-Executing state is pre-existing `create_plan_with_kind` semantics.
`project_root() == None` (and the `Some("")` variant, Finding 3) degrades to
a skip note — plan always still created.

**Safety.** Flag injection closed: both `branch` **and** `base` validated in
plan.rs (245-250) *and* re-validated in `prepare_branch_sync` (260-265);
carried paths passed after `--`; the existence probe is `rev-parse --verify
--quiet refs/heads/<branch>` which can't start with `-`. Args are discrete
`Command` argv (no shell) on both paths. Memory stores are categorically
excluded (`is_bookkeeping`, git_ops.rs:228-235 — and exclusion errs on the
safe side rather than touching them). No `commit`, `merge`, `push`, or
anything that can land or publish commits anywhere in `prepare_branch` — it
only creates/switches branches and rewrites `.coding` bookkeeping bytes.
Consistency with the repo's security model: the approval gate is defined over
core operations (merge/push — `GitTool::is_core_operation`), and run-all's
existing ungated internals already include `rollback` = `git reset --hard`,
which is strictly more destructive; an ungated branch fork is consistent.
`git.rs`'s switch to the shared `valid_branch_name` is byte-identical logic
(all 5 call sites converted; no behavior change), and `git()` still trims via
`git_raw().map(trim)` for every existing caller (checkpoint / commit_success /
rollback / rev-parse) — behavior unchanged.

**Run-all interplay.** Ordering is strictly sequential: checkpoint commits the
dirty tree and lands *before* dispatch; the InFlight flip lands after it; the
agent's `create_plan(branch)` (and thus `prepare_branch`) runs strictly inside
the item's turn — no mid-item checkpoint can interleave. The carried flip
riding `commit_success` on the item's own branch is the designed behavior. On
item failure, `rollback` (`reset --hard <checkpoint-sha>`) now runs while the
new branch is checked out, pointing it at the previous branch's checkpoint
commit — an orphan-ref nuisance only (the work was being discarded and the
next item forks from main again); identical to the pre-feature manual dance,
no action needed.

**Constitution.** All new public items have doc comments (`valid_branch_name`,
`PrepareBranchOutcome` + variants/fields, `describe`, `prepare_branch`,
`project_root`); no `#[allow]` anywhere in the diff; tests exist for every
new behavior except the two gaps called out in Findings 1-2; both crates'
green test runs are the author's report (warning-free under `deny(warnings)`
follows from green compile).

---

## Verdict

The design and the happy/skip paths are solid and well-tested; the security
model holds (no injection, no core operations, memory stores untouched, gate
ordering correct). Fix Findings 1 (restore the original branch on the
post-base-checkout failure path + regression test) and 2 (deleted-vs-unreadable
distinction, parent-dir creation, deletion-carry test) before committing;
Finding 3's doc fix and Finding 4's nits can ride along in the same pass.
