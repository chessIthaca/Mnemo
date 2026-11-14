+++
title = "Mnemo main protection — two rulesets: main-protection (PR, 0 approvals) and ci-check-required (the `check` status)"
supersedes = "2027-01-11-decision-mnemo-main-protection-ruleset-main-prot"
created = "2026-11-14"
evidence = "gh api repos/chessIthaca/Mnemo/rulesets and rulesets/24182945, rulesets/27026992, run 2026-11-14; ci.yml job steps."
+++

chessIthaca/Mnemo protects `main` with two active rulesets:

- `main-protection`, id 24182945: deletion and non-fast-forward blocked; `pull_request` with required_approving_review_count 0 (merge methods merge/squash/rebase allowed; use merge for wt/mnemo so its history stays joined with main). No bypass.
- `ci-check-required`, id 27026992 (added 2026-11-11): requires the GitHub Actions status `check` (the job in .github/workflows/ci.yml). Only the admin role can bypass, and only when merging a PR.

Landing path: push the lead branch, open a PR to main, enable auto-merge (merge method "merge"); it merges once `check` passes. No human approval is required. The older ruleset id 23755694 (1 required approval) no longer exists, so any doc that cites it (agent.md Environment and Branch policy) is stale.

Amended 2026-11-14: Carried over from the superseded 2027-01-11 ruleset record: release tags are immutable on GitHub. GH013 blocks moving refs/tags/v1.2.0 and releases show `immutable: true`, so a bad tag gets a new version, never a retag. agent.md (Branch policy) now carries the two current rulesets and the auto-merge landing path.
