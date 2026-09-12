+++
title = "symbol-wiring questions → graph_context, not grep+read"
created = "2026-08-29"
+++

User correction (2026-09-10, merge_to_main skill-optimization session): during code investigation I used `search`+slice-reads for SYMBOL-WIRING questions ("who consumes active_skill", "who calls seed_skills") where graph_context was the mandated first step — one call each would have replaced two grep+read round-trips. Rule as applied to this repo: string literals (tool names like merge_to_main, config keys, test assertion text) → `search`; symbol wiring (callers/consumers/definitions of fns/methods/structs) → graph_search/graph_context FIRST, search only as fallback. Memory_search stays the opener for context (merge history, SPECs like skills-seeded-at-init) but never substitutes for reading the file under edit.
