## Verdict: FINDINGS (0 high, 4 low)

Review of ALL uncommitted changes on `wt/macos-fix` for plan a93fa25b / backlog 64662ef2 (sweep merged `wt/runall-*` branches after PR merges — the follow-up to review L4 of the b52b041a landing). The sweeper's core mechanics are correct and safe: the ancestry gate's direction and exit-code semantics are right (`git_raw` maps non-zero exits to `Err`, so `.is_ok()` = exit 0 = ancestor = merged), namespace isolation holds (glob + prefix guard, proven by a fully-merged non-runall branch in the test), the fetch is best-effort with a safe under-delete direction, and the wiring preserves both source pins on `backlog_run_all`. Four low findings: the `+ ` worktree-branch prefix is unhandled in the parse with three comments misattributing the keep to git's refusal, the wiring placement invariant is unpinned, the squash-merge residual is undocumented, and the b52b041a SPEC knowledge record is now stale.

## Scope & method

Reviewed the full uncommitted diff (5 modified files + the untracked plan file `.coding/plans/a93fa25b.md`): `src/project/worktrees.rs` (the sweeper `sweep_merged_runall_branches` + impl + the regression test + three doc sites), `src-tauri/src/ipc/backlog_cmds.rs` (the run-start wiring + eprintln), `agent.md`, `PLAN.md`, `.coding/backlog.jsonl`. Cross-checked against review L4 of `.coding/reviews/2026-09-21-b52b041a-ruleset-aware-run-all-landing-review.md`, re-derived `git_raw`'s exit-code contract (src/project/git_ops.rs:33-55), re-derived both source pins that extract `backlog_run_all` (run_all.rs:1920 heartbeat pin, run_all.rs:3017 adopt pin) against the marker positions in the edited file, and checked every doc/knowledge site that mentions the branch-keeping promise.

## Core mechanics verified (correctness)

1. **Ancestor gate direction + exit-code semantics — CORRECT.** `git_raw` returns `Ok(stdout)` only on `output.status.success()` (git_ops.rs:47-54), so `git_raw(..., ["merge-base", "--is-ancestor", branch, "origin/main"]).is_ok()` means exit 0 = the branch tip IS an ancestor of `origin/main` = its commits are in main → delete. Exit 1 (not merged) → `Err` → kept; exit 128 (no `origin/main`, bad ref, no remote) → `Err` → kept. The gate can only under-delete, never over-delete.
2. **`-D` after the explicit ancestor check — correct and REQUIRED.** A branch merged into `origin/main` but not into the current HEAD (the main worktree sits on `wt/macos-fix`) would make `-d` refuse ("not fully merged"); the ancestry gate already proved redundancy, so `-D` is safe.
3. **Namespace isolation.** `git branch --list 'wt/runall-*'` + the `starts_with("wt/runall-")` guard; the test proves a fully-merged NON-runall branch (`wt/other` at main's tip) is never touched — the strongest keep-case.
4. **Best-effort fetch.** `let _ = git_raw(..., ["fetch", "origin"])`: a failed fetch leaves a stale `origin/main` → under-delete only (documented). No remote / no `origin/main` → merge-base `Err` → keep.
5. **Injection safety.** Every subprocess is `Command::new` + argv (no shell on any platform); branch names come from git's own listing and are guarded to the `wt/runall-` prefix, so no flag-shaped name can reach an argument position.
6. **Wiring + pins.** The sweep sits between `adopt_orphaned_in_flight` (backlog_cmds.rs:548) and the pending-count early return (578); marker first-occurrences verified (539 < 548 < 555 < 578), so the adopt pin (run_all.rs:3027-3031) holds, and the heartbeat pin (run_all.rs:1920-1937: armed < spawn < first_dispatch) is unaffected (all three markers sit after the insertion). The eprintln matches the codebase's `backlog: ...` diagnostics convention (90 eprintln sites); the inline `root.lock().await.root.clone()` argument matches 7 established sites in run_all.rs.
7. **The regression test genuinely covers the acceptance.** Merged runall branch (commit + `merge --no-ff` into main + push) swept; unmerged runall branch kept; merged non-runall branch kept; worktree-checked-out merged branch kept + its worktree intact; exact `assert_eq!` on the swept list. It fails without the sweeper's delete (swept would be empty). The bare-upstream-outside-the-tree subtlety (an in-tree upstream gets tracked by `git add -A` and later rewound by a checkout) is real and documented — the same trap the existing sync tests work around.
8. **Docs.** agent.md, PLAN.md, the module doc, `remove_worktree_only`'s doc, and `land_via_pull_request`'s doc all point at the sweeper consistently with the code; no "no sweeper yet" note remains in shipped docs.

## Findings

### L1 (low, robustness + comment accuracy): the `+ ` worktree-branch prefix is unhandled in the parse, and three comments misattribute the keep to git's refusal

`git branch --list` marks branches checked out in OTHER worktrees with `+ ` — exactly the in-flight runall shape (`+ wt/runall-cccccccc`). `strip_prefix("* ")` leaves it, `trim()` leaves the `+`, and `starts_with("wt/runall-")` then fails → the line is SKIPPED by the namespace guard; git's "cannot delete branch used by worktree" refusal is never reached. Yet the doc comment (worktrees.rs:185-187: "refused by git itself … the in-flight case needs no extra code"), the test comment (816-818: "git refuses the delete"), and the inline comment (209-211, which claims the only prefixes are `* ` and two spaces) all tell the git-refusal story. The observable behavior is correct in every case (and would remain correct if `+ ` were stripped, since git does refuse as a second guard), but the mechanism is documented wrong three times and the unhandled prefix is silent — an auditor asking "does the parse handle `+ `?" finds no acknowledgment. Fix: strip `+ ` explicitly alongside `* ` and correct the comments to name BOTH guards; optionally `git worktree prune` before listing (mirroring `remove_worktree_only_impl`'s R3-L2 lesson) so an externally-deleted worktree's stale metadata doesn't pin its branch in the listing forever.

