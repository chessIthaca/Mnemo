// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! SQLite schema for the CodeGraph store.
//!
//! Tables:
//! - `cg_files` — one row per indexed source file (path, content hash, mtime)
//! - `cg_symbols` — one row per defined symbol (functions, structs, …)
//! - `cg_edges` — resolved relationships between symbols (calls / imports /
//!   contains), indexed in both directions for neighborhood queries
//! - `cg_refs` — unresolved per-file references. Persisting these lets
//!   `Store::rebuild_edges` re-resolve every cross-file edge after any file
//!   changes, without re-parsing the unchanged files. The `kind` column is
//!   the persisted form of `RefKind`: `"call"`, `"method_call"` (receiver
//!   syntax, plan 0d53d659), `"import"`.
//! - `cg_content` — FTS5 full-text index of every source line, the
//!   index-backed path for the `search` tool's literal queries.
//! - `cg_content_meta` — one row per file (path → content hash). It doubles
//!   as the backfill detector: indexed files without a meta row predate the
//!   content index and need a rebuild.
//! - `cg_extract_meta` — one row per file (path → [`EXTRACTOR_VERSION`]),
//!   the extractor-semantics backfill detector: files stamped with an
//!   older (or missing) version predate a change in what the extractor
//!   persists (v1: refs carry the method-call shape, plan 0d53d659) and
//!   need one re-parse — their rows cannot be re-derived in place.
//!
//! The DB is a derivable cache (gitignored, rebuilt from source on startup),
//! so the schema is applied idempotently with `CREATE TABLE IF NOT EXISTS` and
//! carries no migration machinery: a breaking schema change ships by deleting
//! the file and letting the next index run rebuild it.

use rusqlite::Connection;

use crate::error::Result;

/// The version of the extractor's persisted output semantics, stamped per
/// file in `cg_extract_meta` on every upsert. Bump it whenever a change
/// alters what `extract_file` persists (v1: `RefKind::MethodCall` — refs
/// record receiver syntax so the resolver can refuse cross-language and
/// method-call-to-free-fn targets, plan 0d53d659). A plain index pass
/// re-parses stamped-older files exactly once — same backfill shape as
/// `cg_content_meta`, never perpetual.
pub const EXTRACTOR_VERSION: i64 = 1;

/// Enable the same pragmas the memory store relies on: WAL (concurrent readers
/// never block the background index writer), a busy timeout so a reader waits
/// out a momentary commit instead of erroring, and `synchronous=NORMAL` (safe
/// under WAL, skips one fsync per commit).
pub fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=5000;
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
}

/// Apply the schema to a fresh (or existing) connection. Idempotent.
pub fn apply_schema(conn: &Connection) -> Result<()> {
    apply_pragmas(conn)?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS cg_files (
            path         TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            mtime        INTEGER NOT NULL,
            indexed_at   INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS cg_symbols (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            kind       TEXT NOT NULL,
            file       TEXT NOT NULL,
            start_line INTEGER NOT NULL,
            end_line   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_cg_symbols_name ON cg_symbols(name);
        CREATE INDEX IF NOT EXISTS idx_cg_symbols_file ON cg_symbols(file);

        CREATE TABLE IF NOT EXISTS cg_edges (
            from_id TEXT NOT NULL,
            to_id   TEXT NOT NULL,
            kind    TEXT NOT NULL,
            UNIQUE(from_id, to_id, kind)
        );
        CREATE INDEX IF NOT EXISTS idx_cg_edges_from ON cg_edges(from_id);
        CREATE INDEX IF NOT EXISTS idx_cg_edges_to ON cg_edges(to_id);

        CREATE TABLE IF NOT EXISTS cg_refs (
            file    TEXT NOT NULL,
            from_id TEXT NOT NULL,
            name    TEXT NOT NULL,
            kind    TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_cg_refs_file ON cg_refs(file);

        -- Full-text content index: one row per source line. `path`/`line`
        -- are stored but not tokenized (UNINDEXED); `text` is tokenized by
        -- the default unicode61 tokenizer (underscore is a separator, so
        -- snake_case identifiers index as adjacent tokens — phrase queries
        -- still find them).
        CREATE VIRTUAL TABLE IF NOT EXISTS cg_content USING fts5(
            path UNINDEXED,
            line UNINDEXED,
            text
        );
        CREATE TABLE IF NOT EXISTS cg_content_meta (
            path         TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS cg_extract_meta (
            path    TEXT PRIMARY KEY,
            version INTEGER NOT NULL
        );
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_applies_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        apply_schema(&conn).unwrap(); // second application must not error
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM cg_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        // The FTS5 content table exists and answers a phrase query (bundled
        // SQLite carries FTS5 — the memory store's fts5 test proves it too).
        conn.execute(
            "INSERT INTO cg_content (path, line, text) VALUES ('a.rs', 1, 'fn helper() {}')",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM cg_content WHERE cg_content MATCH '\"helper\"'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1);
    }
}
