// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! SQLite schema for the memory store.
//!
//! Tables:
//! - `memories` — all tiers, with strength, access_count, timestamps, embedding
//!   BLOB, and the semantic-record columns (`record_class` authored/derived,
//!   `record_type` spec/decision/bug/plan/how/none, `superseded_by` for
//!   supersede-not-delete history)
//! - `memories_fts` — FTS5 virtual table for full-text search on title + content
//! - `sessions` — session provenance
//! - `derived_index_state` — Phase 2 incremental-indexing state: one row per
//!   indexed source (`source_key` like `plan:<id>` / `review:<file-stem>` /
//!   `backlog:<id>`) with the content hash that produced its derived memory,
//!   so re-runs skip unchanged sources entirely (no re-embed, no write)

use rusqlite::Connection;

use crate::error::Result;

/// Enable pragmas that make concurrent multi-connection access safe and fast.
///
/// - `journal_mode=WAL` — readers never block the writer and vice versa. This
///   is what lets `MemoryStore` split reads onto a second connection without
///   the single-connection lock serializing every recall behind a write.
///   (A no-op for in-memory databases, which don't use a journal.)
/// - `busy_timeout` — with two connections a reader can still momentarily
///   meet the writer's commit; wait briefly instead of erroring with
///   `SQLITE_BUSY`.
/// - `synchronous=NORMAL` — safe under WAL, skips one fsync per commit.
pub fn apply_pragmas(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA busy_timeout=5000;
         PRAGMA synchronous=NORMAL;",
    )?;
    Ok(())
}

