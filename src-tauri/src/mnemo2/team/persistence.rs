// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `team.json` ledger: the persisted shape, its backup-recoverable
//! store, and the legacy tier-id conversion.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use mnemo::config::endpoints::TierLadder;
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
}

impl TeamPersistence {
    pub(super) fn new(state_dir: &std::path::Path) -> Self {
        Self {
            path: state_dir.join(TEAM_STATE_FILE),
            slots: Arc::new(AtomicUsize::new(DEFAULT_SLOTS)),
            flush_scheduled: Arc::new(AtomicBool::new(false)),
            closed: Arc::new(AtomicBool::new(false)),
        }
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

    pub(super) fn load(&self) -> Result<Option<PersistedTeam>, String> {
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
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("could not create the team state dir: {err}"))?;
        }
        let bytes = serde_json::to_vec(team)
            .map_err(|err| format!("could not encode the team ledger: {err}"))?;
        let temp = self.path.with_extension(format!(
            "json.{}.{}.tmp",
            std::process::id(),
            SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        retry_io(|| std::fs::write(&temp, &bytes))
            .map_err(|err| format!("could not write the team ledger: {err}"))?;
        let backup = self.path.with_extension("json.bak");
        if !self.path.exists() && backup.exists() {
            match retry_io(|| std::fs::rename(&backup, &self.path)) {
                Ok(()) => {}
                // Another writer restored the ledger first: its snapshot stands.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(format!("could not restore the team ledger backup: {err}"));
                }
            }
        }
        if backup.exists() {
            match retry_io(|| std::fs::remove_file(&backup)) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(format!("could not clear the old team ledger backup: {err}"));
                }
            }
        }
        if self.path.exists() {
            match retry_io(|| std::fs::rename(&self.path, &backup)) {
                Ok(()) => {}
                // Another writer moved the ledger between the check and the
                // rename; every writer persists a consistent snapshot, so the
                // newest temp still replaces it below.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(format!("could not back up the team ledger: {err}")),
            }
        }
        match retry_io(|| std::fs::rename(&temp, &self.path)) {
            Ok(()) => {
                let _ = std::fs::remove_file(backup);
                Ok(())
            }
            Err(err) => {
                if backup.exists() {
                    let _ = retry_io(|| std::fs::rename(&backup, &self.path));
                }
                // A vanished temp means another writer already replaced the
                // ledger with a consistent snapshot.
                if err.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(format!("could not replace the team ledger: {err}"))
                }
            }
        }
    }
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
