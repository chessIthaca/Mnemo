// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Worker provisioning and session start: model validation, the
//! worktree checkout, the guardrailed session spawn and failure
//! recording.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mnemo::{PermissionPolicy, route_question};
use mnemo::app::worktrees::{agent_short_id, current_branch, provision_agent_worktree};
use mnemo::headless::{AgentSession, SessionCommand, SpawnOptions, Transcript};
use mnemo::provider::trace::{TraceActorKind, TraceIdentity};

use crate::mnemo2::phase3;

use tokio::sync::Mutex;

use super::dispatch::PlanRef;
use super::guardrails::guardrails_for;
use super::nudge::NUDGE_STRIKE_LIMIT;
use super::record::{kickoff_message, note_model, session_model_spec, WorkerRecord, WorkerStatus};
use super::routing::{logs_label, outcome_row};
use super::{pump_worker, LiveWorker, TeamRuntime};

/// The inputs one live worker session needs: where and on what it runs, its
/// transcript to resume from, its trace identity, and the task pinned into
/// its system head.
pub(crate) struct WorkerSessionSpec {
    /// The exact model spec, when the lead chose one.
    pub(crate) model: Option<String>,
    /// The chosen reasoning effort, when the lead chose one.
    pub(crate) reasoning_effort: Option<String>,
    /// The checkout the session is bound to.
    pub(crate) worktree: PathBuf,
    /// The saved transcript to resume from.
    pub(crate) resume: Option<Transcript>,
    /// `implementation` or `audit`, selecting the guardrails.
    pub(crate) kind: String,
    /// The trace identity every model call is attributed to.
    pub(crate) trace_identity: TraceIdentity,
    /// The task rendered into the system head for the session's whole life.
    pub(crate) pinned_task: Option<String>,
}

impl TeamRuntime {
    /// Stamp the resolved tier for a lead's explicit tier choice. An empty
    /// requested tier folds down exactly as it did for model selection.
    pub(super) async fn record_tier(&self, worker_id: &str, tier: &str) {
        let Ok((resolved, _)) = self.resolve_tier_choice(tier) else {
            return;
        };
        {
            let mut records = self.records.lock().await;
            match records.iter_mut().find(|w| w.id == worker_id) {
                Some(worker) if worker.tier.as_deref() != Some(resolved.as_str()) => {
                    worker.tier = Some(resolved);
                }
                _ => return,
            }
        }
        let _ = self.persist().await;
    }

    pub(super) fn validate_model(
        &self,
        model: &Option<String>,
        reasoning_effort: &Option<String>,
    ) -> Result<(), String> {
        let models = self.current_models();
        let configured = match model {
            Some(spec) => Some(
                models
                    .iter()
                    .find(|configured| &configured.spec == spec)
                    .ok_or_else(|| {
                        format!("'{spec}' is not a configured model; choose one from team_dispatch's model list")
                    })?,
            ),
            None => None,
        };
        let Some(effort) = reasoning_effort else {
            return Ok(());
        };
        let Some(configured) = configured else {
            return Err(format!(
                "reasoning_effort '{effort}' needs an explicit model to apply to"
            ));
        };
        if configured.reasoning_efforts.is_empty() {
            return Err(format!(
                "'{}' does not support a reasoning effort; omit reasoning_effort",
                configured.spec
            ));
        }
        if !configured
            .reasoning_efforts
            .iter()
            .any(|allowed| allowed == effort)
        {
            return Err(format!(
                "'{}' does not support reasoning_effort '{effort}'; choose one of: {}",
                configured.spec,
                configured.reasoning_efforts.join(", ")
            ));
        }
        Ok(())
    }

