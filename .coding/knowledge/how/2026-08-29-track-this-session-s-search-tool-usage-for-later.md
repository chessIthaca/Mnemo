+++
title = "track this session's search-tool usage for later steering optimization analysis"
created = "2026-08-29"
+++

User instruction (steering follow-ups session, 2026-09-15): keep track of the agent's own SEARCH tool usage so it can be analyzed later for further optimization (beyond the current steering counts). Concretely: note when a `search`/`search_read` tree-walk was used where a graph_* or memory lookup would have been cheaper (or vice versa), engine used (index vs walk), hit counts, round-trips saved/wasted. Observations to attach to future steering-data analysis alongside get_steering_stats. Log entries in the working tier; synthesize into the step-4 DECISION record / steering analysis.
