// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Laya nudges: the failing-tool-call streak, the one nudge a
//! worker may be offered, its routing row, and the brief helpers a
//! routed fix and its pending downshift use.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};

use crate::mnemo2::dispatch_routing::{
    DispatchRoutingRow, EnabledTiers, RoutingMode, RoutingSettings,
};
use crate::mnemo2::phase3::Finding;
use crate::mnemo2::runtime::LeadModel;

use super::record::{ChangeSource, ToolErrorNote, WorkerRecord};
use super::routing::{
    direct_tiers_over, final_tier, logs_routing, next_tier_above, tier_is_above,
    NUDGE_LADDER,
};
use super::{unix_ms, BoardSignal};

/// Consecutive failed or verbatim-repeated tool calls that fire a nudge
/// (design §4.6). Deliberately still 3 after plan 865249fd raised the
/// session's doom-guard cap (`MAX_RETRIES`, src/agent/mod.rs) 3 → 5: the
/// stuck-worker nudge tells the lead early, while the worker itself keeps
/// running to the 5th consecutive failing batch before the turn aborts.
pub(super) const NUDGE_STRIKE_LIMIT: u32 = 3;

/// How many failed tool calls a worker's streak keeps: enough for the lead to
/// see the last few errors without unbounded growth.
pub(super) const TOOL_ERRORS_KEPT: usize = 3;

/// How many characters of one failed call's output the streak keeps.
pub(super) const TOOL_ERROR_CHARS: usize = 300;

/// Fold one finished tool call into the worker's current failing streak: a
/// successful call clears it, a failed one appends the tool and its cleaned
/// output, keeping only the latest [`TOOL_ERRORS_KEPT`].
pub(super) fn note_tool_outcome(worker: &mut WorkerRecord, tool: &str, success: bool, output: &str) {
    if success {
        worker.tool_errors.clear();
        return;
    }
    let collapsed = output.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut note: String = collapsed.chars().take(TOOL_ERROR_CHARS).collect();
    if collapsed.chars().count() > TOOL_ERROR_CHARS {
        note.push('…');
    }
    worker.tool_errors.push(ToolErrorNote {
        tool: tool.to_string(),
        output: note,
    });
    let overflow = worker.tool_errors.len().saturating_sub(TOOL_ERRORS_KEPT);
    if overflow > 0 {
        worker.tool_errors.drain(..overflow);
    }
}

/// The worker's current failing streak as notice text: empty when the last
/// tool call succeeded, otherwise the kept failures, oldest first.
pub(crate) fn tool_errors_text(worker: &WorkerRecord) -> String {
    if worker.tool_errors.is_empty() {
        return String::new();
    }
    let mut text = String::from("\nLast tool errors:");
    for note in &worker.tool_errors {
        text.push_str("\n- ");
        text.push_str(&note.tool);
        text.push_str(": ");
        text.push_str(&note.output);
    }
    text
}

/// The one nudge claimed for a worker: what the notice says and the row
/// logs (design §4.6/§4.7).
pub(super) struct ClaimedNudge {
    /// The worker's display name, for the notice.
    name: String,
    /// The tier the worker is on.
    from_tier: String,
    /// The tier the nudge suggests.
    to_tier: String,
    /// The plan the step belongs to, when known.
    plan_id: String,
    /// The 1-based step number in the plan.
    step: usize,
    /// Whether the routing rows' `logs_routing` gate passes for this
    /// worker: the notice may fire without it, the row may not.
    logs: bool,
}

