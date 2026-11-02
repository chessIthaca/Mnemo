+++
title = "an idle Claude Code lead comes back only through its own session's hooks"
created = "2027-01-11"
+++

Claude Code does not tell MCP servers its session id, so Mnemo2 learns it through the waiter. Hooks receive session_id on stdin. The SessionStart hook teaches each session its own waiter command, `mnemo2-mcp wait --session <id>`. The PostToolUse drain repeats it while workers run and no waiter is alive, and the Stop hook uses it too. /lead/wait with a session stakes ExternalLead.session_id (src-tauri/src/mnemo2/team.rs). After the 10-minute idle release, McpServer::notices (mcp_server.rs) re-attaches the lead only when it is idle, the call is not a peek, and the hook's session_id equals the staked one (TeamRuntime::reattach_idle_external_lead). A bare MCP tool call attaches anonymously and clears any staked name. An anonymous lead never re-attaches through hooks, and a real detach (/lead/detach) forgets the session. The MCP instructions keep the plain waiter command and add: "If a hook gave you a waiter command with --session, use that one." They must stay under 2048 characters (test the_lead_instructions_fit_claude_codes_limit_and_name_real_tools).
