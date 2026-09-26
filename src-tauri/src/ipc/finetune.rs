// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The startup failure-triage fine-tune (managed Laya runtime only).
//!
//! Gated on `[general.laya] auto_finetune` (Laya enabled + MANAGED mode +
//! the flag + the managed runtime and checkpoint installed — see
//! [`startup_finetune_checkpoint`]). At startup (after the managed sidecar
//! spawn) the gate spawns a background task that reads the failure-triage
//! training log (`~/.mnemo/laya/training/failure_triage.jsonl`, written by
//! `mnemo::agent::failure_triage` with one COMPLETE row per classified
//! failure), counts the labeled rows newer than the last-fine-tune marker
//! (the TRIGGER) and — at [`FINETUNE_MIN_NEW_ROWS`] or more — runs the
//! fine-tune through the managed venv on the FULL resolved corpus (the
//! marker gates the trigger, never the dataset — a retrain never discards
//! earlier labeled rows):
//!
//! - **success** → the marker is rewritten (the rows are spent), the managed
//!   server restarts against the fine-tuned artifact (`LAYA_MODELS` carries
//!   the artifact path — best-effort, see [`LayaManager::spawn_server`]) and
//!   the shared classifier slot is swapped to a client for the new port, so
//!   every consumer (failure triage, auto-typing, the Settings status) picks
//!   the fine-tuned checkpoint up. On a failed readiness probe the ORIGINAL
//!   checkpoint is restarted and re-swapped — the degradation is bounded to
//!   one attempt.
//! - **ineligible** (the installed `laya` package ships no training surface —
//!   0.3.20 ships none) → the labeled dataset stays exported under the
//!   managed dir for an external fine-tune and the run skips cleanly; the
//!   sidecar keeps serving the current checkpoint.
//! - **failure** → logged, skipped; the sidecar keeps serving.
//!
//! The task never blocks or panics the startup path: it is spawned after the
//! app is up and every failure path only logs. While it runs the classifier
//! status is `FineTuning { label }` — restored to `Ready` on any skip (only
//! a still-own `FineTuning` is ever overwritten).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, RwLock};

use tauri::{AppHandle, Emitter};

use mnemo::agent::failure_triage::{read_rows, training_log_path, FailureLogRow};
use mnemo::config::Config;
use mnemo::config::general::{LayaConfig, LayaMode};
use mnemo::memory::classifier::{Classifier, ClassifierStatus};

use super::laya::{
    build_managed_classifier, find_checkpoint, hide_console_window, start_managed_server,
    start_server_models, LayaManager,
};

/// The minimum number of NEW labeled rows (newer than the last-fine-tune
/// marker) that trigger a fine-tune at startup.
pub const FINETUNE_MIN_NEW_ROWS: u64 = 50;

/// The module the managed venv's python runs (`python -m laya.finetune …`).
/// Best-effort by design: the installed `laya` version may not ship a
/// training surface (0.3.20 ships none) — the runner then treats the run as
/// INELIGIBLE and skips cleanly (the dataset is exported either way).
const FINETUNE_MODULE: &str = "laya.finetune";

/// The exported fine-tune dataset (one `{ text, label }` JSONL row per
/// labeled failure) — written under the managed dir on every eligible run,
/// so an ineligible run still leaves the labeled set for an external
/// fine-tune.
pub fn dataset_path(manager: &LayaManager) -> PathBuf {
    manager
        .base_dir()
        .join("finetune")
        .join("failure_triage-dataset.jsonl")
}

/// Where a successful fine-tune writes its artifact (the path the restart
/// serves via `LAYA_MODELS`).
pub fn finetune_out_dir(manager: &LayaManager) -> PathBuf {
    manager.base_dir().join("finetune").join("out")
}

/// The fine-tune runner log (the python process's stdout+stderr go here).
fn finetune_log_path(manager: &LayaManager) -> PathBuf {
    manager.base_dir().join("finetune").join("finetune.log")
}

