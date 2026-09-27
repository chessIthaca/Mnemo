// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Persistent index-staleness log (backlog fc1d57fe).
//!
//! Every inline index repair — the content index's freshness pass and the
//! symbol index's stale sweep — used to report only a COUNT ("reindexed N
//! stale file(s)"), so WHY files went stale could not be learned after the
//! fact. This module records ONE structured line per stale file to
//! `<global config dir>/index-staleness.jsonl` (`~/.mnemo/`): timestamp, tool,
//! path, cause class, index-vs-disk mtime + disk size, and whether the file
//! was re-indexed inline or only surfaced as stale (above the re-index cap).
//!
//! The log is append-only, size-capped and rotated to a single `.1` backup; a
//! failed write is reported on stderr and swallowed — a diagnostic must never
//! break the search that triggered it (the index repair itself is
//! best-effort, and so is its record).
//!
//! Classification is deliberately cheap. The store keeps a per-file content
//! hash (the indexer's raw-bytes `DefaultHasher` hash) plus the as-of-index
//! mtime, so a stale file whose hash still matches is a [`Cause::Touch`]
//! (save-all, git checkout, formatter), one that matches only after CRLF→LF
//! normalization is a [`Cause::LineEndingFlip`], and anything else is a
//! [`Cause::ContentEdit`]. The hash probe runs only for sets small enough to
//! repair inline (≤ the callers' `STALE_REINDEX_CAP`): reading thousands of
//! files to classify an above-cap staleness would defeat the cap, so those
//! records carry the cheap stats and a coarse cause instead.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

/// The log file name inside the global config dir.
pub const STALENESS_LOG_FILE: &str = "index-staleness.jsonl";

/// Rotate the log to this suffix once it exceeds [`STALENESS_LOG_CAP_BYTES`].
pub const ROTATED_SUFFIX: &str = ".1";

/// Rotate the log once it grows past this many bytes (a single `.1` backup is
/// kept; an older backup is replaced).
pub const STALENESS_LOG_CAP_BYTES: u64 = 512 * 1024;

/// Why a file was flagged stale by an index freshness check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cause {
    /// The content hash differs from the indexed one — a real edit.
    ContentEdit,
    /// The indexed mtime moved but the content hash is identical: a save-all,
    /// a git checkout, a formatter run — no content change.
    Touch,
    /// The bytes match the indexed hash only after CRLF→LF normalization — a
    /// line-ending flip (git `core.autocrlf`, an editor rewrite).
    LineEndingFlip,
    /// The file has no `cg_files` row at all (written since the last index
    /// pass, or an index DB predating it).
    Unindexed,
    /// The file could not be read or stat'd: vanished, unreadable, or
    /// not-UTF-8 — the index still serves its old rows until the prune.
    VanishedOrUnreadable,
    /// The mtime moved and no hash evidence was gathered: the staleness was
    /// above the inline-repair cap, or the file is not content-indexed.
    MtimeDrift,
}

/// Whether the stale file was repaired inline or only surfaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Re-indexed by the inline repair pass that observed it.
    InlineReindex,
    /// Reported stale without repair (above the inline cap, or the repair did
    /// not run) — the caller walked or served the stale index.
    SurfacedOnly,
}

/// One stale file's record.
#[derive(Debug, Clone, Serialize)]
pub struct StaleEntry {
    /// Project-relative path with `/` separators.
    pub path: String,
    /// The classified cause.
    pub cause: Cause,
    /// The as-of-index mtime (unix millis); `None` when the file has no row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_index: Option<i64>,
    /// The on-disk mtime (unix millis); `None` when the file could not be
    /// stat'd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime_disk: Option<i64>,
    /// The on-disk size in bytes; `None` when the file could not be stat'd.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_disk: Option<u64>,
}

/// The on-disk evidence for one stale file — what the classifier compares
/// against the index.
#[derive(Debug, Clone)]
pub struct DiskProbe {
    /// On-disk mtime, unix millis (the unit the index stores).
    pub mtime: i64,
    /// On-disk size in bytes.
    pub size: u64,
    /// Hash of the raw bytes, computed exactly like the indexer (same hasher,
    /// same input).
    pub raw_hash: String,
    /// Hash of the bytes with CRLF normalized to LF — the line-ending probe.
    pub lf_hash: String,
}

