+++
title = "git tool commit auto-stages everything — use shell for selective commits"
created = "2026-08-26"
status = "superseded"
+++

HOW: the `git` tool's commit subcommand runs `git add -A` unconditionally before `git commit` (src/tool/agent/git.rs commit arm ~line 520). This silently overrides selective staging — if you `git add <specific files>` via shell and then call the `git` tool to commit, it re-stages EVERYTHING (all untracked + modified files), sweeping in stray files from other branches. LESSON: when staging matters (excluding stray .coding/ files), use `shell` with explicit `git add <files> && git commit -m "..."` — NEVER the `git` tool. PROPOSED FIX: only auto-`add -A` when nothing is already staged (`git diff --cached --quiet` succeeds); if something is staged, commit only what's staged. Backward-compatible (common case — nothing staged — still auto-stages).
