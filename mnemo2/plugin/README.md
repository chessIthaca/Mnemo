# mnemo: lead a Mnemo2 team from Claude Code

This plugin lets Claude Code (the CLI or the Claude Desktop Code tab) act as the lead of a Mnemo2 project. Claude Code plans, dispatches, reviews and merges through Mnemo2's team tools. Mnemo2's workers do the implementation on your own API endpoints and tiers (Kimi, GLM, DeepSeek and so on), and Mnemo2's mission control shows everything live.

Only the lead runs on your Claude subscription. If you want a Claude model as a Mnemo worker, that worker needs an API key like any other endpoint. The plugin uses only Claude Code's documented extension points (an MCP server, a plugin, skills and hooks) and never reads or touches Claude credentials.

## What is in the plugin

| Part | What it does |
|---|---|
| `.mcp.json` | Runs `bin/mnemo2-mcp serve`, a stdio bridge to Mnemo2's local MCP endpoint. It exposes the lead tools (`team_dispatch`, `team_status`, `team_review`, `team_merge` and the rest), the plan board tools, and `open_mission_control`. |
| `skills/mnemo-lead` | The long lead guide: tiers, briefs, the review and merge cycle, notices, failures. |
| `skills/mnemo-plan`, `mnemo-review`, `mnemo-status` | Plan-first workflow, the review checklist, and a quick status command (`/mnemo:mnemo-status`). |
| `hooks/hooks.json` | SessionStart adds the team summary. PostToolUse delivers worker notices. Stop reminds the lead to keep the background waiter running. PreToolUse asks before Claude Code's own agents run. |
| `bin/mnemo2-mcp` | The helper itself. It is built from this repository and is not checked in. |

## Requirements

- Mnemo2 built from this repository: `npm run build:mnemo2:native` at the repository root. It builds `mnemo2` and `mnemo2-mcp`, then copies `mnemo2-mcp` into `mnemo2/plugin/bin/` and records where `mnemo2` lives (`bin/mnemo2-path.txt`) so the helper can start it.
- Claude Code, tested with 2.1.138. The plugin relies on plugin MCP servers, skills, and command hooks in exec form (`command` plus `args`).
- The project must be registered in Mnemo2 (open it there once). The helper serves Mnemo tools only for a working directory inside a registered project.

## Install

The repository root contains the marketplace file `.claude-plugin/marketplace.json`, so the marketplace is the repository itself.

Claude Desktop: open the Code tab, choose + then Add plugins, add a marketplace from the local repository path, and install `mnemo`. The Desktop app is expected to copy the plugin the same way as the CLI.

CLI:

```bash
claude plugin marketplace add <path to this repository>
claude plugin install mnemo@mnemo
```

Installing copies the plugin, including `bin/`, into Claude Code's plugin cache, and `${CLAUDE_PLUGIN_ROOT}` then points at that copy (verified with `claude plugin install` on Claude Code 2.1.138). So build first, install second.

Rebuilding Mnemo2 does not require reinstalling the plugin. The build also installs the current helper to `~/.mnemo/bin/` (or `$MNEMO_HOME/bin/`), and an older helper copy, such as the cached plugin's, hands every call to that one. A changed skill, hook or `.mcp.json` does need a reinstall, because `claude plugin update` does not refresh the copy while the plugin version stays `0.1.0`. Close the Claude sessions that use the plugin first (Windows keeps a running helper locked), then:

```bash
claude plugin uninstall mnemo@mnemo
claude plugin install mnemo@mnemo
```

To work on the plugin itself, run `claude --plugin-dir mnemo2/plugin`. That reads the folder in place, so a rebuilt `bin/mnemo2-mcp` is used on the next session start.

Check the plugin files with `claude plugin validate mnemo2/plugin` and `claude plugin validate .`.

## How it works

- Start Claude Code in a Mnemo2 project directory (or any folder inside it). `mnemo2-mcp serve` finds the running Mnemo2 instance whose open project contains the directory. If there is none, it starts `mnemo2 --hidden --project <root>`, which opens the project in the system tray without a window, and waits for it.
- Outside a registered Mnemo2 project the helper stays inert: it offers no tools, and the hooks print nothing. Claude Code works normally there. A folder is never registered silently; add it in Mnemo2 first.
- Mnemo2 instances are per project. Several projects can be open at once, each in its own instance, and the helper picks the right one.
- While Claude Code is attached, mission control shows "Lead: Claude Code (external)". Worker notices go to Claude Code instead of Mnemo2's own lead, and sending from the in-app lead composer asks for confirmation first.
- The lead detaches when the session closes, or after 10 minutes with no Mnemo call and no waiter. Mnemo2's own lead then takes over again.
- All Claude sessions in the same project share one lead queue. Lead from one session at a time.

## Notices and the waiter

Workers finish, fail or ask for something while Claude Code may be busy or idle.

