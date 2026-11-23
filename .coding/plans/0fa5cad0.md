# Plan: Rebuild refreshes the installed plugin copy's helper binary

## Goal
After `node scripts/rebuild-mnemo2.mjs`, the Claude Code plugin cache copy of the mnemo plugin runs the freshly built mnemo2-mcp helper, so its hooks (WorktreeCreate, notices, gate-agent) never hit an older helper that lacks a command.

## Kind
implementation

## Context
WHY: on 2026-11-23 a Claude Code agent worker (bandAgentModel01) missed its Mnemo pool and ran in a plain Claude worktree based on main: the installed plugin copy at ~/.claude/plugins/cache/mnemo/mnemo/2.5.0 had current hooks (scripts/install-mnemo2-mcp.mjs refreshInstalledPlugin copies hooks, skills, .claude-plugin, .mcp.json, README.md) but its bin/mnemo2-mcp.exe was the Nov 16 build, which does not know `worktree-create` and printed its usage text; the Mnemo2 workers band and the hooks looked half-loaded to the user. The header comment's claim that an older helper copy forwards its arguments to <config>/bin does not hold for that old build. The lead replaced that exe by hand; this plan makes it permanent.

DECISION: in refreshInstalledPlugin (scripts/install-mnemo2-mcp.mjs:106-133), for each installed copy (target), after the file refresh, call the existing `install(join(target, "bin"))` — it already sweeps `.old-` leftovers, copies the helper with the rename-aside rollover (a running session keeps its open exe) and writes mnemo2-path.txt. A failure there is logged but must not make the script exit non-zero (the cache copy is best effort, like the rest of the refresh). Update the header comment: the cached plugin copy's bin is refreshed on every install, so no reinstall is needed; drop or qualify the forwarding claim. Keep the `resolve(target) === resolve(source)` skip.

VERIFY: run `node scripts/install-mnemo2-mcp.mjs` once (target/release has a built helper) and confirm ~/.claude/plugins/cache/mnemo/mnemo/2.5.0/bin/mnemo2-mcp.exe has the same size as mnemo2/plugin/bin/mnemo2-mcp.exe and that running it with no args prints the current usage text. If a node test harness exists for scripts (check package.json scripts and scripts/*.test.mjs), add a test there; otherwise the manual verification, quoted in the commit message, is the proof. npm test --workspace mnemo2 and npm run check --workspace mnemo2 must stay green.

## Acceptance scenarios
- After `node scripts/install-mnemo2-mcp.mjs`, ~/.claude/plugins/cache/mnemo/mnemo/<version>/bin/mnemo2-mcp.exe matches mnemo2/plugin/bin/mnemo2-mcp.exe, even while a Claude session holds the old one open (it is renamed aside).
- A missing plugin cache or a locked file never makes the install exit non-zero.

## Steps
- [x] 1. **Install the helper into the cached plugin copy** — file: scripts/install-mnemo2-mcp.mjs only (plus a scripts test file only if a scripts test harness already exists). Implement the context DECISION exactly: in refreshInstalledPlugin call install(join(target, "bin")) for each installed copy, best effort (log, never exit non-zero for it), and fix the header comment. Verify as the context says and quote the verification output in the commit message. DONE: one commit; do not touch rebuild-mnemo2.mjs, the plugin hooks or any Rust/TS source.

## Decisions
- 2026-11-23: no spec impact: Developer install script only; no product behavior or governed capability changes.

## Progress
- 2026-11-23 step 1 done: step 1 merged: pluginBinRefresh01 (pluginBinRefresh01) on deepseek-v4.1-flash:1017, merge commit 7ccdb16c; review: The install script now refreshes the cached plugin copy's bin (helper + mnemo2-path.txt) with the rename-aside rollover, best effort; header comment corrected; three sandboxed tests.
