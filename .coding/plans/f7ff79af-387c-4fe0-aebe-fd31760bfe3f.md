# Plan: Show git subcommand in the agent ToolCard header

## Goal
Make the git ToolCard header show the subcommand being called (e.g. "git (status)") instead of a bare "git".

## Context
The ToolCard header in Message.tsx renders displayName(name) + a parenthetical from argLabel(c.args, name). argLabel already handles shell (purpose), spawn_agent (name), skill_start (skill), and path-bearing tools. The git tool has no case, so its card shows a bare "git" with no subcommand. The git tool's args always include a "subcommand" field (status/diff/log/commit/merge/checkout/stash/branch/push). Adding a git case to argLabel that returns the subcommand makes the card read "git (status)" etc., consistent with the existing pattern.

## Steps
- [x] 1. Add a `git` case to argLabel in Message.tsx that parses the args and returns the `subcommand` field (trimmed), so the ToolCard header reads "git (status)" / "git (commit)" etc.
- [x] 2. Run vitest + tsc to confirm no regressions; commit on the feature branch.
