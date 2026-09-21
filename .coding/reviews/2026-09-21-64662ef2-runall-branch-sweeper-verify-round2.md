## Verdict: PASS

Round-2 verification of the four LOW remediations from `.coding/reviews/2026-09-21-64662ef2-runall-branch-sweeper-review.md` (plan a93fa25b / backlog 64662ef2, branch `wt/macos-fix`). All four landed exactly as described and are correct; the diff since round 1 contains nothing beyond the described remediations plus the round-1 report file itself; round 1's verified core mechanics are intact.

## Scope & method

Read the round-1 report first, then verified against the working tree: the sweeper impl + doc comments + test in `src/project/worktrees.rs`, the wiring in `src-tauri/src/ipc/backlog_cmds.rs`, the extended pin test in `src-tauri/src/ipc/run_all.rs`, the amended SPEC record, and the FULL uncommitted diff (`git diff HEAD` + status). Re-derived the parse by hand for all three listing shapes, re-read `git_raw`'s exit-code contract (git_ops.rs:33-55), re-read `provision_item_worktree_impl` (worktrees.rs:91-109) and `init_repo` (628-637) to prove the test's worktree-held branch is kept by git's refusal (not by the namespace guard), and confirmed every pin-test marker's first occurrence in backlog_cmds.rs by exhaustive search (each string occurs exactly once in that file).

## Remediation verification

### L1 — the `+ ` strip, the prune, and the comments: CORRECT

