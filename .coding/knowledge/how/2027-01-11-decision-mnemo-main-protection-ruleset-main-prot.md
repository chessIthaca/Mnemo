+++
title = "DECISION: Mnemo main protection — ruleset main-protection id 24182945 (0 required approvals, PR path); release tags immutable"
supersedes = "2027-01-11-chessithaca-mnemo-release-tags-are-immutable-gh0"
created = "2027-01-11"
+++

Live-verified against the GitHub API at the 1.3.0 close-out: chessIthaca/Mnemo has EXACTLY ONE ruleset — `main-protection`, id 24182945 (branch, refs/heads/main, enforcement active, created 2026-09-29T09:33-04:00) with rules deletion + non_fast_forward + pull_request: required_approving_review_count 0, dismiss_stale_reviews_on_push true, merge/squash/rebase allowed, no bypass actors (current_user_can_bypass = never). So: direct pushes to main are rejected, work must land via a PR, but the author may self-merge (0 approvals). CORRECTS the earlier belief that the rulesets were emptied on 2026-09-28 — they were, for the attribution-rewrite force-push, and the protection was re-created the next day; the previous record's "rulesets now empty" is obsolete. STILL TRUE: release tags are immutable on GitHub (GH013 blocks moving refs/tags/v1.2.0; releases show immutable: true). STALE REFERENCES flagged, not edited: agent.md and .coding/skills/new_release.toml still cite ruleset id 23755694 with required_approving_review_count 1 — the live id is 24182945 with 0 — reconcile before the next release/landing run.
