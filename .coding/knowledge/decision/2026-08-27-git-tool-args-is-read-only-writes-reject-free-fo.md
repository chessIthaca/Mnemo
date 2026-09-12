+++
title = "git tool args is read-only — writes reject free-form flags loudly"
created = "2026-08-27"
status = "superseded"
+++

The `git` tool's `args` parameter (src/tool/agent/git.rs, GitArgs) is READ-ONLY by design: only status/diff/log forward validated extra flags (denylist in validate_read_args: --output/-o/--exec/--no-index/--ext-diff/--textconv); every write subcommand (commit/merge/checkout/stash/branch/push/restore) errors loudly on non-empty args (the "args are not supported for git {sub}" error). Rationale (plan .coding/plans/47472e35-16b7-4224-8d16-70fcc854ea27.md): (1) read queries are where precision is needed and argv is discrete (no shell), so a small denylist makes them safe; (2) write subcommands have structured params (branch/message/remote/action; restore: paths/source/target) instead — free-form flags there could create branches, overwrite files from any ref (including protected .coding/ state), or go interactive; (3) the loud error replaced the original defect where serde silently dropped unknown fields. WORKAROUND for precise restores: use the tool's own structured `restore` subcommand (paths/source/target; plan 514dcee1, commits e039f56/07fcb9e) — see its SPEC record; the shell tool remains the fallback for exotic git forms restore does not model (e.g. `restore -p`).
