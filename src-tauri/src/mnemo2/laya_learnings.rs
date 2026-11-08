// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Laya's routing decisions travelling with the repository and the installer.
//!
//! The dispatch routing log only grows on the machine that wrote its rows. A
//! project that keeps a `.coding/laya/` folder carries a verbatim copy of its
//! OWN rows in `.coding/laya/dispatch_routing.jsonl`, and the installer ships
//! the same file as a `laya/dispatch_routing.jsonl` resource. Export is the
//! union of the bundle and the log's project rows — it appends and never
//! removes a line. Import is append-only too: the kNN overlay reads the log by
//! byte offset, so the log is never rewritten.
//!
//! A bundle version is the sha256 of its bytes. A copy the log already
//! contains is up to date; a copy whose rows the log lacks is a fast-forward
//! (import silently) unless this computer holds rows the copy lacks — then it
//! is a conflict and the user decides. "Update" appends the missing rows and
//! removes nothing; "Not now" remembers that version, so the same bytes never
//! ask again while a later version always does.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::mnemo2::phase3;

/// The opt-in folder: only a project that keeps it exports its rows.
pub(crate) const LEARNINGS_DIR: &str = ".coding/laya";

/// The bundle file inside [`LEARNINGS_DIR`], and the installer resource name.
pub(crate) const LEARNINGS_FILE: &str = "dispatch_routing.jsonl";

/// The per-machine file recording what was decided for each copy's version.
pub(crate) const IMPORT_RECORD_FILE: &str = "learnings_imports.json";

/// One copy of the decisions this computer can import.
#[derive(Clone, Debug)]
pub(crate) struct LearningsSource {
    /// `installer` or `project:<root display>`.
    pub(crate) key: String,
    /// The label the question names the copy by.
    pub(crate) label: String,
    /// The copy's file.
    pub(crate) path: PathBuf,
    /// `Some` for a project source: scopes `local_only` to that project's
    /// rows.
    pub(crate) plans_dir: Option<PathBuf>,
}

/// A conflicting copy the user decides about.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LearningsOffer {
    /// The [`LearningsSource::key`] the answer applies to.
    pub source_key: String,
    /// The [`LearningsSource::label`] the question names the copy by.
    pub source_label: String,
    /// The copy's version: the lowercase hex sha256 of its bytes.
    pub sha256: String,
    /// Rows this computer's log lacks.
    pub new_rows: usize,
    /// This computer's rows the copy lacks.
    pub local_only_rows: usize,
}

/// What a copy's check found.
#[derive(Debug, PartialEq)]
pub(crate) enum CheckResult {
    /// Nothing to import (absent, empty, or already up to date).
    Nothing,
    /// The missing rows were appended.
    Imported { rows: usize },
    /// A conflicting copy waits for the user's answer.
    Offer(LearningsOffer),
}

/// One recorded decision about one copy version.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ImportRecord {
    sha256: String,
    decision: String,
    at_ms: u128,
}

/// The record's on-disk shape: one entry per source key.
type RecordMap = BTreeMap<String, ImportRecord>;

/// The sources to check, installer first, the project's own bundle last.
pub(crate) fn learnings_sources(resource_dir: Option<&Path>, root: &Path) -> Vec<LearningsSource> {
    let mut sources = Vec::new();
    if let Some(resource_dir) = resource_dir {
        sources.push(LearningsSource {
            key: "installer".to_string(),
            label: "This Mnemo2 install".to_string(),
            path: resource_dir.join("laya").join(LEARNINGS_FILE),
            plans_dir: None,
        });
    }
    sources.push(LearningsSource {
        key: format!("project:{}", root.display()),
        label: "This project's repository".to_string(),
        path: root.join(LEARNINGS_DIR).join(LEARNINGS_FILE),
        plans_dir: Some(root.join(".coding").join("plans")),
    });
    sources
}

