// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! SQLite-backed graph store for CodeGraph.
//!
//! [`Store`] owns the symbols/edges/refs tables (see [`super::schema`]) and
//! the incremental-update rules that keep them consistent:
//!
//! - [`Store::upsert_file`] replaces one file's symbols + refs + FTS content
//!   lines in a single transaction (delete-then-insert), so re-indexing a
//!   changed file never leaves duplicates or torn content rows.
//! - [`Store::rebuild_edges`] re-resolves **all** call/import edges from the
//!   persisted refs after a batch of file changes. Cross-file edges are
//!   derived data: a symbol moved to another file changes its id, and edges
//!   from files that did *not* change must still re-point to the new id.
//!   Re-deriving from refs (instead of storing edges per file) keeps that
//!   correct without re-parsing unchanged files. Contains edges are stored
//!   per file (their endpoints are both in the same file, so `upsert_file`
//!   can replace them directly).
//! - [`Store::remove_file`] drops one file's rows (and its edges) — used when
//!   a source file is deleted.
//! - [`Store::prune_missing`] drops every file row not in the "still present"
//!   set — the startup sweep that forgets deleted files.
//!
//! Symbol rows are read back out through [`crate::codegraph::extract::Symbol`]
//! (one row ↔ one struct), which the query layer ([`super::query`]) consumes.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{params, Connection};
use serde::Serialize;

use crate::codegraph::extract::{
    lang_family, EdgeKind, EMBEDDABLE_KINDS, RefKind, Symbol, SymbolKind,
};
use crate::codegraph::schema;
use crate::error::Result;

/// Aggregate counts for the status surface (`codegraph_status`, tools, tests).
/// Consumed field-by-field by the IPC status command (never serialized as a
/// whole — review F4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphStats {
    /// Number of indexed source files.
    pub files: usize,
    /// Number of stored symbols (module symbols included).
    pub symbols: usize,
    /// Number of stored edges.
    pub edges: usize,
    /// Unix seconds of the most recent `indexed_at`, if any file is indexed.
    pub last_indexed_at: Option<i64>,
}

/// An edge row as read back from the store (ids + kind). Serializable so the
/// Graph tab's `codegraph_graph` command can hand slices straight to the
/// frontend (`from_id`/`to_id`/`kind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EdgeRow {
    /// Source symbol id.
    pub from_id: String,
    /// Target symbol id.
    pub to_id: String,
    /// Relationship kind.
    pub kind: EdgeKind,
}

/// One FTS content hit: the matched line in a source file. Returned by
/// [`Store::search_content`], the index-backed path for literal searches.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentHit {
    /// Project-relative file path (`/`-separated, the DB key form).
    pub path: String,
    /// 1-based line number.
    pub line: usize,
    /// The line's text (terminator stripped, never `\r`-suffixed).
    pub text: String,
    /// FTS5 bm25 rank (lower = better).
    pub rank: f64,
}

/// One indexed file's worth of data: its symbols (module symbol included) and
/// its unresolved refs (calls + imports), as produced by
/// [`crate::codegraph::extract::extract_file`].
#[derive(Debug, Clone, Default)]
pub struct FileData {
    /// Symbols defined in the file.
    pub symbols: Vec<Symbol>,
    /// Unresolved refs `(from symbol id, name, kind)` where kind is
    /// `"call"`/`"method_call"`/`"import"` (the persisted form of
    /// [`RefKind`](crate::codegraph::extract::RefKind)).
    pub refs: Vec<(String, String, String)>,
    /// Containment pairs `(parent id, child id)` — pre-resolved, stored
    /// directly as `contains` edges.
    pub contains: Vec<(String, String)>,
}

/// One stored file's index metadata — the batch-loaded snapshot the
/// parallel index pass (backlog 5a85e36c) checks the mtime/hash fast paths
/// against: ONE SELECT for the whole tree instead of 3 per file under the
/// store lock.
#[derive(Debug, Clone, Default)]
pub struct FileMeta {
    /// The stored content hash (None when the file has no `cg_files` row).
    pub content_hash: Option<String>,
    /// The stored mtime (None when the file has no `cg_files` row).
    pub mtime: Option<i64>,
    /// Whether the file has a `cg_content_meta` row (the FTS backfill
    /// check — false for files indexed before the content index existed).
    pub has_content: bool,
    /// The extractor version that produced the file's `cg_refs` rows (the
    /// [`schema::EXTRACTOR_VERSION`] backfill check — `None` for files
    /// indexed before the version marker existed). A mismatch forces one
    /// re-parse, exactly like the content backfill: it fires once per old
    /// file, never perpetually.
    pub extract_version: Option<i64>,
}

/// One file's fully-extracted index result, ready to apply — produced by
/// the parallel index workers and applied in waves via
/// [`Store::upsert_files`] (backlog 5a85e36c).
#[derive(Debug, Clone)]
pub struct UpsertEntry {
    /// Project-relative path with `/` separators (the DB key).
    pub path: String,
    /// The content hash (hex string).
    pub content_hash: String,
    /// The on-disk mtime (Unix seconds).
    pub mtime: i64,
    /// The extracted symbols/refs/contains edges (empty for content-only
    /// files).
    pub data: FileData,
    /// The file's source text (the FTS content rows).
    pub content: String,
}

/// One embeddable symbol awaiting a vector (plan d3336a8a step 9): the
/// background pass reads batches of these, embeds `text`, and writes
/// the vectors keyed by `symbol_id`/`text_hash`.
#[derive(Debug, Clone)]
pub struct PendingSymbol {
    /// The symbol id (`{relpath}::{name}::{start_line}`).
    pub symbol_id: String,
    /// The chunk text to embed: `<kind> <name>\n<signature>\n<doc>\n<file>`
    /// — rebuilt from the stored rows exactly the way
    /// [`symbol_chunk_text`] built the stored hash, so the two never
    /// drift.
    pub text: String,
    /// The stored chunk hash — the vector row records it, so a later
    /// text change (hash mismatch) makes the symbol pending again.
    pub text_hash: String,
}

/// One stored symbol vector — the `cg_symbol_vec` row (plan d3336a8a
/// step 9). `model` always names the real embedding model that produced
/// `vec`; a model change re-embeds lazily.
#[derive(Debug, Clone)]
pub struct SymbolVec {
    /// The symbol id (the row key).
    pub symbol_id: String,
    /// The embedding model id that produced the vector.
    pub model: String,
    /// The vector's dimension.
    pub dim: usize,
    /// The vector (raw little-endian f32 in the DB, decoded here).
    pub vec: Vec<f32>,
    /// The chunk text hash the vector was built from.
    pub text_hash: String,
}

