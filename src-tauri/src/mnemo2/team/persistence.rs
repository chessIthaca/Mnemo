// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The `team.json` ledger: the persisted shape, its backup-recoverable
//! store, and the legacy tier-id conversion.

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
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

/// The backup-recoverable `team.json` store.
#[derive(Clone, Debug)]
pub(super) struct TeamPersistence {
    pub(super) path: PathBuf,
    pub(super) slots: Arc<AtomicUsize>,
}

impl TeamPersistence {
    pub(super) fn new(state_dir: &std::path::Path) -> Self {
        Self {
            path: state_dir.join(TEAM_STATE_FILE),
            slots: Arc::new(AtomicUsize::new(DEFAULT_SLOTS)),
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
        let bytes = serde_json::to_vec_pretty(team)
            .map_err(|err| format!("could not encode the team ledger: {err}"))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)
            .map_err(|err| format!("could not write the team ledger: {err}"))?;
        let backup = self.path.with_extension("json.bak");
        if !self.path.exists() && backup.exists() {
            std::fs::rename(&backup, &self.path)
                .map_err(|err| format!("could not restore the team ledger backup: {err}"))?;
        }
        if backup.exists() {
            std::fs::remove_file(&backup)
                .map_err(|err| format!("could not clear the old team ledger backup: {err}"))?;
        }
        if self.path.exists() {
            std::fs::rename(&self.path, &backup)
                .map_err(|err| format!("could not back up the team ledger: {err}"))?;
        }
        match std::fs::rename(&temp, &self.path) {
            Ok(()) => {
                let _ = std::fs::remove_file(backup);
                Ok(())
            }
            Err(err) => {
                if backup.exists() {
                    let _ = std::fs::rename(&backup, &self.path);
                }
                Err(format!("could not replace the team ledger: {err}"))
            }
        }
    }
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
