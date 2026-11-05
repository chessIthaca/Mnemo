+++
title = "Rebuild Mnemo2 without killing it: npm run rebuild:mnemo2"
created = "2026-11-04"
+++

`npm run rebuild:mnemo2` (scripts/rebuild-mnemo2.mjs) writes `<config>/run/rebuilding` (15-minute TTL), runs the installed `mnemo2-mcp quit [--force]` (POST /app/quit to every run file's instance, then waits for the pids to exit), builds with build:mnemo2:native and removes the marker. If an instance refuses (workers running: rerun with `-- --force`), does not answer, is an older build without /app/quit, or does not exit within 60 s, it exits 1 before building and kills nothing. While the marker is fresh, the helper's start_instance does not spawn Mnemo2. Caveat: Claude Code's long-lived `mnemo2-mcp serve` keeps the helper code it started with, so spawn-gate changes apply only after Claude Code restarts the plugin server.
