+++
title = "load_tools confirmation is final — the response renders the schemas; never re-call"
supersedes = "2027-01-07-load-tools-confirmation-is-final-never-re-call-l"
created = "2027-01-07"
+++

load_tools(group) success is final AND self-sufficient: the response renders each newly revealed tool's schema — name + required parameters with types for every tool, plus descriptions and optional params for small groups (≤ 8 tools; image renders full, the browser family renders compact to protect the deferral's token budget). Pre-flight your calls from the response; the tools also join the compiled list from the next message on. NEVER re-call load_tools for a group already loaded — it returns "was already loaded" (short, by design) and wastes a turn (2027-01-09 incident: 4 consecutive redundant calls). The app-side fix is plan de884bcf / backlog cc52264b (commit "feat: load_tools renders revealed tools' schemas"); the renderer lives in src/tool/agent/load_tools.rs (render_schemas + group_schemas + FULL_SCHEMA_GROUP_MAX). Supersedes the 2027-01-07 record whose premise — schemas absent from the response — the fix removed; the knowledge file .coding/knowledge/how/2027-01-07-load-tools-confirmation-is-final-never-re-call-l.md carries the original lesson.