    // One parameter per provisioning input the dispatch sites pass explicitly.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn provision_worker(
        &self,
        brief: String,
        opening_prompt: Option<String>,
        model: Option<String>,
        reasoning_effort: Option<String>,
        worker_id: Option<String>,
        review_risk: String,
        tier: Option<String>,
        plan: Option<PlanRef>,
    ) -> Result<WorkerRecord, String> {
        let base_ref = current_branch(&self.workspace_root).map_err(|err| {
            format!(
                "cannot dispatch a tracked worker: the selected project is not a usable Git checkout. Open the repository root (the folder containing .git), then retry. Git reported: {err}"
            )
        })?;
        // The lead names its workers (`ghostai0001`): that id is the name. A
        // generated id is opaque, so it shows as "Worker <short id>".
        let (id, name) = match worker_id {
            Some(id) => (id.clone(), id),
            None => {
                let seq = self.next_worker.fetch_add(1, Ordering::SeqCst);
                let millis = phase3::now_ms();
                let id = format!("{seq:08x}{millis:012x}");
                let name = format!("Worker {}", agent_short_id(&id));
                (id, name)
            }
        };
        let millis = phase3::now_ms();
        let (worktree, branch) =
            provision_agent_worktree(self.workspace_root.clone(), id.clone(), base_ref).await?;
        let base_tip = phase3::tip(&worktree).await?;
        let record = WorkerRecord {
            id: id.clone(),
            inbox: Vec::new(),
            name,
            brief,
            opening_prompt,
            model,
            model_id: None,
            reasoning_effort,
            branch,
            worktree,
            status: WorkerStatus::Dispatching,
            generation: 0,
            transcript: Transcript::default(),
            pending: None,
            used_tokens: 0,
            max_tokens: 0,
            brief_sent: false,
            read_back: false,
            compacting: false,
            total_tokens: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            requests: 0,
            cost_micros: None,
            cost_notified_micros: 0,
            created_at_ms: millis,
            elapsed_ms: 0,
            turns: 0,
            last_activity_ms: millis,
            checks: Vec::new(),
            review_risk,
            review: None,
            review_rounds: 0,
            fix_rounds: 0,
            audits: 0,
            models_used: Vec::new(),

            kind: "implementation".to_string(),
            audit_target: None,
            audit_tip: None,
            report_delivered: false,
            findings: Vec::new(),

            fixer_finding_id: None,
            merge: None,
            error: None,
            tool_errors: Vec::new(),
            plan_id: plan.as_ref().map(|plan| plan.plan_id.clone()),
            plan_step: plan.map(|plan| plan.step),
            tier,
            routing: None,
            reroutes: Vec::new(),
        };
        self.records.lock().await.push(record.clone());
        if let Err(err) = self.persist().await {
            self.records.lock().await.retain(|worker| worker.id != id);
            let rollback = mnemo::app::worktrees::rollback_agent_worktree(
                self.workspace_root.clone(),
                id.clone(),
                base_tip,
            )
            .await;
            return Err(match rollback {
                Ok(()) => err,
                Err(rollback) => {
                    let error = format!(
                        "{err}; worker '{id}' retains its checkout because rollback was refused: {rollback}"
                    );
                    self.records.lock().await.push(WorkerRecord {
                        status: WorkerStatus::Failed,
                        error: Some(error.clone()),
                        ..record
                    });
                    let _ = self.persist().await;
                    error
                }
            });
        }
        Ok(record)
    }

    pub(super) async fn start_worker(&self, record: WorkerRecord) -> Result<WorkerRecord, String> {
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        let id = record.id.clone();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.update_worker(&id, |worker| {
            worker.generation = generation;
            worker.status = WorkerStatus::Dispatching;
            worker.pending = None;
            worker.error = None;
            worker.compacting = false;
            let model = session_model_spec(worker).to_string();
            note_model(worker, &model);
        })
        .await;
        self.persist().await?;
        let session = match self
            .spawn_worker_session(WorkerSessionSpec {
                model: record.model.clone(),
                reasoning_effort: record.reasoning_effort.clone(),
                worktree: record.worktree.clone(),
                resume: Some(record.transcript.clone()),
                kind: record.kind.clone(),
                trace_identity: Self::worker_trace_identity(&record),
                pinned_task: Some(
                    record
                        .opening_prompt
                        .clone()
                        .unwrap_or_else(|| record.brief.clone()),
                ),
            })
            .await
        {
            Ok(session) => session,
            Err(err) => {
                let error = format!(
                    "{err}; worker '{id}' retains its checkout; retry with the same worker ID"
                );
                self.fail_worker(&id, error.clone()).await;
                return Err(error);
            }
        };
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(true));
        let receiver = session.events();
        let strikes = Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT)));
        let steer_pending = Arc::new(AtomicBool::new(false));
        let pump = tokio::spawn(pump_worker(
            id.clone(),
            generation,
            self.engine.clone(),
            session_id,
            receiver,
            self.events.clone(),
            busy.clone(),
            self.messages.clone(),
            self.records.clone(),
            self.usage.clone(),
            self.save_lock.clone(),
            self.pricing.clone(),
            self.persistence.clone(),
            self.routing_log_path.clone(),
            self.routing.clone(),
            self.signals.clone(),
            strikes.clone(),
            steer_pending.clone(),
            self.models.clone(),
            self.tiers.clone(),
            self.live.clone(),
            self.fail_next_close.clone(),
            self.board(),
        ));
        if let Err(err) = session
            .send(SessionCommand::Prompt {
                text: kickoff_message(&record).to_string(),
                images: Vec::new(),
            })
            .await
        {
            pump.abort();
            let _ = session.close().await;
            let error = format!(
                "could not send the opening prompt to worker '{id}': {err}; retry with the same worker ID"
            );
            self.fail_worker(&id, error.clone()).await;
            return Err(error);
        }
        self.live.lock().await.insert(
            id.clone(),
            LiveWorker {
                session,
                busy,
                pump,
                cleanup_retry_only: false,
                strikes,
                steer_pending,
            },
        );
        self.update_worker(&id, |worker| {
            worker.status = WorkerStatus::Working;
            worker.generation = generation;
            worker.brief_sent = true;
            worker.error = None;
        })
        .await;
        if let Err(err) = self.persist().await {
            let _ = self.stop_inner(&id).await;
            return Err(format!(
                "worker '{id}' accepted the brief but its ledger update failed: {err}; use the same worker ID and inspect team_status"
            ));
        }
        self.worker(&id).await
    }

    /// The trace identity for one worker (plan c3ba2b58 step B): built from
    /// its record at spawn, so every model call the worker makes is
    /// attributed to this worker, its plan, and its step. The agent and
    /// session ids are filled in by the spawn path; the plan step converts
    /// from the record's 0-based step to the plan's 1-based numbering.
    pub(super) fn worker_trace_identity(record: &WorkerRecord) -> TraceIdentity {
        TraceIdentity {
            actor_kind: TraceActorKind::Worker,
            agent_id: String::new(),
            session_id: String::new(),
            agent_name: record.name.clone(),
            worker_id: Some(record.id.clone()),
            plan_id: record.plan_id.clone(),
            plan_step: record.plan_step.map(|step| step as u32 + 1),
        }
    }

    /// Open one live worker session on this team's engine (the brief's
    /// spawn path): the guardrails match the worker `kind`, the worktree is
    /// bound to the session, and the trace identity attributes every model
    /// call to this worker (plan c3ba2b58 step B). The session's display
    /// name is the trace identity's `agent_name` — both are the worker
    /// record's name at every call site.
    pub(crate) async fn spawn_worker_session(
        &self,
        spec: WorkerSessionSpec,
    ) -> Result<AgentSession, String> {
        let WorkerSessionSpec {
            model,
            reasoning_effort,
            worktree,
            resume,
            kind,
            trace_identity,
            pinned_task,
        } = spec;
        let name = trace_identity.agent_name.clone();
        self.engine
            .spawn(SpawnOptions {
                name: Some(name),
                model,
                reasoning_effort,
                worktree: Some(worktree),
                permissions: Some(PermissionPolicy::OverridePermissions),
                policy: None,
                resume,
                limits: None,
                host_tools: self.worker_host_tools(),
                external_policy: None,
                guardrails: Some(guardrails_for(&kind)),
                // Plan c3ba2b58 step B: the worker's trace identity, built
                // from its record — every model call the worker makes is
                // attributed to this worker, its plan, and its step.
                trace_identity: Some(trace_identity),
                // Plan 40eef714 step 2: the worker's whole task — the plan,
                // its step spec, and the lead's brief — pinned into the
                // system head so compaction and model switches never
                // summarize it away.
                pinned_task,
                // Plan 2ba2168a step 1: the worker's turns are never
                // classified by the per-prompt gate — Mnemo2 routes at
                // dispatch instead.
                skip_turn_routing: true,
            })
            .await
            .map_err(|err| format!("could not spawn the worker session: {err}"))
    }

    /// Mark a worker failed after its session could not spawn or resume.
    /// Failed-and-stopped is a terminal status (design §4.7): the worker's
    /// `outcome` row is appended once the record persists.
    async fn fail_worker(&self, worker_id: &str, error: String) {
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Failed;
            worker.error = Some(error);
        })
        .await;
        let _ = self.persist().await;
        if let Ok(record) = self.worker(worker_id).await {
            // A failure is not terminal — the worker can still be resumed
            // on a stronger tier and take its nudge — so only the outcome
            // row, never nudge_ignored.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "failed") {
                    self.append_dispatch_routing_row(&row);
                }
            }
        }
    }

}
