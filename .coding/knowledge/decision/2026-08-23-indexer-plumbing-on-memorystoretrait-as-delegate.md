+++
title = "indexer plumbing on MemoryStoreTrait as delegates"
created = "2026-08-23"
+++

DECISION: indexer plumbing (index_state_*/get_memory/set_derived_metadata/delete_derived) added to MemoryStoreTrait as thin delegates to inherent MemoryStore methods, so trait-object holders (memory tools' KnowledgeSupport, finish capture) can reindex. Only one impl exists (no mocks).
