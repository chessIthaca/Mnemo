// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The UI log: every popup and every uncaught frontend error, appended as JSONL.
//!
//! A popup can vanish before the user reads it — an info toast clears itself
//! after 4.2 s (`Toast.svelte`) — so the frontend mirrors every popup and every
//! uncaught error into this file, where the user (or the lead) can look it up
//! afterwards. The frontend batches entries in `mnemo2/src/lib/uiLog.ts` and
//! hands them to [`ui_log`]; logging is one-way: a failed write is swallowed
//! (and noted on stderr), never surfaced as a new error path in the UI.
//!
//! While a project is open the file is `<project>/.coding/logs/mnemo2-ui.jsonl`
//! — that folder already holds `provider-errors.jsonl` and the trace log, and
//! the scaffold gitignores it. With no project open it is
//! `<config dir>/logs/mnemo2-ui.jsonl`, so a startup failure before any project
//! opens still leaves a record. The file is capped at [`MAX_LOG_BYTES`] with a
//! single rotation to `mnemo2-ui.jsonl.1`, so a long-lived install cannot grow
//! it without bound.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::mnemo2::ipc::Mnemo2State;
use crate::mnemo2::run_file::now_ms;

/// The log file's name, under `.coding/logs/` (project) or `logs/` (config).
pub const LOG_FILE: &str = "mnemo2-ui.jsonl";

/// The folder under the config dir that takes the log when no project is open.
pub const FALLBACK_DIR: &str = "logs";

/// The size at which the file is rotated once to `<file>.1`.
pub const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

/// One line of the UI log.
///
/// The frontend sends this shape; every field is defaulted so one malformed
/// entry can never reject a whole batch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiLogEntry {
    /// When the entry happened, in Unix epoch milliseconds. Filled in on the
    /// host when the frontend leaves it out.
    #[serde(default)]
    pub ts: Option<u64>,
    /// `"info"` (a popup that reports success or a neutral notice) or
    /// `"error"` (a failure, a refused request, or an uncaught exception).
    #[serde(default)]
    pub level: String,
    /// Where the entry came from: `"toast"`, `"window"` or `"promise"`.
    #[serde(default)]
    pub source: String,
    /// The text the user saw (or the exception's message).
    #[serde(default)]
    pub message: String,
    /// The screen the app was on, so a popup can be placed later.
    #[serde(default)]
    pub route: Option<String>,
    /// The worker the popup was about, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    /// Extra context: an exception's stack, the failing command's detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `"error"` or `"info"`: anything else becomes `"info"`, so the file's level
/// column stays filterable.
pub fn normalize_level(level: &str) -> &'static str {
    if level.eq_ignore_ascii_case("error") {
        "error"
    } else {
        "info"
    }
}

/// Where the UI log lives: the open project's `.coding/logs/`, else the config
/// dir's `logs/`.
pub fn log_path(project: Option<&Path>, config_dir: &Path) -> PathBuf {
    match project {
        Some(root) => root
            .join(".coding")
            .join("logs")
            .join(LOG_FILE),
        None => config_dir.join(FALLBACK_DIR).join(LOG_FILE),
    }
}

/// The single rotated copy's path (`mnemo2-ui.jsonl.1`).
pub fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".1");
    PathBuf::from(name)
}

/// Stamp `ts`, normalize `level`, and default `source` on one entry's way to
/// the file.
fn stamped(entry: &UiLogEntry, now: u64) -> UiLogEntry {
    UiLogEntry {
        ts: Some(entry.ts.filter(|ts| *ts > 0).unwrap_or(now)),
        level: normalize_level(&entry.level).to_string(),
        source: if entry.source.trim().is_empty() {
            "ui".to_string()
        } else {
            entry.source.trim().to_string()
        },
        message: entry.message.clone(),
        route: entry
            .route
            .as_deref()
            .map(str::trim)
            .filter(|route| !route.is_empty())
            .map(str::to_string),
        worker_id: entry.worker_id.clone(),
        detail: entry.detail.clone(),
    }
}