/// Refresh the project's bundle; `Ok(false)` when the folder is absent or
/// nothing is new.
///
/// Existing bundle lines stay, in order; the project's log rows the bundle
/// lacks are appended in log order. A line is never removed.
pub(crate) fn export_project_learnings(log_path: &Path, root: &Path) -> Result<bool, String> {
    let learnings_dir = root.join(LEARNINGS_DIR);
    if !learnings_dir.is_dir() {
        return Ok(false);
    }
    let bundle_path = learnings_dir.join(LEARNINGS_FILE);
    let bundle = read_text(&bundle_path)?.unwrap_or_default();
    let log = read_text(log_path)?.unwrap_or_default();
    let plans_dir = root.join(".coding").join("plans");
    let bundle_lines = lines(&bundle);
    let mut seen: HashSet<&str> = bundle_lines.iter().copied().collect();
    let new_rows: Vec<&str> = lines(&log)
        .into_iter()
        .filter(|line| is_project_row(line, &plans_dir) && seen.insert(*line))
        .collect();
    if new_rows.is_empty() {
        return Ok(false);
    }
    let mut merged = String::new();
    for line in bundle_lines.iter().copied().chain(new_rows) {
        merged.push_str(line);
        merged.push('\n');
    }
    write_atomic(&bundle_path, &merged)?;
    Ok(true)
}

/// Check one copy against this computer's log: import silently, offer when it
/// conflicts, or report nothing to do.
pub(crate) fn check_source(
    source: &LearningsSource,
    log_path: &Path,
    record_path: &Path,
) -> Result<CheckResult, String> {
    let Some(bundle) = read_text(&source.path)? else {
        return Ok(CheckResult::Nothing);
    };
    let bundle_lines = lines(&bundle);
    if bundle_lines.is_empty() {
        return Ok(CheckResult::Nothing);
    }
    let sha = sha256_hex(bundle.as_bytes());
    let mut record = load_record(record_path);
    if record
        .get(&source.key)
        .is_some_and(|entry| entry.sha256 == sha)
    {
        return Ok(CheckResult::Nothing);
    }
    let log = read_text(log_path)?.unwrap_or_default();
    let log_lines = lines(&log);
    let log_set: HashSet<&str> = log_lines.iter().copied().collect();
    let mut seen_missing = HashSet::new();
    let missing: Vec<&str> = bundle_lines
        .iter()
        .copied()
        .filter(|line| !log_set.contains(*line) && seen_missing.insert(*line))
        .collect();
    let bundle_set: HashSet<&str> = bundle_lines.iter().copied().collect();
    let mut seen_local = HashSet::new();
    let local_only: Vec<&str> = log_lines
        .iter()
        .copied()
        .filter(|line| !bundle_set.contains(*line) && seen_local.insert(*line))
        .filter(|line| {
            source
                .plans_dir
                .as_deref()
                .is_none_or(|plans_dir| is_project_row(line, plans_dir))
        })
        .collect();
    if missing.is_empty() {
        save_decision(&mut record, &source.key, &sha, "current");
        save_record(record_path, &record)?;
        return Ok(CheckResult::Nothing);
    }
    let declined = record
        .get(&source.key)
        .is_some_and(|entry| entry.decision == "declined");
    if !local_only.is_empty() || declined {
        return Ok(CheckResult::Offer(LearningsOffer {
            source_key: source.key.clone(),
            source_label: source.label.clone(),
            sha256: sha,
            new_rows: missing.len(),
            local_only_rows: local_only.len(),
        }));
    }
    let missing: Vec<String> = missing.iter().map(|line| (*line).to_string()).collect();
    let rows = append_rows(log_path, &missing)?;
    save_decision(&mut record, &source.key, &sha, "imported");
    save_record(record_path, &record)?;
    Ok(CheckResult::Imported { rows })
}

/// Apply the user's answer about an offered copy; returns the rows appended.
pub(crate) fn decide_offer(
    source: &LearningsSource,
    sha256: &str,
    accept: bool,
    log_path: &Path,
    record_path: &Path,
) -> Result<usize, String> {
    let bundle = read_text(&source.path)?.unwrap_or_default();
    if sha256_hex(bundle.as_bytes()) != sha256 {
        return Err(
            "the copy changed; it will be offered again when the project next opens".to_string(),
        );
    }
    let mut record = load_record(record_path);
    if !accept {
        save_decision(&mut record, &source.key, sha256, "declined");
        save_record(record_path, &record)?;
        return Ok(0);
    }
    let log = read_text(log_path)?.unwrap_or_default();
    let log_set: HashSet<&str> = lines(&log).into_iter().collect();
    let mut seen = HashSet::new();
    let missing: Vec<String> = lines(&bundle)
        .into_iter()
        .filter(|line| !log_set.contains(*line) && seen.insert(*line))
        .map(str::to_string)
        .collect();
    let rows = append_rows(log_path, &missing)?;
    save_decision(&mut record, &source.key, sha256, "merged");
    save_record(record_path, &record)?;
    Ok(rows)
}

