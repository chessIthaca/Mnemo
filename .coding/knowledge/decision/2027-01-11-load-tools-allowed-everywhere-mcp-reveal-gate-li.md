+++
title = "load_tools allowed everywhere — mcp reveal gate lifted + skill membership (77efc1b7)"
created = "2027-01-11"
status = "superseded"
+++

User policy (request 2027-02-05, backlog 77efc1b7, plan 402cc091, commits 0c4ede0 + ef4e2ed on wt/mnemo — UNMERGED, verify with git_read before assuming it's in main): load_tools is callable in every workflow state and inside skills. (1) The mcp.* reveal state gate (mcp_reveal_allowed + its dispatch enforcement) is deleted — reveals succeed in Planning/Complete/research too; the revealed McpTools stay filter-gated (mcp__ exclusion in Planning/Complete/research), so nothing unusable ever dispatches. (2) load_tools rides the ToolFilter::Skill always-available set — a skill whose allow-list names MCP tools can materialize them (they don't exist in the registry until the group is revealed; named browser/image tools already bypass deferral via explicitly_names). The reviewer subagent still never gets load_tools (its strict allow-list never names it). Pin: tool::tests::load_tools_allowed_in_every_state_and_inside_skills (src/tool/mod.rs). The MCP SPEC record (41a23e88) is amended with the lift.
