# mnemo-workers (Claude Desktop mod)

A band above the prompt in Claude Desktop's Code tab that lists the running Mnemo2
workers of the session's project with their short model name (`kimi`, `glm`,
`deepseek-f`; `-f` marks a flash model), plus a status entry `Mnemo ▶N ◆M`
(running / other shown). Workers blocked on your approval or answer stay listed;
finished ones (completed, in review, failed) show for two minutes after their last
activity, then leave the band. Clicking a worker's name opens that worker in Mnemo2's
mission control.

It reads `mnemo2-mcp team-summary --json` every 4 s (and after each Mnemo tool call)
and runs `mnemo2-mcp show-worker <id>` on a click. Both are read-only; the helper is
expected at `%USERPROFILE%\.mnemo\bin\mnemo2-mcp.exe`. Outside a registered Mnemo2
project the band stays hidden.

## Install

Mnemo2's Settings → Claude Code → Connect installs this mod together with the
`mnemo` plugin. From a source checkout whose `mnemo` marketplace points at
this repository, run `claude plugin install mnemo-workers@mnemo` once. The
band only appears in a new or restarted Claude Code session. For development,
load the folder as a plugin directory: add it to `CLAUDE_CODE_PLUGIN_DIRS`, or
start Claude Code with `--plugin-dir <path>/mnemo2/claude-desktop-mod/mnemo-workers`.

## Check

```
claude plugin validate mnemo2/claude-desktop-mod/mnemo-workers
claude plugin test mnemo2/claude-desktop-mod/mnemo-workers
```