/// Rotate `path` to its single `.1` copy once `incoming` more bytes would push
/// it past `max_bytes`. A missing file is not an error: the first write just
/// creates it.
fn rotate_if_needed(path: &Path, incoming: u64, max_bytes: u64) -> std::io::Result<()> {
    let current = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    if current == 0 || current + incoming <= max_bytes {
        return Ok(());
    }
    let rotated = rotated_path(path);
    // Windows refuses a rename onto an existing file: drop the old copy first.
    let _ = std::fs::remove_file(&rotated);
    std::fs::rename(path, &rotated)
}

/// Append `entries` to `path` as JSONL, creating the folder, rotating the file
/// once it would pass `max_bytes`, and stamping `now` on entries without a
/// timestamp.
pub fn append_entries(
    path: &Path,
    entries: &[UiLogEntry],
    max_bytes: u64,
    now: u64,
) -> std::io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut body = String::new();
    for entry in entries {
        let line = serde_json::to_string(&stamped(entry, now))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        body.push_str(&line);
        body.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let incoming = u64::try_from(body.len()).unwrap_or(u64::MAX);
    rotate_if_needed(path, incoming, max_bytes).or_else(|error| {
        // A rotation that fails must not cost the new line: keep appending to
        // the existing file, which the next rotation attempt can still cap.
        if path.exists() {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(body.as_bytes())
}

/// Write `entries` to the UI log for `project` (or the config dir), swallowing
/// every failure — logging must never become an error the UI has to handle.
pub fn record(project: Option<&Path>, config_dir: &Path, entries: &[UiLogEntry]) {
    let path = log_path(project, config_dir);
    if let Err(error) = append_entries(&path, entries, MAX_LOG_BYTES, now_ms()) {
        eprintln!(
            "mnemo2: could not append to the UI log at '{}': {error}",
            path.display()
        );
    }
}

/// Append a batch of popups and frontend errors to the UI log.
///
/// The result is always `Ok`: a failing write must be invisible to the page, so
/// the command swallows it (and notes it on stderr) rather than turning logging
/// into a new error path.
#[tauri::command]
pub async fn ui_log(
    state: State<'_, Mnemo2State>,
    entries: Vec<UiLogEntry>,
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    let project = state
        .open_lead()
        .await
        .map(|lead| lead.workspace_root().to_path_buf());
    let config_dir = mnemo::config::resolve_config_dir(None);
    let _ = tokio::task::spawn_blocking(move || {
        record(project.as_deref(), &config_dir, &entries);
    })
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entry with the fields a toast sets.
    fn entry(level: &str, message: &str) -> UiLogEntry {
        UiLogEntry {
            ts: None,
            level: level.to_string(),
            source: "toast".to_string(),
            message: message.to_string(),
            route: Some("team".to_string()),
            worker_id: Some("worker-1".to_string()),
            detail: None,
        }
    }

    /// Every non-empty line of `path`, parsed.
    fn read_lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .expect("read the log")
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).expect("a JSON line"))
            .collect()
    }

    #[test]
    fn appends_one_json_line_per_entry() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("mnemo2-ui.jsonl");

        append_entries(
            &path,
            &[entry("error", "Could not stop the team."), entry("info", "Merged.")],
            MAX_LOG_BYTES,
            1_700_000_000_000,
        )
        .expect("append");

        let appended = read_lines(&path);
        assert_eq!(appended.len(), 2);
        assert_eq!(appended[0]["ts"], 1_700_000_000_000_u64);
        assert_eq!(appended[0]["level"], "error");
        assert_eq!(appended[0]["source"], "toast");
        assert_eq!(appended[0]["message"], "Could not stop the team.");
        assert_eq!(appended[0]["route"], "team");
        assert_eq!(appended[0]["worker_id"], "worker-1");
        assert!(
            appended[0].get("detail").is_none(),
            "an absent detail is omitted"
        );
        assert_eq!(appended[1]["level"], "info");
        assert_eq!(appended[1]["message"], "Merged.");


        // A second batch appends rather than replacing.
        append_entries(&path, &[entry("info", "Third.")], MAX_LOG_BYTES, 1_700_000_001_000)
            .expect("append again");
        assert_eq!(read_lines(&path).len(), 3);
    }

    #[test]
    fn keeps_the_frontend_timestamp_and_fills_a_missing_one() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("mnemo2-ui.jsonl");
        let mut with_ts = entry("error", "Boom.");
        with_ts.ts = Some(42);

        append_entries(&path, &[with_ts, entry("info", "Untimed.")], MAX_LOG_BYTES, 7)
            .expect("append");

        let stamped = read_lines(&path);
        assert_eq!(stamped[0]["ts"], 42);
        assert_eq!(stamped[1]["ts"], 7);
    }

    #[test]
    fn an_unknown_level_reads_as_info() {
        assert_eq!(normalize_level("error"), "error");
        assert_eq!(normalize_level("ERROR"), "error");
        assert_eq!(normalize_level("info"), "info");
        assert_eq!(normalize_level(""), "info");
        assert_eq!(normalize_level("warning"), "info");
    }

    #[test]
    fn rotates_once_past_the_cap_and_keeps_the_new_line() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("mnemo2-ui.jsonl");
        std::fs::write(&path, "old line that is long enough\n").expect("seed the log");
        let rotated = rotated_path(&path);
        std::fs::write(&rotated, "an older copy\n").expect("seed the rotated copy");

        // The cap is smaller than the seeded file plus the new line.
        append_entries(&path, &[entry("error", "New failure.")], 20, 1).expect("append");

        let kept = std::fs::read_to_string(&rotated).expect("read the rotated copy");
        assert_eq!(kept, "old line that is long enough\n", "the previous copy is replaced");
        let fresh = read_lines(&path);
        assert_eq!(fresh.len(), 1, "the new line starts a fresh file");
        assert_eq!(fresh[0]["message"], "New failure.");
    }

    #[test]
    fn does_not_rotate_below_the_cap() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("mnemo2-ui.jsonl");
        std::fs::write(&path, "{\"level\":\"info\",\"message\":\"earlier\"}\n").expect("seed the log");

        append_entries(&path, &[entry("info", "Small.")], MAX_LOG_BYTES, 1).expect("append");

        assert!(!rotated_path(&path).exists(), "no rotation below the cap");
        assert_eq!(read_lines(&path).len(), 2);
    }


    #[test]
    fn no_open_project_falls_back_to_the_config_dir() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let config = dir.path().join("config");
        let project = dir.path().join("project");

        let in_project = log_path(Some(&project), &config);
        assert_eq!(
            in_project,
            project.join(".coding").join("logs").join("mnemo2-ui.jsonl")
        );
        let fallback = log_path(None, &config);
        assert_eq!(fallback, config.join("logs").join("mnemo2-ui.jsonl"));

        record(None, &config, &[entry("error", "No project open.")]);
        let fallback_lines = read_lines(&fallback);
        assert_eq!(fallback_lines.len(), 1);
        assert_eq!(fallback_lines[0]["message"], "No project open.");
        assert!(!project.exists(), "the fallback never touches a project root");
    }

    #[test]
    fn an_open_project_writes_under_coding_logs() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let config = dir.path().join("config");
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).expect("create the project root");

        record(Some(&project), &config, &[entry("info", "In the project.")]);

        let path = project.join(".coding").join("logs").join("mnemo2-ui.jsonl");
        assert_eq!(read_lines(&path)[0]["message"], "In the project.");
        assert!(!config.join("logs").join("mnemo2-ui.jsonl").exists());
    }

    #[test]
    fn a_failed_write_is_swallowed() {
        let dir = tempfile::tempdir().expect("temporary directory");
        // The "project root" is a file, so `.coding/logs/` cannot be created.
        let project = dir.path().join("not-a-folder");
        std::fs::write(&project, b"file").expect("write the file");
        let config = dir.path().join("also-a-file");
        std::fs::write(&config, b"file").expect("write the config file");

        // Neither call may panic or return an error the caller has to handle.
        record(Some(&project), dir.path(), &[entry("error", "Lost.")]);
        record(None, &config, &[entry("error", "Lost too.")]);
    }
}
