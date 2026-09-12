+++
title = "git tool structured `restore` subcommand — MERGED into main (731bc802)"
supersedes = "2026-08-27-git-tool-structured-restore-subcommand-paths-sou"
created = "2026-08-27"
+++

SPEC: The git tool has a structured `restore` subcommand (plan 514dcee1) — MERGED into main at 731bc802 (2026-09-08, merge_to_main skill), branch wt/agenticcoder deleted, not pushed. Use it instead of the shell for `git checkout -- <path>`-style restores. Fields (src/tool/agent/git.rs, GitArgs): `paths` (REQUIRED array of pathspecs; a singular string `path` is forgivingly lifted when `paths` is absent/empty), `source` (optional tree-ish — branch/tag/commit/HEAD~1 — validated like a branch name, embedded as one `--source=<src>` argv element), `target` ("worktree" default | "staged" = unstage | "both" = discard staged+worktree). argv: restore [--staged] [--worktree] [--source=<src>] -- paths… — the mandatory `--` separator plus no-empty/no-'-'-leading pathspec validation makes flag injection impossible; free-form `args` stay REJECTED for restore (read-only-args decision stands). git defaults apply: worktree restores from the index, staged/both from HEAD. Tool-card label: `restore [target] [source] -- paths`. Side effect: tools-array budget ceilings raised deliberately (factory.rs: Executing 23_500, ExecutingResearch 20_100, dated justification comment).
