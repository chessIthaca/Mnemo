// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! `memory_git_state_sweep` — run the step-3 git/plan-state sweep against a
//! COPY of a project's memory store (plan 373dec00).
//!
//! [`quarantine_git_state_facts`] retires semantic, authored, untyped facts
//! that only restate what git or the plan board already shows. The startup
//! path runs it automatically; this example exists so the candidate set can
//! be read and audited BEFORE a merge. It copies `<coding>/memory.db` (plus
//! its `-wal` / `-shm` sidecars) and `<coding>/knowledge` into `--out-dir`,
//! opens the COPY — the live store is only ever read, never written to — and
//! runs the sweep there. The quarantined rows are written to `--candidates`
//! as JSON lines (`id`, `title`, `content`).
//!
//! Usage, from the repo root:
//!
//! ```text
//! cargo run --example memory_git_state_sweep -- \
//!     --coding C:\Mnemo\.coding \
//!     --out-dir .coding\tmp\swept-coding \
//!     --candidates .coding\tmp\git_state_candidates.jsonl
//! ```
//!
//! `--out-dir` is also a valid `--coding` argument for `mnemo2_memory_eval`:
//! the copy carries the swept store and the knowledge files the eval's
//! control pass resolves its expected titles from.
//!
//! An optional `--survey <file>` writes every semantic row of the copy with
//! its eligibility flags and whether the matcher selected it — the audit
//! trail behind a matcher change.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use mnemo::memory::integrity::{quarantine_git_state_facts, restates_repo_state};
use mnemo::memory::{
    HashEmbedder, Memory, MemoryFilter, MemoryStore, MemoryStoreTrait, MemoryTier,
};
use serde_json::json;

/// The store file plus the SQLite sidecars a copy has to carry with it.
const STORE_FILES: [&str; 3] = ["memory.db", "memory.db-wal", "memory.db-shm"];

#[tokio::main]
async fn main() -> Result<(), String> {
    let options = Options::parse(std::env::args().skip(1))?;
    copy_store(&options.coding, &options.out_dir)?;
    let store = MemoryStore::open(
        &options.out_dir.join("memory.db"),
        Arc::new(HashEmbedder::new()),
    )
    .map_err(|e| format!("open store copy: {e}"))?;

    if let Some(survey_path) = &options.survey {
        write_survey(&store, survey_path).await?;
    }

    let quarantined = quarantine_git_state_facts(&store)
        .await
        .map_err(|e| format!("git-state sweep: {e}"))?;
    write_candidates(&store, &quarantined, &options.candidates).await?;

    println!(
        "git-state sweep: quarantined {} row(s) in the copy; candidates written to {}",
        quarantined.len(),
        options.candidates.display()
    );
    Ok(())
}

/// The command line: where the live store is read from, where the swept copy
/// and its candidate list are written, and the optional survey file.
struct Options {
    coding: PathBuf,
    out_dir: PathBuf,
    candidates: PathBuf,
    survey: Option<PathBuf>,
}

impl Options {
    /// Parse `--flag value` pairs; unknown flags are an error, never ignored.
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut coding = None;
        let mut out_dir = None;
        let mut candidates = None;
        let mut survey = None;
        let mut args = args;
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
            match flag.as_str() {
                "--coding" => coding = Some(PathBuf::from(value)),
                "--out-dir" => out_dir = Some(PathBuf::from(value)),
                "--candidates" => candidates = Some(PathBuf::from(value)),
                "--survey" => survey = Some(PathBuf::from(value)),
                other => {
                    return Err(format!(
                        "unknown argument {other:?} (expected --coding, --out-dir, \
                         --candidates, --survey)"
                    ))
                }
            }
        }
        Ok(Self {
            coding: coding.ok_or("--coding is required")?,
            out_dir: out_dir.ok_or("--out-dir is required")?,
            candidates: candidates.ok_or("--candidates is required")?,
            survey,
        })
    }
}

/// Copy the store and its knowledge files into `out_dir`, refusing to target
/// the live store itself.
fn copy_store(coding: &Path, out_dir: &Path) -> Result<(), String> {
    let source_db = coding.join("memory.db");
    if !source_db.is_file() {
        return Err(format!(
            "{} does not exist — point --coding at a project's .coding directory",
            source_db.display()
        ));
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("create {}: {e}", out_dir.display()))?;
    let coding =
        std::fs::canonicalize(coding).map_err(|e| format!("resolve {}: {e}", coding.display()))?;
    let out_dir = std::fs::canonicalize(out_dir)
        .map_err(|e| format!("resolve {}: {e}", out_dir.display()))?;
    if coding == out_dir {
        return Err("--out-dir must be a COPY — refusing to sweep the live store in place".into());
    }
    for name in STORE_FILES {
        let from = coding.join(name);
        if from.is_file() {
            std::fs::copy(&from, out_dir.join(name))
                .map_err(|e| format!("copy {}: {e}", from.display()))?;
        }
    }
    let knowledge = coding.join("knowledge");
    if knowledge.is_dir() {
        copy_dir(&knowledge, &out_dir.join("knowledge"))?;
    }
    Ok(())
}

/// Recursively copy a directory (the eval's control pass reads the expected
/// titles from `knowledge/`).
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("read {}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("read {}: {e}", from.display()))?;
        let target = to.join(entry.file_name());
        if entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", entry.path().display()))?
            .is_dir()
        {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("copy {}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Write every semantic row with its eligibility flags and the matcher's
/// verdict, so a matcher change can be audited against the store it ran on.
async fn write_survey(store: &MemoryStore, path: &Path) -> Result<(), String> {
    let rows = store
        .list_filtered(&MemoryFilter::new().tier(MemoryTier::Semantic))
        .await
        .map_err(|e| format!("list semantic rows: {e}"))?;
    let mut out = String::new();
    for row in &rows {
        let line = json!({
            "id": row.id,
            "title": row.title,
            "content": row.content,
            "access_count": row.access_count,
            "record_class": row.record_class.as_str(),
            "record_type": row.record_type.as_str(),
            "data": row.data,
            "matched": restates_repo_state(&row.content),
        });
        out.push_str(&line.to_string());
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Write the swept rows (`id`, `title`, `content`) as JSON lines — the
/// sample the lead audits before a merge.
async fn write_candidates(store: &MemoryStore, ids: &[String], path: &Path) -> Result<(), String> {
    let mut out = String::new();
    for id in ids {
        let row: Memory = store
            .get_memory(id)
            .await
            .map_err(|e| format!("read quarantined row {id}: {e}"))?
            .ok_or_else(|| format!("quarantined row {id} disappeared"))?;
        let line = json!({"id": row.id, "title": row.title, "content": row.content});
        out.push_str(&line.to_string());
        out.push('\n');
    }
    std::fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))
}
