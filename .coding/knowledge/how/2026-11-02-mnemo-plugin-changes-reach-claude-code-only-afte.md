+++
title = "Mnemo plugin changes reach Claude Code only after a plugin update and a new session"
created = "2026-11-02"
+++

Claude Code runs the Mnemo plugin (marketplace "mnemo", directory source C:\Mnemo) from a frozen copy at ~/.claude/plugins/cache/mnemo/mnemo/<version>/, including its own bin/mnemo2-mcp.exe, hooks/hooks.json and skills. `npm run build:mnemo2:native` (scripts/install-mnemo2-mcp.mjs) refreshes mnemo2/plugin/bin and ~/.mnemo/bin but not that cache, so new MCP tools (e.g. `search`), hooks (e.g. the Grep|Glob `hint --pre-search`) and skill text stay invisible to Claude Code until the plugin is updated (claude plugin update mnemo@mnemo; bump .claude-plugin/plugin.json version if the update sees no change) and a new Claude Code session starts. Check with: grep pre-search ~/.claude/plugins/cache/mnemo/mnemo/*/hooks/hooks.json. Pipe-test hooks from PowerShell (`'{"tool_name":"Grep","tool_input":{"pattern":"X"},"cwd":"C:\\Mnemo"}' | & mnemo2/plugin/bin/mnemo2-mcp.exe hint --pre-search`); Git Bash quoting of the JSON easily yields silent no-output.
