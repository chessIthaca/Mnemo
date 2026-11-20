+++
title = "codegraph extractor version backfill (EXTRACTOR_VERSION + cg_extract_meta)"
created = "2026-11-02"
+++

Codegraph extractor-semantics versioning: `schema::EXTRACTOR_VERSION` (i64, currently 1) + the per-file `cg_extract_meta (path, version)` stamp in src/codegraph/schema.rs. Whenever a change alters what `extract_file` persists (first instance: RefKind::MethodCall — refs record receiver syntax), bump the const; every `Store::upsert_file` stamps the current version (and `stored_file_meta` surfaces it as `FileMeta.extract_version`), and `extract_job` re-parses files whose stamp is missing or old — including inside the mtime fast-path, which otherwise would skip old files forever. This is exactly the cg_content_meta backfill shape: fires once per old file on the next plain index pass (the one every app startup runs), never perpetually. It also guarantees `rebuild_edges` runs after the re-parse (index_inner only rebuilds edges when something was re-indexed), so old dbs actually heal their stale cross-language edges instead of keeping them until a file changes.

Amended 2026-11-20: the version history so far is v1 (RefKind::MethodCall), v2 (step 9: per-symbol signature + doc text rows for semantic code search), v3 (step 15: the embedding chunk gains the name's split words, the enclosing type's name and the body excerpt, rebuilt from cg_content rows and contains edges — so every text_hash changes and every symbol re-embeds lazily). schema.rs's EXTRACTOR_VERSION doc comment carries the authoritative version list; keep it in sync on every bump.
