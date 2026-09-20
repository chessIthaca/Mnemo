+++
title = "load_tools allowed everywhere — mcp reveal gate lifted + skill membership (77efc1b7) — MERGED into main (752164e)"
supersedes = "2027-01-11-load-tools-allowed-everywhere-mcp-reveal-gate-li"
created = "2027-01-11"
+++

MERGED into main at 752164e (752164e420903fa893f4449c4fb02f85a1911dd7) on 2026-09-20 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip c91650c; work commits 0c4ede0 + ef4e2ed) - supersedes this record's earlier "on wt/mnemo - UNMERGED" marker. WHAT: load_tools is callable in every workflow state and inside skills. (1) The mcp.* reveal state gate (mcp_reveal_allowed + its dispatch enforcement) is deleted - reveals succeed in Planning/Complete/research too; the revealed McpTools stay filter-gated (mcp__ exclusion in Planning/Complete/research), so nothing unusable ever dispatches. (2) load_tools rides the ToolFilter::Skill always-available set - a skill whose allow-list names MCP tools can materialize them (they don't exist in the registry until the group is revealed; named browser/image tools already bypass deferral via explicitly_names). The reviewer subagent still never gets load_tools (its strict allow-list never names it). Pin: tool::tests::load_tools_allowed_in_every_state_and_inside_skills (src/tool/mod.rs). Full detail: .coding/knowledge/decision/2027-01-11-load-tools-allowed-everywhere-mcp-reveal-gate-li.md.
