+++
title = "an agent-tier dispatch test must call handle_lead_host_tool_from with ExternalLead"
created = "2026-11-25"
evidence = "tests.rs remote_agent_dispatch_answers_claude_session_block failed with a node session spawn for model test/scripted-difficult until the sender was ExternalLead; source: src-tauri/src/mnemo2/team/lead_tools.rs:634-647 and team/routing.rs:896-917."
+++

In Mnemo2's team tests, `TeamRuntime::handle_lead_host_tool(name, args)` passes MessageSender::Lead, so a team_dispatch through it never settles on a `claude_agent` tier (routing.rs `claude_agent_dispatch` requires sender == MessageSender::ExternalLead) — the step runs as a Mnemo worker instead. Use `handle_lead_host_tool_from(name, args, MessageSender::ExternalLead)` (pub, lead_tools.rs ~:641) to exercise the agent-tier dispatch answer end to end. Related: a dispatch answer's output is the JSON on the first line plus instruction lines after "\n", so parse `answer.output.lines().next()`, not the whole string; and `remote_testkit::Rig`'s fields are pub(super) to check_run, so tests outside it only get `Rig::start`/`team()`/`call_node`/`worker_on_node` and must link SSH via the `team_hosts` tool instead of `rig.hub.nodes()`.