/// Apply the schema to a fresh (or existing) connection.
pub fn apply_schema(conn: &Connection) -> Result<()> {
    apply_pragmas(conn)?;
    // Main memories table.
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS memories (
            id              TEXT PRIMARY KEY,
            tier            TEXT NOT NULL,
            title           TEXT NOT NULL,
            content         TEXT NOT NULL,
            data            TEXT NOT NULL DEFAULT '{}',
            strength        REAL NOT NULL DEFAULT 1.0,
            access_count    INTEGER NOT NULL DEFAULT 0,
            created_at      INTEGER NOT NULL,
            last_accessed_at INTEGER NOT NULL,
            source_session_ids TEXT NOT NULL DEFAULT '[]',
            embedding       BLOB,
            embed_model     TEXT,
            embed_dim       INTEGER,
            record_class    TEXT NOT NULL DEFAULT 'authored',
            record_type     TEXT NOT NULL DEFAULT 'none',
            superseded_by   TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
        CREATE INDEX IF NOT EXISTS idx_memories_strength ON memories(strength DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_last_accessed ON memories(last_accessed_at DESC);

        -- Phase 2: incremental-indexing state for derived records. One row
        -- per indexed source; the indexer compares content_hash to skip
        -- unchanged sources (no re-embed, no write), and removal detection
        -- diffs these keys against the sources seen on the current run.
        CREATE TABLE IF NOT EXISTS derived_index_state (
            source_key   TEXT PRIMARY KEY,
            content_hash TEXT NOT NULL,
            memory_id    TEXT NOT NULL,
            indexed_at   INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_derived_index_state_memory
            ON derived_index_state(memory_id);

        CREATE TABLE IF NOT EXISTS sessions (
            id          TEXT PRIMARY KEY,
            project     TEXT NOT NULL,
            created_at  INTEGER NOT NULL,
            ended_at    INTEGER
        );

        -- Per-LLM-request token + timing stats. One row per Usage event,
        -- plus error rows (outcome = 'error'), compaction's own calls
        -- (purpose = 'summarize'), and cancelled rows (outcome = 'cancelled'
        -- — the consumer dropped the stream mid-flight on a user
        -- interrupt/cancel/compact/clear; our estimated prompt tokens,
        -- completion 0, cached_tokens NULL). `cached_tokens` is a subset of
        -- `prompt_tokens` (already included, reported for pricing — not a
        -- separate bucket); NULL when the provider never reported usage (an
        -- error or cancelled row) so a real reported miss (0) stays
        -- distinguishable.
        -- `ttft_ms` / `generation_ms` are NULL when the provider didn't
        -- report timing (e.g. mocked in tests). `endpoint` is the serving
        -- endpoint's `endpoints.toml` name (NULL for rows recorded before
        -- the endpoint dimension existed).
        CREATE TABLE IF NOT EXISTS request_stats (
            id                  TEXT PRIMARY KEY,
            session_id          TEXT,
            model               TEXT NOT NULL,
            endpoint            TEXT,
            prompt_tokens       INTEGER NOT NULL,
            completion_tokens   INTEGER NOT NULL,
            reasoning_tokens    INTEGER NOT NULL,
            cached_tokens       INTEGER,
            ttft_ms             INTEGER,
            generation_ms       INTEGER,
            created_at          INTEGER NOT NULL,
            outcome             TEXT,
            purpose             TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_request_stats_session ON request_stats(session_id);
        CREATE INDEX IF NOT EXISTS idx_request_stats_model ON request_stats(model);
        CREATE INDEX IF NOT EXISTS idx_request_stats_created ON request_stats(created_at);

        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );
        "#,
    )?;

    // FTS5 virtual table for full-text search. We use an external-content table
    // so the FTS index mirrors the memories table without duplicating data.
    // If FTS5 is unavailable (SQLite compiled without it), this will fail and
    // we fall back to a full-table scan in recall().
    let fts_result = conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            title,
            content,
            content='memories',
            content_rowid='rowid'
        );
        "#,
    );
    if fts_result.is_ok() {
        // FTS5 is available. Create triggers that keep the external-content
        // index in sync with the memories table. The UPDATE trigger has a
        // WHEN guard so that `access()` (which only bumps access_count +
        // last_accessed_at) does NOT cause FTS churn on the recall hot path.
        conn.execute_batch(
            r#"
            CREATE TRIGGER IF NOT EXISTS memories_fts_ai AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, title, content)
                VALUES (new.rowid, new.title, new.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_fts_ad AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, content)
                VALUES('delete', old.rowid, old.title, old.content);
            END;
            CREATE TRIGGER IF NOT EXISTS memories_fts_au AFTER UPDATE ON memories
            WHEN old.title IS NOT new.title OR old.content IS NOT new.content
            BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, content)
                VALUES('delete', old.rowid, old.title, old.content);
                INSERT INTO memories_fts(rowid, title, content)
                VALUES (new.rowid, new.title, new.content);
            END;
            "#,
        )?;
        // One-time migration: if memories already exist but the FTS index is
        // empty (e.g. a DB created before triggers were added), rebuild the
        // index from the content table. This is a no-op for fresh DBs.
        let mem_has_rows: bool =
            conn.query_row("SELECT EXISTS(SELECT 1 FROM memories LIMIT 1)", [], |row| {
                row.get(0)
            })?;
        let fts_empty: bool = conn.query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM memories_fts LIMIT 1)",
            [],
            |row| row.get(0),
        )?;
        if mem_has_rows && fts_empty {
            conn.execute_batch("INSERT INTO memories_fts(memories_fts) VALUES('rebuild');")?;
        }
    } else {
        // FTS5 unavailable — create a simple flag so the store knows to scan.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fts_unavailable (flag INTEGER NOT NULL DEFAULT 1);",
        )?;
    }

    // Idempotent column migration: add the model-fingerprint columns to
    // existing DBs created before they existed. `CREATE TABLE IF NOT EXISTS`
    // does NOT add new columns to an existing table, so ALTER TABLE is needed.
    // Guard each with a PRAGMA table_info check so it's a no-op on fresh DBs
    // (which already have the columns) and on DBs already migrated. Existing
    // rows get NULL → treated as stale → re-embedded on the next startup.
    migrate_add_column_if_missing(conn, "memories", "embed_model", "TEXT")?;
    migrate_add_column_if_missing(conn, "memories", "embed_dim", "INTEGER")?;

    // Semantic-record columns (typed memory records + supersede-not-delete
    // history). record_class/record_type carry NOT NULL defaults so existing
    // rows migrate to ('authored', 'none'); superseded_by stays NULL (= live).
    migrate_add_column_if_missing(
        conn,
        "memories",
        "record_class",
        "TEXT NOT NULL DEFAULT 'authored'",
    )?;
    migrate_add_column_if_missing(
        conn,
        "memories",
        "record_type",
        "TEXT NOT NULL DEFAULT 'none'",
    )?;
    migrate_add_column_if_missing(conn, "memories", "superseded_by", "TEXT")?;

    // The serving-endpoint dimension on request stats (per-endpoint tok/s
    // breakdown). Existing rows get NULL → grouped as one per-model row,
    // exactly the pre-dimension behavior.
    migrate_add_column_if_missing(conn, "request_stats", "endpoint", "TEXT")?;

    // R21: outcome/purpose tags + a nullable cached_tokens. An error row
    // (outcome = 'error') records our estimated prompt tokens with
    // cached_tokens NULL — not 0 — so a real reported miss stays
    // distinguishable from "the provider never reported usage"; a
    // compaction's own call is tagged purpose = 'summarize'. D1 adds the
    // cancelled class (outcome = 'cancelled' — the consumer dropped the
    // stream mid-flight on a user interrupt; same NULL-cached shape as an
    // error row). SQLite cannot
    // drop a NOT NULL constraint via ALTER TABLE, so the column-set change
    // is a table rebuild: copy the rows, swap the tables, recreate the
    // indexes. Gated on `outcome` (added together with `purpose` and the
    // nullability change — one rebuild covers all three). Runs AFTER the
    // endpoint migration above so the copied rows carry the endpoint.
    let has_outcome = {
        let mut stmt = conn.prepare("PRAGMA table_info(request_stats)")?;
        let mut rows = stmt.query([])?;
        let mut found = false;
        while let Some(row) = rows.next()? {
            if row.get::<_, String>(1)? == "outcome" {
                found = true;
                break;
            }
        }
        found
    };
    if !has_outcome {
        // The swap is atomic: SQLite DDL is transactional, and execute_batch
        // does NOT wrap statements in a transaction of its own — without the
        // explicit BEGIN/COMMIT a crash between DROP TABLE and RENAME would
        // leave every row orphaned in request_stats_new, silently lost on the
        // next open (CREATE IF NOT EXISTS + the outcome gate would never
        // clean it up). An uncommitted transaction rolls back on close, so a
        // mid-batch crash leaves the old table intact.
        conn.execute_batch(
            r#"
            BEGIN IMMEDIATE;
            DROP TABLE IF EXISTS request_stats_new;
            CREATE TABLE request_stats_new (
                id                  TEXT PRIMARY KEY,
                session_id          TEXT,
                model               TEXT NOT NULL,
                endpoint            TEXT,
                prompt_tokens       INTEGER NOT NULL,
                completion_tokens   INTEGER NOT NULL,
                reasoning_tokens    INTEGER NOT NULL,
                cached_tokens       INTEGER,
                ttft_ms             INTEGER,
                generation_ms       INTEGER,
                created_at          INTEGER NOT NULL,
                outcome             TEXT,
                purpose             TEXT
            );
            INSERT INTO request_stats_new
                (id, session_id, model, endpoint, prompt_tokens, completion_tokens,
                 reasoning_tokens, cached_tokens, ttft_ms, generation_ms, created_at)
            SELECT id, session_id, model, endpoint, prompt_tokens, completion_tokens,
                   reasoning_tokens, cached_tokens, ttft_ms, generation_ms, created_at
            FROM request_stats;
            DROP TABLE request_stats;
            ALTER TABLE request_stats_new RENAME TO request_stats;
            COMMIT;
            "#,
        )?;
        // The indexes were dropped with the old table — recreate them.
        conn.execute_batch(
            r#"
            CREATE INDEX IF NOT EXISTS idx_request_stats_session ON request_stats(session_id);
            CREATE INDEX IF NOT EXISTS idx_request_stats_model ON request_stats(model);
            CREATE INDEX IF NOT EXISTS idx_request_stats_created ON request_stats(created_at);
            "#,
        )?;
    }

    // Indexes over the new columns must be created AFTER the migrations
    // above: on a legacy DB the columns do not exist until the ALTER TABLEs
    // run, so indexing them in the first batch fails with "no such column".
    // (Scale requirement: record-class/type filters must hit indexes, never
    // full scans.) Idempotent, so a no-op once created.
    conn.execute_batch(
        r#"
        CREATE INDEX IF NOT EXISTS idx_memories_record_class ON memories(record_class);
        CREATE INDEX IF NOT EXISTS idx_memories_record_type ON memories(record_type);
        CREATE INDEX IF NOT EXISTS idx_memories_created_at ON memories(created_at DESC);
        "#,
    )?;

    Ok(())
}