### L2 (low, test gap): the wiring placement invariant is unpinned

The placement — sweep BEFORE the pending-count early-return, so a run-start that finds no eligible items still sweeps (the acceptance's "the next run-start deletes the merged branch") — is enforced only by the comment at backlog_cmds.rs:549-554. The codebase's established mechanism for exactly this class of Tauri-command wiring invariant is the source-pin test: the adopt pin (run_all.rs:3014-3031) asserts the identical shape for `adopt_orphaned_in_flight`. A later edit that moves the sweep after the early-return (or into the post-count region) silently regresses the acceptance for empty/deferred-only backlogs with no test failing. Two lines in the existing pin test — `find("sweep_merged_runall_branches")` and `assert!(adopt_call < sweep_call && sweep_call < pending_count)` — pin it.

### L3 (low, design residual): a squash-merged PR breaks ancestry and the branch is never swept

The human merges on GitHub and may pick "Squash and merge": the squash commit is content-identical but SHA-different, so `merge-base --is-ancestor` fails and the branch lingers — the exact accumulation this change exists to remove, in a plausible merge method (the repo's ruleset restricts pushes, not merge methods). The direction is safe (under-delete only, never over-delete) and the no-`gh pr view` double-guard decision is documented — but its cost on this edge is not. One doc-comment sentence acknowledging the squash residual (or an optional `gh pr view --json state` == MERGED fallback in a follow-up) would pin the decision's boundary.

### L4 (low, documentation sync / memory hygiene): the b52b041a SPEC knowledge record is now stale

`.coding/knowledge/spec/2027-01-11-ruleset-aware-run-all-landing-two-paths-pr-on-pr.md:6` still says "branch KEPT — the PR's head; no sweeper yet, follow-up backlog 64662ef2" and carries the invariant "no path deletes a PR-head branch" — now false in the literal sense: the sweeper deletes merged PR-head branches by design. Amend at finish (memory_amend) to record the sweeper and restate the invariant as "no path deletes a PR-head branch before its commits are in `origin/main`" — the same protocol as the prior review's L6.

## Non-finding observations (no action required)

- The `root.lock().await.root.clone()` temporary guard is held across the sweep's await (temporary-lifetime rule) — but this matches 7 established inline sites in run_all.rs, and nothing inside the sweep re-locks, so there is no deadlock; the fetch adds at most network latency to run-start under the same lock the landing sites already hold across multi-subprocess git operations.
- A branch named exactly `wt/runall-` (no suffix) matches both the glob and the guard and would be swept if merged — consistent with "the app-managed namespace ONLY"; the app never creates it (`item8` is always 8 chars).
- The landing-time item notes ("branch {} kept for the human merge", run_all.rs:4664/4848/5130) remain accurate as statements of the landing moment; the lifecycle docs cover the sweep.
- The plan file's step 2 names run_all.rs as the wiring file while `backlog_run_all` lives in backlog_cmds.rs — the implementation targeted the right file; plan-text attribution only.
- eprintln vanishes in release GUI-subsystem builds (run_all.rs:3614 awareness) — the sweep's observable effect is the deletion itself; the log is best-effort, matching the `backlog: ...` convention.

## Constitution & project checks

- **Never commit to main**: no commits in the diff; changes sit uncommitted on `wt/macos-fix`. ✓
- **Tests before complete**: `cargo test --workspace` reported green + warning-free (deny(warnings) makes green prove zero warnings); this reviewer is read-only and re-derived the module test and both source pins by reading the code. ✓
- **Doc comments on public functions**: `sweep_merged_runall_branches` (pub) + its impl documented per project style. ✓
- **Regression test for the defect**: `sweep_deletes_only_runall_branches_merged_into_origin_main` covers all four acceptance cases and fails without the delete. ✓
- **Multi-platform neutrality**: no platform-specific code in the change (CREATE_NO_WINDOW stays cfg(windows)-gated inside `git_raw`); the test's tempdir/`to_str` pattern matches the existing sync tests. ✓
- **File-tools-first policy**: no shell-based file mutation in the diff; the test's `std::fs::write` fixtures are the module's established pattern. ✓
- **Documentation sync**: agent.md + PLAN.md + the three worktrees.rs doc sites consistent with the code (the one stale record is finding L4). ✓
- **Security**: argv arrays only; the namespace guard prevents flag injection; no force-push/admin/ruleset interaction anywhere. ✓

## Verdict rationale

The change does what the plan promised: the conservative ancestry gate with correct exit-code semantics, namespace isolation proven by test, a best-effort fetch with a safe direction, wiring that preserves both source pins, and docs synced across all five sites. All four findings are comment accuracy, pin coverage, a documented-residual boundary, and knowledge hygiene — none blocks landing the work. Fix or consciously accept each per the fix-every-finding rule.
