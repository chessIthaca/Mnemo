+++
title = "codegraph extractor version backfill (EXTRACTOR_VERSION + cg_extract_meta)"
created = "2026-11-02"
+++

Codegraph extractor-semantics versioning: `schema::EXTRACTOR_VERSION` (i64, currently 1) + the per-file `cg_extract_meta (path, version)` stamp in src/codegraph/schema.rs. Whenever a change alters what `extract_file` persists (first instance: RefKind::MethodCall — refs record receiver syntax), bump the const; every `Store::upsert_file` stamps the current version (and `stored_file_meta` surfaces it as `FileMeta.extract_version`), and `extract_job` re-parses files whose stamp is missing or old — including inside the mtime fast-path, which otherwise would skip old files forever. This is exactly the cg_content_meta backfill shape: fires once per old file on the next plain index pass (the one every app startup runs), never perpetually. It also guarantees `rebuild_edges` runs after the re-parse (index_inner only rebuilds edges when something was re-indexed), so old dbs actually heal their stale cross-language edges instead of keeping them until a file changes.