/// Add a column to a table if it doesn't already exist (idempotent migration).
///
/// SQLite has no `ADD COLUMN IF NOT EXISTS`, so we check `PRAGMA table_info`
/// first. Safe on fresh tables (the column is already there → no-op) and on
/// existing tables (adds the column; existing rows get NULL).
fn migrate_add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> Result<()> {
    let exists: bool = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        let mut found = false;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                found = true;
                break;
            }
        }
        found
    };
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {col_type};"
        ))?;
    }
    Ok(())
}

/// Whether FTS5 is available on this connection.
pub fn fts_available(conn: &Connection) -> bool {
    // Check by looking for the fts_unavailable flag table.
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories_fts'",
        [],
        |_| Ok(()),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_applies_cleanly() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        // Tables exist.
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        apply_schema(&conn).unwrap(); // second time should not error
    }

    #[test]
    fn fts5_is_available_with_bundled_sqlite() {
        // The `bundled` feature of rusqlite ships SQLite with FTS5 enabled.
        // recall()'s FTS pre-filter path depends on this; if it ever becomes
        // unavailable, recall falls back to a full scan (still correct, slower).
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        assert!(fts_available(&conn));
    }

    #[test]
    fn fresh_db_has_record_columns_and_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        // The new columns exist and carry the right defaults on INSERT.
        let (class, rtype, superseded): (String, String, Option<String>) = conn
            .query_row(
                "INSERT INTO memories (id, tier, title, content, created_at, last_accessed_at)
                 VALUES ('x', 'semantic', 't', 'c', 0, 0)
                 RETURNING record_class, record_type, superseded_by",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(class, "authored");
        assert_eq!(rtype, "none");
        assert_eq!(superseded, None);
        // The Phase-1 indexes exist (scale: record filters must never full-scan).
        for index in [
            "idx_memories_record_class",
            "idx_memories_record_type",
            "idx_memories_created_at",
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index' AND name=?1)",
                    [index],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(exists, "missing index {index}");
        }
    }

    #[test]
    fn fresh_db_has_derived_index_state_table() {
        // Phase 2: the incremental-indexing state table exists on a fresh DB
        // with its memory_id index (removal detection + rebuild lookups must
        // hit an index, never a full scan at 1000s of sources).
        let conn = Connection::open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        // Roundtrip a row through the real columns.
        conn.execute(
            "INSERT INTO derived_index_state (source_key, content_hash, memory_id, indexed_at)
             VALUES ('plan:abc', 'deadbeef', 'mem-1', 1000)",
            [],
        )
        .unwrap();
        let (hash, mem, at): (String, String, i64) = conn
            .query_row(
                "SELECT content_hash, memory_id, indexed_at FROM derived_index_state
                 WHERE source_key = 'plan:abc'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (hash.as_str(), mem.as_str(), at),
            ("deadbeef", "mem-1", 1000)
        );
        let index_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='index'
                 AND name='idx_derived_index_state_memory')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(index_exists, "missing idx_derived_index_state_memory");
    }

    #[test]
    fn migration_adds_record_columns_to_legacy_db() {
        // A DB created before the record_class/record_type/superseded_by
        // columns existed must gain them on the next apply_schema (idempotent
        // ALTER TABLE); existing rows get the defaults ('authored', 'none',
        // NULL = live).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                tier TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                data TEXT NOT NULL DEFAULT '{}',
                strength REAL NOT NULL DEFAULT 1.0,
                access_count INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                last_accessed_at INTEGER NOT NULL,
                source_session_ids TEXT NOT NULL DEFAULT '[]',
                embedding BLOB,
                embed_model TEXT,
                embed_dim INTEGER
            );
            INSERT INTO memories (id, tier, title, content, created_at, last_accessed_at)
            VALUES ('old', 'semantic', 't', 'c', 0, 0);",
        )
        .unwrap();
        apply_schema(&conn).unwrap();
        let (class, rtype, superseded): (String, String, Option<String>) = conn
            .query_row(
                "SELECT record_class, record_type, superseded_by FROM memories WHERE id='old'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (class.as_str(), rtype.as_str(), superseded),
            ("authored", "none", None)
        );
        // Re-applying is a no-op (idempotent).
        apply_schema(&conn).unwrap();
    }

    #[test]
    fn migration_adds_fingerprint_columns_to_legacy_db() {
        // A DB created before the embed_model/embed_dim columns existed must
        // gain them on the next apply_schema (idempotent ALTER TABLE). Existing
        // rows get NULL → treated as stale → re-embedded on startup.
        let conn = Connection::open_in_memory().unwrap();
        // Create a legacy memories table WITHOUT the fingerprint columns.
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                tier TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                data TEXT NOT NULL DEFAULT '{}',
                strength REAL NOT NULL DEFAULT 1.0,
                access_count INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                last_accessed_at INTEGER NOT NULL,
                source_session_ids TEXT NOT NULL DEFAULT '[]',
                embedding BLOB
            );",
        )
        .unwrap();
        // apply_schema must add the missing columns without error.
        apply_schema(&conn).unwrap();
        // The columns now exist (INSERT referencing them must parse).
        conn.execute(
            "INSERT INTO memories (id, tier, title, content, created_at, last_accessed_at, embed_model, embed_dim)
             VALUES ('x', 'semantic', 't', 'c', 0, 0, 'all-MiniLM-L6-v2', 384)",
            [],
        )
        .unwrap();
        // Re-applying is a no-op (idempotent).
        apply_schema(&conn).unwrap();
    }

    #[test]
    fn migration_adds_endpoint_column_to_legacy_request_stats() {
        // A DB created before the endpoint dimension existed must gain the
        // request_stats.endpoint column on the next apply_schema (idempotent
        // ALTER TABLE); existing rows keep NULL → they group into one
        // per-model row, exactly the pre-dimension behavior.
        let conn = Connection::open_in_memory().unwrap();
        // Create a legacy request_stats table WITHOUT the endpoint column.
        conn.execute_batch(
            "CREATE TABLE request_stats (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                model TEXT NOT NULL,
                prompt_tokens INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                reasoning_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL,
                ttft_ms INTEGER,
                generation_ms INTEGER,
                created_at INTEGER NOT NULL
            );
            INSERT INTO request_stats (id, session_id, model, prompt_tokens, completion_tokens,
                                       reasoning_tokens, cached_tokens, created_at)
            VALUES ('old', NULL, 'glm-5.3', 10, 20, 0, 0, 0);",
        )
        .unwrap();
        apply_schema(&conn).unwrap();
        // The pre-dimension row survived and reads back with a NULL endpoint.
        let endpoint: Option<String> = conn
            .query_row(
                "SELECT endpoint FROM request_stats WHERE id='old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(endpoint, None);
        // The column now accepts writes (INSERT referencing it must parse).
        conn.execute(
            "INSERT INTO request_stats (id, session_id, model, endpoint, prompt_tokens,
                                        completion_tokens, reasoning_tokens, cached_tokens, created_at)
             VALUES ('new', NULL, 'glm-5.3', 'a100', 1, 2, 0, 0, 0)",
            [],
        )
        .unwrap();
        // Re-applying is a no-op (idempotent).
        apply_schema(&conn).unwrap();
    }

    #[test]
    fn migration_rebuilds_request_stats_for_outcome_purpose_and_nullable_cached() {
        // R21: a DB created before the outcome/purpose tags existed must be
        // rebuilt on the next apply_schema — cached_tokens becomes nullable
        // (an error row records NULL, not 0, so a real reported miss stays
        // distinguishable from "the provider never reported usage") and the
        // outcome/purpose columns appear. Existing rows survive the rebuild.
        let conn = Connection::open_in_memory().unwrap();
        // Legacy shape: NOT NULL cached_tokens, no outcome/purpose.
        conn.execute_batch(
            "CREATE TABLE request_stats (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                model TEXT NOT NULL,
                endpoint TEXT,
                prompt_tokens INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                reasoning_tokens INTEGER NOT NULL,
                cached_tokens INTEGER NOT NULL,
                ttft_ms INTEGER,
                generation_ms INTEGER,
                created_at INTEGER NOT NULL
            );
            INSERT INTO request_stats (id, session_id, model, endpoint, prompt_tokens,
                                       completion_tokens, reasoning_tokens, cached_tokens, created_at)
            VALUES ('old', 's1', 'glm-5.3', 'a100', 10, 20, 0, 0, 0);",
        )
        .unwrap();
        apply_schema(&conn).unwrap();
        // The pre-rebuild row survived the copy.
        let (prompt, cached): (i64, Option<i64>) = conn
            .query_row(
                "SELECT prompt_tokens, cached_tokens FROM request_stats WHERE id='old'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((prompt, cached), (10, Some(0)));
        // cached_tokens is now nullable: an error row records NULL.
        conn.execute(
            "INSERT INTO request_stats (id, session_id, model, prompt_tokens,
                                        completion_tokens, reasoning_tokens, cached_tokens,
                                        created_at, outcome)
             VALUES ('err', 's1', 'glm-5.3', 300000, 0, 0, NULL, 0, 'error')",
            [],
        )
        .unwrap();
        // The tags exist and separate rows (the extraction-separation pin).
        let errors: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM request_stats WHERE outcome = 'error'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(errors, 1);
        // Re-applying is a no-op (idempotent).
        apply_schema(&conn).unwrap();
    }
}
