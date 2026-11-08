// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `team.json` ledger: the persisted shape, its backup-recoverable
//! store, and the legacy tier-id conversion.
//!
//! Two instances can share one state dir through "Open anyway"
//! (`project::attach(root, true)`). Between them `team.json` is
//! last-writer-wins, as it always was, and a worker one instance never knew is
//! safe from the other's saves. Two limits are accepted: driving the *same*
//! worker from both instances means the last replaced transcript wins, and a
//! save does not notice foreign bytes behind an unchanged `Arc`; and deleting a
//! worker in one instance while another saves it can leave that record without
//! a transcript file, which load reports on the record's `error`. The old
//! format was worse on both counts, since any stale save replaced every
//! worker's transcript.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use mnemo::config::endpoints::TierLadder;
use mnemo::headless::Transcript;
use serde::{Deserialize, Serialize};

use crate::mnemo2::dispatch_routing::DispatchRoutingRow;
use crate::mnemo2::usage::ProjectUsage;

use super::record::WorkerRecord;
use super::DEFAULT_SLOTS;

/// The persisted worker ledger inside the team's state directory.
pub(super) const TEAM_STATE_FILE: &str = "team.json";
/// Original bytes preserved before the one-time legacy tier-id conversion.
pub(super) const TEAM_PRE_TIERS_BACKUP: &str = "team.json.pre-tiers.bak";
/// The longest a telemetry-only change (usage counters, context size) waits
/// before it is written.
pub(super) const LEDGER_FLUSH_DELAY: std::time::Duration = std::time::Duration::from_millis(1000);
/// The sub-directory (beside `team.json`) holding one transcript file per
/// worker: `<worker_id>.json`.
pub(super) const TRANSCRIPTS_DIR: &str = "transcripts";
/// How old a save temp file must be before a sweep removes it: two
/// [`TeamPersistence`] instances can share one state dir - "Open anyway"
/// attaches a second instance to the same project, and a test can run a second
/// runtime on it - so a young temp may belong to a save that is between its
/// write and its rename right now.
const TEMP_SWEEP_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// The persisted team ledger.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct PersistedTeam {
    pub(super) workers: Vec<WorkerRecord>,
    #[serde(default)]
    pub(super) slots: Option<usize>,
    /// Raw per-model usage accumulated from live lead and worker events.
    /// The `#[serde(default)]` keeps ledgers written before this field
    /// (legacy `team.json`) decodable, starting them at an empty accumulator.
    #[serde(default)]
    pub(super) usage: ProjectUsage,
}

/// Distinguishes the temp files of concurrent savers (two runtimes on one
/// state dir, or a cleanup racing a pump save): a shared temp name lets one
/// saver consume the other's temp file and fail its rename.
static SAVE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// The backup-recoverable `team.json` store.
#[derive(Clone, Debug)]
pub(super) struct TeamPersistence {
    pub(super) path: PathBuf,
    pub(super) slots: Arc<AtomicUsize>,
    /// Set while a coalesced telemetry flush is pending (see
    /// [`save_workers_soon`](super::pump::save_workers_soon)).
    pub(super) flush_scheduled: Arc<AtomicBool>,
    /// Set once the runtime shuts down, voiding a pending telemetry flush so
    /// the final persist stays the last ledger write.
    pub(super) closed: Arc<AtomicBool>,
    /// Per worker id, the transcript `Arc` last written to (or read from) that
    /// worker's file. Holding the `Arc` is what makes `Arc::ptr_eq` a sound
    /// "unchanged" test: the comparing code always holds it too.
    written: Arc<Mutex<HashMap<String, Arc<Transcript>>>>,
}

