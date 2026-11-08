// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Worker control: messages and steers, drift checks, pause, stop,
//! resume, model changes and compaction - everything that drives a live
//! worker session from the outside.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mnemo::route_question;
use mnemo::app::worktrees::current_branch;
use mnemo::headless::{SessionCommand, Transcript};
use mnemo::headless::session::ModelSwitch;
use tokio::sync::Mutex;

use crate::mnemo2::phase3;
use crate::mnemo2::dispatch_routing::{
    DispatchRoutingRow, RoutingMode, TEXT_MAX_CHARS, classify_dispatch,
};

use super::{
    LiveWorker, RECOVERED_WORKTREE_CLEANUP_ERROR, TeamRuntime, WorkerSessionSpec,
    cleanup_agent_worktree, merge_conflicts_only, merge_record_from_git, now_key,
};
use super::nudge::{
    NUDGE_STRIKE_LIMIT, claim_nudge, deliver_nudge, message_can_show_drift, nudge_ignored_row,
};
use super::pump::{pump_worker, snapshot_model};
use super::record::{
    ChangeSource, InboxEntry, InboxKind, MessageSender, WorkerRecord, WorkerStatus, append_inbox,
    audit_finished, audit_finished_error, kickoff_message, note_model, session_model_spec,
};
use super::routing::{
    NUDGE_LADDER, final_tier, logs_label, logs_routing, next_tier_above, outcome_row,
    reroute_destination,
};

impl TeamRuntime {
    /// Send a message to a worker with its sender attached. A busy worker gets
    /// it as a steer; an idle worker starts a new turn with it.
    pub async fn message(
        &self,
        worker_id: &str,
        text: String,
        sender: MessageSender,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        self.message_inner(worker_id, text, sender).await
    }

    pub(super) async fn message_inner(
        &self,
        worker_id: &str,
        text: String,
        sender: MessageSender,
    ) -> Result<WorkerRecord, String> {
        self.message_inner_with_kind(worker_id, text, sender, None)
            .await
    }

