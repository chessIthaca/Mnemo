// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! A rebuildable full-text index over past session transcripts.
//!
//! The memory store records what the project learned; this index records what
//! actually happened — the raw worker and lead conversations behind a record,
//! a plan or a merge. It lives in its own SQLite file because it is a
//! per-machine cache: it can always be rebuilt from the transcript JSON, so it
//! never touches the memory schema, its write lock or the recall eval.
//!
//! One row per message. The worker transcript files
//! (`<state dir>/transcripts/<worker id>.json`) and the lead's `lead.json`
//! both hold a `messages` array; the index accepts either shape and skips
//! system messages, empty text and reasoning text. Tool calls are searchable
//! as one `-> <name> <arguments>` line per call, and every stored text has
//! credentials redacted before it lands in the index.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::tool::agent::output_compactor::redact_secrets;

/// The schema version stored in the index file's `PRAGMA user_version`.
/// A file carrying any other version is rebuilt from the transcripts.
const SCHEMA_VERSION: i64 = 1;

/// How long a writer waits for the index lock before failing.
const BUSY_TIMEOUT_MS: u64 = 5_000;

/// The most characters one stored message keeps; longer messages keep the
/// first [`KEEP_HEAD`] and the last [`KEEP_TAIL`] characters.
const MAX_MESSAGE_CHARS: usize = 8_000;
/// How many leading characters a capped message keeps.
const KEEP_HEAD: usize = 6_000;
/// How many trailing characters a capped message keeps.
const KEEP_TAIL: usize = 2_000;
/// The marker spliced between the kept head and tail of a capped message.
const CUT_MARKER: &str = " ... [cut] ... ";
/// How many characters of a tool call's arguments are searchable.
const TOOL_ARGUMENT_CHARS: usize = 500;

/// The schema of the transcript index file.
const SCHEMA: &str = r#"
PRAGMA user_version = 1;

CREATE TABLE IF NOT EXISTS sources (
    source   TEXT PRIMARY KEY,
    len      INTEGER NOT NULL,
    mtime_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS chunks (
    id     INTEGER PRIMARY KEY,
    source TEXT NOT NULL,
    seq    INTEGER NOT NULL,
    role   TEXT NOT NULL,
    text   TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_chunks_source_seq ON chunks(source, seq);

CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    text,
    content='chunks',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS chunks_fts_ai
AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER IF NOT EXISTS chunks_fts_ad
AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
"#;

/// One transcript file the index reads: a worker transcript file or the
/// lead's own `lead.json`.
#[derive(Debug, Clone)]
pub struct TranscriptSource {
    /// The source name: the worker id (the transcript's file stem) or `lead`.
    pub name: String,
    /// The JSON file to read.
    pub path: PathBuf,
}

/// One indexed message, as a search hit or a direct lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptHit {
    /// The source the message belongs to.
    pub source: String,
    /// The message's zero-based index in the transcript file.
    pub seq: usize,
    /// The message role: `user`, `assistant` or `tool`.
    pub role: String,
    /// The matched snippet (search) or the stored text (message lookup).
    pub text: String,
}

/// What one [`TranscriptIndex::sync`] call changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// How many message rows were indexed from new or changed sources.
    pub indexed: usize,
    /// How many message rows were dropped because their source disappeared.
    pub removed: usize,
    /// The names of the sources that could not be read or parsed. Their old
    /// rows stay in the index.
    pub unreadable: Vec<String>,
}

/// The index over the session transcripts. It is a cache: an unreadable or
/// outdated file is thrown away and built again on [`open`](Self::open).
pub struct TranscriptIndex {
    conn: Connection,
}

impl TranscriptIndex {
    /// Open the index at `db_path`, rebuilding the file when it is corrupt or
    /// carries a different schema version.
    pub fn open(db_path: &Path) -> Result<Self, String> {
        if db_path.exists() {
            if let Ok(index) = Self::open_existing(db_path) {
                return Ok(index);
            }
            // This file is only a cache: an unreadable or outdated index is
            // deleted and built again from the transcripts.
            remove_file_if_present(db_path)?;
            remove_file_if_present(&with_suffix(db_path, "-wal"))?;
            remove_file_if_present(&with_suffix(db_path, "-shm"))?;
        }
        Self::create(db_path)
    }