impl TeamPersistence {
    pub(super) fn new(state_dir: &std::path::Path) -> Self {
        Self {
            path: state_dir.join(TEAM_STATE_FILE),
            slots: Arc::new(AtomicUsize::new(DEFAULT_SLOTS)),
            flush_scheduled: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
            written: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The directory holding the per-worker transcript files.
    fn transcripts_dir(&self) -> Option<PathBuf> {
        Some(self.path.parent()?.join(TRANSCRIPTS_DIR))
    }

    /// The file holding one worker's transcript. A worker id that could not be
    /// a file stem (not ASCII alphanumeric, or shorter than the validated
    /// minimum) keeps its transcript embedded in `team.json` instead.
    fn transcript_file(&self, worker_id: &str) -> Option<PathBuf> {
        let usable = worker_id.len() >= 8
            && worker_id
                .chars()
                .all(|character| character.is_ascii_alphanumeric());
        if !usable {
            return None;
        }
        Some(self.transcripts_dir()?.join(format!("{worker_id}.json")))
    }

    fn decode(path: &std::path::Path) -> Result<Option<PersistedTeam>, String> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(format!("could not read {}: {err}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| format!("could not decode {}: {err}", path.display()))
    }

    /// Remove temp files a crashed save left behind (`team.json.<pid>.<seq>.tmp`).
    /// The name is unique per save, so only a crash leaves one; a young one may
    /// belong to a save running right now in another [`TeamPersistence`] over
    /// the same state dir ("Open anyway" lets two instances share a project),
    /// so it is left alone.
    fn sweep_stale_temps(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(parent) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with("team.json.") && name.ends_with(".tmp") && !is_young(&entry) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Load each worker's transcript from its own file. The file wins over the
    /// embedded copy. A missing file is a ledger written before transcripts
    /// moved out: the embedded transcript stays, and is not recorded, so the
    /// next save writes the file and strips `team.json`.
    ///
    /// A file that cannot be read fails the load: `team.json` keeps only a
    /// placeholder, so the save that follows a load would erase the worker's
    /// conversation with it. A file that reads but does not decode is kept
    /// aside as `<id>.json.corrupt` - its bytes are never destroyed, and a
    /// rename that cannot do that fails the load too - logged, and rewritten by
    /// the next save. A worker that ran whose file is gone is flagged on the
    /// record instead: one lost file must not block the project.
    fn attach_transcripts(&self, team: &mut PersistedTeam) -> Result<(), String> {
        for worker in &mut team.workers {
            let Some(file) = self.transcript_file(&worker.id) else {
                continue;
            };
            match file.try_exists() {
                Ok(true) => {}
                Ok(false) => {
                    // A file the ledger says held saved messages is gone: the
                    // conversation is lost. An empty file would hide it, so the
                    // record carries it instead.
                    if worker.saved_messages > 0 {
                        let path = format!("{TRANSCRIPTS_DIR}/{}.json", worker.id);
                        let missing =
                            format!("this worker's saved conversation is missing ({path})");
                        eprintln!("team: {missing}");
                        if worker.error.is_none() {
                            worker.error = Some(missing);
                        }
                    }
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "could not inspect the transcript for {}: {error}",
                        worker.id
                    ));
                }
            }
            let bytes = retry_io(|| std::fs::read(&file)).map_err(|err| {
                format!("could not read the transcript for {}: {err}", worker.id)
            })?;
            match serde_json::from_slice(&bytes) {
                Ok(transcript) => {
                    worker.transcript = Arc::new(transcript);
                    self.written_guard()
                        .insert(worker.id.clone(), worker.transcript.clone());
                }
                Err(error) => {
                    let kept = corrupt_path(&file);
                    retry_io(|| std::fs::rename(&file, &kept)).map_err(|err| {
                        format!(
                            "could not keep the damaged transcript for {} aside: {err}",
                            worker.id
                        )
                    })?;
                    eprintln!(
                        "team: transcript for {} could not be decoded ({error}); kept as {}",
                        worker.id,
                        kept.display()
                    );
                    // The embedded copy stands, unrecorded, so the next save
                    // writes a valid file (or none, when it is empty).
                }
            }
        }
        // The count is only meaningful on disk: the in-memory record and every
        // IPC snapshot start from zero again.
        for worker in &mut team.workers {
            worker.saved_messages = 0;
        }
        Ok(())
    }

    pub(super) fn load(&self) -> Result<Option<PersistedTeam>, String> {
        self.sweep_stale_temps();
        let backup = self.path.with_extension("json.bak");
        let loaded = match Self::decode(&self.path) {
            Ok(Some(team)) => Some((team, self.path.clone())),
            Ok(None) => Self::decode(&backup)?.map(|team| (team, backup.clone())),
            Err(primary_error) => match Self::decode(&backup) {
                Ok(Some(team)) => Some((team, backup.clone())),
                Ok(None) => return Err(primary_error),
                Err(backup_error) => {
                    return Err(format!(
                        "{primary_error}; backup also failed: {backup_error}"
                    ));
                }
            },
        };
        let Some((mut team, source)) = loaded else {
            return Ok(None);
        };
        // Before the conversion save below: writing first would overwrite the
        // newer transcript files of a legacy ledger with empty placeholders.
        self.attach_transcripts(&mut team)?;
        if convert_legacy_team_tiers(&mut team) {
            let pre_tiers = self.path.with_file_name(TEAM_PRE_TIERS_BACKUP);
            if !pre_tiers.exists() {
                std::fs::copy(&source, &pre_tiers).map_err(|error| {
                    format!(
                        "could not preserve legacy team ledger as {}: {error}",
                        pre_tiers.display()
                    )
                })?;
            }
            self.save(&team)?;
        }
        Ok(Some(team))
    }

    pub(super) fn save(&self, team: &PersistedTeam) -> Result<(), String> {
        let Some(state_dir) = self.path.parent() else {
            return Err("the team ledger path has no state directory".to_string());
        };
        std::fs::create_dir_all(state_dir)
            .map_err(|err| format!("could not create the team state dir: {err}"))?;
        std::fs::create_dir_all(state_dir.join(TRANSCRIPTS_DIR))
            .map_err(|err| format!("could not create the transcript dir: {err}"))?;
        // The transcript files land first: a failure here leaves the ledger
        // itself untouched, and a crash between the two leaves a transcript
        // file that a load prefers over the (older) embedded placeholder.
        self.write_transcripts(team)?;
        let ledger = self.ledger_without_transcripts(team);
        let bytes = serde_json::to_vec(&ledger)
            .map_err(|err| format!("could not encode the team ledger: {err}"))?;
        let temp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        retry_io(|| std::fs::write(&temp, &bytes))
            .map_err(|err| format!("could not write the team ledger: {err}"))?;
        // Replace the ledger, never move it aside first: `rename` swaps the
        // target atomically, so a reader (a concurrent `decode`) can never find
        // `team.json` missing and read that as "no ledger".
        let backup = self.path.with_extension("json.bak");
        let result = match retry_io(|| std::fs::rename(&temp, &self.path)) {
            Ok(()) => {
                // A backup only survives an older build's crash or a failed
                // save of ours; the replaced ledger makes it redundant.
                let _ = std::fs::remove_file(&backup);
                Ok(())
            }
            Err(err) => {
                // The ledger itself was never touched and still stands.
                let _ = std::fs::remove_file(&temp);
                Err(format!("could not replace the team ledger: {err}"))
            }
        };
        if result.is_ok() {
            // Only once team.json names the removed workers are their files
            // unreachable: a save that did not replace the ledger still lists
            // them.
            self.sweep_orphan_transcripts(team);
        }
        result
    }

    /// Write the transcript files of the workers whose transcript `Arc` changed
    /// since the last write (or read) of that worker's file, and of those whose
    /// file has since disappeared. `written` is not the sole authority: the
    /// ledger keeps only a placeholder, so a missing file means a lost worker
    /// conversation. A worker with no transcript file keeps its transcript in
    /// `team.json` and is skipped.
    fn write_transcripts(&self, team: &PersistedTeam) -> Result<(), String> {
        // Under the lock, only compare the `Arc`s; the per-worker stat and the
        // writes happen with it released.
        let same_arc: HashSet<String> = {
            let written = self.written_guard();
            team.workers
                .iter()
                .filter(|worker| {
                    written
                        .get(&worker.id)
                        .is_some_and(|known| Arc::ptr_eq(known, &worker.transcript))
                })
                .map(|worker| worker.id.clone())
                .collect()
        };
        let mut pending: Vec<(String, PathBuf, Arc<Transcript>)> = Vec::new();
        for worker in &team.workers {
            let Some(file) = self.transcript_file(&worker.id) else {
                continue;
            };
            let exists = file.exists();
            // An empty transcript needs no file: a load yields the same empty
            // value either way, and writing one would hide a conversation that
            // was lost. A worker whose file exists is still rewritten, so a
            // transcript that really became empty cannot leave stale content
            // winning.
            if worker.transcript.messages.is_empty() && !exists {
                continue;
            }
            if exists && same_arc.contains(&worker.id) {
                continue;
            }
            pending.push((worker.id.clone(), file, worker.transcript.clone()));
        }
        for (id, file, transcript) in pending {
            let bytes = serde_json::to_vec(&*transcript)
                .map_err(|err| format!("could not encode the transcript for {id}: {err}"))?;
            // A temp name unique per write, as for `team.json`: two
            // `TeamPersistence` instances can share one state dir ("Open
            // anyway" attaches a second instance to one project), and between
            // them `team.json` stays last-writer-wins; a shared temp name would
            // let one saver consume the other's temp and fail its rename.
            let temp = file.with_extension(format!(
                "json.{}.{}.tmp",
                std::process::id(),
                SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            retry_io(|| std::fs::write(&temp, &bytes))
                .map_err(|err| format!("could not write the transcript for {id}: {err}"))?;
            retry_io(|| std::fs::rename(&temp, &file))
                .map_err(|err| format!("could not replace the transcript for {id}: {err}"))?;
            self.written_guard().insert(id, transcript);
        }
        Ok(())
    }

    /// The ledger as stored in `team.json`: a worker with its own transcript
    /// file carries an empty transcript placeholder there.
    fn ledger_without_transcripts(&self, team: &PersistedTeam) -> PersistedTeam {
        PersistedTeam {
            workers: team
                .workers
                .iter()
                .map(|worker| {
                    if self.transcript_file(&worker.id).is_some() {
                        WorkerRecord {
                            transcript: Arc::default(),
                            saved_messages: worker.transcript.messages.len(),
                            ..worker.clone()
                        }
                    } else {
                        worker.clone()
                    }
                })
                .collect(),
            slots: team.slots,
            usage: team.usage.clone(),
        }
    }

    /// Delete the transcript files of the workers this instance removed from the
    /// ledger, plus the aged temp files a crashed write left behind.
    ///
    /// Only files this instance wrote are candidates: another instance can share
    /// the state dir ("Open anyway" attaches a second instance to one project),
    /// so an unknown `<id>.json` belongs to that instance's worker and is never
    /// touched. A crash can leave such a file behind, which is harmless - a
    /// later dispatch of that id rewrites it.
    fn sweep_orphan_transcripts(&self, team: &PersistedTeam) {
        let known: HashSet<&str> = team
            .workers
            .iter()
            .map(|worker| worker.id.as_str())
            .collect();
        let removed: Vec<String> = {
            let written = self.written_guard();
            written
                .keys()
                .filter(|id| !known.contains(id.as_str()))
                .cloned()
                .collect()
        };
        for id in removed {
            let Some(file) = self.transcript_file(&id) else {
                continue;
            };
            let gone = match retry_io(|| std::fs::remove_file(&file)) {
                Ok(()) => true,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
                // Keep the record so the next save retries: an unknown file is
                // deliberately preserved, so nothing else would remove it.
                Err(_) => false,
            };
            if gone {
                self.written_guard().remove(&id);
            }
        }
        let Some(dir) = self.transcripts_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.ends_with(".tmp") && !is_young(&entry) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// The transcripts already on disk, recovering the lock from a panicked
    /// writer instead of failing every later save.
    fn written_guard(&self) -> MutexGuard<'_, HashMap<String, Arc<Transcript>>> {
        self.written
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Whether a directory entry is too fresh to sweep: a save running in another
/// [`TeamPersistence`] over the same state dir - "Open anyway" attaches a
/// second instance to one project - may own it right now.
fn is_young(entry: &std::fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|metadata| metadata.modified())
        .map(|modified| modified.elapsed().unwrap_or_default() < TEMP_SWEEP_GRACE)
        .unwrap_or(true)
}

/// Where a transcript that failed to decode is kept: its bytes are never
/// destroyed, and an earlier copy is replaced.
fn corrupt_path(file: &std::path::Path) -> PathBuf {
    file.with_extension("json.corrupt")
}

/// Windows can transiently deny a rename or remove while an indexer, an
/// antivirus scan, or a second ledger writer still holds the file: retry a
/// few times before reporting a failure.
fn retry_io<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut last = None;
    for _ in 0..5 {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                last = Some(err);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.expect("a retried IO attempt"))
}

/// Convert one stored tier id, returning whether it changed.
fn convert_legacy_tier(tier: &mut String) -> bool {
    let Some(converted) = TierLadder::convert_old_id(tier) else {
        return false;
    };
    *tier = converted.to_string();
    true
}

/// Convert an optional stored tier id, returning whether it changed.
fn convert_legacy_optional_tier(tier: &mut Option<String>) -> bool {
    tier.as_mut().is_some_and(convert_legacy_tier)
}

/// Convert every tier id embedded in one historical routing row.
fn convert_legacy_row_tiers(row: &mut DispatchRoutingRow) -> bool {
    let mut changed = false;
    match row {
        DispatchRoutingRow::Route {
            tier,
            lead_tier,
            ran_tier,
            ..
        } => {
            changed |= convert_legacy_optional_tier(tier);
            changed |= convert_legacy_optional_tier(lead_tier);
            changed |= convert_legacy_optional_tier(ran_tier);
        }
        DispatchRoutingRow::LeadOverride {
            laya_tier,
            lead_tier,
            requested_tier,
            ..
        } => {
            changed |= convert_legacy_optional_tier(laya_tier);
            changed |= convert_legacy_tier(lead_tier);
            changed |= convert_legacy_optional_tier(requested_tier);
        }
        DispatchRoutingRow::LeadReroute {
            from_tier,
            to_tier,
            ..
        }
        | DispatchRoutingRow::UserReroute {
            from_tier,
            to_tier,
            ..
        }
        | DispatchRoutingRow::Nudge {
            from_tier,
            to_tier,
            ..
        }
        => {
            changed |= convert_legacy_tier(from_tier);
            changed |= convert_legacy_tier(to_tier);
        }
        DispatchRoutingRow::NudgeIgnored { tier, .. } => {
            changed |= convert_legacy_tier(tier);
        }
        DispatchRoutingRow::Outcome {
            final_tier,
            label,
            ..
        } => {
            changed |= convert_legacy_tier(final_tier);
            changed |= convert_legacy_optional_tier(label);
        }
    }
    changed
}

/// Convert legacy ids throughout a decoded ledger. The caller preserves the
/// original bytes once before saving the converted records.
fn convert_legacy_team_tiers(team: &mut PersistedTeam) -> bool {
    let mut changed = false;
    for worker in &mut team.workers {
        changed |= worker.tier.as_mut().is_some_and(convert_legacy_tier);
        if let Some(routing) = &mut worker.routing {
            changed |= convert_legacy_optional_tier(&mut routing.laya_tier);
            changed |= convert_legacy_optional_tier(&mut routing.lead_tier);
            changed |= convert_legacy_optional_tier(&mut routing.ran_tier);
            changed |= convert_legacy_optional_tier(&mut routing.nudged);
        }
        for row in &mut worker.reroutes {
            changed |= convert_legacy_row_tiers(row);
        }
    }
    changed
}
