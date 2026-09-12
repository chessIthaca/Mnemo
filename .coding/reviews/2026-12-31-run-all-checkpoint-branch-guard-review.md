## Verdict: FINDINGS (0 high, 2 low)

Bug-fix plan a91e71d3 (backlog 04bd6977, security review MEDIUM): the Run-All pre-item checkpoint branch guard is **correct, complete for the scoped defect, and well tested** — the fix genuinely closes the commit-to-main hole, and the stricter stop-the-run semantics are the right call. Both findings are documentation-only stragglers from the `checkpoint` → `checkpoint_on_work_branch` rename (three broken intra-doc links; one stale module-doc bullet). No behavioral defects found.

## Scope & method

- Reviewed: the full uncommitted diff on `wt/agenticcoding` (HEAD = 2ac5c83) — `.coding/backlog.jsonl` (this item's in-flight flip), `PLAN.md` (two clauses), `src-tauri/src/ipc/run_all.rs` (import + call swap + block comment), `src/project/git_ops.rs` (+151/−25: async rename, new guard core, three regression tests) — plus the untracked `.coding/knowledge/bug/2026-12-31-run-all-pre-item-checkpoint-commits-to-main-no-b.md` (BUG record) and `.coding/plans/a91e71d3.md` (plan file).
- Method: read the changed code in full plus the machinery the guard rides (`ensure_work_branch_impl`, `prepare_branch_impl`, `work_branch_name`, `MockGit::run`); traced all three regression tests command-by-command through the mock; text-searched every `.rs` and root `.md` file for old-name stragglers and stale checkpoint claims; cross-checked the pre-edit code graph, which independently confirms `run_all_dispatch_next` was the old `checkpoint`'s sole caller.

## The six requested checks

### 1. Bug-plan requirements — PASS

- **Regression tests exercise the changed path.** All three call `checkpoint_on_work_branch_impl` directly (git_ops.rs:1607, 1647, 1674) — the new guard, not merely the unchanged commit core. `checkpoint_impl` keeps its own direct tests (git_ops.rs:1044-1114), so both layers are covered.
- **Root cause documented.** The BUG knowledge file (untracked, ships with this commit), the semantic BUG memory (ad81eb38), and the plan file's Bug section all carry symptom → root cause (branch-agnostic `checkpoint_impl` is the only pre-dispatch git step; the wt/* auto-fork lives later, inside `create_plan`) → fix + regression-test names.
- **RED phase genuine** (verified by logic — I cannot run shell): against the old `checkpoint_impl`, test 1 fails its final assertion (`current_branch() == work_branch_name(root)` — the old code never switches branches, so the branch stays `"main"`); test 3 fails at `expect_err` (the old code returns `Ok` and commits, so `commits != ["initial"]`); test 2 is a green pin the old behavior already satisfied. The assertions are constructed so pre-fix behavior cannot pass them — the RED demonstration is sound.

### 2. Err-propagation choice (stop the run) — PASS, justified and correctly wired

- **Stricter than create_plan is right.** create_plan's fork refusal proceeds on the current branch with a skip note — safe there because planning writes only `.coding/` files. The dispatch path is different: proceeding would run the item's real work on main, and `commit_success` at resolution would land it on main too. Stopping is the only constitution-safe choice, and it preserves the rollback-anchor chain (the checkpoint sha, backlog 45dcf577).
- **Wiring verified** (run_all.rs:968-986): `Err` → `annotate("git checkpoint failed: work-branch fork refused: …")` + `end_run` + `return Err`. The item is never stamped: the InFlight stamp happens only at Executing entry (forwarder → `stamp_backlog_in_flight`), strictly after dispatch, and the failure path returns before dispatch. The existing source-contract test `checkpoint_failure_leaves_the_item_pending` (run_all.rs:740) still pins annotate-not-stamp and still passes — the annotate format string is unchanged.
- **Refusal mutates nothing.** The outside-`.coding/` classification error in `prepare_branch_impl` (git_ops.rs:450-455) returns before any restore/checkout/commit — matching test 3's `commits == ["initial"]` and `current_branch == "main"` assertions exactly.

### 3. Ok(None) fall-through (non-repo) — PASS

- Non-repo: `rev-parse --abbrev-ref HEAD` fails → `Ok(None)` (git_ops.rs:634-637) → `Ok(_) => checkpoint_impl` → `status --porcelain` fails → `Err` → the same dispatch failure path as pre-fix. Net user-visible behavior identical (one extra failed probe first). MockGit's `set_not_a_repo` models exactly this for the existing `ensure_work_branch` none-test.
- Other `Ok` arms verified too: **AlreadyOnBranch** (any non-main/master/detached branch — no mutation, checkpoint commits there; constitution-safe, and matches create_plan's reuse semantics including sub-plan parent preservation); **forked** (from main or detached HEAD — `prepare_branch_impl` carries only `.coding/` bookkeeping; the detached-HEAD path transits main only inside one synchronous closure, with no commit possible in between).

### 4. Documentation sync — FINDINGS (2 low, below)

- **PLAN.md**: both new clauses (~:49, ~:916) accurately describe the behavior ("forked/reused first, never main; a refused fork stops the run"). The other two checkpoint mentions (:71 auto-feed, :74 requeue) are unrelated and accurate.
- **README.md**: zero "checkpoint" matches — the "nothing to update" claim is verified.
- **Module docs**: two gaps → Findings 1 and 2.

### 5. Multi-platform neutrality — PASS

Pure git-subprocess orchestration (Command argv, no shell strings), `tempfile` in tests, no `cfg(windows)`, no path or platform assumptions anywhere in the diff.

### 6. Dead code / contract drift — one straggler class (Finding 1)

- **No code references to the old `checkpoint` remain.** Full-text search of all 257 `.rs` files: the only old-name occurrences are three doc-comment links (below). The warning-free build under `#![deny(warnings)]` independently proves it — an unresolved import or a dead export would fail compilation.
- The pre-edit code graph independently corroborates `run_all_dispatch_next` was the sole caller, so removing the unguarded wrapper (rather than keeping it) was correct — it would have been dead code under deny-warnings anyway.
- `checkpoint_impl` itself is legitimately still alive: called by the new guard and directly tested.

## Findings

### F1 (LOW) — three stale `[`checkpoint`]` intra-doc links pointing at the removed function

- `src/project/git_ops.rs:172` (`commit_success` doc), `:206` (`rollback` doc), `:600` (`prepare_branch_with` doc) — each says "(see [`checkpoint`])", but `pub async fn checkpoint` was removed by this change. The links are broken (`rustdoc::broken_intra_doc_links` under `cargo doc`); they are invisible to `cargo test` because rustdoc lints don't run in test builds — which is exactly why the reported green suite didn't catch them. This is the one place the rename left stragglers.
- **Fix:** `[`checkpoint`]` → `[`checkpoint_on_work_branch`]` in all three doc comments.

### F2 (LOW) — run_all.rs module doc still describes the pre-fix branch-agnostic checkpoint

- `src-tauri/src/ipc/run_all.rs:13-17` — the safety-model bullet "**Git checkpoint per item.** Before dispatching, the loop commits any dirty working tree and records the HEAD sha." was not updated. The branch guard is a constitution-level guarantee (never commit to main), the module doc is the run-all contract's front door, and the project's review expectations explicitly cover module doc comments. PLAN.md got the clause; this doc didn't.
- **Fix:** extend the bullet, e.g. "Before dispatching, the loop forks/reuses the per-directory wt/* work branch (never main — a refused fork stops the run), commits any dirty working tree, and records the HEAD sha."

## Non-blocking observations (no action required for this plan)

1. **Same defect class at resolution time.** `commit_success` (and the manual `rollback`) remain branch-agnostic — if the user manually switches to main mid-item (e.g. lands a merge while an item runs), the success commit lands on main. Out of scope here (the security finding scoped the pre-item checkpoint; the trigger requires user action mid-item), but it is the natural follow-up backlog item if you want the class fully closed.
2. **master-trunk repos.** `ensure_work_branch_impl` treats `master` as protected and forks from `main` — on a repo whose only trunk is `master`, every Run-All dispatch now stops with "work-branch fork refused: git checkout main failed: …". That is the safe direction (pre-fix committed to the trunk) and identical to create_plan's pre-existing semantics; noted only so the behavior change is conscious.
3. **Error wording.** "work-branch fork refused: {reason}" also wraps non-refusal fork failures (checkout/restore errors). Slightly imprecise, but it conveys the load-bearing fact (no fork → nothing committed → run stopped). Fine as-is.

## Test evidence

- Reported: root `cargo test` 1967 passed / 0 failed / 4 ignored (1964 baseline + 3 new — arithmetic consistent); src-tauri 186 + 4 doc-tests (baseline unchanged — consistent with the call-swap-only change); frontend untouched (confirmed: no TS in the diff).
- I could not re-run tests (no shell); the results are consistent with everything verified by reading, and the mock-traced expectations match the code paths exactly (fork → `checkout -b` switches `current_branch` and the commit lands after it; AlreadyOnBranch → no mutation; refusal → pre-mutation `Err`).
- Live corroboration: the backlog.jsonl flip in this very diff records item 04bd6977's checkpoint note as `2ac5c832…` — the current HEAD sha on `wt/agenticcoding`. The new code ran for this item (AlreadyOnBranch, clean tree, sha recorded, no spurious commit).

## Conclusion

The fix is sound and the bug-plan requirements are met. Fix F1 (three doc-link one-liners) and F2 (one doc bullet), re-run `cargo test` (doc-only changes — the suite should stay green), then commit everything including this report.