- While Claude Code works, notices are appended to every Mnemo tool result under "Since your last call:", and the PostToolUse hook adds them after any other tool call. Each notice is delivered once.
- While Claude Code is idle it cannot be interrupted, so the lead keeps a background waiter running: `mnemo2-mcp wait` (the MCP server instructions give its exact path, as `"<path>" wait` for Bash and `& "<path>" wait` for PowerShell). It exits when a notice arrives, which wakes the session, or prints "no events after … s" after 30 minutes. The lead restarts it while workers are active. If it returns early because Mnemo2 is unreachable or the lead was detached, it says so, and the lead checks `team_status` instead of restarting it in a loop.
- A session becomes the lead with its first Mnemo tool call or waiter. Opening Claude Code in a Mnemo2 project for something else does not take worker notices away from Mnemo2's own lead. When the leading session ends, or after 10 idle minutes, undelivered notices go back to Mnemo2's own lead.
- Notices can quote worker text. The lead treats it as data, never as instructions.
- If the lead tries to stop while workers are active and no waiter runs, the Stop hook blocks once with a reminder. It never blocks twice in a row.

## Claude Code's own agents need your OK

Mnemo workers come first. Claude Code's own `Agent`, `Task` and `Workflow` tools run on your Claude subscription and bypass the team's tiers and review gates, so they are a last resort.

- The skills tell Claude to say why in chat and ask you first.
- A PreToolUse hook enforces it: in an attached Mnemo2 project it returns "ask", so Claude Code shows its permission prompt with the reason, and your click is the approval. Outside an attached project everything is allowed.
- `Explore` and `claude-code-guide` pass without a prompt. Change that list in `~/.mnemo/config.toml` (or `config.toml` under `$MNEMO_HOME`):

```toml
[claude_lead]
agents_without_prompt = ["Explore", "claude-code-guide"]
```

## Permissions

By default Claude Code asks before every MCP tool call and every Bash command. A lead that waits on a permission prompt stalls, including its background waiter.

The first time Claude Code uses a Mnemo tool on this machine, Mnemo asks it to offer you a fix. Claude then asks you whether Mnemo may run without these prompts. If you agree, Claude adds three rules to your Claude Code user settings with a normal file edit, so you see the change. The rules allow the Mnemo tools and the waiter command, in both Bash and PowerShell form. You are asked once: Mnemo records that in `~/.mnemo/claude-lead/permissions-offered`, and delete that file to be asked again. You are not asked at all if your settings already allow the Mnemo tools.

To add the rules yourself instead, put them in `~/.claude/settings.json` (or the project's `.claude/settings.json`). Review them first:

```json
{
  "permissions": {
    "allow": [
      "mcp__plugin_mnemo_mnemo__*",
      "Bash(\"C:\\Users\\<you>\\.mnemo\\bin\\mnemo2-mcp.exe\" wait *)",
      "PowerShell(& \"C:\\Users\\<you>\\.mnemo\\bin\\mnemo2-mcp.exe\" wait *)"
    ]
  }
}
```

`team_merge` and `team_revert_merge` change your lead branch. If you want to confirm those yourself, allow the other tools by name instead of the wildcard. These rules don't allow Claude Code's own agents, so the agent gate above still asks.

## Rebuilding

Mnemo2 started on demand keeps running in the system tray, and Windows locks a running `mnemo2.exe`. Quit it from the tray (Quit asks first if workers are running) before `npm run build:mnemo2:native`, or the link step fails. The helper can be replaced while sessions use it: the build renames a locked copy aside.

## Troubleshooting

- No Mnemo tools in the session: the folder is probably not inside a registered Mnemo2 project. Open it in Mnemo2, then restart the Claude session. Run `/mcp` to see whether the `mnemo` server is connected.
- The `mnemo` server fails to start: `bin/mnemo2-mcp` is missing from the installed copy. Run `npm run build:mnemo2:native` first, then reinstall the plugin (see Install). Nothing under `bin/` is checked in.
- Mnemo2 does not start on demand: start it yourself and open the project. If you moved the build, rerun the native build so `bin/mnemo2-path.txt` points to the right `mnemo2`.
- Connection lost after Mnemo2 restarted: the helper retries the lookup once and tells Claude in plain text if it still cannot connect. Restart the Claude session in that case. Stale run files in `~/.mnemo/run/` (dead process or no answer) are ignored and removed.
- No notices while idle: check that the waiter is running (the lead starts it with Bash in the background). `mnemo2-mcp wait` run by hand prints a notice or "no events".
- Hooks seem silent: they are, outside an attached Mnemo2 project. Inside one, `/hooks` lists them.
- Hooks and Claude Code versions: the hooks run `mnemo2-mcp` directly, with no shell. Versions that ignore a hook's `args` (2.1.138 does) still work, because the helper reads which hook fired from the event Claude Code sends it.
- Claude says Mnemo2 "could not open the project": the reason that follows comes from Mnemo2 itself, for example a missing API key for the default model (`not_configured`). Fix it in Mnemo2 (open it from the tray), then retry. A failed hidden Mnemo2 stays in the tray, and no second one is started: a retry within 30 s of the failure gets the same reason, and a later one asks that Mnemo2 to try opening the project again.
- Tier errors on dispatch: set up tiers in Mnemo2 under Settings, Tiers. The lead may only use models a tier names.
