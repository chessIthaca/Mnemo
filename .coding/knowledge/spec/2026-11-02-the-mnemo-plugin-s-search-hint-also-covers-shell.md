+++
title = "The Mnemo plugin's search hint also covers shell grep, rg, git grep, findstr and Select-String"
created = "2026-11-02"
+++

The PreToolUse `mnemo2-mcp hint --pre-search` hook (src-tauri/src/bin/mnemo2_mcp/hooks.rs; plugin hooks.json matcher "Grep|Glob|Bash|PowerShell", plugin 0.1.2) also parses Bash/PowerShell commands: it splits on | ; && and newlines with quote handling, ignores any search fed by a pipe (output filters like `cargo test | grep FAILED`), and for grep/egrep/rg/git grep/findstr/Select-String that search files (grep needs a path or -r) feeds the pattern and path into the same classifier as Grep/Glob: a bare identifier or `fn X` gets the graph_search/search hint, a typed title or .coding/knowledge path gets the memory_search hint, regexes and prose stay silent. Never blocks; about 20 ms per call. Effective in Claude Code after the plugin update and a new session.