/// What the startup fine-tune decided (pure, testable).
#[derive(Debug, Clone, PartialEq)]
pub enum FinetuneDecision {
    /// Enough new labeled rows accrued — run the fine-tune. `new_rows` is
    /// the accrual since the last-fine-tune marker (the trigger); `dataset`
    /// is the FULL resolved corpus (every row with a disposition), so a
    /// retrain never discards earlier labeled rows.
    Run {
        /// How many new labeled rows accrued.
        new_rows: usize,
        /// The labeled rows for the run.
        dataset: Vec<FailureLogRow>,
    },
    /// Fewer new rows than [`FINETUNE_MIN_NEW_ROWS`] — wait for more.
    BelowThreshold {
        /// How many new labeled rows accrued so far.
        new_rows: usize,
    },
    /// The managed venv is missing — nothing to run with (a no-op).
    NoRuntime,
}

/// The startup fine-tune decision over the training log: the TRIGGER counts
/// the COMPLETE rows (the log writes one row per resolved failure —
/// `disposition` is always present in practice; the check is defensive)
/// newer than `last_finetune_ts` (`None` = never fine-tuned → every resolved
/// row counts), while the DATASET for a [`FinetuneDecision::Run`] is the
/// full resolved corpus. `runtime_present` is the managed venv's python
/// executable; without it the fine-tune is a no-op.
pub fn finetune_decision(
    rows: &[FailureLogRow],
    last_finetune_ts: Option<u64>,
    runtime_present: bool,
) -> FinetuneDecision {
    if !runtime_present {
        return FinetuneDecision::NoRuntime;
    }
    let new_rows = rows
        .iter()
        .filter(|r| r.disposition.is_some() && last_finetune_ts.is_none_or(|ts| r.ts > ts))
        .count();
    if (new_rows as u64) < FINETUNE_MIN_NEW_ROWS {
        FinetuneDecision::BelowThreshold { new_rows }
    } else {
        FinetuneDecision::Run {
            new_rows,
            dataset: rows
                .iter()
                .filter(|r| r.disposition.is_some())
                .cloned()
                .collect(),
        }
    }
}

/// Read the last-fine-tune marker ts. `None` = never fine-tuned — a missing,
/// empty, or unparseable marker reads as "never", so a corrupt marker must
/// not freeze the fine-tune forever.
pub fn read_finetune_marker(manager: &LayaManager, checkpoint_id: &str) -> Option<u64> {
    let text = std::fs::read_to_string(manager.finetune_marker_path(checkpoint_id)).ok()?;
    text.trim().parse::<u64>().ok()
}

/// Write the last-fine-tune marker (`ts` unix seconds — the training rows
/// newer than it form the next fine-tune's ≥50-row trigger; never its
/// dataset — the run retrains on the full resolved corpus).
pub fn write_finetune_marker(
    manager: &LayaManager,
    checkpoint_id: &str,
    ts: u64,
) -> std::io::Result<()> {
    let path = manager.finetune_marker_path(checkpoint_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, ts.to_string())
}

/// One exported fine-tune row: the neutral `{ text, label }` JSONL shape a
/// fine-tune runner (or a human) can consume — `text` is the error text the
/// classifier saw, `label` its class.
#[derive(Debug, serde::Serialize)]
struct DatasetRow<'a> {
    text: &'a str,
    label: &'a str,
}

/// Export `rows` as the fine-tune dataset JSONL at `path` (created as
/// needed). Returns the exported row count.
pub fn export_dataset(rows: &[FailureLogRow], path: &Path) -> std::io::Result<usize> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    for row in rows {
        let line = serde_json::to_string(&DatasetRow {
            text: &row.error_text,
            label: &row.class,
        })
        .expect("a two-string row always serializes");
        body.push_str(&line);
        body.push('\n');
    }
    std::fs::write(path, body)?;
    Ok(rows.len())
}