/// Claim the worker's one nudge (design §4.6) under the record lock: mark
/// `routing.nudged` with the tier the nudge suggests, so at most one ever
/// fires for a worker, and report what the notice and row need. `None`
/// when the worker is gone, carries no routing record (there is nowhere to
/// mark the one nudge), was already nudged, or has no enabled tier above
/// its own. `suggest` names the tier Laya cleared (the drift nudge), which
/// must still sit above the worker's own; `None` takes the next enabled
/// tier up. The caller must not hold the records lock.
pub(super) async fn claim_nudge(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    worker_id: &str,
    enabled: EnabledTiers,
    mode: RoutingMode,
    suggest: Option<&str>,
) -> Option<ClaimedNudge> {
    let mut records = records.lock().await;
    let worker = records.iter_mut().find(|worker| worker.id == worker_id)?;
    let from_tier = final_tier(worker);
    let logs = logs_routing(worker, mode);
    let routing = worker.routing.as_mut()?;
    if routing.nudged.is_some() {
        return None;
    }
    let to_tier = match suggest {
        Some(tier) if tier_is_above(&from_tier, tier) => tier.to_string(),
        Some(_) => return None,
        None => next_tier_above(enabled, &from_tier)?.to_string(),
    };
    routing.nudged = Some(to_tier.clone());
    Some(ClaimedNudge {
        name: worker.name.clone(),
        from_tier,
        to_tier,
        plan_id: worker.plan_id.clone().unwrap_or_default(),
        step: worker.plan_step.map_or(0, |step| step + 1),
        logs,
    })
}

/// Deliver a claimed nudge: queue the lead notice on the board-signal bus
/// (the runtime delivers it prefixed like every TeamManager notice) and
/// append the `Nudge` row (design §4.7) behind the same `logs_routing`
/// gate as the reroute rows. Never switches the worker's model. `tool_errors`
/// is the strike nudge's failing-streak text (empty for every other nudge).
pub(super) fn deliver_nudge(
    signals: &broadcast::Sender<BoardSignal>,
    routing_log_path: &Path,
    worker_id: &str,
    claim: &ClaimedNudge,
    reason: &str,
    detail: &str,
    tool_errors: &str,
) {
    let tool_errors = if tool_errors.is_empty() {
        String::new()
    } else {
        format!("{tool_errors}\nRead these first: a tool problem is not fixed by a stronger model.")
    };
    let _ = signals.send(BoardSignal::Notice {
        key: format!("nudge:{worker_id}"),
        text: format!(
            "worker_id={worker_id} Laya nudge: {} {detail}; consider team_resume with tier {}.{tool_errors}",
            claim.name, claim.to_tier
        ),
    });
    if claim.logs {
        mnemo::model_routing::append_row(
            routing_log_path,
            &DispatchRoutingRow::Nudge {
                ts: unix_ms(),
                worker_id: worker_id.to_string(),
                plan_id: claim.plan_id.clone(),
                step: claim.step,
                reason: reason.to_string(),
                from_tier: claim.from_tier.clone(),
                to_tier: claim.to_tier.clone(),
                detail: detail.to_string(),
            },
        );
    }
}

/// The `nudge_ignored` row (design §4.7): a nudged worker reached a
/// truly terminal status — merged, stopped, or abandoned in cleanup —
/// still on the tier it was nudged from, so the lead never acted on the
/// nudge. A failed worker is resumable (`team_resume` on a stronger tier
/// takes the nudge and logs a reroute), so failure never logs it.
/// `None` when the worker was not nudged, or ended on the suggested
/// tier (every reroute clears the nudge, so an acted-on nudge is never
/// seen here).
pub(super) fn nudge_ignored_row(record: &WorkerRecord) -> Option<DispatchRoutingRow> {
    let nudged = record.routing.as_ref()?.nudged.as_deref()?;
    let tier = final_tier(record);
    if tier == nudged {
        return None;
    }
    Some(DispatchRoutingRow::NudgeIgnored {
        ts: unix_ms(),
        worker_id: record.id.clone(),
        plan_id: record.plan_id.clone().unwrap_or_default(),
        step: record.plan_step.map_or(0, |step| step + 1),
        tier,
    })
}

/// The shared handles a pump-side strike nudge needs (design §4.6): the
/// records to claim on, the tier-ladder inputs, the notice bus, the routing
/// log, and the settings in force.
pub(super) struct StrikeNudges<'a> {
    pub(super) records: &'a Arc<Mutex<Vec<WorkerRecord>>>,
    pub(super) models: &'a Arc<std::sync::RwLock<Vec<LeadModel>>>,
    pub(super) tiers: &'a Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    pub(super) signals: &'a broadcast::Sender<BoardSignal>,
    pub(super) routing_log_path: &'a Path,
    pub(super) settings: &'a RoutingSettings,
}