/// Classify one stale file from the index's stored hash and the disk probe.
///
/// `stored_hash` is `None` when the file has no `cg_files` row
/// ([`Cause::Unindexed`]); `disk` is `None` when the file could not be read
/// ([`Cause::VanishedOrUnreadable`]). A raw-hash match is a [`Cause::Touch`],
/// an LF-normalized match a [`Cause::LineEndingFlip`], anything else a
/// [`Cause::ContentEdit`].
pub fn classify(stored_hash: Option<&str>, disk: Option<&DiskProbe>) -> Cause {
    let Some(stored) = stored_hash else {
        return Cause::Unindexed;
    };
    let Some(disk) = disk else {
        return Cause::VanishedOrUnreadable;
    };
    if stored == disk.raw_hash {
        Cause::Touch
    } else if stored == disk.lf_hash {
        Cause::LineEndingFlip
    } else {
        Cause::ContentEdit
    }
}

/// One written record line: the per-entry fields plus the shared envelope.
#[derive(Debug, Serialize)]
struct Record<'a> {
    /// Unix millis when the record was written.
    ts: i64,
    /// The tool that observed the staleness (`search`, `search_read`,
    /// `graph_search`, ...).
    tool: &'a str,
    /// Inline-repaired vs surfaced-only.
    action: Action,
    /// The stale file's fields.
    #[serde(flatten)]
    entry: &'a StaleEntry,
}

/// The append-only staleness log: a directory plus the recorder.
///
/// Construct with [`StalenessLog::global`] in production and
/// [`StalenessLog::at`] in tests, so no test ever writes the real config dir.
#[derive(Debug, Clone)]
pub struct StalenessLog {
    dir: PathBuf,
}

impl StalenessLog {
    /// The production log: `<global config dir>/index-staleness.jsonl`.
    pub fn global() -> Self {
        Self::at(crate::config::global_config_dir())
    }

    /// A log rooted at an explicit directory (tests, diagnostics).
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The log file path (`<dir>/index-staleness.jsonl`).
    pub fn path(&self) -> PathBuf {
        self.dir.join(STALENESS_LOG_FILE)
    }

    /// Append one JSON line per entry. Best-effort: a failed write is
    /// reported on stderr and swallowed (a diagnostic must never fail the
    /// search), and no file is created for an empty entry set. A log already
    /// past [`STALENESS_LOG_CAP_BYTES`] rotates to `<name>.1` first, so the
    /// record that trips the rotation opens the fresh log.
    pub fn record(&self, tool: &str, entries: &[StaleEntry], action: Action) {
        if entries.is_empty() {
            return;
        }
        if let Err(e) = self.record_inner(tool, entries, action) {
            eprintln!(
                "mnemo: could not write the index-staleness log {:?}: {e}",
                self.path()
            );
        }
    }

    fn record_inner(
        &self,
        tool: &str,
        entries: &[StaleEntry],
        action: Action,
    ) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path();
        // Rotate BEFORE writing: a log already past the cap moves to `.1` and
        // this record starts a fresh live log (rotating after the append would
        // carry the new record into the backup and leave no live file).
        rotate_if_needed(&path)?;
        let ts = now_millis();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        for entry in entries {
            let record = Record {
                ts,
                tool,
                action,
                entry,
            };
            // A record that cannot serialize must not kill the batch: the
            // shape is a plain struct (infallible in practice), but the log
            // stays best-effort either way.
            let line = match serde_json::to_string(&record) {
                Ok(line) => line,
                Err(e) => {
                    eprintln!("mnemo: could not serialize a staleness record: {e}");
                    continue;
                }
            };
            writeln!(file, "{line}")?;
        }
        drop(file);
        Ok(())
    }
}

/// Rotate `path` to `<path>.1` when it exceeds [`STALENESS_LOG_CAP_BYTES`].
///
/// The previous backup is replaced — the log is a diagnostic, not an archive;
/// one generation back is all the "did this happen before the last rotation"
/// question needs.
fn rotate_if_needed(path: &std::path::Path) -> std::io::Result<()> {
    let len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(_) => return Ok(()),
    };
    if len <= STALENESS_LOG_CAP_BYTES {
        return Ok(());
    }
    let mut backup = path.as_os_str().to_os_string();
    backup.push(ROTATED_SUFFIX);
    let backup = PathBuf::from(backup);
    // A failed remove (a missing backup) is the common case, not an error.
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(path, &backup)
}