/// The python args for the fine-tune run (`python -m laya.finetune
/// --dataset <jsonl> --out <dir>`). Best-effort: the module may not exist in
/// the installed laya — the runner treats that as ineligible.
pub fn build_finetune_args(dataset: &Path, out_dir: &Path) -> Vec<String> {
    vec![
        "-m".to_string(),
        FINETUNE_MODULE.to_string(),
        "--dataset".to_string(),
        dataset.display().to_string(),
        "--out".to_string(),
        out_dir.display().to_string(),
    ]
}

/// The startup fine-tune gate: `Some(checkpoint_id)` when the fine-tune may
/// run — Laya enabled, MANAGED mode, `auto_finetune` on, and the managed
/// runtime + configured checkpoint installed. Anything else is `None` and
/// the startup hook spawns nothing (byte-identical startup). The checkpoint
/// id defaults to `"english"` exactly like the managed start does.
pub fn startup_finetune_checkpoint(laya: &LayaConfig, manager: &LayaManager) -> Option<String> {
    if !laya.enabled || !laya.auto_finetune || laya.mode != LayaMode::Managed {
        return None;
    }
    let id = laya
        .checkpoint
        .clone()
        .unwrap_or_else(|| "english".to_string());
    if !manager.is_checkpoint_installed(&id) {
        return None;
    }
    Some(id)
}

/// Set + emit a classifier status (the same discipline as the setup/start
/// writers in `laya.rs`).
fn set_status(
    app: &Option<AppHandle>,
    status: &Arc<RwLock<ClassifierStatus>>,
    s: ClassifierStatus,
) {
    *status.write().expect("classifier status lock poisoned") = s.clone();
    if let Some(app) = app {
        let _ = app.emit("classifier://status", &s);
    }
}

/// On a skip path, hand the status back to `Ready` — but ONLY when it is
/// still our own `FineTuning` (a concurrent writer's state is never
/// overwritten; the lock is dropped before the emit).
fn restore_status_if_ours(app: &Option<AppHandle>, status: &Arc<RwLock<ClassifierStatus>>) {
    let s = {
        let mut guard = status.write().expect("classifier status lock poisoned");
        if matches!(*guard, ClassifierStatus::FineTuning { .. }) {
            let s = ClassifierStatus::Ready;
            *guard = s.clone();
            s
        } else {
            return;
        }
    };
    if let Some(app) = app {
        let _ = app.emit("classifier://status", &s);
    }
}

/// Whether the classifier slot STILL holds what the fine-tune task observed
/// at its start — the swap-side only-if-own discipline: a Settings save that
/// re-wired the classifier during the run is never stomped by the hot-swap.
fn slot_still_holds(
    classifier_slot: &Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    observed: &Option<Arc<dyn Classifier>>,
) -> bool {
    let guard = classifier_slot
        .read()
        .expect("classifier slot lock poisoned");
    match (&*guard, observed) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

/// How the fine-tune run ended.
enum FinetuneOutcome {
    /// The module ran and produced an artifact — the caller restarts the
    /// sidecar against it.
    Served,
    /// The installed laya ships no training surface (the run reported a
    /// missing module) — the dataset stays exported and the run skips
    /// cleanly.
    Ineligible(String),
    /// Anything else that went wrong (spawn failure, crash, no artifact).
    Failed(String),
}

/// Run `python -m laya.finetune …` through the managed venv. The caller
/// runs this on the blocking pool (`tokio::task::spawn_blocking`) — the
/// child's `wait()` blocks for the run's full duration and must never
/// occupy an async worker. stdout/stderr go to the finetune log file.
/// Classifies the exit:
/// a zero exit WITH artifacts in `out_dir` is
/// [`FinetuneOutcome::Served`]; a "No module named" crash is ineligible;
/// everything else failed.
fn run_finetune_process(
    manager: &LayaManager,
    dataset: &Path,
    out_dir: &Path,
    exported_rows: usize,
) -> FinetuneOutcome {
    let log_path = finetune_log_path(manager);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &log_path,
        format!(
            "failure-triage fine-tune run @ {} — {exported_rows} rows, dataset {}\n",
            now_unix_secs(),
            dataset.display()
        ),
    );
    let Ok(log) = std::fs::OpenOptions::new().append(true).open(&log_path) else {
        return FinetuneOutcome::Failed(format!(
            "opening the fine-tune log {} failed",
            log_path.display()
        ));
    };
    let mut cmd = Command::new(manager.venv_python());
    cmd.args(build_finetune_args(dataset, out_dir))
        .stdout(Stdio::from(
            log.try_clone().expect("the log handle always clones"),
        ))
        .stderr(Stdio::from(log));
    hide_console_window(&mut cmd);
    let Ok(mut child) = cmd.spawn() else {
        return FinetuneOutcome::Failed(format!(
            "spawning {} failed — see {}",
            manager.venv_python().display(),
            log_path.display()
        ));
    };
    let status = match child.wait() {
        Ok(s) => s,
        Err(e) => {
            return FinetuneOutcome::Failed(format!(
                "waiting on the fine-tune failed: {e}"
            ))
        }
    };
    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log_path) {
        let _ = writeln!(f, "\nexit: {status}");
    }
    if !status.success() {
        let log_text = std::fs::read_to_string(&log_path).unwrap_or_default();
        if log_text.contains("No module named") || log_text.contains("no module named") {
            return FinetuneOutcome::Ineligible(format!(
                "the installed laya ships no `{FINETUNE_MODULE}` module"
            ));
        }
        return FinetuneOutcome::Failed(format!(
            "the fine-tune exited with {status} — see {}",
            log_path.display()
        ));
    }
    let produced = std::fs::read_dir(out_dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if produced {
        FinetuneOutcome::Served
    } else {
        FinetuneOutcome::Failed(format!(
            "the fine-tune exited cleanly but wrote nothing to {}",
            out_dir.display()
        ))
    }
}

