# Plan: Claude Code (Claude Desktop) as the lead of Mnemo2's team

Branch `wt/claudeLead`, worktree `.worktrees/claudeLead`, based on `wt/mnemo` 4fd1e97.

## Goal

The user works in Claude Code (CLI or the Claude Desktop Code tab) on their
own Claude subscription. Claude Code acts as the **lead**: it plans,
dispatches, reviews and merges through Mnemo2's team. Mnemo2's **workers**
do the work on the user's own API endpoints and tiers (Kimi, GLM, DeepSeek,
…). Mnemo2's mission control shows it all live.

Claude Code is extended only through its official extension points (an MCP
server, a plugin, skills and hooks). Subscription use stays inside Claude
Code, so this is ordinary Claude Code use.

## Decisions (agreed)

- **Mnemo2 starts on demand.** A stdio helper connects to a running Mnemo2
  or launches it hidden in the tray. The team manager stays in the app; no
  daemon split.
- **Lead guidance:** MCP server `instructions` give a short lead brief, and a
  `mnemo-lead` skill holds the long form. The team tools' own descriptions
  carry over unchanged. Mnemo2's lead has no separate prompt today; its
  guidance lives in the `lead_host_tools()` descriptions and the runtime
  notices.
- **Notices while the lead is busy:** they arrive appended to every Mnemo
  tool result, and through a PostToolUse hook on all tools.
- **Notices while the lead is idle:** a background waiter (`mnemo2 wait`)
  that the lead keeps running while workers are active. A Stop hook is the
  safety net.
- **Channels** are a later upgrade, once they leave research preview and
  work in the Desktop app.
- **Claude models as Mnemo workers** still need an API key. Accepted.
- **New:** Claude Code's own agents (the `Agent` and `Workflow` tools) are a
  last resort. Mnemo workers come first, and **using its own agents needs
  the user's explicit OK**. A hook enforces this; it is not just guidance.

## Architecture

```
Claude Code (Desktop / CLI)                 Mnemo2 (tray or window)
 ├─ plugin "mnemo"                           ├─ TeamManager (unchanged)
 │   ├─ .mcp.json ── stdio ──► mnemo2-mcp ──HTTP(127.0.0.1, token)──► MCP endpoint
 │   ├─ skills/  (mnemo-lead, plan, review…)  │    tools/list  = lead_host_tools() + extras
 │   ├─ hooks/   (PostToolUse, Stop,           │    tools/call  = handle_lead_host_tool()
 │   │            SessionStart, PreToolUse)    │    instructions = lead brief
 │   └─ agents/  (optional)                    ├─ notice queue for the external lead
 └─ Bash bg: mnemo2-mcp wait ─────────────────►└─ mission control UI
```

### 1. `mnemo2-mcp`, a small console binary

- It is a **separate console binary**, not a subcommand of `mnemo2`.
  Release `mnemo2` builds are GUI-subsystem binaries on Windows
  (`windows_subsystem = "windows"`), and a stdio MCP server needs a console
  stdin and stdout. The new binary is `src-tauri/src/bin/mnemo2_mcp.rs` with
  no Tauri dependency.
