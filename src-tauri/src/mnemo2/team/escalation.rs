// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Automatic idle escalation: the shared handles a pump or review
//! path needs, and the switch of a nudged idle worker onto the
//! suggested tier without stopping its session.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use mnemo::headless::session::ModelSwitch;
use tokio::sync::{broadcast, Mutex};

use crate::mnemo2::dispatch_routing::DispatchRoutingRow;
use crate::mnemo2::runtime::LeadModel;
use crate::mnemo2::usage::ProjectUsage;

use super::dispatch::BoxFuture;
use super::nudge::append_reroute_metadata;
use super::persistence::TeamPersistence;
use super::record::{
    note_model, session_model_spec, ChangeSource, MessageSender, WorkerRecord, WorkerStatus,
};
use super::routing::{final_tier, resolve_tier_over, tier_ladder_from_dto};
use super::pump::save_workers;
use super::{status_name, unix_ms, BoardSignal, LiveWorker, TeamRuntime};

/// The shared state an automatic idle escalation touches. The worker pump and
/// the lead review path both have these handles already.
pub(super) struct AutoEscalation<'a> {
    pub(super) records: &'a Arc<Mutex<Vec<WorkerRecord>>>,
    pub(super) live: &'a Arc<Mutex<HashMap<String, LiveWorker>>>,
    pub(super) models: &'a Arc<std::sync::RwLock<Vec<LeadModel>>>,
    pub(super) tiers: &'a Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    pub(super) signals: &'a broadcast::Sender<BoardSignal>,
    pub(super) routing_log_path: &'a Path,
    pub(super) usage: &'a Arc<Mutex<ProjectUsage>>,
    pub(super) save_lock: &'a Arc<Mutex<()>>,
    pub(super) persistence: &'a TeamPersistence,
}

/// Switch a nudged idle worker without stopping its session. `Ok(false)` means
/// the original suggestion remains in force (busy, already claimed, or no live
/// session); it is not an error and a later idle event may try again.
pub(super) async fn auto_escalate_idle(worker_id: &str, ctx: AutoEscalation<'_>) -> Result<bool, String> {
    let record = ctx
        .records
        .lock()
        .await
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .ok_or_else(|| format!("worker '{worker_id}' not found"))?;
    if record.kind == "audit" {
        return Ok(false);
    }
    let Some(to_tier) = record
        .routing
        .as_ref()
        .and_then(|routing| routing.nudged.clone())
    else {
        return Ok(false);
    };
    let (ran_tier, choice, effort) = {
        let models = ctx.models.read().unwrap_or_else(|e| e.into_inner());
        let tiers = ctx.tiers.read().unwrap_or_else(|e| e.into_inner());
        let ladder = tier_ladder_from_dto(&tiers);
        let (ran_tier, choice) = resolve_tier_over(&models, &ladder, &to_tier)?;
        let effort = ladder.get(&ran_tier).and_then(|rung| rung.effort.clone());
        (ran_tier, choice, effort)
    };
    let model = choice.spec.clone();
    let new_model_id = {
        let live = ctx.live.lock().await;
        let Some(live) = live.get(worker_id) else {
            return Ok(false);
        };
        if live.busy.load(Ordering::SeqCst) {
            return Ok(false);
        }
        live.admit_session_command(worker_id)?;
        match live.session.set_model(&model, effort.clone()).await {
            Ok(ModelSwitch::NextRequest { model })
            | Ok(ModelSwitch::CompactsFirst { model }) => model,
            Err(error) => {
                return Err(format!(
                    "could not switch worker '{worker_id}' to '{model}': {error}"
                ));
            }
        }
    };
    let from_model = record.model.clone();
    let from_tier = final_tier(&record);
    let row = DispatchRoutingRow::LeadReroute {
        ts: unix_ms(),
        worker_id: worker_id.to_string(),
        plan_id: record.plan_id.clone().unwrap_or_default(),
        step: record.plan_step.map_or(0, |step| step + 1),
        from_tier: from_tier.clone(),
        to_tier: ran_tier.clone(),
        from_model,
        to_model: Some(model.clone()),
        status: status_name(record.status),
        review_rounds: record.review_rounds,
        fix_rounds: record.fix_rounds,
        nudged: true,
    };
    let mut records = ctx.records.lock().await;
    let worker = records
        .iter_mut()
        .find(|worker| worker.id == worker_id)
        .ok_or_else(|| format!("worker '{worker_id}' not found"))?;
    if worker.routing.as_ref().and_then(|routing| routing.nudged.as_deref())
        != Some(to_tier.as_str())
    {
        return Ok(false);
    }
    worker.model = Some(model.clone());
    if worker.model_id.as_deref() != Some(new_model_id.as_str()) {
        worker.model_id = None;
    }
    worker.reasoning_effort = effort;
    worker.tier = Some(ran_tier.clone());
    worker.reroutes.push(row.clone());
    if let Some(routing) = worker.routing.as_mut() {
        routing.nudged = None;
    }
    note_model(worker, &model);
    drop(records);
    if let Some(live) = ctx.live.lock().await.get(worker_id) {
        live.strikes.lock().await.reset();
    }
    save_workers(ctx.records, ctx.usage, ctx.save_lock, ctx.persistence).await?;
    append_reroute_metadata(ctx.routing_log_path, &row, ChangeSource::Escalation, None);
    let _ = ctx.signals.send(BoardSignal::Notice {
        key: format!("escalation:auto:{worker_id}"),
        text: format!(
            "Auto escalation switched worker {worker_id} from {from_tier} to {ran_tier} ({model}); its session and transcript were kept."
        ),
    });
    Ok(true)
}