/// Restart the managed sidecar against the fine-tuned artifact and swap the
/// shared classifier slot to a client for it (every consumer of the slot —
/// failure triage, auto-typing — picks the fine-tuned checkpoint up). On a
/// failed readiness probe the ORIGINAL checkpoint is restarted and
/// re-swapped: the failed start killed the previous sidecar (spawn stops
/// before spawning), so the degradation is bounded to one attempt. Returns
/// whether the fine-tuned checkpoint is live.
async fn restart_and_swap(
    app: &Option<AppHandle>,
    manager: &Arc<LayaManager>,
    classifier_slot: &Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    observed_slot: &Option<Arc<dyn Classifier>>,
    status: &Arc<RwLock<ClassifierStatus>>,
    fine_tuned: &Path,
    original_checkpoint: &str,
) -> bool {
    // The rewire check FIRST: if a Settings save re-wired the classifier
    // during the run, that choice is authoritative — starting the fine-tuned
    // server would kill the rewire's fresh sidecar and the swap would stomp
    // its client (the same only-if-own discipline as the status restore).
    if !slot_still_holds(classifier_slot, observed_slot) {
        eprintln!(
            "info: failure-triage hot-swap skipped — the classifier was \
             re-wired during the fine-tune; that choice stays live"
        );
        return false;
    }
    let Some(port) = LayaManager::alloc_free_port() else {
        eprintln!("error: failure-triage fine-tune swap skipped — no free loopback port");
        restore_status_if_ours(app, status);
        return false;
    };
    // Serve the fine-tuned artifact (best-effort: LAYA_MODELS carries the
    // artifact path; a laya-serve that cannot load it simply never gets
    // ready, and the restore below re-serves the original checkpoint).
    if start_server_models(
        app,
        manager.clone(),
        &fine_tuned.display().to_string(),
        port,
        None,
    )
    .await
    .is_none()
    {
        eprintln!(
            "error: the fine-tuned checkpoint never got ready — restoring \
             {original_checkpoint}"
        );
        if let Some(restore_port) = LayaManager::alloc_free_port() {
            let checkpoint = find_checkpoint(original_checkpoint);
            if start_managed_server(app, manager.clone(), checkpoint, restore_port)
                .await
                .is_some()
            {
                if let Some(client) = build_managed_classifier(restore_port, status.clone()) {
                    if slot_still_holds(classifier_slot, observed_slot) {
                        *classifier_slot
                            .write()
                            .expect("classifier slot lock poisoned") = Some(client);
                    }
                }
            }
        }
        return false;
    }
    let Some(client) = build_managed_classifier(port, status.clone()) else {
        restore_status_if_ours(app, status);
        return false;
    };
    // Re-check at the write: a rewire landing during the readiness probe
    // keeps its client (ours is dropped — the next save or startup re-check
    // converges).
    if !slot_still_holds(classifier_slot, observed_slot) {
        eprintln!(
            "info: failure-triage swap skipped — the classifier was re-wired \
             during the fine-tune; that choice stays live"
        );
        return false;
    }
    *classifier_slot
        .write()
        .expect("classifier slot lock poisoned") = Some(client);
    true
}