    /// Open an existing index file, failing when it is not a valid index of
    /// the current schema version.
    fn open_existing(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path).map_err(|err| err.to_string())?;
        conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(|err| err.to_string())?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|err| err.to_string())?;
        if version != SCHEMA_VERSION {
            return Err(format!(
                "index schema version {version}, expected {SCHEMA_VERSION}"
            ));
        }
        Ok(Self { conn })
    }

    /// Create (or recreate) the index file and its schema.
    fn create(db_path: &Path) -> Result<Self, String> {
        let conn = Connection::open(db_path)
            .map_err(|err| format!("open {}: {err}", db_path.display()))?;
        conn.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))
            .map_err(|err| err.to_string())?;
        conn.execute_batch(SCHEMA)
            .map_err(|err| format!("create {}: {err}", db_path.display()))?;
        Ok(Self { conn })
    }

    /// Re-read every source whose file length or modification time changed,
    /// and drop the rows of sources that are no longer passed in. A source
    /// that cannot be read or parsed keeps its old rows and is named in
    /// [`SyncReport::unreadable`]; it never fails the whole sync.
    pub fn sync(&mut self, sources: &[TranscriptSource]) -> SyncReport {
        let mut report = SyncReport::default();
        let known = match self.known_sources() {
            Ok(known) => known,
            // The index file itself is broken. `open` validates it, so this
            // is a rare on-disk corruption; the next `open` rebuilds it.
            Err(_) => return report,
        };
        let wanted: std::collections::HashSet<&str> =
            sources.iter().map(|source| source.name.as_str()).collect();
        for (name, _, _) in &known {
            if wanted.contains(name.as_str()) {
                continue;
            }
            match self.drop_source(name) {
                Ok(removed) => report.removed += removed,
                Err(_) => report.unreadable.push(name.clone()),
            }
        }
        for source in sources {
            let metadata = match std::fs::metadata(&source.path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    report.unreadable.push(source.name.clone());
                    continue;
                }
            };
            let len = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
            let mtime_ms = mtime_ms(&metadata);
            let unchanged = known.iter().any(|(name, old_len, old_mtime_ms)| {
                name == &source.name && *old_len == len && *old_mtime_ms == mtime_ms
            });
            if unchanged {
                continue;
            }
            let text = match std::fs::read_to_string(&source.path) {
                Ok(text) => text,
                Err(_) => {
                    report.unreadable.push(source.name.clone());
                    continue;
                }
            };
            let messages = match parse_transcript(&text) {
                Ok(messages) => messages,
                Err(_) => {
                    report.unreadable.push(source.name.clone());
                    continue;
                }
            };
            match self.replace_source(&source.name, len, mtime_ms, &messages) {
                Ok(indexed) => report.indexed += indexed,
                Err(_) => report.unreadable.push(source.name.clone()),
            }
        }
        report
    }

    /// Search the indexed messages. `query` is a set of words matched as an
    /// AND first and as an OR only when nothing matches all of them; the
    /// optional `source` narrows the search to one transcript. `limit` is
    /// clamped to 1..=30. A query with no usable terms returns no hits.
    pub fn search(
        &self,
        query: &str,
        source: Option<&str>,
        limit: usize,
    ) -> Result<Vec<TranscriptHit>, String> {
        let terms = fts_terms(query);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 30) as i64;
        let mut hits = self.search_terms(&terms.join(" AND "), source, limit)?;
        if hits.is_empty() {
            hits = self.search_terms(&terms.join(" OR "), source, limit)?;
        }
        Ok(hits)
    }

    /// Read one indexed message by its source and zero-based sequence number.
    /// The returned text is the stored text, not a snippet.
    pub fn message(&self, source: &str, seq: usize) -> Result<Option<TranscriptHit>, String> {
        let seq = i64::try_from(seq).map_err(|_| "seq is too large".to_string())?;
        self.conn
            .query_row(
                "SELECT source, seq, role, text FROM chunks WHERE source = ?1 AND seq = ?2",
                params![source, seq],
                |row| {
                    Ok(TranscriptHit {
                        source: row.get(0)?,
                        seq: usize::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
                        role: row.get(2)?,
                        text: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|err| err.to_string())
    }

    /// The `(source, len, mtime_ms)` rows currently indexed.
    fn known_sources(&self) -> rusqlite::Result<Vec<(String, i64, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source, len, mtime_ms FROM sources")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Replace every row of one source with the freshly parsed messages, in
    /// one transaction. Returns how many message rows were inserted.
    fn replace_source(
        &mut self,
        name: &str,
        len: i64,
        mtime_ms: i64,
        messages: &[IndexedMessage],
    ) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM chunks WHERE source = ?1", params![name])?;
        {
            let mut insert =
                tx.prepare("INSERT INTO chunks (source, seq, role, text) VALUES (?1, ?2, ?3, ?4)")?;
            for message in messages {
                insert.execute(params![
                    name,
                    i64::try_from(message.seq).unwrap_or(i64::MAX),
                    message.role,
                    message.text
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO sources (source, len, mtime_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT(source) DO UPDATE SET len = excluded.len, mtime_ms = excluded.mtime_ms",
            params![name, len, mtime_ms],
        )?;
        tx.commit()?;
        Ok(messages.len())
    }

    /// Drop one source and its rows. Returns how many message rows were
    /// removed.
    fn drop_source(&mut self, name: &str) -> rusqlite::Result<usize> {
        let tx = self.conn.transaction()?;
        let removed = tx.execute("DELETE FROM chunks WHERE source = ?1", params![name])?;
        tx.execute("DELETE FROM sources WHERE source = ?1", params![name])?;
        tx.commit()?;
        Ok(removed)
    }

    /// Run one FTS query, optionally narrowed to one source.
    fn search_terms(
        &self,
        terms: &str,
        source: Option<&str>,
        limit: i64,
    ) -> Result<Vec<TranscriptHit>, String> {
        let sql = match source {
            Some(_) => {
                "SELECT c.source, c.seq, c.role, \
                        snippet(chunks_fts, 0, '[', ']', '...', 24) \
                 FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid \
                 WHERE chunks_fts MATCH ?1 AND c.source = ?3 \
                 ORDER BY rank LIMIT ?2"
            }
            None => {
                "SELECT c.source, c.seq, c.role, \
                        snippet(chunks_fts, 0, '[', ']', '...', 24) \
                 FROM chunks_fts JOIN chunks c ON c.id = chunks_fts.rowid \
                 WHERE chunks_fts MATCH ?1 \
                 ORDER BY rank LIMIT ?2"
            }
        };
        let mut stmt = self.conn.prepare(sql).map_err(|err| err.to_string())?;
        let to_hit = |row: &rusqlite::Row<'_>| {
            Ok(TranscriptHit {
                source: row.get(0)?,
                seq: usize::try_from(row.get::<_, i64>(1)?).unwrap_or_default(),
                role: row.get(2)?,
                text: row.get(3)?,
            })
        };
        let rows = match source {
            Some(source) => stmt.query_map(params![terms, limit, source], to_hit),
            None => stmt.query_map(params![terms, limit], to_hit),
        }
        .map_err(|err| err.to_string())?;
        let mut hits = Vec::new();
        for row in rows {
            hits.push(row.map_err(|err| err.to_string())?);
        }
        Ok(hits)
    }
}

/// One message as it goes into the index.
struct IndexedMessage {
    /// The message's zero-based index in the transcript file.
    seq: usize,
    /// The normalized role: `user`, `assistant` or `tool`.
    role: String,
    /// The redacted, capped searchable text.
    text: String,
}

/// Parse a transcript file into index rows.
///
/// Accepts both stored shapes: `{"messages": [...]}` (worker transcript
/// files) and `{"transcript": {"messages": [...]}}` (`lead.json`). System
/// messages and messages without searchable text are skipped.
fn parse_transcript(text: &str) -> Result<Vec<IndexedMessage>, String> {
    let value: Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
    let messages = value
        .get("messages")
        .or_else(|| {
            value
                .get("transcript")
                .and_then(|transcript| transcript.get("messages"))
        })
        .and_then(Value::as_array)
        .ok_or_else(|| "transcript has no messages array".to_string())?;
    let mut indexed = Vec::new();
    for (seq, message) in messages.iter().enumerate() {
        let Some(role) = message.get("role").and_then(Value::as_str) else {
            continue;
        };
        let role = role.to_ascii_lowercase();
        if !matches!(role.as_str(), "user" | "assistant" | "tool") {
            continue;
        }
        let mut text = content_text(message.get("content"));
        if role == "assistant" {
            if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let name = call.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let arguments = match call.get("arguments") {
                        Some(Value::String(arguments)) => arguments.clone(),
                        Some(arguments) => arguments.to_string(),
                        None => String::new(),
                    };
                    let arguments: String = arguments.chars().take(TOOL_ARGUMENT_CHARS).collect();
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("-> ");
                    text.push_str(name);
                    text.push(' ');
                    text.push_str(&arguments);
                }
            }
        }
        let text = cap_message(&redact_secrets(&text));
        if text.trim().is_empty() {
            continue;
        }
        indexed.push(IndexedMessage { seq, role, text });
    }
    Ok(indexed)
}

/// The text of a message's `content` field: a plain string, or the
/// concatenated text parts of a multipart content array.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Cap a long text at [`MAX_MESSAGE_CHARS`], keeping its first [`KEEP_HEAD`]
/// and last [`KEEP_TAIL`] characters on char boundaries.
fn cap_message(text: &str) -> String {
    let length = text.chars().count();
    if length <= MAX_MESSAGE_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(KEEP_HEAD).collect();
    let tail: String = text.chars().skip(length - KEEP_TAIL).collect();
    format!("{head}{CUT_MARKER}{tail}")
}

/// Build safe FTS5 terms from a free-text query: each whitespace-separated
/// word is stripped of punctuation and quoted, so a stray `-`, `*` or quote
/// can never raise an FTS syntax error.
fn fts_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| term.trim_matches(|c: char| !c.is_alphanumeric() && c != '_'))
        .filter(|term| !term.is_empty())
        .map(|term| format!("\"{}\"", term.replace('"', "")))
        .collect()
}

/// A file's modification time as Unix epoch milliseconds; 0 when the
/// filesystem cannot report one.
fn mtime_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// `path` with one suffix appended to its file name (`-wal`, `-shm`).
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Delete a file, treating "not there" as success.
fn remove_file_if_present(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("remove {}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write one transcript JSON file and describe it as a source.
    fn transcript_source(
        dir: &Path,
        name: &str,
        messages: Value,
        wrapped: bool,
    ) -> TranscriptSource {
        let path = dir.join(format!("{name}.json"));
        let body = if wrapped {
            serde_json::json!({"model": "test", "transcript": {"messages": messages}})
        } else {
            serde_json::json!({"messages": messages})
        };
        std::fs::write(&path, serde_json::to_string(&body).expect("serialize")).expect("write");
        TranscriptSource {
            name: name.to_string(),
            path,
        }
    }

    /// Open an index in `dir`.
    fn index(dir: &Path) -> TranscriptIndex {
        TranscriptIndex::open(&dir.join("transcript_index.db")).expect("open the index")
    }

    #[test]
    fn a_search_finds_a_worker_message_by_its_words() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([
                {"role": "user", "content": "raise the retry budget to five"},
                {"role": "assistant", "content": "the policy file is updated"},
            ]),
            false,
        );
        let mut index = index(dir.path());
        let report = index.sync(std::slice::from_ref(&source));
        assert_eq!(report.indexed, 2, "{report:?}");
        assert!(report.unreadable.is_empty(), "{report:?}");
        let hits = index.search("retry budget", None, 10).expect("search");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].source, "agent-a");
        assert_eq!(hits[0].seq, 0);
        assert_eq!(hits[0].role, "user");
        assert!(hits[0].text.contains("retry"), "{}", hits[0].text);
    }

    #[test]
    fn the_source_filter_narrows_the_search() {
        let dir = tempfile::tempdir().expect("temp dir");
        let a = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "the shared phrase lives here"}]),
            false,
        );
        let b = transcript_source(
            dir.path(),
            "agent-b",
            serde_json::json!([{"role": "tool", "content": "the shared phrase lives there"}]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[a, b]);
        let all = index.search("shared phrase", None, 10).expect("unfiltered");
        assert_eq!(all.len(), 2, "{all:?}");
        let narrowed = index
            .search("shared phrase", Some("agent-b"), 10)
            .expect("filtered");
        assert_eq!(narrowed.len(), 1, "{narrowed:?}");
        assert_eq!(narrowed[0].source, "agent-b");
    }

    #[test]
    fn lead_json_and_worker_files_both_parse() {
        let dir = tempfile::tempdir().expect("temp dir");
        let worker = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "the worker found the widget"}]),
            false,
        );
        let lead = transcript_source(
            dir.path(),
            "lead",
            serde_json::json!([{"role": "assistant", "content": "the lead merged the gadget"}]),
            true,
        );
        let mut index = index(dir.path());
        let report = index.sync(&[worker, lead]);
        assert_eq!(report.indexed, 2, "{report:?}");
        assert_eq!(index.search("widget", None, 10).expect("widget").len(), 1);
        let gadget = index.search("gadget", None, 10).expect("gadget");
        assert_eq!(gadget.len(), 1, "{gadget:?}");
        assert_eq!(gadget[0].source, "lead");
    }

    #[test]
    fn system_messages_are_not_indexed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([
                {"role": "system", "content": "the system prompt says needle"},
                {"role": "user", "content": "the user message"},
            ]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[source]);
        assert!(index.search("needle", None, 10).expect("search").is_empty());
        // The system message still occupies its file index, so seq stays the
        // message's position in the transcript file.
        assert!(index.message("agent-a", 0).expect("lookup").is_none());
        let user = index
            .message("agent-a", 1)
            .expect("lookup")
            .expect("the user message");
        assert_eq!(user.role, "user");
        assert_eq!(user.text, "the user message");
    }

    #[test]
    fn tool_calls_are_searchable_and_reasoning_is_not() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "t1",
                    "name": "shell",
                    "arguments": "{\"command\":\"cargo test\"}"
                }],
                "reasoning_content": "hidden reasoning phrase"
            }]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[source]);
        let call = index.search("cargo test", None, 10).expect("search");
        assert_eq!(call.len(), 1, "{call:?}");
        assert!(call[0].text.contains("-> shell"), "{}", call[0].text);
        assert!(index
            .search("hidden reasoning", None, 10)
            .expect("search")
            .is_empty());
    }

    #[test]
    fn an_unchanged_file_is_not_reindexed_and_a_changed_one_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "one message"}]),
            false,
        );
        let mut index = index(dir.path());
        assert_eq!(index.sync(std::slice::from_ref(&source)).indexed, 1);
        assert_eq!(index.sync(std::slice::from_ref(&source)).indexed, 0);
        transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([
                {"role": "user", "content": "one message"},
                {"role": "assistant", "content": "a second, longer message"}
            ]),
            false,
        );
        assert_eq!(index.sync(std::slice::from_ref(&source)).indexed, 2);
    }

    #[test]
    fn a_removed_source_leaves_the_index() {
        let dir = tempfile::tempdir().expect("temp dir");
        let a = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "the first source word"}]),
            false,
        );
        let b = transcript_source(
            dir.path(),
            "agent-b",
            serde_json::json!([{"role": "user", "content": "the second source word"}]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[a.clone(), b]);
        let report = index.sync(std::slice::from_ref(&a));
        assert_eq!(report.removed, 1, "{report:?}");
        assert!(index
            .search("second source", Some("agent-b"), 10)
            .expect("search")
            .is_empty());
        assert!(index.message("agent-b", 0).expect("lookup").is_none());
        assert_eq!(
            index
                .search("first source", None, 10)
                .expect("search")
                .len(),
            1
        );
    }

    #[test]
    fn a_damaged_file_keeps_its_old_rows_and_is_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "the durable needle"}]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(std::slice::from_ref(&source));
        std::fs::write(&source.path, "this is not JSON").expect("damage the file");
        let report = index.sync(std::slice::from_ref(&source));
        assert_eq!(report.indexed, 0, "{report:?}");
        assert_eq!(report.unreadable, vec!["agent-a".to_string()], "{report:?}");
        assert_eq!(index.search("needle", None, 10).expect("search").len(), 1);
    }

    #[test]
    fn long_messages_are_capped_on_a_char_boundary() {
        let dir = tempfile::tempdir().expect("temp dir");
        let long = "é".repeat(10_000);
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": long}]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[source]);
        let hit = index
            .message("agent-a", 0)
            .expect("lookup")
            .expect("the long message");
        let chars: Vec<char> = hit.text.chars().collect();
        assert_eq!(
            chars.len(),
            KEEP_HEAD + CUT_MARKER.chars().count() + KEEP_TAIL,
            "{}",
            hit.text.len()
        );
        assert!(chars[..KEEP_HEAD].iter().all(|c| *c == 'é'));
        assert!(chars[chars.len() - KEEP_TAIL..].iter().all(|c| *c == 'é'));
        assert!(hit.text.contains(CUT_MARKER));
        // A multi-byte boundary split would have produced replacement chars.
        assert!(!hit.text.contains('\u{FFFD}'));
    }

    #[test]
    fn secrets_are_redacted_in_the_index() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{
                "role": "user",
                "content": "use the token sk-abcdefghijklmnop1234567890 here"
            }]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[source]);
        let hit = index
            .message("agent-a", 0)
            .expect("lookup")
            .expect("the message");
        assert!(!hit.text.contains("abcdefghijklmnop"), "{}", hit.text);
        assert!(hit.text.contains("[REDACTED]"), "{}", hit.text);
    }

    #[test]
    fn odd_query_characters_do_not_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "alpha beta"}]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[source]);
        for query in [
            "\"quoted\"",
            "(parens)",
            "AND",
            "*",
            "-",
            "alpha * - AND",
            "\"",
            "NEAR(",
        ] {
            index
                .search(query, None, 10)
                .unwrap_or_else(|err| panic!("{query}: {err}"));
        }
    }

    #[test]
    fn all_terms_are_preferred_then_any_term() {
        let dir = tempfile::tempdir().expect("temp dir");
        let both = transcript_source(
            dir.path(),
            "agent-both",
            serde_json::json!([{"role": "user", "content": "alpha beta"}]),
            false,
        );
        let alpha = transcript_source(
            dir.path(),
            "agent-alpha",
            serde_json::json!([{"role": "user", "content": "alpha only"}]),
            false,
        );
        let gamma = transcript_source(
            dir.path(),
            "agent-gamma",
            serde_json::json!([{"role": "user", "content": "gamma only"}]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[both, alpha, gamma]);
        let all_terms = index.search("alpha beta", None, 10).expect("search");
        assert_eq!(all_terms.len(), 1, "{all_terms:?}");
        assert_eq!(all_terms[0].source, "agent-both");
        // No document has both words, so the OR pass answers.
        let any_term = index.search("alpha gamma", None, 10).expect("search");
        let mut sources: Vec<&str> = any_term.iter().map(|hit| hit.source.as_str()).collect();
        sources.sort_unstable();
        assert_eq!(sources, vec!["agent-alpha", "agent-both", "agent-gamma"]);
    }

    #[test]
    fn a_corrupt_index_file_is_rebuilt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = dir.path().join("transcript_index.db");
        std::fs::write(&db, b"this is not a sqlite database").expect("plant the corrupt file");
        let mut index = TranscriptIndex::open(&db).expect("rebuild the corrupt index");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([{"role": "user", "content": "the rebuilt needle"}]),
            false,
        );
        assert_eq!(index.sync(&[source]).indexed, 1);
        assert_eq!(index.search("needle", None, 10).expect("search").len(), 1);
    }

    #[test]
    fn message_returns_one_message() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = transcript_source(
            dir.path(),
            "agent-a",
            serde_json::json!([
                {"role": "user", "content": "the first message"},
                {"role": "assistant", "content": "the second message"},
            ]),
            false,
        );
        let mut index = index(dir.path());
        index.sync(&[source]);
        let hit = index
            .message("agent-a", 1)
            .expect("lookup")
            .expect("the second message");
        assert_eq!(hit.source, "agent-a");
        assert_eq!(hit.seq, 1);
        assert_eq!(hit.role, "assistant");
        assert_eq!(hit.text, "the second message");
        assert!(index.message("agent-a", 42).expect("lookup").is_none());
        assert!(index.message("nobody", 0).expect("lookup").is_none());
    }
}
