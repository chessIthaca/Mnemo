+++
title = "git tool per-subcommand parameter reference (branch list now accepts allowlisted read flags)"
created = "2026-08-26"
+++

HOW: git tool per-subcommand parameter reference (verified 2026-09-19 against src/tool/agent/git.rs schema + execute dispatch).

SCHEMA PROPERTIES (all optional at the JSON-schema level — no `required` array, by design: required fields are conditional on the subcommand + action, validated at runtime with helpful error messages):
- subcommand: enum of 14 values (9 canonical: status/diff/log/commit/merge/checkout/stash/branch/push + 5 action names: list/delete/create/pop/drop). May also be given in `action`. Branch/stash action names accepted here too.
- message: Required for commit; optional for merge (custom merge-commit message).
- branch: Required for merge, checkout, branch delete/create; optional for push (default: current branch's upstream).
- action: For stash (push/pop/drop/list, default push) or branch (list/delete/create, default list). A subcommand name here means its default action.
- remote: For push (default "origin").
- args: Extra flags/refs/paths for READ queries only (status/diff/log/branch list). Refused for write subcommands (incl. branch create/delete). status/diff/log use a denylist (--output/-o/--exec/--no-index/--ext-diff/--textconv rejected as unsafe); branch list uses an ALLOWLIST of read-only listing flags (validate_branch_list_args — positionals refused since they create branches; combined short flags must be split, e.g. ["-a","-v"] not "-av"; allowed: -v/-vv/--verbose/-a/--all/-r/--remotes/--list/-q/--quiet/--no-color/--color=/--format=/--sort=/--merged/--no-merged/--contains/--no-contains/-i/--ignore-case).

PER-SUBCOMMAND REQUIREMENTS:
| Subcommand | Required | Optional | Runs |
|---|---|---|---|
| status | none | args | git status --short |
| diff | none | args | git diff (output NEVER truncated) |
| log | none | args | git log --oneline -20 |
| commit | message | (none; args rejected) | auto-stages (git add -A) only when nothing staged, else commits only what's staged |
| merge | branch | message | git merge --no-ff <branch> [-m <msg>] — CORE OP (always approval-gated) |
| checkout | branch | (none; args rejected) | git checkout <branch> |
| stash | none | action (default push) | git stash push/pop/drop/list |
| branch | branch (delete/create only) | action (default list), args (list only) | list: git branch [read-only listing flags via allowlist]; delete: git branch -d <name>; create: git branch <name> (from HEAD, no switch) |
| push | none | remote (default origin), branch | git push <remote> [<branch>] — CORE OP (always approval-gated) |
| restore | paths | source, target | git restore [--staged] [--worktree] [--source=<src>] -- <paths> |

ASSESSMENT: Schema property descriptions are ACCURATE — every parameter correctly states which subcommands require it, and runtime validation matches. branch list now accepts read-only listing flags via an allowlist (plan 3f3a188c, 2026-09-19) — it's a read query, so it joins the args surface, but with an allowlist (not the status/diff/log denylist) because branch positionals create branches. README documents read-args forwarding (incl. branch list), forgiving fields, and diff-never-truncated.