/// Run the startup fine-tune to completion in the background (the spawned
/// task body): gate on the LIVE `[general.laya]` config, then count → maybe
/// run → hot-swap. NEVER panics and never blocks startup — everything runs
/// after the app is up; every failure path only logs.
pub async fn run_startup_finetune(
    app: Option<AppHandle>,
    manager: Arc<LayaManager>,
    classifier_slot: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    status: Arc<RwLock<ClassifierStatus>>,
    config: Arc<tokio::sync::Mutex<Config>>,
) {
    // The gate, read from the LIVE shared config (the same handle settings
    // saves go through): enabled + managed mode + `auto_finetune` + the
    // managed runtime and checkpoint installed. Anything else is a no-op —
    // byte-identical startup.
    let laya_cfg = config.lock().await.general.general.laya.clone();
    let Some(checkpoint_id) = startup_finetune_checkpoint(&laya_cfg, &manager) else {
        return;
    };

    // The slot snapshot at task start: the hot-swap writes only when the
    // slot STILL holds what we observed here — a Settings save re-wiring
    // the classifier during the run is never stomped (mirrors the status
    // restore's only-if-own discipline).
    let observed_slot = classifier_slot
        .read()
        .expect("classifier slot lock poisoned")
        .clone();

    // The decision — over the default training log (the production path;
    // tests drive the decision function directly with their own row sets).
    let rows = read_rows(&training_log_path());
    let last = read_finetune_marker(&manager, &checkpoint_id);
    let runtime_present = manager.venv_python().is_file();
    let (new_rows, dataset) = match finetune_decision(&rows, last, runtime_present) {
        FinetuneDecision::Run { new_rows, dataset } => (new_rows, dataset),
        FinetuneDecision::BelowThreshold { new_rows } => {
            eprintln!(
                "info: failure-triage fine-tune skipped — {new_rows} new labeled rows \
                 (threshold {FINETUNE_MIN_NEW_ROWS})"
            );
            return;
        }
        FinetuneDecision::NoRuntime => {
            eprintln!(
                "info: failure-triage fine-tune skipped — the managed venv is missing"
            );
            return;
        }
    };
    eprintln!(
        "info: failure-triage fine-tune starting — {new_rows} new labeled rows"
    );
    set_status(
        &app,
        &status,
        ClassifierStatus::FineTuning {
            label: checkpoint_id.clone(),
        },
    );

    // Export the dataset (an ineligible run still leaves it for an external
    // fine-tune) and run the fine-tune through the managed venv.
    let dataset_path = dataset_path(&manager);
    let out_dir = finetune_out_dir(&manager);
    let outcome = match export_dataset(&dataset, &dataset_path) {
        Ok(n) => {
            // The python run blocks for its full duration (minutes) — hand
            // it to the blocking pool so no async worker is occupied.
            let run_manager = manager.clone();
            let run_dataset = dataset_path.clone();
            let run_out = out_dir.clone();
            match tokio::task::spawn_blocking(move || {
                run_finetune_process(&run_manager, &run_dataset, &run_out, n)
            })
            .await
            {
                Ok(outcome) => outcome,
                Err(e) => {
                    FinetuneOutcome::Failed(format!("the fine-tune task failed: {e}"))
                }
            }
        }
        Err(e) => FinetuneOutcome::Failed(format!("exporting the dataset failed: {e}")),
    };

    match outcome {
        FinetuneOutcome::Ineligible(hint) => {
            eprintln!(
                "info: failure-triage fine-tune ineligible — {hint}; the labeled dataset \
                 is exported at {}",
                dataset_path.display()
            );
            restore_status_if_ours(&app, &status);
        }
        FinetuneOutcome::Failed(hint) => {
            eprintln!("error: failure-triage fine-tune failed — {hint}");
            restore_status_if_ours(&app, &status);
        }
        FinetuneOutcome::Served => {
            // Marker first: even if the restart fails, these rows are spent
            // (re-training on them would double-count). A failed marker
            // write is ignored — the next run would just re-train on the
            // same rows, which is harmless.
            let _ = write_finetune_marker(&manager, &checkpoint_id, now_unix_secs());
            let swapped = restart_and_swap(
                &app,
                &manager,
                &classifier_slot,
                &observed_slot,
                &status,
                &out_dir,
                &checkpoint_id,
            )
            .await;
            if swapped {
                eprintln!(
                    "info: failure-triage fine-tune complete — the fine-tuned checkpoint \
                     is live on the managed sidecar"
                );
            }
            // A failed restart already restored the original checkpoint and
            // drove the status itself (Starting → Ready/Failed).
        }
    }
}

