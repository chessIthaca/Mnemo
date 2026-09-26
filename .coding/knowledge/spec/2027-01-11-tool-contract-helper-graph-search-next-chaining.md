+++
title = "tool_contract helper + graph_search `next` chaining pointer"
created = "2027-01-11"
+++

Since plan aff95a51 (commit 260d1f5 on wt/mnemo): (1) Every tool with required fields builds its description as format!("{} …prose…", tool_contract::contract(required, example)) — the shared empty-call sentence lives in src/tool/agent/tool_contract.rs, with recovery_hint(field) riding the argument-error path. New tools MUST use the helper, never hand-copy the block; the content-first clause lives once in TOOL_CALL_DISCIPLINE (src/agent/prompt.rs). Drift guard: every_migrated_description_leads_with_the_shared_contract (7 tools, in tool_contract.rs tests). (2) graph_search results carry `next` on non-empty matches — the exact graph_context(id=...) call for the first hit plus graph_impact for blast radius; follow it instead of re-deriving the id. A total miss carries no `next` (its hint redirects to `search`). Detail + measured delta: .coding/knowledge/how/2027-01-11-empty-call-descriptor-boilerplate-cost-vs-effica.md (amended with the landed outcome).
