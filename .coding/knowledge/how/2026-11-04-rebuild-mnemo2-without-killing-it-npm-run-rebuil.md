+++
title = "Rebuild Mnemo2 without killing it: npm run rebuild:mnemo2"
created = "2026-11-04"
+++

`npm run rebuild:mnemo2` (scripts/rebuild-mnemo2.mjs) writes `<config>/run/rebuilding` (15-minute TTL), runs the installed `mnemo2-mcp quit [--force]` (POST /app/quit to every run file's instance, then waits for the pids to exit), builds with build:mnemo2:native and removes the marker. If an instance refuses (workers running: rerun with `-- --force`), does not answer, is an older build without /app/quit, or does not exit within 60 s, it exits 1 before building and kills nothing. While the marker is fresh, the helper's start_instance does not spawn Mnemo2. Caveat: Claude Code's long-lived `mnemo2-mcp serve` keeps the helper code it started with, so spawn-gate changes apply only after Claude Code restarts the plugin server.

Amended 2026-11-08: Run it from the checkout the live mnemo2.exe runs from (`npm --prefix <that checkout> run rebuild:mnemo2` works from another folder), because the install records that target folder in mnemo2-path.txt; building in a temporary worktree would point the helper at a folder that later goes away. A tool schema change reaches a fresh `tools/list` through the helper as soon as the new build starts, but a Claude Code session that is already open keeps the schemas it loaded, so it sees the change only after it restarts (seen on 2026-11-08: the helper listed a new team_dispatch parameter while the open session's own team_dispatch schema still lacked it).