/// Spawn the startup fine-tune (the startup hook calls this at every
/// startup): the spawned task reads the live `[general.laya]` config and
/// runs the gate — a no-op unless it opens (Laya enabled, managed mode,
/// `auto_finetune` on, and the managed runtime + configured checkpoint
/// installed). The task never blocks startup.
pub fn spawn_startup_finetune(
    app: Option<AppHandle>,
    manager: Arc<LayaManager>,
    classifier_slot: Arc<RwLock<Option<Arc<dyn Classifier>>>>,
    status: Arc<RwLock<ClassifierStatus>>,
    config: Arc<tokio::sync::Mutex<Config>>,
) {
    tauri::async_runtime::spawn(run_startup_finetune(
        app,
        manager,
        classifier_slot,
        status,
        config,
    ));
}

/// Unix seconds now (0 when the clock is before the epoch — a marker or log
/// timestamp must never panic).
fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: u64, class: &str, disposition: Option<&str>) -> FailureLogRow {
        FailureLogRow {
            ts,
            site: "tool_batch".into(),
            tool: Some("file_read".into()),
            error_text: format!("err {ts}"),
            class: class.into(),
            confidence: 0.9,
            action: "guidance".into(),
            disposition: disposition.map(str::to_string),
        }
    }

    fn test_manager(dir: &std::path::Path) -> LayaManager {
        LayaManager::with_base(
            dir.to_path_buf(),
            Arc::new(RwLock::new(ClassifierStatus::Disabled)),
        )
    }

    #[test]
    fn no_runtime_is_a_noop_even_with_enough_rows() {
        let rows: Vec<FailureLogRow> = (0..100)
            .map(|i| row(i, "transient", Some("retry_succeeded")))
            .collect();
        assert_eq!(
            finetune_decision(&rows, None, false),
            FinetuneDecision::NoRuntime
        );
    }

    #[test]
    fn trigger_counts_rows_newer_than_the_marker_but_the_dataset_is_the_full_corpus() {
        let rows: Vec<FailureLogRow> = (0..60)
            .map(|i| row(i, "transient", Some("retry_succeeded")))
            .collect();
        // Marker at ts 9 → 50 rows (10..=59) newer → exactly the trigger —
        // but the dataset is the FULL resolved corpus (all 60 rows), never
        // the increment-only slice.
        assert_eq!(
            finetune_decision(&rows, Some(9), true),
            FinetuneDecision::Run {
                new_rows: 50,
                dataset: rows.clone(),
            }
        );
        // One fewer → below the threshold, no run.
        assert_eq!(
            finetune_decision(&rows[..59], Some(9), true),
            FinetuneDecision::BelowThreshold { new_rows: 49 }
        );
    }

    #[test]
    fn no_marker_counts_resolved_rows_only() {
        let mut rows: Vec<FailureLogRow> = (0..50)
            .map(|i| row(i, "transient", Some("retry_succeeded")))
            .collect();
        // An unresolved row never counts (the log never writes one, this is
        // the defensive branch).
        rows.push(row(999, "permanent", None));
        assert_eq!(
            finetune_decision(&rows, None, true),
            FinetuneDecision::Run {
                new_rows: 50,
                dataset: rows[..50].to_vec(),
            }
        );
    }

    #[test]
    fn marker_round_trips_and_corruption_reads_as_never() {
        let dir = tempfile::tempdir().unwrap();
        let manager = test_manager(dir.path());
        assert_eq!(read_finetune_marker(&manager, "english"), None);
        write_finetune_marker(&manager, "english", 42).unwrap();
        assert_eq!(read_finetune_marker(&manager, "english"), Some(42));
        std::fs::write(manager.finetune_marker_path("english"), "not-a-number").unwrap();
        assert_eq!(read_finetune_marker(&manager, "english"), None);
    }

    #[test]
    fn finetune_marker_path_is_per_checkpoint_under_markers() {
        let dir = tempfile::tempdir().unwrap();
        let manager = test_manager(dir.path());
        assert_eq!(
            manager.finetune_marker_path("english"),
            dir.path().join("markers").join("finetune-english")
        );
    }

    #[test]
    fn export_dataset_writes_one_text_label_row_per_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("dataset.jsonl");
        let rows = vec![
            row(1, "transient", Some("retry_succeeded")),
            row(2, "needs_user", Some("escalated")),
        ];
        assert_eq!(export_dataset(&rows, &path).unwrap(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["text"], "err 1");
        assert_eq!(first["label"], "transient");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["label"], "needs_user");
    }

    #[test]
    fn finetune_args_carry_module_dataset_and_out() {
        let args = build_finetune_args(Path::new("d.jsonl"), Path::new("out"));
        assert_eq!(
            args,
            vec![
                "-m",
                "laya.finetune",
                "--dataset",
                "d.jsonl",
                "--out",
                "out"
            ]
        );
    }

    #[test]
    fn startup_gate_requires_enabled_managed_flag_and_install() {
        let dir = tempfile::tempdir().unwrap();
        let manager = test_manager(dir.path());
        let mut laya = LayaConfig::default();
        // Everything off by default → no fine-tune.
        assert_eq!(startup_finetune_checkpoint(&laya, &manager), None);
        // The flag on but external mode → still nothing.
        laya.enabled = true;
        laya.auto_finetune = true;
        assert_eq!(startup_finetune_checkpoint(&laya, &manager), None);
        // Managed mode but nothing installed → nothing.
        laya.mode = LayaMode::Managed;
        assert_eq!(startup_finetune_checkpoint(&laya, &manager), None);
        // Install the runtime (venv launcher) + the checkpoint marker → the
        // gate opens on the default id.
        std::fs::create_dir_all(manager.laya_serve_path().parent().unwrap()).unwrap();
        std::fs::write(manager.laya_serve_path(), "").unwrap();
        std::fs::create_dir_all(manager.base_dir().join("markers")).unwrap();
        std::fs::write(manager.base_dir().join("markers").join("english"), "").unwrap();
        assert_eq!(
            startup_finetune_checkpoint(&laya, &manager),
            Some("english".to_string())
        );
        // An explicitly configured checkpoint is honored (and must itself be
        // installed).
        laya.checkpoint = Some("multilingual".into());
        assert_eq!(startup_finetune_checkpoint(&laya, &manager), None);
        std::fs::write(
            manager.base_dir().join("markers").join("multilingual"),
            "",
        )
        .unwrap();
        assert_eq!(
            startup_finetune_checkpoint(&laya, &manager),
            Some("multilingual".to_string())
        );
    }
}