- **`mnemo2-mcp serve`** (the plugin's MCP command) does three things:
  1. Reads `~/.mnemo/run/mnemo2.json` (port, token, pid). If it's missing or
     stale, it starts `mnemo2 --hidden --project <cwd>` and waits up to 30 s
     for the file.
  2. Bridges stdio JSON-RPC to Mnemo2's localhost MCP endpoint, keyed to a
     session (the Claude Code session id, from the plugin's MCP env, or a
     generated one).
  3. Reconnects if Mnemo2 restarts mid-session, and tells Claude in plain
     text if it can't.
- **`mnemo2-mcp notices [--drain]`** prints pending lead notices as hook
  JSON (`additionalContext`). It must run fast: no notices means exit 0 in
  under 50 ms.
- **`mnemo2-mcp wait [--timeout 1800]`** blocks until a notice arrives,
  prints it and exits 0. It exits 0 with "no events" on timeout. It records
  a waiter heartbeat so the Stop hook knows one is running.
- **`mnemo2-mcp gate-agent`** is the PreToolUse decision for Claude Code's
  own agents (see section 5).

### 2. Mnemo2 app changes

- **Single instance:** add `tauri-plugin-single-instance`. A second launch
  hands its arguments (project, `--hidden`) to the running copy.
- **`--hidden` start and a tray icon:** the tray offers Open mission
  control, Workers (n running), and Quit. Quitting with workers running asks
  first.
- **Run file:** `~/.mnemo/run/mnemo2.json` (port, random token, pid,
  version), written at startup and removed on exit. Readable by the current
  user only.
- **MCP endpoint** (new `src-tauri/src/mnemo2/mcp_server.rs`): Streamable
  HTTP on 127.0.0.1 with JSON responses. It handles `initialize` (with
  `instructions`), `tools/list`, `tools/call` and `ping`, and rejects
  requests without the token.
  - `tools/list` = `lead_host_tools()` plus the extras: `mnemo_status`,
    `open_mission_control`, `plan_create` / `plan_update` if they aren't
    already lead tools (check), and later memory and codegraph.
  - `tools/call` → `handle_lead_host_tool()` for team tools. Each result
    gets any pending notices appended under "Since your last call:".
- **External lead mode:**
  - A project gets an **external lead** when an MCP session attaches.
  - `deliver_lead_notice` then queues notices for the external lead instead
    of steering the in-app lead session.
  - Mission control shows "Lead: Claude Code (external)". The in-app lead
    composer stays usable, but sending there asks first ("an external lead
    is active").
  - Detaching (the MCP session closes, or 10 min pass without a call or
    waiter) returns the project to the in-app lead.
- **Project binding:** the MCP session's working directory picks the Mnemo
  project (it must be a registered project or a subfolder of one).
  Otherwise the tools answer with how to register it.

### 3. Lead guidance

- **`instructions` (short, under ~2k characters):**
  - You lead a Mnemo team.
  - Dispatch by tier.
  - Review, then merge.
  - Keep `mnemo2-mcp wait` running in the background while workers are
    active.
  - Mnemo workers come first; ask the user before using your own agents.
  - For details, use the `mnemo-lead` skill.
- **`skills/mnemo-lead/SKILL.md` (long form):** the plan board, tiers and
  when to use each, dispatch and brief writing, the review and merge cycle,
  findings, audits, failure and model switching (`team_resume` with a tier),
  the waiter pattern, and when the user's OK is needed.
- **Other skills:** `mnemo-plan` (plan-first), `mnemo-review`, and
  `mnemo-status`, each a slash command that maps to tool calls.
- **One source of truth:** the skill and the instructions live in the repo
  next to the team code (`mnemo2/plugin/`). A test checks that every tool
  named in them exists in `lead_host_tools()`.

### 4. Hooks (plugin `hooks/hooks.json`)

| Hook | Command | Effect |
|---|---|---|
| SessionStart | `mnemo2-mcp notices --session-start` | Adds the board summary, running workers and pending notices to the context. |
| PostToolUse (all tools) | `mnemo2-mcp notices --drain` | Delivers busy-time notices at the next tool call. |
| Stop | `mnemo2-mcp notices --stop-check` | If workers are active and no waiter heartbeat exists, blocks once with "start `mnemo2-mcp wait` in the background". It never blocks twice in a row, so it can't loop. |
| PreToolUse, matcher `Agent\|Task\|Workflow` | `mnemo2-mcp gate-agent` | Returns `permissionDecision: "ask"` with the reason (section 5). |

### 5. Claude Code's own agents need the user's OK

- **Policy (instructions and skill):** Mnemo workers do delegated work.
  Claude Code's own agents are only for when Mnemo truly can't do the job,
  for example when Mnemo2 is down or the task needs Claude-only
  capabilities. Before using one, Claude says why in chat and asks.
- **Enforcement:** the PreToolUse hook on `Agent`, `Task` (the older name)
  and `Workflow` returns `ask`. Claude Code then shows its permission
  prompt, with the reason "This bypasses Mnemo's workers (runs on your
  Claude subscription). Allow?" The user's click is the approval, so Claude
  can't skip it.
- **Pass-throughs (no prompt):** read-only lookups the user would never want
  gated, decided by the agent type in the tool input: `Explore` and
  `claude-code-guide`. This is configurable in `~/.mnemo/config.toml`
  (`[claude_lead] agents_without_prompt = [...]`). The default list is
  exactly those two.
- **Only while a Mnemo lead session is attached:** with no project bound,
  the hook allows everything, so Claude Code works normally outside Mnemo
  projects.

### 6. Plugin packaging

- **Location:** `mnemo2/plugin/`, containing `.claude-plugin/plugin.json`,
  `.mcp.json` (`mnemo2-mcp serve`), `hooks/hooks.json`, `skills/…` and
  `README.md`.
- **Install:** a repo marketplace (`.claude-plugin/marketplace.json` at the
  repo root). The user adds it once in the Desktop Code tab or the CLI and
  installs "mnemo".
- **Paths:**
  - `mnemo2-mcp` must be on PATH, or the plugin's MCP and hook commands use
    `${CLAUDE_PLUGIN_ROOT}`-relative paths.
  - The build installs or copies `mnemo2-mcp.exe` next to `mnemo2.exe`.
  - `build:mnemo2:native` also builds `--bin mnemo2_mcp`.
- **Naming:** the plugin is "mnemo"; its text says it works with Claude
  Code. No Anthropic marks are used.

## Phases

0. **Spike (do first).** Use a throwaway plugin in a scratch project to
   check, in the **Desktop Code tab** and in the CLI:
   - a background Bash command finishing wakes an idle session;
   - PostToolUse `additionalContext` reaches the model;
   - a Stop hook block works;
   - PreToolUse `permissionDecision: "ask"` on `Agent` shows the prompt and
     honors the answer;
   - MCP `instructions` reach the system prompt, and how long they can be;
   - plugin MCP env and `${CLAUDE_PLUGIN_ROOT}` are available;
   - which session id is available to the MCP server and to hooks.

   Write down every finding. If background wake doesn't work in Desktop,
   re-plan the idle path before writing more code.
1. **Mnemo2 shell:** single instance, `--hidden`, tray, run file. Tests
   cover the run-file lifecycle and argument handover.
2. **MCP endpoint plus external lead mode:**
   - tools/list and tools/call over the lead tools;
   - auth token;
   - project binding;
   - notice queue and append;
   - attach and detach.

   Rust tests drive a TeamManager through MCP JSON-RPC: dispatch, notices,
   detach fallback.
3. **`mnemo2-mcp` binary:** serve bridge, auto-start, reconnect, `notices`,
   `wait`, `gate-agent`. Tests use a fake endpoint.
4. **Plugin:** manifest, `.mcp.json`, hooks, `mnemo-lead` skill and the
   other skills, instructions, and the tool-name consistency test.
5. **Mission control UI:** the external-lead badge, the in-app composer
   guard, tray worker count (vitest).
6. **Memory, codegraph and workbench tools** as extra MCP tools, from the
   headless engine.
7. **Docs:** `CONFIGURATION.md`, `FEATURES.md`, plugin README and install
   steps.
8. **End to end in Claude Desktop:**
   - install the plugin;
   - start a session in a Mnemo project with Mnemo2 closed (it auto-starts
     in the tray);
   - plan, then dispatch 2 workers by tier;
   - let Claude idle until the waiter wakes it on completion;
   - review and merge;
   - try an `Agent` call and see the permission prompt;
   - quit Mnemo2 mid-session and see the reconnect message.

## Risks

- **Background wake in the Desktop app:** the whole idle path depends on
  it, so the spike checks it first. Fallback: the Stop hook keeps the lead
  in a short polling loop while workers run. That costs usage, so it is
  opt-in.
- **Two leads on one project:** external lead mode plus the composer guard
  prevent conflicting dispatches. The TeamManager's merge lock still
  serializes merges.
- **Hook latency:** PostToolUse runs on every tool call, so `notices` must
  be quick (native binary, one localhost request, under 50 ms).
- **Plugin and hook formats can change between Claude Code versions:** the
  spike pins what works, and the plugin README states the minimum version.
- **Local endpoint security:** bound to 127.0.0.1, random token, run file
  readable by the user only.