impl TeamRuntime {
    /// Continue a worker that failed on a provider usage limit (plan e5d93df3
    /// decision 8, routing mode auto): its pump took `failed_spec` out of the
    /// tiers, so its tier now folds; Mnemo resumes it there with its
    /// transcript through [`TeamRuntime::resume_on_tier`] — the path
    /// `team_resume` with a tier takes — tells it where it is, and notices
    /// the lead. Nothing happens unless the worker is still the failed
    /// generation `generation` (the lead may have moved it first) and its
    /// tier yields a different, available model; a worker with no fold target
    /// stays failed, and its failure notice says so.
    ///
    /// Boxed: the resume starts a new worker pump, and the pump spawns this
    /// future, so its type must not be inferred through the pump's own.
    pub(super) fn resume_usage_limited<'a>(
        &'a self,
        worker_id: &'a str,
        generation: u64,
        failed_spec: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Ok(record) = self.worker(worker_id).await else {
                return;
            };
            if record.status != WorkerStatus::Failed || record.generation != generation {
                return;
            }
            let Some(until_ms) = self.usage_limits.until(failed_spec, unix_ms()) else {
                return;
            };
            let (marked, tier) = self.usage_limit_marked(&record, failed_spec, until_ms);
            let Some((tier, Some(_))) = tier else {
                return;
            };
            let key = format!("usage-limit:{worker_id}:{generation}");
            let resumed = match self
                .resume_on_tier(worker_id, Some(&tier), None, None, ChangeSource::Lead)
                .await
            {
                Ok(resumed) => resumed,
                Err(error) => {
                    self.notify_lead(
                        key,
                        format!(
                            "{marked} Mnemo could not resume {worker_id} there: {error}\nMove it yourself, then team_message it to continue: team_resume worker_id={worker_id} tier={tier}"
                        ),
                    );
                    return;
                }
            };
            let new_spec = session_model_spec(&resumed).to_string();
            let text = format!(
                "Your previous model {failed_spec} hit its usage limit; you are now on {new_spec}. Run git status and git diff --stat to see where you were, then continue your task."
            );
            let told = self.message(worker_id, text, MessageSender::Mnemo).await;
            let mut notice =
                format!("{marked} Mnemo resumed {worker_id} on {new_spec} with its transcript.");
            if let Err(error) = told {
                notice.push_str(&format!(
                    "\nIt could not be told to continue ({error}); team_message it to continue."
                ));
            }
            self.notify_lead(key, notice);
        })
    }

    pub(super) async fn auto_escalate_worker(&self, worker_id: &str) -> Result<bool, String> {
        auto_escalate_idle(
            worker_id,
            AutoEscalation {
                records: &self.records,
                live: &self.live,
                models: &self.models,
                tiers: &self.tiers,
                signals: &self.signals,
                routing_log_path: &self.routing_log_path,
                usage: &self.usage,
                save_lock: &self.save_lock,
                persistence: &self.persistence,
            },
        )
        .await
    }
}