/// Current time as unix millis (the unit the index stores mtimes in).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(path: &str, cause: Cause) -> StaleEntry {
        StaleEntry {
            path: path.to_string(),
            cause,
            mtime_index: Some(1_000),
            mtime_disk: Some(2_000),
            size_disk: Some(42),
        }
    }

    #[test]
    fn record_appends_one_json_line_per_entry() {
        let dir = tempdir().unwrap();
        let log = StalenessLog::at(dir.path());
        log.record(
            "graph_search",
            &[
                entry("src/a.rs", Cause::ContentEdit),
                entry("src/b.rs", Cause::Touch),
            ],
            Action::InlineReindex,
        );
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "one line per entry: {text}");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["tool"], "graph_search");
        assert_eq!(first["action"], "inline-reindex");
        assert_eq!(first["path"], "src/a.rs");
        assert_eq!(first["cause"], "content-edit");
        assert_eq!(first["mtime_index"], 1_000);
        assert_eq!(first["mtime_disk"], 2_000);
        assert_eq!(first["size_disk"], 42);
        assert!(first["ts"].as_i64().unwrap() > 0, "timestamp present");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["cause"], "touch");
    }

    #[test]
    fn empty_entries_write_nothing() {
        let dir = tempdir().unwrap();
        let log = StalenessLog::at(dir.path());
        log.record("search", &[], Action::SurfacedOnly);
        assert!(!log.path().exists(), "no file for an empty record set");
    }

    #[test]
    fn record_creates_a_missing_directory() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("nested").join("cfg");
        let log = StalenessLog::at(nested);
        log.record(
            "search",
            &[entry("a.rs", Cause::Unindexed)],
            Action::SurfacedOnly,
        );
        assert!(log.path().exists(), "the config dir is created on demand");
    }

    #[test]
    fn rotation_caps_the_log_and_keeps_one_backup() {
        let dir = tempdir().unwrap();
        let log = StalenessLog::at(dir.path());
        // A log past the cap, then one more record: the file rotates.
        std::fs::write(log.path(), "x".repeat(STALENESS_LOG_CAP_BYTES as usize + 1)).unwrap();
        log.record(
            "search",
            &[entry("a.rs", Cause::ContentEdit)],
            Action::InlineReindex,
        );
        let rotated = dir
            .path()
            .join(format!("{STALENESS_LOG_FILE}{ROTATED_SUFFIX}"));
        assert!(rotated.exists(), "the oversized log is rotated to .1");
        let big = std::fs::metadata(&rotated).unwrap().len();
        assert!(
            big > STALENESS_LOG_CAP_BYTES,
            "the backup carries the old content ({big} bytes)"
        );
        let fresh = std::fs::metadata(log.path()).unwrap().len();
        assert!(
            fresh < STALENESS_LOG_CAP_BYTES,
            "the live log restarts small ({fresh} bytes)"
        );
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert!(text.contains("a.rs"), "the new record survived rotation");
    }

    #[test]
    fn classify_uses_the_cheapest_sufficient_evidence() {
        let probe = |raw: &str, lf: &str| DiskProbe {
            mtime: 2,
            size: 10,
            raw_hash: raw.to_string(),
            lf_hash: lf.to_string(),
        };
        let same = probe("abc", "abc");
        assert_eq!(classify(Some("abc"), Some(&same)), Cause::Touch);
        let flipped = probe("ab\r\nc", "abc");
        assert_eq!(classify(Some("abc"), Some(&flipped)), Cause::LineEndingFlip);
        let edited = probe("zzz", "zzz");
        assert_eq!(classify(Some("abc"), Some(&edited)), Cause::ContentEdit);
        assert_eq!(classify(None, Some(&same)), Cause::Unindexed);
        assert_eq!(classify(Some("abc"), None), Cause::VanishedOrUnreadable);
    }
}
