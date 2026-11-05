+++
title = "Mnemo search hint covers shell grep/rg/git grep/findstr/Select-String"
created = "2026-11-02"
+++

The `hint --pre-search` PreToolUse hook in src-tauri/src/bin/mnemo2_mcp/hooks.rs reads Bash and PowerShell calls as well as Grep/Glob: `search_hint` parses `tool_input.command` via `shell_search`/`shell_commands` (quotes and escapes resolved; a command fed by a single `|` is an output filter and is skipped), dispatches `grep|egrep|fgrep`, `rg`, `git grep`, `findstr`, `Select-String|sls`, and passes the pattern plus first path to `classify` (`-e`/`-f`/rg's `-r` and unparseable commands stay silent). A shell hint ends "The shell command still runs."; a Grep/Glob hint keeps "Grep still runs." The plugin hook matcher is `Grep|Glob|Bash|PowerShell` and the plugin version is 0.1.2 (a version bump reaches Claude Code only after `claude plugin update` plus a new session).
