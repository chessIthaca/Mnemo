+++
title = "Mnemo2 records the sender of every worker message in the team ledger"
created = "2027-01-11"
+++

Every message sent to a Mnemo2 worker carries a MessageSender (src-tauri/src/mnemo2/team.rs): external_lead (Claude Code via MCP, through TeamRuntime::handle_lead_host_tool_from in mcp_server.rs call_tool), lead (the built-in lead's host tool), user (mission control, ipc.rs team_message), or mnemo (generated route-fix briefs, kind fix). TeamRuntime::message persists an InboxEntry {sender, kind prompt|steer|brief|fix, text, at_ms} on WorkerRecord.inbox (serde default, capped at 200) BEFORE sending the session command, and rolls it back if the send fails. So .coding/mnemo2/team.json shows who sent what even mid-turn. The worker receives the text behind a header such as "[From the lead (Claude Code)]". message_parent's result and the executor prompt state that a lead reply only ever arrives as a new [From …] message and must not be assumed. To check whether a worker really received a message, read its inbox entries (team_status) rather than its transcript.
