+++
title = "graph_search is lexical substring, not semantic — retry name fragments on zero"
created = "2026-08-29"
+++

Data point for the queued steering-optimization analysis (HOW 706d24ad "track search usage"; round: backlog 63cbc20f stuck read_files card, 2026-09-18 session). User asked: "aren't we using semantic search? so it should have found ToolCard?" Verified in code: graph_search is LEXICAL — exact → case-insensitive exact → case-insensitive SUBSTRING where the QUERY must appear inside the symbol name (src/codegraph/query.rs:136 "symbols whose name contains this substring"; src/codegraph/store.rs:521 uses instr; tool desc "exact matches first, then case-insensitive, then substring"). No embeddings/fuzzy/word-token bridge. Only memory_search is semantic (embedding-ranked).

Root cause of this session's search-heavy detour: graph_search("ToolCallCard") → 0 was CORRECT behavior (no symbol name contains "ToolCallCard"; agent hallucinated the compound name from the UI concept), and the agent ignored the zero-result hint "retry with a shorter query". graph_search("Card") finds ToolCard as 2nd hit in one hop (verified live), alongside MemoryEntryCard/VisionEntryCard/UsageCard.

Agent procedure when a compound/conceptual symbol name misses the graph: (1) retry with word fragments of the expected name ("Card", "Reducer", "Result"); (2) only then fall back to text search. Candidate product improvement (offered to user 2026-09-18, NOT yet queued): token-level matching or "did you mean <fragments>" suggestions on graph zero-results, since the current hint is routinely ignored.
