+++
title = "Claude Code is steered to Mnemo's code and memory tools"
created = "2026-11-02"
+++

In a Mnemo2 project, the Mnemo MCP server exposes the engine's `search` and `search_read`, next to graph_* and memory_*. They run on a sandbox at the project root with the shared engine's code graph and memory, so identifier queries answer from the graph and typed-knowledge queries from memory. Output is capped at 20 KB with a "narrow the query" note (mcp_server.rs CODE_SEARCH_OUTPUT_CAP). LEAD_INSTRUCTIONS tell the lead to use search / graph_* for code and memory_search for history before Grep or Glob. A PreToolUse hook on Grep|Glob (`mnemo2-mcp hint --pre-search`, src-tauri/src/bin/mnemo2_mcp/hooks.rs) adds a one-line hint naming the better Mnemo tool and its ToolSearch select string. It fires only for identifier patterns (snake_case, CamelCase, Type::member, a declaration keyword plus a name) and knowledge lookups (SPEC:/DECISION:/BUG:/HOW: or .coding/knowledge). It never blocks, and stays silent for prose, regexes, paths and non-Mnemo directories.