/// Split a JSONL text into its rows: strip a trailing carriage return and
/// drop blank lines.
fn lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim().is_empty())
        .collect()
}

/// The `plan_id` a routing row names, if the line parses and carries one.
fn row_plan_id(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value.get("plan_id")?.as_str().map(str::to_string)
}

/// Whether a routing row belongs to the project whose plans live in
/// `plans_dir`: its plan id is a plain file stem and the plan file exists.
fn is_project_row(line: &str, plans_dir: &Path) -> bool {
    let Some(plan_id) = row_plan_id(line) else {
        return false;
    };
    if plan_id.is_empty() || !plan_id.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return false;
    }
    plans_dir.join(format!("{plan_id}.md")).is_file()
}

/// Read a file as text; a missing file is `Ok(None)`.
fn read_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|err| format!("{} is not UTF-8: {err}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(format!("could not read {}: {err}", path.display())),
    }
}

/// The lowercase hex sha256 of a copy's bytes: its version.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The temporary sibling an atomic write goes through.
fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Write text through a temporary sibling and rename it into place.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    std::fs::write(&tmp, text)
        .map_err(|err| format!("could not write {}: {err}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|err| format!("could not replace {}: {err}", path.display()))
}

/// Append rows to the routing log in one write, repairing a missing final
/// newline first so every row stays whole.
fn append_rows(log_path: &Path, rows: &[String]) -> Result<usize, String> {
    if rows.is_empty() {
        return Ok(0);
    }
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(log_path)
        .map_err(|err| format!("could not open {}: {err}", log_path.display()))?;
    let mut buffer = String::new();
    let len = file
        .metadata()
        .map_err(|err| format!("could not stat {}: {err}", log_path.display()))?
        .len();
    if len > 0 {
        file.seek(SeekFrom::End(-1))
            .map_err(|err| format!("could not seek {}: {err}", log_path.display()))?;
        let mut last = [0u8; 1];
        file.read_exact(&mut last)
            .map_err(|err| format!("could not read {}: {err}", log_path.display()))?;
        if last[0] != b'\n' {
            buffer.push('\n');
        }
    }
    for row in rows {
        buffer.push_str(row);
        buffer.push('\n');
    }
    file.write_all(buffer.as_bytes())
        .map_err(|err| format!("could not append to {}: {err}", log_path.display()))?;
    Ok(rows.len())
}

/// Load the record; a missing or unreadable file counts as empty.
fn load_record(record_path: &Path) -> RecordMap {
    let text = match std::fs::read_to_string(record_path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return RecordMap::new(),
        Err(err) => {
            eprintln!("could not read {}: {err}", record_path.display());
            return RecordMap::new();
        }
    };
    let record: RecordMap = match serde_json::from_str(&text) {
        Ok(record) => record,
        Err(err) => {
            eprintln!("could not read {}: {err}", record_path.display());
            RecordMap::new()
        }
    };
    record
}

/// Remember the decision about one copy version.
fn save_decision(record: &mut RecordMap, key: &str, sha256: &str, decision: &str) {
    record.insert(
        key.to_string(),
        ImportRecord {
            sha256: sha256.to_string(),
            decision: decision.to_string(),
            at_ms: phase3::now_ms(),
        },
    );
}

/// Write the record through a temporary sibling and rename it into place.
fn save_record(record_path: &Path, record: &RecordMap) -> Result<(), String> {
    let text = serde_json::to_string(record)
        .map_err(|err| format!("could not encode {}: {err}", record_path.display()))?;
    write_atomic(record_path, &(text + "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project root with the plans folder every project has.
    fn project_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("project root");
        std::fs::create_dir_all(root.path().join(".coding").join("plans")).expect("plans dir");
        root
    }

    /// Give the project a plan file so rows naming it count as its own.
    fn add_plan(root: &Path, id: &str) {
        std::fs::write(
            root.join(".coding").join("plans").join(format!("{id}.md")),
            "# plan\n",
        )
        .expect("plan file");
    }

    /// One verbatim routing row of this project.
    fn route_row(worker: &str, plan_id: &str) -> String {
        format!(
            r#"{{"event":"route","ts":1,"worker_id":"{worker}","plan_id":"{plan_id}","text":"a dispatched step"}}"#
        )
    }

    /// One routing row without a plan: another project's decision.
    fn route_row_without_plan(worker: &str) -> String {
        format!(r#"{{"event":"route","ts":1,"worker_id":"{worker}","text":"a dispatched step"}}"#)
    }

    fn write_text(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("parent dir");
        std::fs::write(path, text).expect("write file");
    }

    fn read_lines(path: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(path).expect("read file");
        lines(&text).into_iter().map(str::to_string).collect()
    }

    fn log_path(root: &Path) -> PathBuf {
        root.join("machine").join("dispatch_routing.jsonl")
    }

    fn record_path(root: &Path) -> PathBuf {
        root.join("machine").join(IMPORT_RECORD_FILE)
    }

    fn bundle_path(root: &Path) -> PathBuf {
        root.join(LEARNINGS_DIR).join(LEARNINGS_FILE)
    }

    fn project_source(root: &Path) -> LearningsSource {
        let mut sources = learnings_sources(None, root);
        sources.pop().expect("project source")
    }

    fn record_for(root: &Path, key: &str) -> serde_json::Value {
        let text = std::fs::read_to_string(record_path(root)).expect("record file");
        let record: serde_json::Value = serde_json::from_str(&text).expect("record json");
        record[key].clone()
    }

    fn expect_offer(result: CheckResult) -> LearningsOffer {
        match result {
            CheckResult::Offer(offer) => offer,
            other => panic!("expected an offer, got {other:?}"),
        }
    }

    #[test]
    fn export_needs_the_folder() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        let log = log_path(root.path());
        write_text(&log, &format!("{}\n", route_row("w1", "plan0001")));
        assert!(!export_project_learnings(&log, root.path()).expect("export"));
        assert!(!bundle_path(root.path()).exists());
    }

    #[test]
    fn export_keeps_only_this_projects_rows() {
        let root = project_root();
        std::fs::create_dir_all(root.path().join(LEARNINGS_DIR)).expect("learnings dir");
        add_plan(root.path(), "plan0001");
        let keep = route_row("w1", "plan0001");
        let other = route_row("w2", "plan0002");
        let no_plan = route_row_without_plan("w3");
        let log = log_path(root.path());
        write_text(&log, &format!("{keep}\n{other}\n{no_plan}\n"));
        assert!(export_project_learnings(&log, root.path()).expect("export"));
        assert_eq!(
            std::fs::read_to_string(bundle_path(root.path())).expect("bundle"),
            format!("{keep}\n")
        );
    }

    #[test]
    fn export_never_removes_a_line_and_does_not_rewrite_when_unchanged() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        add_plan(root.path(), "plan0002");
        let old = route_row("w0", "plan0002");
        let new = route_row("w1", "plan0001");
        write_text(&bundle_path(root.path()), &format!("{old}\n"));
        let log = log_path(root.path());
        write_text(&log, &format!("{new}\n"));
        assert!(export_project_learnings(&log, root.path()).expect("export"));
        assert_eq!(
            std::fs::read_to_string(bundle_path(root.path())).expect("bundle"),
            format!("{old}\n{new}\n")
        );
        assert!(!export_project_learnings(&log, root.path()).expect("second export"));
        assert_eq!(
            std::fs::read_to_string(bundle_path(root.path())).expect("bundle"),
            format!("{old}\n{new}\n")
        );
    }

    #[test]
    fn a_plan_id_cannot_escape_the_plans_dir() {
        let root = project_root();
        let escape = route_row("w1", "../x");
        let log = log_path(root.path());
        write_text(&log, &format!("{escape}\n"));
        assert!(!export_project_learnings(&log, root.path()).expect("export"));
        assert!(!bundle_path(root.path()).exists());
    }

    #[test]
    fn a_fresh_computer_imports_silently() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        add_plan(root.path(), "plan0002");
        let first = route_row("w1", "plan0001");
        let second = route_row("w2", "plan0002");
        let bundle = bundle_path(root.path());
        write_text(&bundle, &format!("{first}\n{second}\n"));
        let log = log_path(root.path());
        let source = project_source(root.path());
        let result = check_source(&source, &log, &record_path(root.path())).expect("check");
        match result {
            CheckResult::Imported { rows: 2 } => {}
            other => panic!("expected two imported rows, got {other:?}"),
        }
        assert_eq!(read_lines(&log), vec![first.clone(), second.clone()]);
        let entry = record_for(root.path(), &source.key);
        assert_eq!(entry["decision"], "imported");
        assert_eq!(
            entry["sha256"],
            sha256_hex(std::fs::read(&bundle).expect("bundle").as_slice())
        );
        assert!(matches!(
            check_source(&source, &log, &record_path(root.path())).expect("second check"),
            CheckResult::Nothing
        ));
    }

    #[test]
    fn a_subset_log_fast_forwards() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        add_plan(root.path(), "plan0002");
        let first = route_row("w1", "plan0001");
        let second = route_row("w2", "plan0002");
        write_text(&bundle_path(root.path()), &format!("{first}\n{second}\n"));
        let log = log_path(root.path());
        write_text(&log, &format!("{first}\n"));
        let source = project_source(root.path());
        assert!(matches!(
            check_source(&source, &log, &record_path(root.path())).expect("check"),
            CheckResult::Imported { rows: 1 }
        ));
        assert_eq!(read_lines(&log), vec![first, second]);
        assert_eq!(record_for(root.path(), &source.key)["decision"], "imported");
    }

    #[test]
    fn local_rows_the_copy_lacks_make_an_offer() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        add_plan(root.path(), "plan0002");
        let local = route_row("w1", "plan0001");
        let shipped = route_row("w2", "plan0002");
        let bundle_text = format!("{shipped}\n");
        write_text(&bundle_path(root.path()), &bundle_text);
        let log = log_path(root.path());
        write_text(&log, &format!("{local}\n"));
        let source = project_source(root.path());
        let offer =
            expect_offer(check_source(&source, &log, &record_path(root.path())).expect("check"));
        assert_eq!(offer.source_key, source.key);
        assert_eq!(offer.source_label, source.label);
        assert_eq!(offer.sha256, sha256_hex(bundle_text.as_bytes()));
        assert_eq!((offer.new_rows, offer.local_only_rows), (1, 1));
        assert_eq!(read_lines(&log), vec![local]);
        assert!(
            !record_path(root.path()).exists(),
            "an unanswered offer writes no record"
        );
    }

    #[test]
    fn another_projects_rows_do_not_conflict_with_a_project_copy() {
        let root = project_root();
        add_plan(root.path(), "plan0002");
        let other_project = route_row("w1", "plan9999");
        let shipped = route_row("w2", "plan0002");
        write_text(&bundle_path(root.path()), &format!("{shipped}\n"));
        let log = log_path(root.path());
        write_text(&log, &format!("{other_project}\n"));
        let source = project_source(root.path());
        assert!(matches!(
            check_source(&source, &log, &record_path(root.path())).expect("check"),
            CheckResult::Imported { rows: 1 }
        ));
        assert_eq!(read_lines(&log), vec![other_project, shipped]);
    }

    #[test]
    fn any_local_row_conflicts_with_the_installer_copy() {
        let root = project_root();
        add_plan(root.path(), "plan0002");
        let install = tempfile::tempdir().expect("install dir");
        let other_project = route_row("w1", "plan9999");
        let shipped = route_row("w2", "plan0002");
        let bundle = install.path().join("laya").join(LEARNINGS_FILE);
        write_text(&bundle, &format!("{shipped}\n"));
        let log = log_path(root.path());
        write_text(&log, &format!("{other_project}\n"));
        let source = learnings_sources(Some(install.path()), root.path()).remove(0);
        assert_eq!(source.key, "installer");
        let offer =
            expect_offer(check_source(&source, &log, &record_path(root.path())).expect("check"));
        assert_eq!((offer.new_rows, offer.local_only_rows), (1, 1));
        assert_eq!(read_lines(&log), vec![other_project]);
    }

    #[test]
    fn accepting_appends_and_removes_nothing() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        add_plan(root.path(), "plan0002");
        let local = route_row("w1", "plan0001");
        let shipped = route_row("w2", "plan0002");
        write_text(&bundle_path(root.path()), &format!("{shipped}\n"));
        let log = log_path(root.path());
        write_text(&log, &format!("{local}\n"));
        let source = project_source(root.path());
        let record = record_path(root.path());
        let offer = expect_offer(check_source(&source, &log, &record).expect("check"));
        assert_eq!(
            decide_offer(&source, &offer.sha256, true, &log, &record).expect("accept"),
            1
        );
        assert_eq!(read_lines(&log), vec![local, shipped]);
        assert_eq!(record_for(root.path(), &source.key)["decision"], "merged");
        assert!(matches!(
            check_source(&source, &log, &record).expect("after accept"),
            CheckResult::Nothing
        ));
    }

    #[test]
    fn declining_holds_until_the_copy_changes() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        add_plan(root.path(), "plan0002");
        add_plan(root.path(), "plan0003");
        let local = route_row("w1", "plan0001");
        let shipped = route_row("w2", "plan0002");
        let more = route_row("w3", "plan0003");
        write_text(&bundle_path(root.path()), &format!("{shipped}\n"));
        let log = log_path(root.path());
        write_text(&log, &format!("{local}\n"));
        let source = project_source(root.path());
        let record = record_path(root.path());
        let offer = expect_offer(check_source(&source, &log, &record).expect("check"));
        assert_eq!(
            decide_offer(&source, &offer.sha256, false, &log, &record).expect("decline"),
            0
        );
        assert_eq!(read_lines(&log), vec![local.clone()]);
        assert_eq!(record_for(root.path(), &source.key)["decision"], "declined");
        assert!(matches!(
            check_source(&source, &log, &record).expect("same copy"),
            CheckResult::Nothing
        ));
        // The copy gains a row, and this project no longer holds a row of
        // its own: the declined version no longer applies, so it must ask.
        std::fs::remove_file(
            root.path()
                .join(".coding")
                .join("plans")
                .join("plan0001.md"),
        )
        .expect("drop plan");
        write_text(&bundle_path(root.path()), &format!("{shipped}\n{more}\n"));
        let offer = expect_offer(check_source(&source, &log, &record).expect("changed copy"));
        assert_eq!((offer.new_rows, offer.local_only_rows), (2, 0));
        assert_eq!(read_lines(&log), vec![local]);
    }

    #[test]
    fn a_changed_copy_refuses_the_old_answer() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        let shipped = route_row("w1", "plan0001");
        let more = route_row("w2", "plan0001");
        let bundle_text = format!("{shipped}\n");
        write_text(&bundle_path(root.path()), &bundle_text);
        let old_sha = sha256_hex(bundle_text.as_bytes());
        write_text(&bundle_path(root.path()), &format!("{shipped}\n{more}\n"));
        let local = route_row_without_plan("w3");
        let log = log_path(root.path());
        write_text(&log, &format!("{local}\n"));
        let source = project_source(root.path());
        let err = decide_offer(&source, &old_sha, true, &log, &record_path(root.path()))
            .expect_err("stale sha");
        assert!(err.contains("changed"), "{err}");
        assert_eq!(read_lines(&log), vec![local]);
    }

    #[test]
    fn append_repairs_a_missing_final_newline() {
        let root = project_root();
        add_plan(root.path(), "plan0001");
        let local = route_row_without_plan("w1");
        let shipped = route_row("w2", "plan0001");
        write_text(&bundle_path(root.path()), &format!("{shipped}\n"));
        let log = log_path(root.path());
        write_text(&log, &local);
        let source = project_source(root.path());
        assert!(matches!(
            check_source(&source, &log, &record_path(root.path())).expect("check"),
            CheckResult::Imported { rows: 1 }
        ));
        assert_eq!(
            std::fs::read_to_string(&log).expect("log"),
            format!("{local}\n{shipped}\n")
        );
    }

    #[test]
    fn sources_list_the_installer_first() {
        let root = tempfile::tempdir().expect("project root");
        let install = tempfile::tempdir().expect("install dir");
        let sources = learnings_sources(Some(install.path()), root.path());
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].key, "installer");
        assert_eq!(sources[0].label, "This Mnemo2 install");
        assert_eq!(
            sources[0].path,
            install.path().join("laya").join(LEARNINGS_FILE)
        );
        assert_eq!(sources[0].plans_dir, None);
        assert_eq!(sources[1].key, format!("project:{}", root.path().display()));
        assert_eq!(sources[1].label, "This project's repository");
        assert_eq!(
            sources[1].path,
            root.path().join(LEARNINGS_DIR).join(LEARNINGS_FILE)
        );
        assert_eq!(
            sources[1].plans_dir,
            Some(root.path().join(".coding").join("plans"))
        );
        let project_only = learnings_sources(None, root.path());
        assert_eq!(project_only.len(), 1);
        assert_eq!(project_only[0].key, sources[1].key);
    }
}
