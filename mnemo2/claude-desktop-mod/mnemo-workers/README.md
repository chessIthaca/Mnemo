# mnemo-workers (Claude Desktop mod)

A band above the prompt in Claude Desktop's Code tab that lists the running Mnemo2
workers of the session's project with their short model name (`kimi`, `glm`,
`deepseek-f`; `-f` marks a flash model), plus a status entry `Mnemo ▶N ◆M`
(running / waiting on you). Clicking a worker's name opens that worker in Mnemo2's
mission control.

It reads `mnemo2-mcp team-summary --json` every 4 s (and after each Mnemo tool call)
and runs `mnemo2-mcp show-worker <id>` on a click. Both are read-only; the helper is
expected at `%USERPROFILE%\.mnemo\bin\mnemo2-mcp.exe`. Outside a registered Mnemo2
project the band stays hidden.

## Install

Load the folder as a plugin directory, for example by adding it to
`CLAUDE_CODE_PLUGIN_DIRS`, or start Claude Code with
`--plugin-dir <path>/mnemo2/claude-desktop-mod/mnemo-workers`.

## Check

```
claude plugin validate mnemo2/claude-desktop-mod/mnemo-workers
claude plugin test mnemo2/claude-desktop-mod/mnemo-workers
```
