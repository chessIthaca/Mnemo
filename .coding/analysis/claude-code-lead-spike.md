# Spike: Claude Code extension behavior for the external-lead plan (0f378abe step 1)

Date: 2026-10-31. Terminal CLI: `claude` 2.1.138. A throwaway plugin was run with `claude -p --plugin-dir`, in a clean environment (see the last note) and on the Haiku model.

## Verified locally

- **Plugin MCP server:**
  - Connects as `plugin:spike:spike`. Its tools appear as `mcp__plugin_<plugin>_<server>__<tool>`.
  - The server's process cwd is the directory Claude Code was started in, which is the project.
  - Its env includes `CLAUDE_PLUGIN_ROOT` and `CLAUDE_PLUGIN_DATA`.
  - **No `CLAUDE_CODE_SESSION_ID` in 2.1.138.** The docs say newer versions set it.
- **MCP handshake:** `initialize` uses protocolVersion `2025-11-25` with client capabilities `roots` and `elicitation`, then `notifications/initialized`, then `tools/list`. A newline-delimited JSON-RPC server works.
- **Hooks:**
  - The SessionStart hook ran, and its `additionalContext` JSON was accepted (hook_response exit 0).
  - Hook stdin has `session_id`, `cwd`, `transcript_path`, `hook_event_name` and `source`.
  - Hook env has `CLAUDE_PROJECT_DIR`, `CLAUDE_PLUGIN_ROOT`, `CLAUDE_PLUGIN_DATA` and `CLAUDE_ENV_FILE`, but no `CLAUDE_CODE_SESSION_ID` (2.1.138).
- **Tool names:** the tools list names the subagent tool `Task` in 2.1.138, while the docs and the newer Desktop build use `Agent`. Matcher: `Agent|Task|Workflow`.
- **Background wake in Desktop:** this Desktop Code-tab session is woken by background-command completion notices (seen repeatedly, e.g. the cargo warm build). The idle-wake path is therefore viable.

## From the docs (claude-code-guide lookup, 2026-10-31)

- **Variable expansion:** `${CLAUDE_PLUGIN_ROOT}` is expanded in hook `command`/`args` and in MCP stdio `command`/`args`/`env`.
- **Exec form:** hooks with `args` run without a shell. Use that on Windows.
- **Hook outputs:**
  - PreToolUse: `hookSpecificOutput.permissionDecision`, one of `allow|deny|ask|defer`. With `ask`, `permissionDecisionReason` is shown to the user.
  - PostToolUse: `hookSpecificOutput.additionalContext`.
  - Stop: `decision:"block"` with a required `reason`. Guard with `stop_hook_active`; there is a cap of 8 consecutive continuations.
- **MCP instructions** load at session start and are truncated at 4,096 characters.
- **MCP timeouts:** `MCP_TOOL_TIMEOUT` defaults to about 28 h, and a per-server `timeout` overrides it. Stdio servers have an idle timeout of 1,800,000 ms by default.
- **Local marketplace:** a local directory marketplace with a relative `source` reads the plugin files in place. Rebuilding the binary into `plugin/bin/` therefore updates it without reinstalling.

## Design consequences

1. **Session key = canonical project root**, not the Claude session id. The MCP server knows it from its cwd, and hooks know it from `CLAUDE_PROJECT_DIR`/`cwd`. Both sides agree without needing `CLAUDE_CODE_SESSION_ID`, which 2.1.138 doesn't provide. Several Claude sessions in one project share one external-lead queue; this is documented.
2. **Hook and MCP commands use exec form:** `"command": "${CLAUDE_PLUGIN_ROOT}/bin/mnemo2-mcp"` plus `args`. The build copies the helper into `mnemo2/plugin/bin/`, which is gitignored.
3. **`mnemo2-mcp wait`** is a Bash background command that Claude starts. It is not an MCP call, so MCP timeouts don't apply.
4. **The agent gate matches `Agent|Task|Workflow`.**

## Still to verify (needs the model)

The terminal CLI's stored claude.ai sign-in is expired. The API returns 401 "OAuth access token has expired", even though `claude auth status` still says logged in. The user has to run `claude auth login` once in their own terminal. Then re-run `spike/` to check:
- instructions reach the model;
- PostToolUse `additionalContext` reaches the model;
- the Stop block and its continuation;
- PreToolUse `ask` on `Agent`/`Task` in `-p` mode.

These checks fold into the final step-9 run against the real plugin.

Note: a `claude` started from inside a Desktop session inherits the Desktop host's `ANTHROPIC_BASE_URL` and `CLAUDE_CODE_*` environment. Scripted runs therefore unset every `CLAUDE*`/`ANTHROPIC*` variable first (the `cleanrun.sh` wrapper).