- **The parse** (worktrees.rs:226-230): `line.strip_prefix("* ").or_else(|| line.strip_prefix("+ ")).unwrap_or(line).trim()`. Hand-derived for all three shapes: `+ wt/runall-cccccccc` → the `* ` strip misses, the `+ ` strip yields `wt/runall-cccccccc`, trim is a no-op — the branch name. A two-space line `  wt/runall-aaaaaaaa` → both strips miss, `unwrap_or(line)` keeps it, `trim()` eats the padding. A `* ` line → first strip hits. The markers are unambiguous: git refnames cannot contain spaces, so `+ `/`* ` can only be listing markers; a branch literally named `+foo` lists as `  +foo` (two-space prefix), misses both strips, trims to `+foo`, and fails the namespace guard — correctly skipped. A detached-HEAD line (`* (HEAD detached at …)`) also fails the guard. No shape regressed.
- **The prune** (211-214): `let _ = git_raw(main_root, &["worktree", "prune"])` runs BEFORE the `branch --list` at 215, best-effort. Correct and safe: prune only removes registrations whose worktree directories are gone, so live runall worktrees are untouched (the test's `worktree.exists()` assert passes alongside it), while an externally-deleted worktree's stale registration no longer pins its branch undeletable — the R3-L2 lesson applied. The cost is one refused `git branch -D` per worktree-held branch per run-start (the strip now lets those reach git), which is exactly what makes git's refusal the operative guard rather than a parse accident.
- **The comments**: the inline comment (220-225) names all three prefixes and both guards, tagged review L1; the fn doc (185-193) says "IS considered (its `+ ` listing prefix is stripped) but refused by git itself … needs no extra code beyond the parse" and documents the prune; the test comment (834-836) is now mechanically accurate — see L1/test below.

### L2 — the extended pin test: COMPILES AND GENUINELY PINS

In `run_all_start_adopts_orphaned_in_flight_items` (run_all.rs:2961; extension at 3014-3042). The critical precondition holds: **"sweep_merged_runall_branches" occurs exactly once in backlog_cmds.rs — line 555, the call site** (`let swept = mnemo::project::worktrees::sweep_merged_runall_branches(`); the comment above it (549-554) says "sweep wt/runall-* branches" and does not contain the fn name, so `find` cannot hit a comment. The other three markers likewise occur exactly once each at the intended sites: "run-all is already active" → 539 (the error string), "adopt_orphaned_in_flight" → 548 (the call), "no pending backlog items to run" → 578 (the error string). The assert `active_check < adopt_call && adopt_call < sweep_call && sweep_call < pending_count` therefore reads 539 < 548 < 555 < 578 — true, and it genuinely pins the placement: moving the sweep after the early-return (or into the post-count region) flips `sweep_call < pending_count` and fails. `str::find` → `Option<usize>` → `.expect` → usize comparisons — compiles (and the reported green run_all suite includes it).

### L3 — the squash residual: DOCUMENTED

worktrees.rs:194-198 carries the exact "Known residual (review L3, 2026-09-21)" sentence: squash breaks ancestry (content-identical, SHA-different), the branch lingers under-deleting only, with the `gh pr view --json state` == MERGED fallback named should it ever be needed.

### L4 — the SPEC amendment: LANDED

`.coding/knowledge/spec/2027-01-11-ruleset-aware-run-all-landing-two-paths-pr-on-pr.md` gains the dated amendment paragraph (+2 lines in the diff): records the sweeper, its run-start wiring ("after the adoption sweep and before the pending-count early-return — pinned by the source-pin test"), the fetch + prune + `+ `-strip mechanics, the restated invariant ("no path deletes a PR-head branch BEFORE its commits are in `origin/main` — the sweeper deletes exactly the ones after"), and the squash residual. The original body is preserved as history per the amend protocol.

## The sweeper test with the `+ `-strip: STILL EXACT

Derived end-to-end: `provision_item_worktree_impl` runs `git worktree add -b wt/runall-<item8> <dir> main` — the branch is forked from main's tip with NO commit added, and is checked out in the linked worktree. In the test, main's tip is the pushed merge commit, so `wt/runall-cccccccc`'s tip IS an ancestor of `origin/main`: the strip makes it CONSIDERED, `merged` is true, `git branch -D` is attempted, and git refuses ("cannot delete branch used by worktree") — kept by git's refusal, exactly as the comment now claims. `wt/runall-aaaaaaaa` (merged, two-space line) is deleted; `wt/runall-bbbbbbbb` fails the ancestry gate; `wt/other` never matches the glob. The prune is a no-op (the worktree exists). `assert_eq!(swept, vec!["wt/runall-aaaaaaaa"])` holds — the reported 16/16 worktrees run corroborates.

## Diff hygiene — nothing else crept in

`git status`: 7 modified + 2 untracked. Round 1's set was 5 modified (worktrees.rs, backlog_cmds.rs, agent.md, PLAN.md, backlog.jsonl) + the untracked plan file. The deltas since round 1 are exactly: `src-tauri/src/ipc/run_all.rs` (the L2 pin-test extension — the only hunks are at 3011-3042, inside the tests module), the knowledge SPEC record (the L4 amendment), and the round-1 report file itself (untracked — the review artifact, not a code change). Within worktrees.rs, the content beyond round 1's reviewed state is exactly the or_else strip line, the prune block + comment, the inline-comment rewrite, the doc additions (`+ ` sentence, prune sentence, L3 residual), and the test-comment wording; the wiring hunk in backlog_cmds.rs, agent.md, PLAN.md, and backlog.jsonl are byte-identical to what round 1 reviewed. No stray edits anywhere.

## Round 1's core mechanics — re-verified, intact

`git_raw` returns `Ok` only on `output.status.success()` (git_ops.rs:47-54), so `.is_ok()` on `merge-base --is-ancestor` = exit 0 = ancestor = merged; exit 1/128 → kept. The `-D` still follows the explicit ancestor check; namespace isolation still holds (glob + `starts_with("wt/runall-")`); the fetch is still best-effort with the under-delete direction; argv-only subprocesses with git-sourced, prefix-guarded names. The heartbeat pin (run_all.rs:1914-1938) is undisturbed — its markers (`RunAllState {`, `run_all_heartbeat`, `run_all_dispatch_next`) all sit after the wiring insertion and none appear in the inserted text; the adopt pin is now the extended adopt+sweep pin, holding.

## Non-finding observations

- The amendment is dated "Amended 2027-01-11" (memory_amend dates from the system clock) while the sweeper landed 2026-09-21 — the body carries the accurate landing date, and this matches the repo's existing convention (the b52b041a SPEC itself is created 2027-01-11 with 2026-09-21 reviews). No action.
- The doc phrase "pin its branch in the listing forever" is slightly loose (the branch stays listed either way; it is pinned *undeletable*) — the impl comment states it precisely. Cosmetic only.

## Verdict rationale

All four round-1 findings are remediated exactly as described, each verified against the code rather than the description: the parse handles all three listing shapes with the `+ ` case now reaching git's own refusal (proven through `provision_item_worktree`'s fork-from-main-no-commit shape), the prune sits before the listing and is safe for live worktrees, the pin test's marker provably hits the call site and pins the placement, the squash residual and the SPEC amendment are in place, and the diff is clean of anything else. The change is ready to land.