    pub(super) async fn message_inner_with_kind(
        &self,
        worker_id: &str,
        text: String,
        sender: MessageSender,
        kind: Option<InboxKind>,
    ) -> Result<WorkerRecord, String> {
        if text.trim().is_empty() {
            return Err("the worker message is empty".to_string());
        }
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err(
                "a cleaned worker cannot accept new work; review its retained branch".to_string(),
            );
        }
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        if record.compacting {
            return Err(
                "wait for the worker's compaction to finish before messaging it".to_string(),
            );
        }
        let previous_inbox = record.inbox.clone();
        let delivered_text = format!("{}\n{text}", sender.header());
        let (busy, idle, steer_pending, sent) = {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running; resume it first"))?;
            live.admit_session_command(worker_id)?;
            let busy = live.busy.clone();
            let steer_pending = live.steer_pending.clone();
            let idle = !busy.load(Ordering::SeqCst);
            let kind = kind.unwrap_or(if idle {
                InboxKind::Prompt
            } else {
                InboxKind::Steer
            });
            self.update_worker(worker_id, |worker| {
                append_inbox(
                    worker,
                    InboxEntry {
                        sender,
                        kind,
                        text: text.clone(),
                        at_ms: now_key(),
                    },
                );
            })
            .await;
            // Persist the receipt before the command can end another turn.
            if let Err(err) = self.persist().await {
                self.update_worker(worker_id, |worker| {
                    worker.inbox = previous_inbox.clone();
                })
                .await;
                let _ = self.persist().await;
                return Err(err);
            }
            if idle {
                busy.store(true, Ordering::SeqCst);
            }
            let command = if idle {
                SessionCommand::Prompt {
                    text: delivered_text,
                    images: Vec::new(),
                }
            } else {
                SessionCommand::Steer {
                    text: delivered_text,
                    images: Vec::new(),
                }
            };
            // A steer ends the current turn at its next break and drives a
            // follow-up turn: mark the wait before the command can land, so the
            // pump never reads the steered turn end as a finished turn.
            if !idle {
                steer_pending.store(true, Ordering::SeqCst);
            }
            let result = live.session.send(command).await;
            (busy, idle, steer_pending, result)
        };
        if let Err(err) = sent {
            if idle {
                busy.store(false, Ordering::SeqCst);
            }
            if !idle {
                // The steer never reached the session: there is no follow-up
                // turn to wait for.
                steer_pending.store(false, Ordering::SeqCst);
            }
            self.update_worker(worker_id, |worker| worker.inbox = previous_inbox)
                .await;
            let _ = self.persist().await;
            return Err(format!("could not message the worker: {err}"));
        }
        self.update_worker(worker_id, |worker| {
            worker.status = if worker.fixer_finding_id.is_some() {
                WorkerStatus::Fixing
            } else {
                WorkerStatus::Working
            };
            worker.pending = None;
            // A delivered message starts the worker over: a stale failure is
            // no longer the record's truth.
            worker.error = None;
        })
        .await;
        self.persist().await?;
        // `record` was read before this message joined the inbox.
        if message_can_show_drift(&record, sender) {
            self.spawn_drift_check(worker_id, &text).await;
        }
        self.worker(worker_id).await
    }

    /// The drift nudge (design §4.6): a delivered lead or user message that
    /// [`message_can_show_drift`] lets through and that Laya reads a tier above
    /// the worker's own nudges the lead — in the
    /// background, so it never blocks or fails the message. Off in routing
    /// mode `off` (no classifier calls) and when nudges are disabled.
    async fn spawn_drift_check(&self, worker_id: &str, text: &str) {
        let settings = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.mode == RoutingMode::Off || !settings.nudges {
            return;
        }
        if text.split_whitespace().count() < settings.min_words {
            return;
        }
        let Some(classifier) = self
            .classifier
            .as_ref()
            .and_then(|slot| slot.read().unwrap_or_else(|e| e.into_inner()).clone())
        else {
            return;
        };
        // The pre-checks run without detaching: a worker without a routing
        // record, already nudged, or on the top enabled tier never spends
        // a classifier call on drift. The enabled set is built from this
        // worker's own model, so a nudge never suggests the model it runs.
        let (enabled, worth_asking) = {
            let records = self.records.lock().await;
            let Some(worker) = records.iter().find(|worker| worker.id == worker_id) else {
                return;
            };
            let Some(routing) = worker.routing.as_ref() else {
                return;
            };
            let enabled = self.direct_nudge_tiers(worker.model.as_deref());
            (
                enabled,
                routing.nudged.is_none()
                    && next_tier_above(enabled, &final_tier(worker)).is_some(),
            )
        };
        if !worth_asking {
            return;
        }
        let records = self.records.clone();
        let signals = self.signals.clone();
        let routing_log_path = self.routing_log_path.clone();
        let permit = self.routing_permit.clone();
        let route_knn = self.route_knn.clone();
        let worker_id = worker_id.to_string();
        let text: String = text.chars().take(TEXT_MAX_CHARS).collect();
        let mode = settings.mode;
        tokio::spawn(async move {
            // The same 3 s timeout and one-at-a-time permit as dispatch,
            // without holding the merge lock: a message never waits on
            // Laya.
            let classified = async {
                let _permit = permit.acquire().await;
                classify_dispatch(
                    classifier.as_ref(),
                    route_knn.as_deref(),
                    &text,
                    enabled,
                    &settings,
                )
                .await
            };
            let decision =
                match tokio::time::timeout(std::time::Duration::from_secs(3), classified).await {
                    Ok(Some(decision)) => decision,
                    _ => return,
                };
            if !decision.cleared {
                return;
            }
            let Some(to_tier) = decision.tier else {
                return;
            };
            let Some(claim) =
                claim_nudge(&records, &worker_id, enabled, mode, Some(to_tier)).await
            else {
                return;
            };
            let detail = format!(
                "this got harder (Laya reads the message as {}, {:.2})",
                decision.label, decision.confidence
            );
            deliver_nudge(
                &signals,
                &routing_log_path,
                &worker_id,
                &claim,
                "drift",
                &detail,
                "",
            );
        });
    }

    /// Pause the worker's current turn without ending its session.
    pub async fn pause(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
            live.admit_session_command(worker_id)?;
            live.session
                .send(SessionCommand::Interrupt)
                .await
                .map_err(|err| format!("could not pause the worker: {err}"))?;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Paused;
            worker.pending = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Interrupt the worker's current turn but keep the session open.
    pub async fn interrupt(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
            live.admit_session_command(worker_id)?;
            live.session
                .send(SessionCommand::Interrupt)
                .await
                .map_err(|err| format!("could not interrupt the worker: {err}"))?;
        }
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Interrupted;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Stop and detach a worker. The worktree and `wt/agent-*` branch stay for
    /// audit; `resume` can reattach the saved transcript later. A worker that
    /// was not already stopped has reached a terminal status, so its `outcome`
    /// row (design §4.7) is appended beside the record.
    pub async fn stop(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let before = self.worker(worker_id).await?;
        let record = self.stop_inner(worker_id).await?;
        if !matches!(before.status, WorkerStatus::Stopped) {
            // A hand reroute is a learning in every mode, so a rerouted
            // worker's outcome row is written even off; the nudge row is
            // Laya-derived and keeps the `logs_routing` gate.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "stopped") {
                    self.append_dispatch_routing_row(&row);
                }
            }
            if logs_routing(&record, self.routing_mode()) {
                if let Some(row) = nudge_ignored_row(&record) {
                    self.append_dispatch_routing_row(&row);
                }
            }
        }
        Ok(record)
    }

    pub(super) async fn stop_inner(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker has no checkout to stop".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be stopped".to_string());
        }
        let preserve_downshift_finding = record.reroutes.last().is_some_and(|row| {
            matches!(
                row,
                DispatchRoutingRow::LeadReroute {
                    from_tier,
                    to_tier,
                    nudged: true,
                    ..
                } if NUDGE_LADDER.iter().position(|tier| tier == from_tier)
                    > NUDGE_LADDER.iter().position(|tier| tier == to_tier)
            )
        });
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Stopped;
            worker.pending = None;
            if !preserve_downshift_finding {
                worker.fixer_finding_id = None;
            }
            worker.compacting = false;
        })
        .await;
        let live = self.live.lock().await.remove(worker_id);
        if let Some(mut live) = live {
            let snapshot = live.session.snapshot().await.ok();
            live.abort();
            if let Some(snapshot) = snapshot {
                self.update_worker(worker_id, |worker| {
                    if !snapshot.transcript.messages.is_empty() {
                        worker.transcript = Arc::new(snapshot.transcript.clone());
                        worker.used_tokens = snapshot.used_tokens;
                        worker.max_tokens = snapshot.max_tokens;
                        if worker.model_id.is_none() {
                            worker.model_id = snapshot_model(&snapshot);
                        }
                    } else if worker.transcript.messages.is_empty() && worker.brief_sent {
                        worker.transcript = Arc::new(Transcript {
                            messages: vec![mnemo::provider::Message::user_text(
                                kickoff_message(worker).to_string(),
                            )],
                        });
                    }
                })
                .await;
            }
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                let message = format!("could not close worker session: {error}");
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(worker_id.to_string(), live);
                self.update_worker(worker_id, |worker| {
                    worker.error = Some(message.clone());
                })
                .await;
                self.persist().await?;
                return Err(message);
            }
        }
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Reattach a stopped/interrupted worker from its saved transcript. The
    /// session comes back idle; the lead then sends it a message.
    pub async fn resume(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        self.resume_inner(worker_id).await
    }

    pub(super) async fn resume_inner(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let live_idle = {
            let live = self.live.lock().await;
            match live.get(worker_id) {
                Some(live) => {
                    live.admit_session_command(worker_id)?;
                    Some(!live.busy.load(Ordering::SeqCst))
                }
                None => None,
            }
        };
        if let Some(idle) = live_idle {
            let record = self.worker(worker_id).await?;
            // A failed worker whose session is still alive and idle is
            // recoverable in place: the lead can keep the same model with
            // `team_message`, or retry on another one through a resume that
            // respawns the session.
            if idle && record.status == WorkerStatus::Failed {
                self.update_worker(worker_id, |worker| {
                    worker.status = WorkerStatus::Idle;
                    worker.error = None;
                    worker.pending = None;
                })
                .await;
                // Snapshot the resumed record before the save: a `Finished`
                // event still draining from the failed turn can otherwise
                // turn this resume's result back into `NeedsYou`.
                let resumed = self.worker(worker_id).await?;
                self.persist().await?;
                return Ok(resumed);
            }
            return Err(format!(
                "worker '{worker_id}' is already running; team_message it to continue"
            ));
        }
        let active = self.live.lock().await.len();
        let slots = self.persistence.slots.load(Ordering::SeqCst);
        if active >= slots {
            return Err(format!("all {slots} worker slots are busy"));
        }
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err(
                "a cleaned worker cannot be resumed; review its retained branch".to_string(),
            );
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot be resumed".to_string());
        }
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        if !record.worktree.is_dir() {
            return Err(format!(
                "the worker's worktree is missing: {}",
                record.worktree.display()
            ));
        }
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let session = self
            .spawn_worker_session(WorkerSessionSpec {
                model: record.model.clone(),
                reasoning_effort: record.reasoning_effort.clone(),
                worktree: record.worktree.clone(),
                resume: Some((*record.transcript).clone()),
                kind: record.kind.clone(),
                trace_identity: Self::worker_trace_identity(&record),
                pinned_task: Some(
                    record
                        .opening_prompt
                        .clone()
                        .unwrap_or_else(|| record.brief.clone()),
                ),
            })
            .await?;
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(false));
        let receiver = session.events();
        let strikes = Arc::new(Mutex::new(route_question::Strikes::new(NUDGE_STRIKE_LIMIT)));
        let steer_pending = Arc::new(AtomicBool::new(false));
        let pump = tokio::spawn(pump_worker(
            worker_id.to_string(),
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
        self.live.lock().await.insert(
            worker_id.to_string(),
            LiveWorker {
                session,
                busy,
                pump,
                cleanup_retry_only: false,
                strikes,
                steer_pending,
            },
        );
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Idle;
            worker.generation = generation;
            worker.pending = None;
            worker.error = None;
            worker.compacting = false;
            let model = session_model_spec(worker).to_string();
            note_model(worker, &model);
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// Move a worker to another model (and reasoning effort), keeping its
    /// transcript, branch, and worktree. A WORKING worker switches live at
    /// its next request: the call in flight finishes on the old model, the
    /// next one serves on the new one — no stop, no restart. An audit
    /// worker's model is never switchable. An idle live session is
    /// restarted on the new model; a stopped, interrupted, or failed one
    /// is resumed on it; a worker whose brief never reached a session is
    /// started with it.
    ///
    /// A switch that moves the model appends a `lead_reroute` or
    /// `user_reroute` row (design §4.7) — by [`source`] — carrying the
    /// route before and after, in every routing mode: a hand-moved model is
    /// always a learning (decided 2026-11-04). It repoints the record's
    /// tier at the destination tier (`None` for a custom model) and clears
    /// a pending nudge (the nudge was answered by a hand, not by Laya). A
    /// same-tier switch logs with `from_tier == to_tier` and never counts
    /// as an override; an effort-only change logs nothing.
    pub async fn change_model(
        &self,
        worker_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        source: ChangeSource,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        self.change_model_locked(worker_id, model, reasoning_effort, source)
            .await
    }

    pub(super) async fn change_model_locked(
        &self,
        worker_id: &str,
        model: Option<String>,
        reasoning_effort: Option<String>,
        source: ChangeSource,
    ) -> Result<WorkerRecord, String> {
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Cancelled {
            return Err("a cleaned worker cannot change its model".to_string());
        }
        if matches!(record.status, WorkerStatus::Merged | WorkerStatus::Merging) {
            return Err("a merged worker cannot change its model".to_string());
        }
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        self.validate_model(&model, &reasoning_effort)?;
        let from_spec = session_model_spec(&record).to_string();
        let nudged = record
            .routing
            .as_ref()
            .and_then(|routing| routing.nudged.clone());
        // The record keeps its reroute either way (team.json bookkeeping),
        // and the calibration log gets it too: a hand-moved model is a
        // learning in every routing mode (decided 2026-11-04).
        let requested = model.as_deref().filter(|spec| !spec.trim().is_empty());
        if self.worker_is_busy(worker_id).await {
            // Live switch — no stop, no restart: the provider request in
            // flight finishes on the old model; the worker's next request
            // serves on the new one. The kernel emits its model_changed
            // agent event once the new model serves, and the pump re-points
            // `model_id` from it. Moving a working worker to routing
            // (model: None) needs the restart path below — refuse it.
            let Some(spec) = model.clone() else {
                return Err(format!(
                    "worker '{worker_id}' is working; a live model switch needs a model; stop the worker to move it to routing"
                ));
            };
            let reroute =
                self.reroute_row(&record, &from_spec, requested, source, nudged.as_deref());
            let switched = {
                let live = self.live.lock().await;
                let live = live
                    .get(worker_id)
                    .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
                live.admit_session_command(worker_id)?;
                live.session
                    .set_model(&spec, reasoning_effort.clone())
                    .await
            };
            let new_model_id = match switched {
                Ok(ModelSwitch::NextRequest { model: served })
                | Ok(ModelSwitch::CompactsFirst { model: served }) => served,
                Err(error) => {
                    return Err(format!(
                        "could not switch worker '{worker_id}' to '{spec}': {error}"
                    ));
                }
            };
            self.update_worker(worker_id, |worker| {
                worker.model = model.clone();
                // Never erase an already-observed id that already matches
                // the new model: the new model's events may have been
                // pumped before this update (the pin landed mid-turn, or
                // the target was already serving) — an unconditional clear
                // could leave the record without its correct id, with no
                // later event left to repair it.
                if worker.model_id.as_deref() != Some(new_model_id.as_str()) {
                    worker.model_id = None;
                }
                worker.reasoning_effort = reasoning_effort.clone();
                if let Some(row) = &reroute {
                    worker.tier = reroute_destination(row);
                    worker.reroutes.push(row.clone());
                    if let Some(routing) = worker.routing.as_mut() {
                        routing.nudged = None;
                    }
                }
                let model = session_model_spec(worker).to_string();
                note_model(worker, &model);
            })
            .await;
            self.persist().await?;
            if reroute.is_some() {
                // A reroute resets the nudge budget (design §4.6): the
                // strike counter starts over on the new tier. The stopped
                // path below drops the live worker; its replacement on
                // resume builds a fresh counter.
                if let Some(live) = self.live.lock().await.get(worker_id) {
                    live.strikes.lock().await.reset();
                }
            }
            if record.kind != "audit" && source != ChangeSource::FixDownshift {
                if let Some(row) = reroute.as_ref() {
                    self.append_reroute_row(row, source, None);
                }
            }
            return self.worker(worker_id).await;
        }
        if self.live.lock().await.contains_key(worker_id) {
            self.stop_inner(worker_id).await?;
        }
        let reroute = self.reroute_row(&record, &from_spec, requested, source, nudged.as_deref());
        self.update_worker(worker_id, |worker| {
            worker.model = model.clone();
            worker.model_id = None;
            worker.reasoning_effort = reasoning_effort.clone();
            if let Some(row) = &reroute {
                worker.tier = reroute_destination(row);
                worker.reroutes.push(row.clone());
                if let Some(routing) = worker.routing.as_mut() {
                    routing.nudged = None;
                }
            }
            let model = session_model_spec(worker).to_string();
            note_model(worker, &model);
        })
        .await;
        self.persist().await?;
        if record.kind != "audit" && source != ChangeSource::FixDownshift {
            if let Some(row) = reroute.as_ref() {
                self.append_reroute_row(row, source, None);
            }
        }
        let record = self.worker(worker_id).await?;
        if !record.brief_sent {
            if !record.worktree.is_dir() {
                return Err(format!("worker '{worker_id}' has no recoverable worktree"));
            }
            return self.start_worker(record).await;
        }
        self.resume_inner(worker_id).await
    }

    /// Compact a quiescent worker. Blocked or stopped sessions are resumed once.
    pub async fn compact(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let mut record = self.worker(worker_id).await?;
        if record.compacting {
            return Err(format!("worker '{worker_id}' is already compacting"));
        }
        if audit_finished(&record) {
            return Err(audit_finished_error(worker_id));
        }
        if record.status == WorkerStatus::NeedsYou && self.live.lock().await.contains_key(worker_id)
        {
            self.stop_inner(worker_id).await?;
            record = self.worker(worker_id).await?;
        }
        if !self.live.lock().await.contains_key(worker_id) {
            if !matches!(
                record.status,
                WorkerStatus::Stopped
                    | WorkerStatus::Interrupted
                    | WorkerStatus::Paused
                    | WorkerStatus::Completed
                    | WorkerStatus::Failed
                    | WorkerStatus::NeedsYou
                    | WorkerStatus::Idle
            ) {
                return Err(format!(
                    "worker '{worker_id}' cannot be compacted in its current state"
                ));
            }
            record = self.resume_inner(worker_id).await?;
        }
        let busy = {
            let live = self.live.lock().await;
            let live = live
                .get(worker_id)
                .ok_or_else(|| format!("worker '{worker_id}' is not running"))?;
            live.admit_session_command(worker_id)?;
            if live.busy.swap(true, Ordering::SeqCst) {
                return Err(format!(
                    "worker '{worker_id}' is busy; compact it when it becomes idle"
                ));
            }
            live.busy.clone()
        };
        let previous = record;
        self.update_worker(worker_id, |worker| {
            worker.compacting = true;
            worker.status = WorkerStatus::Working;
            worker.error = None;
        })
        .await;
        if let Err(error) = self.persist().await {
            busy.store(false, Ordering::SeqCst);
            self.update_worker(worker_id, |worker| *worker = previous.clone())
                .await;
            let _ = self.persist().await;
            return Err(error);
        }
        let sent = {
            let live = self.live.lock().await;
            match live.get(worker_id) {
                Some(live) => Some(live.session.send(SessionCommand::Compact).await),
                None => None,
            }
        };
        let Some(sent) = sent else {
            busy.store(false, Ordering::SeqCst);
            self.update_worker(worker_id, |worker| *worker = previous.clone())
                .await;
            self.persist().await?;
            return Err(format!("worker '{worker_id}' stopped before compaction"));
        };
        if let Err(error) = sent {
            busy.store(false, Ordering::SeqCst);
            self.update_worker(worker_id, |worker| *worker = previous)
                .await;
            self.persist().await?;
            return Err(format!("could not compact the worker: {error}"));
        }
        self.worker(worker_id).await
    }
}
pub(super) async fn recover_worker(root: &std::path::Path, worker: &mut WorkerRecord) -> Result<(), String> {
    worker.brief_sent = true;
    worker.compacting = false;
    let legacy_budget_pause = worker.error.as_deref().is_some_and(|error| {
        error.starts_with("worker token budget of ")
            || matches!(
                error,
                "increase the worker token budget before continuing"
                    | "increase the worker token budget before resuming"
            )
    });
    if legacy_budget_pause {
        worker.error = None;
        if worker.pending.is_none()
            && matches!(worker.status, WorkerStatus::NeedsYou | WorkerStatus::Failed)
        {
            worker.status = WorkerStatus::Interrupted;
        }
    }
    if worker.total_tokens == 0 && worker.used_tokens > 0 {
        worker.total_tokens = worker.used_tokens as u64;
        worker.prompt_tokens = worker.used_tokens as u64;
    }
    if worker.status == WorkerStatus::Merging {
        let merge_head = phase3::git(root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).await?;
        let lead_tip = phase3::tip(root).await?;
        let second_parent = phase3::git_value(root, &["rev-parse", &format!("{lead_tip}^2")])
            .await
            .ok();
        let reviewed_tip = worker.review.as_ref().map(|review| review.tip.clone());
        let completed = reviewed_tip.is_some() && second_parent == reviewed_tip;
        if completed {
            let target = current_branch(root)?;
            if target == "main" {
                return Err(
                    "an interrupted worker merge appears on main; inspect it before continuing"
                        .to_string(),
                );
            }
            phase3::require_clean_lead(root, "lead after completed merge").await?;
            if merge_head.status.success() {
                let reset = phase3::git(root, &["reset", "--merge"]).await?;
                if !reset.status.success() {
                    return Err(phase3::git_error(&["reset", "--merge"], &reset));
                }
                phase3::require_clean_lead(root, "lead after stale merge cleanup").await?;
            }
            let record = merge_record_from_git(root, worker, lead_tip, target).await?;
            worker.status = WorkerStatus::Merged;
            worker.merge = Some(record);
            worker.error = None;
            if let Err(err) = cleanup_agent_worktree(root, worker).await {
                worker.error = Some(format!("{RECOVERED_WORKTREE_CLEANUP_ERROR}: {err}"));
            }
            return Ok(());
        }

        if merge_head.status.success() {
            if !merge_conflicts_only(root).await? {
                worker.status = WorkerStatus::Conflict;
                worker.error = Some(
                    "the interrupted merge is retained because the lead has unrelated changes or no verifiable conflict-only state; inspect it before recovery"
                        .to_string(),
                );
                return Ok(());
            }
            let abort = phase3::git(root, &["merge", "--abort"]).await?;
            if !abort.status.success() {
                return Err(phase3::git_error(&["merge", "--abort"], &abort));
            }
            phase3::require_clean_lead(root, "lead after merge abort").await?;
            worker.status = WorkerStatus::Conflict;
            worker.error = Some(
                "the interrupted merge was aborted; resolve the conflict and review the worker tip again"
                    .to_string(),
            );
        } else {
            phase3::require_clean_lead(root, "lead after interrupted merge").await?;
            worker.status = WorkerStatus::Conflict;
            worker.error = Some(
                "the interrupted merge left no verifiable merge commit; inspect the lead and worker before retrying"
                    .to_string(),
            );
        }
        worker.review = None;
        worker.checks.clear();
        worker.merge = None;
        worker.fixer_finding_id = None;
    } else if matches!(
        worker.status,
        WorkerStatus::Dispatching
            | WorkerStatus::Working
            | WorkerStatus::NeedsYou
            | WorkerStatus::Fixing
    ) {
        worker.status = if worker.pending.is_some() {
            WorkerStatus::NeedsYou
        } else {
            WorkerStatus::Interrupted
        };
    }
    Ok(())
}
