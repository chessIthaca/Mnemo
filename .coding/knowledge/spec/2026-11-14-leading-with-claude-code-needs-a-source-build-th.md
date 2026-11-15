+++
title = "leading with Claude Code needs a source build — the release installers do not ship the mnemo2-mcp helper"
created = "2026-11-14"
evidence = "tauri.conf.json bundle.resources lists only the Laya file; build.yml has no mnemo2-mcp step; mnemo2/plugin/README.md: \"It is built from this repository and is not checked in.\""
status = "superseded"
+++

The `mnemo` Claude Code plugin runs `bin/mnemo2-mcp` (mnemo2/plugin/.mcp.json), and `bin/` is not checked in: only `npm run build:mnemo2:native` (or `rebuild:mnemo2`) builds the helper and scripts/install-mnemo2-mcp.mjs copies it into mnemo2/plugin/bin/ and ~/.mnemo/bin/. The installers do not include it: src-tauri/mnemo2-app/tauri.conf.json bundles only `laya/dispatch_routing.jsonl` as a resource (no externalBin), and .github/workflows/build.yml never builds or copies mnemo2-mcp. The plugin installs from a local clone (`claude plugin marketplace add <path>`). So a user who installs a release cannot lead with Claude Code; README.md's Quickstart ("Lead with Claude Code") says so. Closing the gap means bundling the helper (e.g. as a Tauri externalBin), installing it to ~/.mnemo/bin at app start, and letting the plugin fall back to that copy when its own bin/ is empty; the README section must then change.