/// Try the worker's one strike nudge (design §4.6): the pump saw its
/// third consecutive failed or verbatim-repeated tool call. Claims the
/// nudge slot, queues the notice, and appends the row; `repeated` says
/// whether the firing call repeated the previous one verbatim ("going in
/// circles") or failed anew. No-op when nudges are off, the worker was
/// already nudged, has no routing record, or sits on the top enabled
/// tier.
pub(super) async fn try_strike_nudge(worker_id: &str, nudges: StrikeNudges<'_>, repeated: bool) {
    // The worker's model and its current failing streak come from one lock
    // read; the records lock must be released before `claim_nudge` takes it.
    let (current_model, tool_errors) = {
        let records = nudges.records.lock().await;
        let Some(worker) = records.iter().find(|worker| worker.id == worker_id) else {
            return;
        };
        (worker.model.clone(), tool_errors_text(worker))
    };
    let enabled = direct_tiers_over(
        &nudges.models.read().unwrap_or_else(|e| e.into_inner()),
        &nudges.tiers.read().unwrap_or_else(|e| e.into_inner()),
        current_model.as_deref(),
    );
    let Some(claim) =
        claim_nudge(nudges.records, worker_id, enabled, nudges.settings.mode, None).await
    else {
        return;
    };
    let (reason, detail) = if repeated {
        ("circles", "looks stuck (going in circles)")
    } else {
        ("strikes", "looks stuck (3 failing tool calls in a row)")
    };
    deliver_nudge(
        nudges.signals,
        nudges.routing_log_path,
        worker_id,
        &claim,
        reason,
        detail,
        &tool_errors,
    );
}

pub(super) fn append_reroute_metadata(
    path: &Path,
    row: &DispatchRoutingRow,
    source: ChangeSource,
    finding_closed: Option<bool>,
) {
    let mut value = match serde_json::to_value(row) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("failed to serialize dispatch routing row: {error}");
            return;
        }
    };
    if let Some(object) = value.as_object_mut() {
        if !matches!(source, ChangeSource::Lead | ChangeSource::User) {
            object.insert("source".into(), source.as_str().into());
        }
        if let Some(closed) = finding_closed {
            object.insert("finding_closed".into(), closed.into());
        }
    }
    mnemo::model_routing::append_row(path, &value);
}

/// The unreviewed model downshift on this routed fix, if any. `nudged` is the
/// durable pending marker on this internal row; settling the review clears it.
pub(super) fn pending_fix_downshift(
    worker: &WorkerRecord,
    findings: &[Finding],
) -> Option<(DispatchRoutingRow, bool)> {
    let finding_id = worker.fixer_finding_id.as_deref()?;
    let DispatchRoutingRow::LeadReroute {
        from_tier,
        to_tier,
        nudged: true,
        ..
    } = worker.reroutes.last()?
    else {
        return None;
    };
    let from = NUDGE_LADDER.iter().position(|tier| tier == from_tier)?;
    let to = NUDGE_LADDER.iter().position(|tier| tier == to_tier)?;
    if from <= to {
        return None;
    }
    let closed = !findings
        .iter()
        .any(|finding| finding.id == finding_id && finding.status == "open");
    Some((worker.reroutes.last()?.clone(), closed))
}

pub(super) fn route_fix_brief(worker: &WorkerRecord, routed: &Finding) -> String {
    // Keep the established routed-finding prefix for callers that display or
    // match it, then include every OTHER open finding in full as well.
    let mut brief = format!(
        "Fix review finding {}: {}\n{}\nFile: {}:{}\n",
        routed.id, routed.title, routed.detail, routed.file, routed.line
    );
    for open in worker
        .findings
        .iter()
        .filter(|candidate| candidate.status == "open" && candidate.id != routed.id)
    {
        brief.push_str(&format!(
            "\nAlso fix open finding {} [{}]: {}\n{}\nFile: {}:{}\n",
            open.id, open.severity, open.title, open.detail, open.file, open.line
        ));
    }
    brief.push_str(
        "\nCommit fixes for every finding above, rerun the affected checks, and report evidence for delta review.",
    );
    brief
}
