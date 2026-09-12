+++
title = "branch topology collapsed to wt/* → main (develop tier removed)"
supersedes = "2026-08-23-merge-to-main-two-hop-ceremony-reconcile-kick"
created = "2026-08-24"
+++

DECISION (user, 2026-08-24): the `develop` integration tier is REMOVED. The topology is now **wt/* → main**: each plan works on its own `wt/*` branch, and `merge_to_main` merges it straight into main. Main stays protected — it only ever receives merge commits, never a direct commit — and each plan still gets its own working branch. Only the middle tier is gone.

Rationale: develop bought conflict isolation this single-maintainer repo was not using, at the cost of a second long-lived branch that silently drifted. On 2026-08-24 it sat 3 commits behind main (two fixes had landed on main directly), so anyone branching from it would have missed them.

Implementation — two functional changes; everything else was prose, comments and tests:

- `src/tool/workflow/plan.rs`: create_plan's default `base` is `"main"` (was `"develop"`).
- `src/project/git_ops.rs`: `prepare_branch` drops the develop auto-create special case. That arm forked develop from main on demand; with wt/* forking straight from main the base always exists, so a missing base is once again a HARD ERROR — silently forking from the wrong ref is worse than refusing. `develop` now gets no special treatment.
- `.coding/skills/merge_to_main.toml`: steps 3 and 4 collapse into a single `git checkout main` + `git merge --no-ff <branch>`; the "KEEP develop" instruction is gone.

`APP RULES` needed NO edit: it says "commit to the current working branch", which was never about develop.

Regression test: `prepare_branch_errors_on_a_missing_base` (src/project/git_ops.rs) — the former `prepare_branch_creates_develop_when_missing` repurposed into its opposite, so reintroducing an auto-create path fails the suite. Plus `prepare_branch_forks_wt_from_main`.

`base` is a call-time argument only — `PlanFile` has no `base` field and `stack.json` (gitignored) stores none — so no existing plan pins `develop` and resuming one cannot re-run branch prep against the removed branch.

MERGED into main at e7d1fed on 2026-08-24 via wt/collapse-develop-topology (the first branch to use the new topology). Local `develop` deleted.

Records that state "MERGED ... via develop" for work landed BEFORE this date remain accurate history and are deliberately left alone.