/// The graph store. Not `Sync` — wrap in a `Mutex` for shared access (the
/// `CodeGraph` facade in `mod.rs` does exactly that, mirroring `MemoryStore`).
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) the on-disk store at `path` and apply the
    /// schema. Parent directory must exist (the project `.coding/` dir always
    /// does by the time this runs). The DB file is restricted to the current
    /// user (mirroring `keys.toml` / `traces.jsonl` / `memory.db` — review
    /// L6); best-effort, because a restriction failure must never break the
    /// graph (the DB is a rebuildable cache).
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        schema::apply_schema(&conn)?;
        if let Err(e) = crate::config::keys::restrict_permissions(path) {
            eprintln!(
                "codegraph: warning — could not restrict permissions on '{}': {e}",
                path.display()
            );
        }
        Ok(Self { conn })
    }

    /// Open a transient in-memory store (tests, scratch indexing).
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        schema::apply_schema(&conn)?;
        Ok(Self { conn })
    }

    /// The content hash recorded for `path` at last index, if indexed. The
    /// indexer compares this against the current file's hash to decide whether
    /// re-parsing is needed.
    pub fn file_hash(&self, path: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT content_hash FROM cg_files WHERE path = ?1")?;
        let mut rows = stmt.query(params![path])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    /// The mtime recorded for `path` at last index, if indexed. The indexer
    /// uses this as a fast-path: when the on-disk mtime is unchanged the file's
    /// content cannot have changed, so the read+hash can be skipped entirely
    /// (the stored hash is still valid). When the mtime *did* advance, the
    /// authoritative hash comparison still runs — so editors that touch a file
    /// without changing its content do not trigger a re-parse.
    pub fn file_mtime(&self, path: &str) -> Result<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT mtime FROM cg_files WHERE path = ?1")?;
        let mut rows = stmt.query(params![path])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get(0)?),
            None => None,
        })
    }

    /// All indexed file paths with their content hashes — the indexer's
    /// change-detection map.
    pub fn indexed_files(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, content_hash FROM cg_files")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
        let mut out = HashMap::new();
        for row in rows {
            let (p, h) = row?;
            out.insert(p, h);
        }
        Ok(out)
    }

    /// Whether `path` has a content-index meta row. False for files indexed
    /// before the content index existed — the indexer re-parses such files
    /// once to populate their FTS rows (the backfill path for old DBs).
    pub fn has_content(&self, path: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM cg_content_meta WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// All stored files' `(content_hash, mtime, has_content)` in ONE
    /// round-trip — the parallel index pass (backlog 5a85e36c) loads this
    /// snapshot once instead of paying [`Store::file_hash`] +
    /// [`Store::file_mtime`] + [`Store::has_content`] per file under the
    /// lock.
    pub fn stored_file_meta(&self) -> Result<HashMap<String, FileMeta>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.path, f.content_hash, f.mtime, \
             (SELECT COUNT(*) FROM cg_content_meta m WHERE m.path = f.path), \
             (SELECT e.version FROM cg_extract_meta e WHERE e.path = f.path) \
             FROM cg_files f",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                FileMeta {
                    content_hash: r.get(1)?,
                    mtime: r.get(2)?,
                    has_content: r.get::<_, i64>(3)? > 0,
                    extract_version: r.get(4)?,
                },
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (path, meta) = row?;
            out.insert(path, meta);
        }
        Ok(out)
    }

    /// Replace one file's rows with `data` in a single transaction. Deletes
    /// the file's old symbols/refs/edges first (both edge directions: a
    /// re-indexed file may have been the *target* of another file's import).
    /// Call/import edges are re-derived globally by [`Store::rebuild_edges`];
    /// contains edges are re-inserted here from `data.contains`.
    ///
    /// `content` is the file's source text: its non-blank lines replace the
    /// file's FTS rows (`cg_content`, keyed by 1-based line number) and the
    /// `cg_content_meta` hash row is rewritten — all in the same transaction,
    /// so the content index can never tear from the symbol tables.
    pub fn upsert_file(
        &mut self,
        path: &str,
        content_hash: &str,
        mtime: i64,
        data: &FileData,
        content: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        Self::upsert_entry(&tx, path, content_hash, mtime, data, content)?;
        tx.commit()?;
        Ok(())
    }

    /// Apply a whole wave of file upserts in ONE transaction (backlog
    /// 5a85e36c: the parallel index pass pays one store round-trip per
    /// wave, not one transaction per file). Entries are applied in the
    /// given order — the walk's sorted order, so the result is
    /// deterministic regardless of worker interleaving. A failure rolls
    /// the whole wave back (the caller falls back to per-file application
    /// so one bad file cannot lose the wave).
    pub fn upsert_files(&mut self, entries: &[UpsertEntry]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for e in entries {
            Self::upsert_entry(&tx, &e.path, &e.content_hash, e.mtime, &e.data, &e.content)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// The per-file row replacement shared by [`Store::upsert_file`] and
    /// [`Store::upsert_files`]: deletes the file's old symbols/refs/edges
    /// (both edge directions) and re-inserts symbols/refs/contains-edges +
    /// the FTS content rows + the meta hash row, all against the caller's
    /// open transaction. Every statement goes through the connection's
    /// statement cache (`prepare_cached`), so the wave loop pays no
    /// per-entry prepare (backlog 5a85e36c).
    fn upsert_entry(
        tx: &rusqlite::Transaction,
        path: &str,
        content_hash: &str,
        mtime: i64,
        data: &FileData,
        content: &str,
    ) -> Result<()> {
        tx.prepare_cached("DELETE FROM cg_files WHERE path = ?1")?
            .execute(params![path])?;
        // Symbols of this file, to remove their edges on both directions.
        let old_ids: HashSet<String> = {
            let mut stmt = tx.prepare_cached("SELECT id FROM cg_symbols WHERE file = ?1")?;
            let rows = stmt.query_map(params![path], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<HashSet<String>>>()?
        };
        tx.prepare_cached("DELETE FROM cg_symbols WHERE file = ?1")?
            .execute(params![path])?;
        tx.prepare_cached("DELETE FROM cg_refs WHERE file = ?1")?
            .execute(params![path])?;
        tx.prepare_cached("DELETE FROM cg_content WHERE path = ?1")?
            .execute(params![path])?;
        tx.prepare_cached("DELETE FROM cg_content_meta WHERE path = ?1")?
            .execute(params![path])?;
        tx.prepare_cached("DELETE FROM cg_extract_meta WHERE path = ?1")?
            .execute(params![path])?;
        // Drop edges touching any of this file's old symbols (either end) plus
        // every edge sourced in this file (its module is always one of the old
        // symbols, so the symbol-based sweep already covers these; the explicit
        // per-id loop keeps it precise when a stale from_id lingers).
        let mut del =
            tx.prepare_cached("DELETE FROM cg_edges WHERE from_id = ?1 OR to_id = ?1")?;
        for id in &old_ids {
            del.execute(params![id])?;
        }
        drop(del);

        // Symbol text + vectors (plan d3336a8a step 9): the file's old
        // text rows go now (the fresh extract re-inserts them below);
        // the vector rows of symbols that SURVIVE the re-upsert stay —
        // a surviving symbol whose text is unchanged keeps its vector
        // (same id, same text hash), a changed one turns pending (the
        // hash comparison makes the embed pass re-embed it lazily) —
        // only vanished symbols' vectors are pruned.
        {
            let mut del_text =
                tx.prepare_cached("DELETE FROM cg_symbol_text WHERE symbol_id = ?1")?;
            for id in &old_ids {
                del_text.execute(params![id])?;
            }
        }
        {
            let new_ids: HashSet<&str> = data.symbols.iter().map(|s| s.id.as_str()).collect();
            let mut del_vec =
                tx.prepare_cached("DELETE FROM cg_symbol_vec WHERE symbol_id = ?1")?;
            for id in &old_ids {
                if !new_ids.contains(id.as_str()) {
                    del_vec.execute(params![id])?;
                }
            }
        }

        let now = now_secs();
        tx.prepare_cached(
            "INSERT INTO cg_files (path, content_hash, mtime, indexed_at)
             VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![path, content_hash, mtime, now])?;
        tx.prepare_cached("INSERT INTO cg_content_meta (path, content_hash) VALUES (?1, ?2)")?
            .execute(params![path, content_hash])?;
        // The extractor version stamps every upsert (even content-only
        // files), so the version backfill fires once per old file, never
        // perpetually — same shape as the cg_content_meta backfill above.
        tx.prepare_cached(
            "INSERT OR REPLACE INTO cg_extract_meta (path, version) VALUES (?1, ?2)",
        )?
        .execute(params![path, schema::EXTRACTOR_VERSION])?;
        {
            let mut ins_line =
                tx.prepare_cached("INSERT INTO cg_content (path, line, text) VALUES (?1, ?2, ?3)")?;
            // Blank lines carry no tokens and can never match — skip them.
            // `str::lines` strips \r\n terminators, so stored lines are
            // \r-free regardless of the file's line endings.
            for (i, line) in content.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                ins_line.execute(params![path, (i + 1) as i64, line])?;
            }
        }
        {
            let mut ins_sym = tx.prepare_cached(
                "INSERT INTO cg_symbols (id, name, kind, file, start_line, end_line)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for s in &data.symbols {
                ins_sym.execute(params![
                    s.id,
                    s.name,
                    s.kind.as_str(),
                    s.file,
                    s.start_line as i64,
                    s.end_line as i64
                ])?;
            }
        }
        {
            // One text row per real symbol (plan d3336a8a step 9) — the
            // synthetic module has no source anchor and carries none.
            // The text hash keys incremental embedding: the background
            // pass embeds a symbol only when no vector row pairs this
            // hash with the current model.
            let mut ins_text = tx.prepare_cached(
                "INSERT OR REPLACE INTO cg_symbol_text (symbol_id, signature, doc, text_hash)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for s in &data.symbols {
                if s.kind == SymbolKind::Module {
                    continue;
                }
                let chunk =
                    symbol_chunk_text(s.kind.as_str(), &s.name, &s.signature, &s.doc, path);
                ins_text.execute(params![
                    s.id,
                    s.signature,
                    s.doc,
                    crate::codegraph::content_hash(chunk.as_bytes())
                ])?;
            }
        }
        {
            let mut ins_ref = tx.prepare_cached(
                "INSERT INTO cg_refs (file, from_id, name, kind) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (from, name, kind) in &data.refs {
                ins_ref.execute(params![path, from, name, kind])?;
            }
        }
        {
            let mut ins_edge = tx.prepare_cached(
                "INSERT OR IGNORE INTO cg_edges (from_id, to_id, kind) VALUES (?1, ?2, ?3)",
            )?;
            for (parent, child) in &data.contains {
                ins_edge.execute(params![parent, child, EdgeKind::Contains.as_str()])?;
            }
        }
        Ok(())
    }

    /// Delete one file's rows (and its edges) — used when a source file is
    /// removed from the project. Call/import edges are refreshed by
    /// [`Store::rebuild_edges`] afterwards.
    pub fn remove_file(&mut self, path: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM cg_symbols WHERE file = ?1")?;
            let rows = stmt.query_map(params![path], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()?
        };
        tx.execute("DELETE FROM cg_files WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM cg_symbols WHERE file = ?1", params![path])?;
        tx.execute("DELETE FROM cg_refs WHERE file = ?1", params![path])?;
        tx.execute("DELETE FROM cg_content WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM cg_content_meta WHERE path = ?1", params![path])?;
        tx.execute("DELETE FROM cg_extract_meta WHERE path = ?1", params![path])?;
        {
            let mut del = tx.prepare("DELETE FROM cg_edges WHERE from_id = ?1 OR to_id = ?1")?;
            for id in &ids {
                del.execute(params![id])?;
            }
        }
        {
            // Symbol text + vectors die with the symbol (plan d3336a8a
            // step 9).
            let mut del_text = tx.prepare("DELETE FROM cg_symbol_text WHERE symbol_id = ?1")?;
            let mut del_vec = tx.prepare("DELETE FROM cg_symbol_vec WHERE symbol_id = ?1")?;
            for id in &ids {
                del_text.execute(params![id])?;
                del_vec.execute(params![id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete rows for every indexed file **not** in `present`. Returns how
    /// many files were pruned. The startup sweep that forgets deleted files.
    pub fn prune_missing(&mut self, present: &HashSet<String>) -> Result<usize> {
        let indexed: Vec<String> = {
            let mut stmt = self.conn.prepare("SELECT path FROM cg_files")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()?
        };
        let mut pruned = 0;
        for path in indexed {
            if !present.contains(&path) {
                self.remove_file(&path)?;
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Re-derive every call/import edge from the persisted refs, replacing the
    /// current set. Contains edges are untouched (they are stored per file and
    /// already correct). Resolution mirrors
    /// [`crate::codegraph::extract::resolve_edges`]: same-file exact match
    /// wins; otherwise a unique project-wide exact match; otherwise (calls
    /// only) a unique case-insensitive match; otherwise the ref is dropped.
    /// Returns the number of refs that could not be resolved (dropped).
    pub fn rebuild_edges(&mut self) -> Result<usize> {
        // Load all non-module symbols grouped by name.
        let mut by_name: HashMap<String, Vec<Symbol>> = HashMap::new();
        for s in self.all_symbols()? {
            if s.kind != SymbolKind::Module {
                by_name.entry(s.name.clone()).or_default().push(s);
            }
        }
        // Load all refs.
        let refs: Vec<(String, String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT from_id, name, kind FROM cg_refs")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        // The file each from_id belongs to (for same-file preference) — from
        // the symbols table, falling back to the ref's own file column.
        let from_file: HashMap<String, String> = {
            let mut stmt = self.conn.prepare("SELECT id, file FROM cg_symbols")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            rows.collect::<rusqlite::Result<HashMap<_, _>>>()?
        };

        let mut edges: HashSet<(String, String, &'static str)> = HashSet::new();
        let mut unresolved = 0;
        for (from, name, kind) in &refs {
            let file = from_file.get(from).cloned().unwrap_or_default();
            let ref_kind = ref_kind_of(kind);
            let target = resolve(name, &file, ref_kind, &by_name);
            match target {
                Some(t) => {
                    let ek = if matches!(ref_kind, RefKind::Call | RefKind::MethodCall) {
                        EdgeKind::Calls.as_str()
                    } else {
                        EdgeKind::Imports.as_str()
                    };
                    if from != &t.id {
                        edges.insert((from.clone(), t.id.clone(), ek));
                    }
                }
                None => unresolved += 1,
            }
        }

        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM cg_edges WHERE kind != ?1",
            params![EdgeKind::Contains.as_str()],
        )?;
        {
            let mut ins = tx.prepare(
                "INSERT OR IGNORE INTO cg_edges (from_id, to_id, kind) VALUES (?1, ?2, ?3)",
            )?;
            for (from, to, kind) in &edges {
                ins.execute(params![from, to, kind])?;
            }
        }
        tx.commit()?;
        Ok(unresolved)
    }

    /// Aggregate counts for the status surface.
    pub fn stats(&self) -> Result<GraphStats> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cg_files", [], |r| r.get(0))?;
        let symbols: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cg_symbols", [], |r| r.get(0))?;
        let edges: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cg_edges", [], |r| r.get(0))?;
        let last: Option<i64> =
            self.conn
                .query_row("SELECT MAX(indexed_at) FROM cg_files", [], |r| r.get(0))?;
        Ok(GraphStats {
            files: files as usize,
            symbols: symbols as usize,
            edges: edges as usize,
            last_indexed_at: last,
        })
    }

    /// Every stored symbol, in stable (id) order. The query layer loads this
    /// once per query batch.
    pub fn all_symbols(&self) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file, start_line, end_line FROM cg_symbols ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, kind, file, start_line, end_line) = row?;
            let Some(kind) = SymbolKind::from_str_opt(&kind) else {
                continue; // unknown kind from a future version — skip, never fail
            };
            out.push(Symbol {
                id,
                name,
                kind,
                file,
                start_line: start_line as usize,
                end_line: end_line as usize,
                // The signature/doc text lives in cg_symbol_text (plan
                // d3336a8a step 9); the symbol read path stays a light
                // single-table SELECT, so these stay empty here.
                signature: String::new(),
                doc: String::new(),
            });
        }
        Ok(out)
    }

    /// Every stored edge, in stable order.
    pub fn all_edges(&self) -> Result<Vec<EdgeRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT from_id, to_id, kind FROM cg_edges ORDER BY from_id, to_id, kind")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (from_id, to_id, kind) = row?;
            let Some(kind) = EdgeKind::from_str_opt(&kind) else {
                continue;
            };
            out.push(EdgeRow {
                from_id,
                to_id,
                kind,
            });
        }
        Ok(out)
    }

    /// Symbols whose name matches `name` exactly (case-sensitive first) —
    /// the query layer's name resolution reads this via `all_symbols`; this
    /// helper exists for cheap lookups.
    pub fn symbols_named(&self, name: &str) -> Result<Vec<Symbol>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, file, start_line, end_line
             FROM cg_symbols WHERE name = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![name], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, kind, file, start_line, end_line) = row?;
            let Some(kind) = SymbolKind::from_str_opt(&kind) else {
                continue;
            };
            out.push(Symbol {
                id,
                name,
                kind,
                file,
                start_line: start_line as usize,
                end_line: end_line as usize,
                // The signature/doc text lives in cg_symbol_text (plan
                // d3336a8a step 9); the symbol read path stays a light
                // single-table SELECT, so these stay empty here.
                signature: String::new(),
                doc: String::new(),
            });
        }
        Ok(out)
    }

    /// The id of the first symbol named exactly `name` (case-sensitive), or
    /// `None` when no symbol matches — the store-side half of
    /// [`crate::codegraph::CodeGraph::symbol_id`]. Same exact-match contract
    /// as [`Self::symbols_named`], one `LIMIT 1` SELECT.
    pub fn symbol_id_named(&self, name: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM cg_symbols WHERE name = ?1 ORDER BY id LIMIT 1")?;
        let mut rows = stmt.query_map(params![name], |r| r.get::<_, String>(0))?;
        Ok(rows.next().transpose()?)
    }

    /// Resolve ONE symbol by name, loose: exact case-sensitive first, then
    /// case-insensitive equality, then the shortest case-insensitive
    /// substring match — `LIMIT 1`, returning `(id, matched name)`.
    /// `instr` (not LIKE) does the substring test, so identifier
    /// underscores never act as wildcards. Read side of
    /// [`crate::codegraph::CodeGraph::symbol_id_fuzzy`], the
    /// alternation-branch resolver behind the search symbol nudge's
    /// absorption variant (C1): agents write partial names
    /// (`fn .*watcher` → `GraphWatcher`), so exact-only would miss the very
    /// hunts the nudge exists to absorb.
    pub fn symbol_id_fuzzy(&self, name: &str) -> Result<Option<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name FROM cg_symbols
             WHERE name = ?1 OR lower(name) = lower(?1)
                OR instr(lower(name), lower(?1)) > 0
             ORDER BY CASE
                 WHEN name = ?1 THEN 0
                 WHEN lower(name) = lower(?1) THEN 1
                 ELSE 2
             END, length(name), id
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![name], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.next().transpose()?)
    }

    /// The number of indexed symbols whose `file` column equals `path`
    /// (project-relative, forward slashes — the store's key form). One
    /// COUNT SELECT; zero for unknown/unindexed files. Read side of
    /// [`crate::codegraph::CodeGraph::symbol_count_in_file`].
    pub fn symbols_in_file_count(&self, path: &str) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM cg_symbols WHERE file = ?1",
            params![path],
            |r| r.get(0),
        )?)
    }

    /// Phrase-query the FTS content index with a literal string, best matches
    /// first (bm25 rank, ascending). The literal is quoted and internal
    /// quotes doubled, so it parses as ONE FTS5 phrase: with the unicode61
    /// tokenizer, punctuation acts as a separator — the literal
    /// `memory_recall` queries as the phrase "memory recall" and still
    /// matches the identifier. Returns at most `limit` hits.
    pub fn search_content(&self, literal: &str, limit: usize) -> Result<Vec<ContentHit>> {
        let escaped = literal.trim().replace('"', "\"\"");
        if escaped.is_empty() {
            return Ok(Vec::new());
        }
        let query = format!("\"{escaped}\"");
        let mut stmt = self.conn.prepare(
            "SELECT path, line, text, rank FROM cg_content
             WHERE cg_content MATCH ?1 ORDER BY rank LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, f64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (path, line, text, rank) = row?;
            out.push(ContentHit {
                path,
                line: line as usize,
                text,
                rank,
            });
        }
        Ok(out)
    }

    /// `(indexed files, files with a content-index meta row)`. A gap means
    /// the DB predates the content index — the backfill trigger (the caller
    /// at graph-open time runs a reindex when `with_content < files`).
    pub fn content_coverage(&self) -> Result<(usize, usize)> {
        let files: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cg_files", [], |r| r.get(0))?;
        let with_content: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM cg_content_meta", [], |r| r.get(0))?;
        Ok((files as usize, with_content as usize))
    }

    /// Indexed file paths with their as-of-index mtime (unix millis) —
    /// the freshness check for index-served search hits (F10): an on-disk
    /// mtime differing from the stored one means the watcher has not
    /// re-indexed the edit yet and the FTS rows are stale.
    pub fn stored_mtimes(&self) -> Result<HashMap<String, i64>> {
        let mut stmt = self.conn.prepare("SELECT path, mtime FROM cg_files")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
        let mut out = HashMap::new();
        for row in rows {
            let (p, m) = row?;
            out.insert(p, m);
        }
        Ok(out)
    }

    /// The next batch of embeddable symbols whose text has no matching
    /// vector for `model` (plan d3336a8a step 9): no vector row, a
    /// different model's row, or a drifted text hash. The background
    /// pass's work queue — embeddable kinds only, in stable symbol-id
    /// order. One SELECT under one short lock; the caller embeds WITHOUT
    /// holding the write connection.
    pub fn pending_symbols(&self, model: &str, limit: usize) -> Result<Vec<PendingSymbol>> {
        let mut stmt = self.conn.prepare_cached(pending_symbols_sql())?;
        let rows = stmt.query_map(params![model, limit as i64], |r| {
            let kind: String = r.get(1)?;
            let name: String = r.get(2)?;
            let signature: String = r.get(3)?;
            let doc: String = r.get(4)?;
            let file: String = r.get(5)?;
            // Rebuilt exactly the way the upsert built the stored hash
            // (see [`symbol_chunk_text`]), so a matching vector row truly
            // means "this text is already embedded".
            let text = symbol_chunk_text(&kind, &name, &signature, &doc, &file);
            Ok(PendingSymbol {
                symbol_id: r.get(0)?,
                text,
                text_hash: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Store computed symbol vectors (plan d3336a8a step 9): one short
    /// transaction, one insert per row. `model` is whatever produced the
    /// vectors — the pass validated it names the real code model, so the
    /// vec table's `model` column always does. An all-zero vector (a
    /// degraded embedder's zero-vector contract) is skipped rather than
    /// stored, and so is a vector whose length disagrees with `dim`.
    /// Review finding orphanVecRows: a row lands only while its symbol
    /// AND its text row still exist with the SAME text hash the vector
    /// was built from — a reindex deleting the symbol or swapping its
    /// text between the batch read and this write must not (re-)create a
    /// vector row for a vanished symbol. Returns the number of rows
    /// actually written.
    pub fn store_symbol_vecs(
        &mut self,
        model: &str,
        dim: usize,
        batch: &[PendingSymbol],
        vectors: &[Vec<f32>],
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut ins = tx.prepare_cached(
            "INSERT OR REPLACE INTO cg_symbol_vec (symbol_id, model, dim, vec, text_hash)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE EXISTS (
                 SELECT 1 FROM cg_symbols s
                 JOIN cg_symbol_text t ON t.symbol_id = s.id
                 WHERE s.id = ?1 AND t.text_hash = ?5
             )",
        )?;
        let mut stored = 0usize;
        for (sym, vec) in batch.iter().zip(vectors) {
            if vec.iter().all(|f| *f == 0.0) || vec.len() != dim {
                continue;
            }
            let blob: Vec<u8> = vec.iter().flat_map(|f| f.to_le_bytes()).collect();
            stored += ins.execute(params![
                sym.symbol_id,
                model,
                dim as i64,
                blob,
                sym.text_hash
            ])?;
        }
        drop(ins);
        tx.commit()?;
        Ok(stored)
    }

    /// How many symbol vectors are stored — the pass's progress surface
    /// (tests and the step-9 report).
    pub fn symbol_vec_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM cg_symbol_vec", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    /// One symbol's stored vector, if any — the decode path (raw
    /// little-endian f32 bytes → `Vec<f32>`), proven by round-trip tests.
    pub fn symbol_vec(&self, symbol_id: &str) -> Result<Option<SymbolVec>> {
        let mut stmt = self.conn.prepare(
            "SELECT symbol_id, model, dim, vec, text_hash FROM cg_symbol_vec WHERE symbol_id = ?1",
        )?;
        let rows = stmt.query_map(params![symbol_id], |r| {
            let dim: i64 = r.get(2)?;
            let blob: Vec<u8> = r.get(3)?;
            let vec: Vec<f32> = blob
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            Ok(SymbolVec {
                symbol_id: r.get(0)?,
                model: r.get(1)?,
                dim: dim as usize,
                vec,
                text_hash: r.get(4)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out.into_iter().next())
    }

    /// Test seam (plan d3336a8a step 9): stamp every stored vector row
    /// with a different model id — the exact state a future
    /// `CODE_EMBEDDING_MODEL` change leaves behind, proving the pass
    /// re-embeds lazily (the rows' model column no longer matches).
    #[cfg(test)]
    pub fn overwrite_symbol_vec_model_for_test(&mut self, model: &str) -> Result<usize> {
        self.conn
            .execute("UPDATE cg_symbol_vec SET model = ?1", params![model])?;
        Ok(self.conn.changes() as usize)
    }
}

/// Current time as unix seconds (indexed_at bookkeeping).
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The pending-symbols query, built once: the embeddable-kind list is
/// derived from [`crate::codegraph::extract::EMBEDDABLE_KINDS`] — the
/// single source of truth — so the cached statement's SQL and the kind
/// list can never drift (a per-call `format!` would defeat the statement
/// cache).
fn pending_symbols_sql() -> &'static str {
    static SQL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SQL.get_or_init(|| {
        let kinds = EMBEDDABLE_KINDS
            .iter()
            .map(|k| format!("'{}'", k.as_str()))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "SELECT t.symbol_id, s.kind, s.name, t.signature, t.doc, s.file, t.text_hash
             FROM cg_symbol_text t
             JOIN cg_symbols s ON s.id = t.symbol_id
             LEFT JOIN cg_symbol_vec v ON v.symbol_id = t.symbol_id
             WHERE s.kind IN ({kinds})
               AND (v.symbol_id IS NULL OR v.model <> ?1 OR v.text_hash <> t.text_hash)
             ORDER BY t.symbol_id
             LIMIT ?2"
        )
    })
}

/// The embedding chunk text for one symbol (plan d3336a8a step 9):
/// `<kind> <name>\n<signature>\n<doc>\n<file path>` — the text whose hash
/// keys incremental embedding and the text the code-search model
/// embeds. Built identically at upsert time (the stored `text_hash`)
/// and at embed time (the pending batch's `text`), so the two can never
/// drift.
fn symbol_chunk_text(kind: &str, name: &str, signature: &str, doc: &str, file: &str) -> String {
    format!("{kind} {name}\n{signature}\n{doc}\n{file}")
}

/// The [`RefKind`] persisted in a `cg_refs.kind` row (`"call"`,
/// `"method_call"`, `"import"` — see [`FileData::refs`]). Unknown strings
/// read as imports, the historical fallback for rows written by an older
/// extractor (they resolve as before until their file is re-parsed).
fn ref_kind_of(kind: &str) -> RefKind {
    match kind {
        "call" => RefKind::Call,
        "method_call" => RefKind::MethodCall,
        _ => RefKind::Import,
    }
}

/// Resolve one ref name against the symbol index: same-file exact match wins;
/// otherwise a unique project-wide exact match; otherwise (calls only) a
/// unique case-insensitive project-wide match; otherwise `None`. Mirrors
/// [`extract::resolve_edges`](crate::codegraph::extract::resolve_edges) —
/// including the two target gates (plan 0d53d659): a candidate outside the
/// ref's language family, or a non-`Method` symbol answering a `MethodCall`
/// ref, is no candidate at all (a bare name never guesses a target it
/// cannot plausibly be).
fn resolve<'a>(
    name: &str,
    file: &str,
    kind: RefKind,
    by_name: &'a HashMap<String, Vec<Symbol>>,
) -> Option<&'a Symbol> {
    let candidates = by_name.get(name)?;
    let family = lang_family(file);
    let answerable = |symbol: &Symbol| {
        lang_family(&symbol.file) == family
            && (kind != RefKind::MethodCall || symbol.kind == SymbolKind::Method)
    };
    let mut same_file = candidates
        .iter()
        .filter(|s| s.file == file && answerable(s));
    if let (Some(only), None) = (same_file.next(), same_file.next()) {
        return Some(only);
    }
    let project: Vec<&Symbol> = candidates.iter().filter(|s| answerable(s)).collect();
    if project.len() == 1 {
        return project.first().copied();
    }
    if matches!(kind, RefKind::Call | RefKind::MethodCall) {
        let mut ci = project
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(name));
        if let (Some(only), None) = (ci.next(), ci.next()) {
            return Some(only);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a symbol row for tests (id derived from file+name).
    fn sym(file: &str, name: &str, kind: SymbolKind, start: usize, end: usize) -> Symbol {
        Symbol {
            id: format!("{file}::{name}::{start}"),
            name: name.to_string(),
            kind,
            file: file.to_string(),
            start_line: start,
            end_line: end,
            signature: String::new(),
            doc: String::new(),
        }
    }

    /// A minimal two-file graph: `a.rs` defines `caller` (calls `helper`) and
    /// `helper`; `b.rs` defines `user` (calls `helper` too). The module
    /// symbols are included, mirroring `extract_file` output.
    fn file_a() -> FileData {
        FileData {
            symbols: vec![
                Symbol {
                    id: "a.rs::module".to_string(),
                    name: "a.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "a.rs".to_string(),
                    start_line: 1,
                    end_line: 10,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("a.rs", "caller", SymbolKind::Function, 2, 5),
                sym("a.rs", "helper", SymbolKind::Function, 6, 9),
            ],
            refs: vec![(
                "a.rs::caller::2".to_string(),
                "helper".to_string(),
                "call".to_string(),
            )],
            contains: vec![
                ("a.rs::module".to_string(), "a.rs::caller::2".to_string()),
                ("a.rs::module".to_string(), "a.rs::helper::6".to_string()),
            ],
        }
    }

    fn file_b() -> FileData {
        FileData {
            symbols: vec![
                Symbol {
                    id: "b.rs::module".to_string(),
                    name: "b.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "b.rs".to_string(),
                    start_line: 1,
                    end_line: 6,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("b.rs", "user", SymbolKind::Function, 2, 5),
            ],
            refs: vec![(
                "b.rs::user::2".to_string(),
                "helper".to_string(),
                "call".to_string(),
            )],
            contains: vec![("b.rs::module".to_string(), "b.rs::user::2".to_string())],
        }
    }

    #[test]
    fn upsert_then_stats_counts_rows() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        let st = s.stats().unwrap();
        assert_eq!(st.files, 1);
        assert_eq!(st.symbols, 3);
        assert_eq!(st.edges, 2, "contains edges stored directly");
        assert!(st.last_indexed_at.is_some());
    }

    #[test]
    fn re_upsert_replaces_without_duplicates() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        // Re-index same file with a new hash — row counts must not grow.
        s.upsert_file("a.rs", "h2", 200, &file_a(), "").unwrap();
        let st = s.stats().unwrap();
        assert_eq!(st.files, 1);
        assert_eq!(st.symbols, 3);
        assert_eq!(st.edges, 2);
        assert_eq!(s.file_hash("a.rs").unwrap().as_deref(), Some("h2"));
    }

    #[test]
    fn cross_file_call_edge_resolves_after_rebuild() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        s.upsert_file("b.rs", "h1", 100, &file_b(), "").unwrap();
        let unresolved = s.rebuild_edges().unwrap();
        // Same-file ref a.rs::caller→helper resolves; b.rs::user→helper
        // resolves project-wide (unique non-file helper is a.rs's). Both
        // resolve → 0 unresolved.
        assert_eq!(unresolved, 0);
        let edges = s.all_edges().unwrap();
        assert!(
            edges.iter().any(|e| {
                e.from_id == "b.rs::user::2"
                    && e.to_id == "a.rs::helper::6"
                    && e.kind == EdgeKind::Calls
            }),
            "cross-file call edge must point at a.rs's helper"
        );
    }

    #[test]
    fn edges_repoint_when_symbol_moves_file() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        s.upsert_file("b.rs", "h1", 100, &file_b(), "").unwrap();
        s.rebuild_edges().unwrap();
        // Move `helper` from a.rs to a new file c.rs: a.rs keeps only caller.
        let a_without_helper = FileData {
            symbols: vec![
                Symbol {
                    id: "a.rs::module".to_string(),
                    name: "a.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "a.rs".to_string(),
                    start_line: 1,
                    end_line: 10,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("a.rs", "caller", SymbolKind::Function, 2, 5),
            ],
            refs: vec![(
                "a.rs::caller::2".to_string(),
                "helper".to_string(),
                "call".to_string(),
            )],
            contains: vec![("a.rs::module".to_string(), "a.rs::caller::2".to_string())],
        };
        let file_c = FileData {
            symbols: vec![
                Symbol {
                    id: "c.rs::module".to_string(),
                    name: "c.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "c.rs".to_string(),
                    start_line: 1,
                    end_line: 5,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("c.rs", "helper", SymbolKind::Function, 2, 4),
            ],
            refs: vec![],
            contains: vec![("c.rs::module".to_string(), "c.rs::helper::2".to_string())],
        };
        s.upsert_file("a.rs", "h2", 200, &a_without_helper, "")
            .unwrap();
        s.upsert_file("c.rs", "h1", 100, &file_c, "").unwrap();
        s.rebuild_edges().unwrap();
        let edges = s.all_edges().unwrap();
        // b.rs::user→helper must now point at c.rs's helper, not the stale
        // a.rs id (which no longer exists at all).
        assert!(
            edges.iter().any(|e| {
                e.from_id == "b.rs::user::2"
                    && e.to_id == "c.rs::helper::2"
                    && e.kind == EdgeKind::Calls
            }),
            "edge must re-point to the moved symbol"
        );
        assert!(
            !edges.iter().any(|e| e.to_id.starts_with("a.rs::helper")),
            "no edge may reference the deleted a.rs helper id"
        );
    }

    #[test]
    fn remove_file_drops_rows_and_orphan_edges() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        s.upsert_file("b.rs", "h1", 100, &file_b(), "").unwrap();
        s.rebuild_edges().unwrap();
        s.remove_file("a.rs").unwrap();
        s.rebuild_edges().unwrap();
        let st = s.stats().unwrap();
        assert_eq!(st.files, 1);
        assert_eq!(st.symbols, 2, "only b.rs rows remain");
        let edges = s.all_edges().unwrap();
        assert!(
            !edges
                .iter()
                .any(|e| e.to_id.starts_with("a.rs::") || e.from_id.starts_with("a.rs::")),
            "no edge may touch the removed file's symbols"
        );
        // b.rs's ref to helper is now unresolved — counted, not fatal.
        let unresolved = s.rebuild_edges().unwrap();
        assert_eq!(unresolved, 1);
    }

    #[test]
    fn prune_missing_clears_stale_files() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        s.upsert_file("b.rs", "h1", 100, &file_b(), "").unwrap();
        let present: HashSet<String> = ["b.rs".to_string()].into_iter().collect();
        let pruned = s.prune_missing(&present).unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(s.stats().unwrap().files, 1);
        assert!(s.file_hash("a.rs").unwrap().is_none());
        // Pruning nothing is a no-op.
        let present_both: HashSet<String> = ["a.rs".to_string(), "b.rs".to_string()]
            .into_iter()
            .collect();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        assert_eq!(s.prune_missing(&present_both).unwrap(), 0);
    }

    #[test]
    fn on_disk_store_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cg.db");
        {
            let mut s = Store::open(&db).unwrap();
            s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        }
        let s = Store::open(&db).unwrap();
        assert_eq!(s.stats().unwrap().symbols, 3);
        assert_eq!(s.file_hash("a.rs").unwrap().as_deref(), Some("h1"));
    }

    #[test]
    fn indexed_files_returns_hash_map() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "").unwrap();
        s.upsert_file("b.rs", "h2", 100, &file_b(), "").unwrap();
        let m = s.indexed_files().unwrap();
        assert_eq!(m.get("a.rs").map(String::as_str), Some("h1"));
        assert_eq!(m.get("b.rs").map(String::as_str), Some("h2"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn upsert_file_stores_content_lines() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file(
            "a.rs",
            "h1",
            100,
            &file_a(),
            "fn caller() {}\n\nfn helper() {}\n",
        )
        .unwrap();
        let hits = s.search_content("helper", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "a.rs");
        assert_eq!(hits[0].line, 3, "blank line skipped but numbering kept");
        assert_eq!(hits[0].text, "fn helper() {}");
        assert_eq!(s.content_coverage().unwrap(), (1, 1));
    }

    #[test]
    fn re_upsert_replaces_content_rows() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "old_text_here\n")
            .unwrap();
        s.upsert_file("a.rs", "h2", 200, &file_a(), "new_text_here\n")
            .unwrap();
        assert!(s.search_content("old_text_here", 10).unwrap().is_empty());
        assert_eq!(s.search_content("new_text_here", 10).unwrap().len(), 1);
        assert_eq!(
            s.content_coverage().unwrap(),
            (1, 1),
            "no duplicate meta row"
        );
    }

    #[test]
    fn remove_file_drops_content_rows() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "fn helper() {}\n")
            .unwrap();
        s.remove_file("a.rs").unwrap();
        assert!(s.search_content("helper", 10).unwrap().is_empty());
        assert_eq!(s.content_coverage().unwrap(), (0, 0));
    }

    #[test]
    fn search_content_finds_snake_case_identifier() {
        // unicode61 treats `_` as a separator: the literal "memory_recall"
        // becomes the phrase "memory recall" and still matches. Pin this —
        // the search tool's index path depends on it.
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "let memory_recall = 1;\n")
            .unwrap();
        let hits = s.search_content("memory_recall", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn search_content_empty_and_quote_safe() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "let s = \"quoted\";\n")
            .unwrap();
        assert!(s.search_content("   ", 10).unwrap().is_empty());
        // An embedded double quote must not break the FTS query parse.
        assert_eq!(s.search_content("\"quoted\"", 10).unwrap().len(), 1);
    }

    #[test]
    fn method_call_refs_never_resolve_across_languages_or_to_free_fns() {
        // The persisted-refs half of plan 0d53d659: `cg_refs` rows carry the
        // method-call shape as the kind string `method_call`, and
        // `rebuild_edges` applies the resolver's two gates — same language
        // family, and a method-call ref only ever to a Method-kind symbol
        // (never a free fn).
        let mut s = Store::open_in_memory().unwrap();
        let rust_join = FileData {
            symbols: vec![
                Symbol {
                    id: "m.rs::module".to_string(),
                    name: "m.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "m.rs".to_string(),
                    start_line: 1,
                    end_line: 5,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("m.rs", "join", SymbolKind::Function, 683, 685),
            ],
            refs: vec![],
            contains: vec![("m.rs::module".to_string(), "m.rs::join::683".to_string())],
        };
        let python_dump = FileData {
            symbols: vec![
                Symbol {
                    id: "u.py::module".to_string(),
                    name: "u.py".to_string(),
                    kind: SymbolKind::Module,
                    file: "u.py".to_string(),
                    start_line: 1,
                    end_line: 6,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("u.py", "dump", SymbolKind::Function, 2, 4),
            ],
            refs: vec![(
                "u.py::dump::2".to_string(),
                "join".to_string(),
                "method_call".to_string(),
            )],
            contains: vec![("u.py::module".to_string(), "u.py::dump::2".to_string())],
        };
        s.upsert_file("m.rs", "h1", 100, &rust_join, "").unwrap();
        s.upsert_file("u.py", "h2", 100, &python_dump, "").unwrap();
        let unresolved = s.rebuild_edges().unwrap();
        assert_eq!(unresolved, 1, "the cross-language method call stays unresolved");
        let edges = s.all_edges().unwrap();
        assert!(
            !edges.iter().any(|e| e.to_id == "m.rs::join::683" && e.kind != EdgeKind::Contains),
            "no resolved-call/import edge may target the Rust free fn"
        );
    }

    #[test]
    fn method_call_refs_resolve_to_same_language_methods() {
        // The positive control: a `method_call` ref resolves to a
        // Method-kind symbol in the same language family, as a Calls edge.
        let mut s = Store::open_in_memory().unwrap();
        let ts_model = FileData {
            symbols: vec![
                Symbol {
                    id: "model.ts::module".to_string(),
                    name: "model.ts".to_string(),
                    kind: SymbolKind::Module,
                    file: "model.ts".to_string(),
                    start_line: 1,
                    end_line: 5,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("model.ts", "fetch", SymbolKind::Method, 2, 4),
            ],
            refs: vec![],
            contains: vec![(
                "model.ts::module".to_string(),
                "model.ts::fetch::2".to_string(),
            )],
        };
        let ts_view = FileData {
            symbols: vec![
                Symbol {
                    id: "view.ts::module".to_string(),
                    name: "view.ts".to_string(),
                    kind: SymbolKind::Module,
                    file: "view.ts".to_string(),
                    start_line: 1,
                    end_line: 4,
                    signature: String::new(),
                    doc: String::new(),
                },
                sym("view.ts", "render", SymbolKind::Function, 2, 3),
            ],
            refs: vec![(
                "view.ts::render::2".to_string(),
                "fetch".to_string(),
                "method_call".to_string(),
            )],
            contains: vec![(
                "view.ts::module".to_string(),
                "view.ts::render::2".to_string(),
            )],
        };
        s.upsert_file("model.ts", "h1", 100, &ts_model, "").unwrap();
        s.upsert_file("view.ts", "h2", 100, &ts_view, "").unwrap();
        assert_eq!(s.rebuild_edges().unwrap(), 0);
        let edges = s.all_edges().unwrap();
        assert!(
            edges.iter().any(|e| {
                e.from_id == "view.ts::render::2"
                    && e.to_id == "model.ts::fetch::2"
                    && e.kind == EdgeKind::Calls
            }),
            "the method call resolves to the same-language Method"
        );
    }

    // ============ Symbol text + vectors (plan d3336a8a step 9) ============

    /// A file with one real symbol carrying signature + doc, plus the
    /// synthetic module row (which stays text-less, as the extractor
    /// produces it).
    fn file_with_text(doc: &str) -> FileData {
        let mut helper = sym("t.rs", "helper", SymbolKind::Function, 3, 6);
        helper.signature = "pub fn helper(x: u32) -> u32".to_string();
        helper.doc = doc.to_string();
        FileData {
            symbols: vec![
                Symbol {
                    id: "t.rs::module".to_string(),
                    name: "t.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "t.rs".to_string(),
                    start_line: 1,
                    end_line: 6,
                    signature: String::new(),
                    doc: String::new(),
                },
                helper,
            ],
            refs: vec![],
            contains: vec![],
        }
    }

    fn text_row_count(s: &Store) -> i64 {
        s.conn
            .query_row("SELECT COUNT(*) FROM cg_symbol_text", [], |r| r.get(0))
            .unwrap()
    }

    /// The step-9 text/vector lifecycle at the store level: upserts write
    /// text rows (real symbols only), a stored vector makes the symbol
    /// non-pending, an UNCHANGED re-upsert keeps it non-pending (the
    /// step's "unchanged symbol is not re-embedded"), a changed doc makes
    /// it pending again (hash drift), and a vanished symbol's text and
    /// vector rows are pruned while nothing else loses its vector.
    #[test]
    fn symbol_text_and_vecs_follow_upserts_and_prune() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("t.rs", "h1", 100, &file_with_text("Adds one."), "src").unwrap();
        // Real symbols only — the module row carries no text row.
        assert_eq!(text_row_count(&s), 1);

        let pending = s
            .pending_symbols("jina-embeddings-v2-base-code", 100)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].symbol_id, "t.rs::helper::3");
        let expected_chunk = "function helper\npub fn helper(x: u32) -> u32\nAdds one.\nt.rs";
        assert_eq!(pending[0].text, expected_chunk);
        assert_eq!(
            pending[0].text_hash,
            crate::codegraph::content_hash(expected_chunk.as_bytes())
        );
        let expected_hash = pending[0].text_hash.clone();

        // Embed it (the store takes the caller's word for the model — the
        // pass validated it) and verify the round-trip decode.
        let vectors = vec![vec![0.25, -0.5, 1.0]];
        assert_eq!(
            s.store_symbol_vecs("jina-embeddings-v2-base-code", 3, &pending, &vectors)
                .unwrap(),
            1
        );
        assert!(
            s.pending_symbols("jina-embeddings-v2-base-code", 100)
                .unwrap()
                .is_empty()
        );
        let row = s.symbol_vec("t.rs::helper::3").unwrap().unwrap();
        assert_eq!(row.model, "jina-embeddings-v2-base-code");
        assert_eq!(row.dim, 3);
        assert_eq!(row.vec, vec![0.25, -0.5, 1.0]);
        assert_eq!(row.text_hash, expected_hash);

        // UNCHANGED re-upsert: same text → same hash → the vector stays
        // valid and the symbol is NOT re-embedded.
        s.upsert_file("t.rs", "h1", 100, &file_with_text("Adds one."), "src").unwrap();
        assert!(
            s.pending_symbols("jina-embeddings-v2-base-code", 100)
                .unwrap()
                .is_empty()
        );
        assert_eq!(s.symbol_vec_count().unwrap(), 1);

        // A DIFFERENT model re-embeds everything it owns (lazily: the row
        // belongs to the old model, so the symbol is pending for the new
        // one).
        assert_eq!(
            s.pending_symbols("other-model", 100).unwrap().len(),
            1,
            "a different model must find the symbol pending"
        );

        // CHANGED doc: the hash drifts, the symbol is pending again — even
        // for the same model.
        s.upsert_file("t.rs", "h2", 100, &file_with_text("Adds TWO."), "src").unwrap();
        assert_eq!(
            s.pending_symbols("jina-embeddings-v2-base-code", 100)
                .unwrap()
                .len(),
            1
        );

        // VANISHED symbol (the file no longer defines it): its text and
        // vector rows are pruned.
        s.upsert_file("t.rs", "h3", 100, &FileData::default(), "src").unwrap();
        assert_eq!(text_row_count(&s), 0);
        assert_eq!(s.symbol_vec_count().unwrap(), 0);
    }

    /// A degraded embedder's zero vectors never persist, a vector whose
    /// length disagrees with the declared dim is skipped as malformed,
    /// and skipped symbols stay pending (a later pass retries them).
    #[test]
    fn store_symbol_vecs_skips_zero_and_mismatched_vectors() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "fn caller() {}").unwrap();
        let pending = s.pending_symbols("m", 100).unwrap();
        assert_eq!(pending.len(), 2);
        // First vector all zeros (a degraded embedder's answer), second
        // disagrees with the declared dim — neither persists.
        let bad = vec![vec![0.0, 0.0, 0.0], vec![0.5, 0.5]];
        assert_eq!(s.store_symbol_vecs("m", 3, &pending, &bad).unwrap(), 0);
        assert_eq!(s.symbol_vec_count().unwrap(), 0);
        assert_eq!(
            s.pending_symbols("m", 100).unwrap().len(),
            2,
            "skipped symbols stay pending for a later pass"
        );
        let good = vec![vec![0.25, 0.5, 1.0], vec![0.25, 0.5, 1.0]];
        assert_eq!(s.store_symbol_vecs("m", 3, &pending, &good).unwrap(), 2);
    }

    /// Review finding orphanVecRows: a symbol deleted between the batch
    /// read and the write gets no vector row — the insert is guarded by
    /// the symbol and its text row still existing with the same text
    /// hash the vector was built from.
    #[test]
    fn store_symbol_vecs_skips_symbols_deleted_mid_batch() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "fn caller() {}").unwrap();
        let pending = s.pending_symbols("m", 100).unwrap();
        assert_eq!(pending.len(), 2);
        // The reindex that deleted the helper lands between the read
        // and the write: re-upsert the file without it, then store the
        // STALE batch.
        let mut sans_helper = file_a();
        sans_helper.symbols.retain(|sym| sym.name != "helper");
        s.upsert_file("a.rs", "h2", 101, &sans_helper, "fn caller() {}").unwrap();
        let vectors: Vec<Vec<f32>> = pending.iter().map(|_| vec![0.25, 0.5]).collect();
        assert_eq!(
            s.store_symbol_vecs("m", 2, &pending, &vectors).unwrap(),
            1,
            "only the surviving symbol's vector is stored"
        );
        let helper_id = pending
            .iter()
            .find(|p| p.symbol_id.contains("helper"))
            .unwrap()
            .symbol_id
            .clone();
        assert!(
            s.symbol_vec(&helper_id).unwrap().is_none(),
            "the vanished symbol gets no vector row"
        );
        let caller_id = pending
            .iter()
            .find(|p| p.symbol_id.contains("caller"))
            .unwrap()
            .symbol_id
            .clone();
        assert!(s.symbol_vec(&caller_id).unwrap().is_some());
        // The survivor's row is valid (its text hash is unchanged), so
        // the queue drains.
        assert!(s.pending_symbols("m", 100).unwrap().is_empty());
    }

    /// Two real symbols carrying signature + doc (plus the module row), so
    /// an edit to ONE symbol's text is observable next to an unchanged
    /// sibling (review finding textDriftUntested).
    fn file_two_texts(helper_doc: &str) -> FileData {
        let mut helper = sym("t.rs", "helper", SymbolKind::Function, 3, 6);
        helper.signature = "pub fn helper() -> u32".to_string();
        helper.doc = helper_doc.to_string();
        let mut other = sym("t.rs", "other", SymbolKind::Function, 9, 11);
        other.signature = "pub fn other()".to_string();
        other.doc = "Unchanged doc.".to_string();
        FileData {
            symbols: vec![
                Symbol {
                    id: "t.rs::module".to_string(),
                    name: "t.rs".to_string(),
                    kind: SymbolKind::Module,
                    file: "t.rs".to_string(),
                    start_line: 1,
                    end_line: 11,
                    signature: String::new(),
                    doc: String::new(),
                },
                helper,
                other,
            ],
            refs: vec![],
            contains: vec![],
        }
    }

    /// Review finding textDriftUntested: editing one symbol's doc drifts
    /// its text hash — the pass re-embeds exactly that symbol while the
    /// unchanged sibling keeps its vector untouched.
    #[test]
    fn changed_symbol_is_reembedded() {
        let mut s = Store::open_in_memory().unwrap();
        let model = "jina-embeddings-v2-base-code";
        s.upsert_file("t.rs", "h1", 100, &file_two_texts("Adds one."), "src").unwrap();
        // First pass: both symbols pending, both stored.
        let first = s.pending_symbols(model, 100).unwrap();
        assert_eq!(first.len(), 2);
        let vectors: Vec<Vec<f32>> = first.iter().map(|_| vec![0.25, 0.5, 0.75]).collect();
        assert_eq!(s.store_symbol_vecs(model, 3, &first, &vectors).unwrap(), 2);
        let helper_before = s.symbol_vec("t.rs::helper::3").unwrap().unwrap();
        let other_before = s.symbol_vec("t.rs::other::9").unwrap().unwrap();
        // Edit helper's doc: same id (the lines did not move), new text
        // hash.
        s.upsert_file("t.rs", "h2", 100, &file_two_texts("Adds TWO."), "src").unwrap();
        let pending = s.pending_symbols(model, 100).unwrap();
        assert_eq!(pending.len(), 1, "only the edited symbol is pending");
        assert_eq!(pending[0].symbol_id, "t.rs::helper::3");
        assert!(pending[0].text.contains("Adds TWO."));
        // Re-embed: helper's row is replaced (new vector + new hash),
        // other's is untouched.
        let vectors: Vec<Vec<f32>> = pending.iter().map(|_| vec![0.9, 0.5, 0.25]).collect();
        assert_eq!(s.store_symbol_vecs(model, 3, &pending, &vectors).unwrap(), 1);
        assert_eq!(s.symbol_vec_count().unwrap(), 2);
        let helper_after = s.symbol_vec("t.rs::helper::3").unwrap().unwrap();
        assert_ne!(helper_after.text_hash, helper_before.text_hash);
        assert_eq!(helper_after.vec, vec![0.9, 0.5, 0.25]);
        let other_after = s.symbol_vec("t.rs::other::9").unwrap().unwrap();
        assert_eq!(other_after.text_hash, other_before.text_hash);
        assert_eq!(other_after.vec, other_before.vec);
        // And the queue is drained.
        assert!(s.pending_symbols(model, 100).unwrap().is_empty());
    }

    /// remove_file drops the file's symbol text and vector rows along with
    /// its symbols.
    #[test]
    fn remove_file_drops_symbol_text_and_vecs() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_file("a.rs", "h1", 100, &file_a(), "fn caller() {}").unwrap();
        let pending = s.pending_symbols("m", 100).unwrap();
        let vectors: Vec<Vec<f32>> = pending.iter().map(|_| vec![1.0, 2.0]).collect();
        s.store_symbol_vecs("m", 2, &pending, &vectors).unwrap();
        assert_eq!(s.symbol_vec_count().unwrap(), 2);
        s.remove_file("a.rs").unwrap();
        assert_eq!(s.symbol_vec_count().unwrap(), 0);
        assert_eq!(text_row_count(&s), 0);
    }

    /// A DB from before step 9 (its two tables absent, rows stamped with
    /// the older extractor version) migrates in place: `Store::open`
    /// re-applies the additive schema — no migration machinery, no
    /// delete-and-rebuild — the old rows survive, and the next upsert
    /// fills the new text rows. (The index-level half — the version bump
    /// making index() re-parse v1-stamped files — lives in
    /// codegraph::mod's migration test.)
    #[test]
    fn migration_from_old_schema_adds_the_step9_tables() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("old.db");
        {
            let mut s = Store::open(&db).unwrap();
            s.upsert_file("a.rs", "h1", 100, &file_a(), "fn caller() {}").unwrap();
        }
        {
            // The pre-step-9 database: same tables minus the two new ones,
            // files stamped with the v1 extractor version.
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "DROP TABLE cg_symbol_text;
                 DROP TABLE cg_symbol_vec;
                 INSERT OR REPLACE INTO cg_extract_meta (path, version) VALUES ('a.rs', 1);",
            )
            .unwrap();
        }
        let mut s = Store::open(&db).unwrap();
        // The old rows survived the in-place migration.
        assert_eq!(s.all_symbols().unwrap().len(), 3);
        assert_eq!(
            s.stored_file_meta().unwrap().get("a.rs").unwrap().extract_version,
            Some(1)
        );
        // The tables exist and work; the next (re-parse) upsert fills the
        // text rows, so the pass sees the symbols pending.
        assert!(s.pending_symbols("m", 100).unwrap().is_empty());
        s.upsert_file("a.rs", "h1", 100, &file_a(), "fn caller() {}").unwrap();
        assert_eq!(text_row_count(&s), 2);
        assert_eq!(s.pending_symbols("m", 100).unwrap().len(), 2);
        assert_eq!(
            s.stored_file_meta().unwrap().get("a.rs").unwrap().extract_version,
            Some(schema::EXTRACTOR_VERSION),
            "the re-upsert stamps the new extractor version"
        );
    }
}
