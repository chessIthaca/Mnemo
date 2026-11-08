// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The worker event pump: one tokio task per live worker that folds
//! session events into the record, serves host tools, notices cost and
//! releases a finished audit.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mnemo::{SerializableAgentEvent, route_question};
use mnemo::app::plan_board::PlanBoard;
use mnemo::headless::{
    AgentSession, Engine, EventReceiver, PendingInteraction, SessionCommand, SessionEvent,
    SessionId, Transcript,
};
use mnemo::tool::ToolResult;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};

use crate::mnemo2::phase3;
use crate::mnemo2::dispatch_routing::{DispatchRoutingRow, RoutingMode, RoutingSettings};
use crate::mnemo2::pricing::usage_model_key;
use crate::mnemo2::runtime::LeadModel;
use crate::mnemo2::usage::{ProjectUsage, event_session};

use super::{BoardSignal, LiveWorker, TeamEvent, TeamMessage};
use super::escalation::{AutoEscalation, auto_escalate_idle};
use super::nudge::{StrikeNudges, note_tool_outcome, try_strike_nudge};
use super::persistence::{LEDGER_FLUSH_DELAY, PersistedTeam, TeamPersistence};
use super::record::{WorkerRecord, WorkerStatus, kickoff_message};
use super::review::clear_review_for_a_newer_tip;
use super::routing::{logs_label, outcome_row};

/// The plan coordinate a worker may read: its own plan, or for an audit the
/// plan of the worker it audits. The step stays 0-based, like the record.
async fn worker_plan_stamp(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    worker_id: &str,
) -> Option<(String, usize)> {
    let records = records.lock().await;
    let record = records.iter().find(|worker| worker.id == worker_id)?;
    let source = if record.kind == "audit" {
        let target = record.audit_target.as_deref()?;
        records.iter().find(|worker| worker.id == target)?
    } else {
        record
    };
    Some((source.plan_id.clone()?, source.plan_step?))
}

/// Handle a worker's host-tool call (`message_parent`, `plan_read`) and return
/// its result.
pub(super) async fn worker_host_tool_result(
    messages: &Arc<Mutex<Vec<TeamMessage>>>,
    worker_id: &str,
    name: &str,
    args: &Value,
    board: Option<&Arc<PlanBoard>>,
    plan_stamp: Option<&(String, usize)>,
) -> ToolResult {
    if name == "plan_read" {
        let Some(board) = board else {
            return ToolResult::error("the plan board is not available");
        };
        if let Some(capability) = args.get("capability") {
            let Some(capability) = capability.as_str() else {
                return ToolResult::error("plan_read expects 'capability' to be a string");
            };
            return match board.capability_spec(capability) {
                Ok(Some(text)) => ToolResult::success(text),
                Ok(None) => {
                    ToolResult::success(format!("No current spec for '{capability}' yet."))
                }
                Err(error) => ToolResult::error(error),
            };
        }
        let Some((plan_id, step)) = plan_stamp else {
            return ToolResult::error("you have no plan to read");
        };
        return match board.worker_view(plan_id, step + 1) {
            Ok(text) => ToolResult::success(text),
            Err(error) => ToolResult::error(error),
        };
    }
    if name != "message_parent" {
        return ToolResult::error(format!("unknown worker host tool '{name}'"));
    }
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return ToolResult::error("message_parent requires a 'text' string");
    };
    let at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    messages.lock().await.push(TeamMessage {
        worker_id: worker_id.to_string(),
        text: text.to_string(),
        at_ms,
    });
    ToolResult::success(
        "message sent to the lead. A reply, if any, arrives later as a new message that starts with a [From …] header; do not assume or invent a reply — continue your task or end your turn.",
    )
}

/// Send a host-tool result back to the worker session.
async fn send_worker_host_result(
    engine: &Arc<Engine>,
    session_id: SessionId,
    call_id: String,
    result: ToolResult,
) {
    if let Some(session) = engine.session(&session_id).await {
        let _ = session
            .send(SessionCommand::HostToolResult {
                call_id,
                result: Some(result),
                error: None,
            })
            .await;
    }
}

pub(super) fn snapshot_model(snapshot: &mnemo::headless::SessionSnapshot) -> Option<String> {
    snapshot.model.clone().or_else(|| {
        snapshot
            .transcript
            .messages
            .iter()
            .rev()
            .find_map(|message| message.origin_model.clone())
    })
}

pub(super) fn usage_cost_micros(
    pricing: &Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    model: Option<&str>,
    prompt_tokens: u64,
    cached_tokens: u64,
    completion_tokens: u64,
) -> Option<u64> {
    let pricing = pricing.read().expect("pricing lock poisoned");
    let model = model?;
    let bare_model = model.rsplit('/').next().unwrap_or(model);
    let rates = pricing
        .iter()
        .find(|entry| entry.model == model || entry.model == bare_model)?;
    let uncached = prompt_tokens.saturating_sub(cached_tokens) as f64;
    let micros = uncached * rates.input_per_1m
        + cached_tokens as f64 * rates.cached_per_1m
        + completion_tokens as f64 * rates.output_per_1m;
    if !micros.is_finite() || micros < 0.0 {
        return None;
    }
    Some(micros.round().min(u64::MAX as f64) as u64)
}

/// The new highest threshold passed, when a cost notice is due. `threshold`
/// is 0 when notices are off.
pub(super) fn cost_notice_due(cost: u64, notified: u64, threshold: u64) -> Option<u64> {
    if threshold == 0 || cost < threshold {
        return None;
    }
    let mut level = threshold;
    loop {
        let doubled = level.saturating_mul(2);
        // `saturating_mul` stops growing at the u64 ceiling: stop there too,
        // so a pathological cost can never spin this loop.
        if doubled > cost || doubled <= level {
            break;
        }
        level = doubled;
    }
    (level > notified).then_some(level)
}

/// Whether this worker's record (this generation, or a generation-0 record)
/// has no observed model id yet.
async fn worker_lacks_model_id(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    worker_id: &str,
    generation: u64,
) -> bool {
    records
        .lock()
        .await
        .iter()
        .find(|worker| {
            worker.id == worker_id && (worker.generation == 0 || worker.generation == generation)
        })
        .is_some_and(|worker| worker.model_id.is_none())
}

/// Forward one worker session's events, update its durable record, and refresh
/// the transcript when the turn ends.
/// How long a steered turn end waits for its follow-up turn before it counts
/// as finished, so a lost steer can never leave a worker Working forever.
const FOLLOW_UP_GRACE: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_millis(200)
} else {
    std::time::Duration::from_secs(60)
};

// Each parameter is one shared worker handle — the pump's wiring stays flat.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pump_worker(
    worker_id: String,
    generation: u64,
    engine: Arc<Engine>,
    session_id: SessionId,
    mut receiver: EventReceiver<SessionEvent>,
    events: broadcast::Sender<TeamEvent>,
    busy: Arc<AtomicBool>,
    messages: Arc<Mutex<Vec<TeamMessage>>>,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    persistence: TeamPersistence,
    routing_log_path: PathBuf,
    routing: Arc<std::sync::RwLock<RoutingSettings>>,
    signals: broadcast::Sender<BoardSignal>,
    strikes: Arc<Mutex<route_question::Strikes>>,
    steer_pending: Arc<AtomicBool>,
    models: Arc<std::sync::RwLock<Vec<LeadModel>>>,
    tiers: Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    live: Arc<Mutex<HashMap<String, LiveWorker>>>,
    fail_next_close: Arc<AtomicBool>,
    board: Option<Arc<PlanBoard>>,
) {
    let mut turn_active = false;
    // A steered turn end is held here until its follow-up turn starts (the
    // held `Finished`) or `FOLLOW_UP_GRACE` passes (the deadline). Both are
    // cleared by `Started`, `Exited`, a final `Error`, and the deadline.
    let mut follow_up_deadline: Option<tokio::time::Instant> = None;
    let mut held_finished: Option<SessionEvent> = None;
    // A `Parked` that settled a held steered turn end: its held `Finished` is
    // re-sent after `save_workers`, so the finished notice reads Completed.
    let mut settled_parked_finish: Option<SessionEvent> = None;
    let mut plan_stamp = worker_plan_stamp(&records, &worker_id).await;
    // The `outcome` row owed when the worker lands in a terminal status this
    // iteration; appended once the record save below has persisted it.
    let mut pending_outcome: Option<DispatchRoutingRow> = None;
    // The tool calls whose results have not landed yet (the nudge strike
    // counter, design §4.6): the session streams arguments as index-keyed
    // fragments, matched to results by the provider's call id.
    let mut call_ids: HashMap<u32, String> = HashMap::new();
    let mut pending_calls: HashMap<String, (String, String)> = HashMap::new();
    let mut previous_signature: Option<String> = None;
    loop {
        // While a steered turn end waits for its follow-up, the receive is
        // bounded by an absolute deadline: the bus is engine-wide, so another
        // session's events must neither satisfy nor extend this wait.
        let received = match follow_up_deadline {
            Some(deadline) => match tokio::time::timeout_at(deadline, receiver.recv()).await {
                Ok(received) => received,
                Err(_) => {
                    // The follow-up never started: settle the steered turn end
                    // as this worker's finished turn, so a lost steer can never
                    // leave it Working forever.
                    let mut found = false;
                    let mut completed = false;
                    {
                        let mut records = records.lock().await;
                        if let Some(worker) = records.iter_mut().find(|worker| {
                            worker.id == worker_id
                                && (worker.generation == 0 || worker.generation == generation)
                        }) {
                            if worker.compacting {
                                // A real compaction is running; its own events
                                // follow, so give it another grace period.
                                follow_up_deadline =
                                    Some(tokio::time::Instant::now() + FOLLOW_UP_GRACE);
                                continue;
                            }
                            found = true;
                            if matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Fixing
                                    | WorkerStatus::Dispatching
                            ) {
                                worker.status = WorkerStatus::Completed;
                                worker.turns = worker.turns.saturating_add(1);
                                completed = true;
                            }
                        }
                    }
                    steer_pending.store(false, Ordering::SeqCst);
                    follow_up_deadline = None;
                    let held = held_finished.take();
                    if !found {
                        // The worker was deleted or replaced: nothing to
                        // settle and no notice to fire.
                        continue;
                    }
                    busy.store(false, Ordering::SeqCst);
                    if completed {
                        let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
                    }
                    if let Some(event) = held {
                        // The held turn end, now that the record reads
                        // Completed: this fires the normal finished notice.
                        let _ = events.send(TeamEvent {
                            worker_id: worker_id.clone(),
                            generation,
                            event,
                        });
                    }
                    continue;
                }
            },
            None => receiver.recv().await,
        };
        let event = match received {
            Ok(event) => event,
            // A slow consumer skips what it missed rather than abandoning
            // the worker's ledger mid-turn.
            Err(mnemo::headless::RecvError::Lagged(_)) => continue,
            Err(mnemo::headless::RecvError::Closed) => break,
        };
        // The engine bus is shared with the lead and every other worker:
        // only this worker's own events may answer its host tools or touch
        // its record (status, telemetry, model, errors).
        if !event.is_from(session_id) {
            continue;
        }
        // A follow-up turn started: the steer's turn end is not this worker's
        // finished turn, so the held event and the deadline end here. This
        // runs before the record block, so a paused worker clears the wait too.
        if matches!(event.event, SerializableAgentEvent::Started) {
            steer_pending.store(false, Ordering::SeqCst);
            follow_up_deadline = None;
            held_finished = None;
        }
        if let SerializableAgentEvent::HostToolCall {
            call_id,
            name,
            args,
        } = &event.event
        {
            if name == "plan_read" {
                plan_stamp = worker_plan_stamp(&records, &worker_id).await;
            }
            let result = worker_host_tool_result(
                &messages,
                &worker_id,
                name,
                args,
                board.as_ref(),
                plan_stamp.as_ref(),
            )
            .await;
            send_worker_host_result(&engine, session_id, call_id.clone(), result).await;
            // An audit's report is the message_parent call that delivered
            // it (plan 5779641): mark the record the moment the message is
            // recorded, so the turn's Finished event below can end the
            // session and free the slot it holds.
            if name == "message_parent" {
                let mut records = records.lock().await;
                if let Some(worker) = records.iter_mut().find(|worker| {
                    worker.id == worker_id
                        && (worker.generation == 0 || worker.generation == generation)
                        && worker.kind == "audit"
                }) {
                    worker.report_delivered = true;
                }
            }
        }
        // The nudge strike counter (design §4.6): fingerprint every finished
        // tool call — failures and verbatim repeats of the previous call
        // both count — and try the one nudge after the record block below
        // releases its lock. `repeated` keeps which kind of streak fired.
        let mut strike_fired: Option<bool> = None;
        // The cost alert this event owes the lead (plan 06f1a204): filled
        // inside the record block below, sent once that lock is released.
        let mut cost_notice: Option<(String, String)> = None;
        // The finished tool call this event (if any) carries, captured beside
        // the strike counter so the record block below can fold it into the
        // worker's current failing streak.
        let mut tool_outcome: Option<(String, bool, String)> = None;
        match &event.event {
            SerializableAgentEvent::ToolCallStart { index, id, name } => {
                // A provider may supersede an unexecuted call at the same
                // index; drop the old entry so the buffers stay bounded.
                if let Some(superseded) = call_ids.insert(*index, id.clone()) {
                    pending_calls.remove(&superseded);
                }
                pending_calls.insert(id.clone(), (name.clone(), String::new()));
            }
            SerializableAgentEvent::ToolCallArgDelta { index, fragment } => {
                if let Some(id) = call_ids.get(index) {
                    if let Some((_, args)) = pending_calls.get_mut(id) {
                        args.push_str(fragment);
                    }
                }
            }
            SerializableAgentEvent::ToolResult { tool_call_id, result } => {
                if let Some((name, args)) = pending_calls.remove(tool_call_id) {
                    let parsed = serde_json::from_str::<Value>(&args)
                        .unwrap_or(Value::String(args));
                    let signature = route_question::call_signature(&name, &parsed);
                    let repeated =
                        previous_signature.as_deref() == Some(signature.as_str());
                    previous_signature = Some(signature.clone());
                    let failed = !result.success;
                    if strikes.lock().await.observe(signature, failed) {
                        strike_fired = Some(repeated);
                    }
                    tool_outcome = Some((name.clone(), result.success, result.output.clone()));
                }
            }
            _ => {}
        }
        let usage_model = if matches!(&event.event, SerializableAgentEvent::Usage { .. })
            && worker_lacks_model_id(&records, &worker_id, generation).await
        {
            match engine.session(&session_id).await {
                Some(session) => session
                    .snapshot()
                    .await
                    .ok()
                    .and_then(|snapshot| snapshot_model(&snapshot)),
                None => None,
            }
        } else {
            None
        };
        let mut terminal = false;
        let mut refresh = false;
        let mut changed = false;
        let mut telemetry = false;
        // Whether this event ended a reported audit's turn (plan 5779641):
        // its live session is released below, once the save persists the
        // report and the `Completed` status.
        let mut audit_release = false;
        {
            let mut records = records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| {
                worker.id == worker_id
                    && (worker.generation == 0 || worker.generation == generation)
            }) {
                let activity_ms = phase3::now_ms();
                worker.last_activity_ms = activity_ms;
                if worker.created_at_ms > 0 {
                    worker.elapsed_ms = activity_ms.saturating_sub(worker.created_at_ms);
                }
                if let Some((tool, success, output)) = &tool_outcome {
                    let had_errors = !worker.tool_errors.is_empty();
                    note_tool_outcome(worker, tool, *success, output);
                    if !*success || had_errors {
                        changed = true;
                    }
                }
                match &event.event {
                    SerializableAgentEvent::Started
                        if !matches!(
                            worker.status,
                            WorkerStatus::Paused
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
                                | WorkerStatus::Cancelled
                        ) =>
                    {
                        turn_active = true;
                        busy.store(true, Ordering::SeqCst);
                        worker.status = if worker.fixer_finding_id.is_some() {
                            WorkerStatus::Fixing
                        } else {
                            WorkerStatus::Working
                        };
                        changed = true;
                    }
                    SerializableAgentEvent::ModelChanged { model, .. }
                        if event_session(&event) == Some(session_id) =>
                    {
                        // The engine's bus is engine-wide: only OUR
                        // session's model change may re-point this
                        // worker's observed model (and its next usage
                        // row) — a concurrent worker's ModelChanged must
                        // not cross into it.
                        worker.model_id = Some(model.clone());
                        changed = true;
                    }
                    SerializableAgentEvent::Usage {
                        prompt_tokens,
                        completion_tokens,
                        cached_tokens,
                        ..
                    } => {
                        let prompt = *prompt_tokens as u64;
                        let completion = *completion_tokens as u64;
                        let cached = *cached_tokens as u64;
                        worker.requests = worker.requests.saturating_add(1);
                        worker.prompt_tokens = worker.prompt_tokens.saturating_add(prompt);
                        worker.completion_tokens =
                            worker.completion_tokens.saturating_add(completion);
                        worker.cached_tokens = worker.cached_tokens.saturating_add(cached);
                        worker.total_tokens = worker
                            .total_tokens
                            .saturating_add(prompt.saturating_add(completion));
                        worker.used_tokens = worker.used_tokens.max(*prompt_tokens);
                        if worker.model_id.is_none() {
                            worker.model_id = usage_model.clone();
                        }
                        if let Some(cost) = usage_cost_micros(
                            &pricing,
                            worker.model_id.as_deref().or(worker.model.as_deref()),
                            prompt,
                            cached,
                            completion,
                        ) {
                            worker.cost_micros =
                                Some(worker.cost_micros.unwrap_or(0).saturating_add(cost));
                        }
                        // The cost alert (plan 06f1a204): the first notice at
                        // `cost_notice_usd`, then one at each doubling. It
                        // never stops or changes the worker.
                        let threshold = (routing
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .cost_notice_usd
                            .max(0.0)
                            * 1_000_000.0) as u64;
                        let spent = worker.cost_micros.unwrap_or(0);
                        if let Some(level) =
                            cost_notice_due(spent, worker.cost_notified_micros, threshold)
                        {
                            worker.cost_notified_micros = level;
                            let dollars = spent as f64 / 1_000_000.0;
                            let model = worker
                                .model_id
                                .as_deref()
                                .or(worker.model.as_deref())
                                .unwrap_or("-");
                            let tier = worker.tier.as_deref().unwrap_or("-");
                            cost_notice = Some((
                                format!("cost:{worker_id}:{level}"),
                                format!(
                                    "worker_id={worker_id} {} has cost about ${dollars:.2} so far: {} requests on {model}, tier {tier}. It keeps running. If that looks wrong, read team_status, then team_message to steer it or team_stop to cancel it.",
                                    worker.name, worker.requests
                                ),
                            ));
                        }
                        // Fold the same report into the durable per-project
                        // accumulator: raw tokens keyed by the observed model,
                        // independent of this worker record so the totals
                        // survive worker deletion and restarts. The engine's
                        // bus is engine-wide, so fold only OUR session's
                        // report — otherwise every listening pump would fold
                        // the same event.
                        if event_session(&event) == Some(session_id) {
                            let accumulator_model = worker
                                .model_id
                                .clone()
                                .or_else(|| usage_model.clone())
                                .or_else(|| {
                                    worker
                                        .model
                                        .as_deref()
                                        .map(|spec| usage_model_key(spec).to_string())
                                });
                            usage.lock().await.record(
                                accumulator_model.as_deref().unwrap_or("unknown"),
                                prompt,
                                cached,
                                completion,
                            );
                        }
                        telemetry = true;
                    }
                    SerializableAgentEvent::ContextUsage { used, max, .. } => {
                        worker.used_tokens = *used;
                        worker.max_tokens = *max;
                        telemetry = true;
                    }
                    SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } => {
                        worker.pending = Some(PendingInteraction::Approval {
                            request_id: tool_call_id.clone(),
                        });
                        worker.status = WorkerStatus::NeedsYou;
                        changed = true;
                    }
                    SerializableAgentEvent::UserQuestion { question_id, .. } => {
                        worker.pending = Some(PendingInteraction::Question {
                            question_id: question_id.clone(),
                        });
                        worker.status = WorkerStatus::NeedsYou;
                        changed = true;
                    }
                    // A worker's host tool (`message_parent`) was answered
                    // above, before this fold: it is ordinary progress, never
                    // a wait on the user.
                    SerializableAgentEvent::CompactStarted => {
                        worker.compacting = true;
                        busy.store(true, Ordering::SeqCst);
                        worker.status = WorkerStatus::Working;
                        worker.error = None;
                        changed = true;
                    }
                    SerializableAgentEvent::Compacted { .. } => {
                        if worker.compacting {
                            worker.compacting = false;
                            // A compaction INSIDE a live turn (a mid-run
                            // compact-then-switch, or the new model's own
                            // trigger right after one) must not idle the
                            // worker: the turn continues on the new model, so
                            // only the badge ends. Between turns (turn_active
                            // false) the worker really is idle again.
                            if !turn_active {
                                busy.store(false, Ordering::SeqCst);
                                if !matches!(
                                    worker.status,
                                    WorkerStatus::Paused
                                        | WorkerStatus::Interrupted
                                        | WorkerStatus::Stopped
                                ) {
                                    worker.status = if worker.pending.is_some() {
                                        WorkerStatus::NeedsYou
                                    } else {
                                        WorkerStatus::Idle
                                    };
                                }
                            }
                            worker.error = None;
                            changed = true;
                        }
                        refresh = true;
                    }
                    SerializableAgentEvent::Finished {
                        reason: mnemo::provider::FinishReason::LimitReached { which },
                    } => {
                        // The session parked on its runaway guard: the work is
                        // unfinished, so it must not read as completed.
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        worker.pending = None;
                        worker.status = WorkerStatus::NeedsYou;
                        worker.error = Some(format!(
                            "the worker session reached a {} limit before finishing; team_stop then team_resume continues it in a fresh session",
                            limit_label(*which)
                        ));
                        terminal = true;
                    }
                    SerializableAgentEvent::Finished { .. }
                        if steer_pending.load(Ordering::SeqCst)
                            && matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Fixing
                                    | WorkerStatus::Dispatching
                            ) =>
                    {
                        // A steer ended this turn at its next break and drives a
                        // follow-up turn (`StopReason::Steer`): the worker has
                        // not finished. Status, busy and the turn count stay as
                        // they are; `Started` clears the wait, and the deadline
                        // settles it as finished if that turn never starts.
                        follow_up_deadline =
                            Some(tokio::time::Instant::now() + FOLLOW_UP_GRACE);
                        held_finished = Some(event.clone());
                        changed = true;
                    }
                    SerializableAgentEvent::Finished { .. } => {
                        let completed_turn = turn_active;
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        worker.pending = None;
                        if completed_turn
                            && !matches!(
                                worker.status,
                                WorkerStatus::Completed
                                    | WorkerStatus::Paused
                                    | WorkerStatus::Interrupted
                                    | WorkerStatus::Stopped
                                    | WorkerStatus::Failed
                                    | WorkerStatus::Merged
                                    | WorkerStatus::Cancelled
                            )
                        {
                            worker.status = WorkerStatus::Completed;
                            worker.turns = worker.turns.saturating_add(1);
                        }
                        // An audit that reported ends its own job: its
                        // session is released below so it stops holding a
                        // worker slot. A turn that ended without a report
                        // (failure, interrupt) keeps today's behavior —
                        // the lead can still resume it.
                        audit_release = worker.kind == "audit"
                            && worker.report_delivered
                            && matches!(worker.status, WorkerStatus::Completed);
                        terminal = true;
                    }
                    SerializableAgentEvent::Exited => {
                        turn_active = false;
                        worker.compacting = false;
                        busy.store(false, Ordering::SeqCst);
                        // The session is gone: no follow-up turn is coming.
                        steer_pending.store(false, Ordering::SeqCst);
                        follow_up_deadline = None;
                        held_finished = None;
                        worker.pending = None;
                        if !matches!(
                            worker.status,
                            WorkerStatus::Completed
                                | WorkerStatus::Idle
                                | WorkerStatus::NeedsYou
                                | WorkerStatus::Paused
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
                                | WorkerStatus::Cancelled
                        ) {
                            worker.status = WorkerStatus::Interrupted;
                            worker.error = Some(
                                "the worker session exited before completing its turn".to_string(),
                            );
                        }
                        terminal = true;
                    }
                    SerializableAgentEvent::Parked { .. } => {
                        turn_active = false;
                        busy.store(false, Ordering::SeqCst);
                        // A parked session has nothing queued: when it parks
                        // while a steered turn end is held, no follow-up turn is
                        // coming, so settle that turn end as finished now —
                        // clearer than waiting for the deadline. Rare race: a
                        // steer that lands right after a natural finish can
                        // settle as Completed and send a notice, then `Started`
                        // flips the worker back to Working, and
                        // `current_notices` drops that notice at delivery while
                        // it is still queued.
                        if held_finished.is_some()
                            && matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Fixing
                                    | WorkerStatus::Dispatching
                            )
                        {
                            worker.status = WorkerStatus::Completed;
                            worker.turns = worker.turns.saturating_add(1);
                            steer_pending.store(false, Ordering::SeqCst);
                            follow_up_deadline = None;
                            settled_parked_finish = held_finished.take();
                        } else if !matches!(
                            worker.status,
                            WorkerStatus::Completed
                                | WorkerStatus::Interrupted
                                | WorkerStatus::Paused
                                | WorkerStatus::Stopped
                                | WorkerStatus::Failed
                                | WorkerStatus::Merged
                        ) {
                            worker.status = WorkerStatus::NeedsYou;
                        }
                        terminal = true;
                    }
                    // The deferred compact-then-switch COMPLETED: the worker
                    // summarized and now runs on the new model. Informational
                    // — the turn continues, so status stays Working, busy
                    // stays set, and no error is recorded (bug 2026-11-02:
                    // this used to arrive as an `Error`, which failed a
                    // perfectly healthy worker). This path emits
                    // CompactStarted but no Compacted, so the badge ends here.
                    SerializableAgentEvent::ModelSwapSummarized { .. } => {
                        changed |= worker.compacting;
                        worker.compacting = false;
                    }
                    // Mnemo2 routes at dispatch; a Mnemo1-only session notice
                    // cannot alter worker status, busy, or compaction state.
                    SerializableAgentEvent::RoutingNotice { .. } => {}
                    SerializableAgentEvent::Error { error, retrying } => {
                        if !retrying {
                            turn_active = false;
                            // A final error ends the turn for good: no
                            // follow-up turn is coming.
                            steer_pending.store(false, Ordering::SeqCst);
                            follow_up_deadline = None;
                            held_finished = None;
                        }
                        if worker.compacting {
                            worker.compacting = false;
                            busy.store(false, Ordering::SeqCst);
                            if !matches!(
                                worker.status,
                                WorkerStatus::Paused
                                    | WorkerStatus::Interrupted
                                    | WorkerStatus::Stopped
                            ) {
                                worker.status = if worker.pending.is_some() {
                                    WorkerStatus::NeedsYou
                                } else {
                                    WorkerStatus::Idle
                                };
                            }
                            worker.error = (!retrying).then(|| error.clone());
                            refresh = true;
                            changed = true;
                        } else if !retrying {
                            busy.store(false, Ordering::SeqCst);
                            // Failed-and-stopped is terminal (design §4.7):
                            // queue the worker's `outcome` row, appended after
                            // the save below. Only the transition into Failed
                            // queues it — a repeated terminal error does not.
                            let first_failure = worker.status != WorkerStatus::Failed;
                            worker.status = WorkerStatus::Failed;
                            worker.error = Some(error.clone());
                            if first_failure {
                                let mode = routing
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .mode;
                                // A failure is resumable, not terminal: the
                                // outcome row only — nudge_ignored waits for
                                // stop, merge, or cleanup.
                                pending_outcome = outcome_row(worker, "failed")
                                    .filter(|_| logs_label(worker, mode));
                            }
                            terminal = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        let _ = events.send(TeamEvent {
            worker_id: worker_id.clone(),
            generation,
            event,
        });
        if terminal || changed {
            let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
        } else if telemetry {
            save_workers_soon(&records, &usage, &save_lock, &persistence);
        }
        if let Some(held) = settled_parked_finish.take() {
            // The held turn end, now that the record reads Completed: this
            // fires the normal finished notice.
            let _ = events.send(TeamEvent {
                worker_id: worker_id.clone(),
                generation,
                event: held,
            });
        }
        if audit_release {
            // The record with the report and the `Completed` status is
            // persisted above; end the live session exactly as
            // `stop_inner` does — but keep the status `Completed`, since
            // the audit finished its job on its own. Then stop pumping:
            // the session is gone and this pump only outlives its turn.
            // The ledger handles move over: the loop breaks right after.
            let _ = release_finished_audit(
                &worker_id,
                generation,
                &live,
                PumpLedger {
                    records: records.clone(),
                    usage: usage.clone(),
                    save_lock: save_lock.clone(),
                    persistence,
                },
                &fail_next_close,
            )
            .await;
            break;
        }
        if let Some(row) = pending_outcome.take() {
            mnemo::model_routing::append_row(&routing_log_path, &row);
        }
        if let Some((key, text)) = cost_notice.take() {
            let _ = signals.send(BoardSignal::Notice { key, text });
        }
        if let Some(repeated) = strike_fired.take() {
            let settings = routing
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if settings.nudges {
                try_strike_nudge(
                    &worker_id,
                    StrikeNudges {
                        records: &records,
                        models: &models,
                        tiers: &tiers,
                        signals: &signals,
                        routing_log_path: &routing_log_path,
                        settings: &settings,
                    },
                    repeated,
                )
                .await;
            }
        }
        let settings = routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if settings.nudges && settings.mode == RoutingMode::Auto {
            // A strike fires during a busy turn. Trying on every event makes
            // the first idle event apply the pending nudge without a restart.
            let _ = auto_escalate_idle(
                &worker_id,
                AutoEscalation {
                    records: &records,
                    live: &live,
                    models: &models,
                    tiers: &tiers,
                    signals: &signals,
                    routing_log_path: &routing_log_path,
                    usage: &usage,
                    save_lock: &save_lock,
                    persistence: &persistence,
                },
            )
            .await;
        }
        if terminal || refresh {
            let engine = engine.clone();
            let records = records.clone();
            let usage = usage.clone();
            let save_lock = save_lock.clone();
            let persistence = persistence.clone();
            let worker_id = worker_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let handle = engine.session(&session_id).await;
                let Some(handle) = handle else {
                    return;
                };
                let Ok(snapshot) = handle.snapshot().await else {
                    return;
                };
                let worktree = {
                    let records = records.lock().await;
                    records
                        .iter()
                        .find(|worker| worker.id == worker_id && worker.generation == generation)
                        .map(|worker| worker.worktree.clone())
                };
                let Some(worktree) = worktree else {
                    return;
                };
                // The tip probe runs outside the ledger lock, so a review recorded
                // while it ran (the lead reviewing the commit it just made) is newer
                // than this observation and cannot be stale against it.
                let tip_read_ms = phase3::now_ms();
                let current_tip = phase3::tip(&worktree).await.ok();
                {
                    let mut records = records.lock().await;
                    if let Some(worker) = records
                        .iter_mut()
                        .find(|worker| worker.id == worker_id && worker.generation == generation)
                    {
                        if worker.model_id.is_none() {
                            worker.model_id = snapshot_model(&snapshot);
                        }
                        worker.transcript = Arc::new(snapshot.transcript);
                        worker.used_tokens = snapshot.used_tokens;
                        worker.max_tokens = snapshot.max_tokens;
                        worker.pending = snapshot.pending;
                        clear_review_for_a_newer_tip(worker, current_tip.as_deref(), tip_read_ms);
                    }
                }
                let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
            });
        }
    }
}

/// Close a worker session for cleanup, honoring the transition tests'
/// injected close failure. Shared by [`TeamRuntime::stop_inner`] and the
/// audit release below so both paths behave identically.
pub(super) async fn close_worker_session(
    session: &AgentSession,
    fail_next_close: &Arc<AtomicBool>,
) -> Result<(), String> {
    if fail_next_close.swap(false, Ordering::SeqCst) {
        return Err("injected close failure for transition tests".to_string());
    }
    session
        .close_and_wait()
        .await
        .map_err(|error| error.to_string())
}

/// The shared ledger handles a worker pump holds: the record store plus
/// everything [`save_workers`] needs. [`release_finished_audit`] takes
/// them as one group because the pump hands them over on its way out.
struct PumpLedger {
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    persistence: TeamPersistence,
}

/// End a finished audit's live session from inside its own pump (plan
/// 5779641): the report was recorded and `Completed` is persisted, so the
/// slot is freed the way [`TeamRuntime::stop_inner`] frees a stopped
/// one — snapshot folded into the record, session closed, record and
/// worktree kept — but the status stays `Completed`, because the audit
/// ended its job on its own rather than being stopped.
///
/// On a failed close the [`LiveWorker`] goes back into `live` with
/// `cleanup_retry_only` set, exactly like a failed stop: a later stop
/// retries the close. The pump calling this breaks its loop right after,
/// so no self-abort is needed.
async fn release_finished_audit(
    worker_id: &str,
    generation: u64,
    live: &Arc<Mutex<HashMap<String, LiveWorker>>>,
    ledger: PumpLedger,
    fail_next_close: &Arc<AtomicBool>,
) -> Result<(), String> {
    let Some(mut live_worker) = live.lock().await.remove(worker_id) else {
        // Not live: a repeated Finished event, or the lead already
        // stopped or deleted the audit. Nothing to release.
        return Ok(());
    };
    let snapshot = live_worker.session.snapshot().await.ok();
    {
        let mut records = ledger.records.lock().await;
        if let Some(worker) = records.iter_mut().find(|worker| {
            worker.id == worker_id
                && (worker.generation == 0 || worker.generation == generation)
        }) {
            if let Some(snapshot) = &snapshot {
                if !snapshot.transcript.messages.is_empty() {
                    worker.transcript = Arc::new(snapshot.transcript.clone());
                    worker.used_tokens = snapshot.used_tokens;
                    worker.max_tokens = snapshot.max_tokens;
                    if worker.model_id.is_none() {
                        worker.model_id = snapshot_model(snapshot);
                    }
                } else if worker.transcript.messages.is_empty() && worker.brief_sent {
                    worker.transcript = Arc::new(Transcript {
                        messages: vec![mnemo::provider::Message::user_text(
                            kickoff_message(worker).to_string(),
                        )],
                    });
                }
            }
        }
    }
    if let Err(error) = close_worker_session(&live_worker.session, fail_next_close).await {
        let message = format!("could not close worker session: {error}");
        live_worker.cleanup_retry_only = true;
        live.lock().await.insert(worker_id.to_string(), live_worker);
        {
            let mut records = ledger.records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| {
                worker.id == worker_id
                    && (worker.generation == 0 || worker.generation == generation)
            }) {
                worker.error = Some(message.clone());
            }
        }
        let _ = save_workers(&ledger.records, &ledger.usage, &ledger.save_lock, &ledger.persistence).await;
        return Err(message);
    }
    let _ = save_workers(&ledger.records, &ledger.usage, &ledger.save_lock, &ledger.persistence).await;
    Ok(())
}

/// Write the whole ledger — workers, slots, and the usage accumulator.
///
/// `save_lock` serializes every `team.json` write (pump saves, the lead's
/// usage recording, cleanup) so a usage report folded into the accumulator
/// can never be lost to a stale interleaving of concurrent saves: each save
/// clones the latest committed state while holding the lock. The encode and
/// the file writes run on the blocking pool, never on a runtime worker.
///
/// A runtime that has shut down skips the write: its final persist is already
/// the last ledger write, so a detached task (the post-turn refresh) cannot
/// resurrect state pruned afterwards.
pub(super) async fn save_workers(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    usage: &Arc<Mutex<ProjectUsage>>,
    save_lock: &Arc<Mutex<()>>,
    persistence: &TeamPersistence,
) -> Result<(), String> {
    if persistence.closed.load(Ordering::SeqCst) {
        return Ok(());
    }
    save_workers_now(records, usage, save_lock, persistence).await
}

/// [`save_workers`] without the shutdown check: the closing runtime's final
/// persist.
pub(super) async fn save_workers_now(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    usage: &Arc<Mutex<ProjectUsage>>,
    save_lock: &Arc<Mutex<()>>,
    persistence: &TeamPersistence,
) -> Result<(), String> {
    let _serialized = save_lock.lock().await;
    let workers = records.lock().await.clone();
    let usage = usage.lock().await.clone();
    let persistence = persistence.clone();
    let team = PersistedTeam {
        workers,
        slots: Some(persistence.slots.load(Ordering::SeqCst)),
        usage,
    };
    tokio::task::spawn_blocking(move || persistence.save(&team))
        .await
        .map_err(|err| format!("the team ledger save task failed: {err}"))?
}

/// Persist the ledger within [`LEDGER_FLUSH_DELAY`] instead of now: for
/// telemetry-only changes (usage counters, context size) that arrive with
/// every LLM request. At most one flush is pending; changes that land while
/// it waits ride along. The in-memory state stays authoritative, so a crash
/// loses at most the last delay's counters.
pub(super) fn save_workers_soon(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    usage: &Arc<Mutex<ProjectUsage>>,
    save_lock: &Arc<Mutex<()>>,
    persistence: &TeamPersistence,
) {
    if persistence.closed.load(Ordering::SeqCst) {
        return;
    }
    if persistence.flush_scheduled.swap(true, Ordering::SeqCst) {
        return;
    }
    let (records, usage, save_lock, persistence) = (
        records.clone(),
        usage.clone(),
        save_lock.clone(),
        persistence.clone(),
    );
    tokio::spawn(async move {
        tokio::time::sleep(LEDGER_FLUSH_DELAY).await;
        // Clear first: a change landing during the save schedules the next flush.
        persistence.flush_scheduled.store(false, Ordering::SeqCst);
        // A shutdown voids the pending flush: its persist is the last write.
        if persistence.closed.load(Ordering::SeqCst) {
            return;
        }
        let _ = save_workers(&records, &usage, &save_lock, &persistence).await;
    });
}

/// A human label for the session limit a worker hit.
fn limit_label(which: mnemo::provider::LimitKind) -> &'static str {
    use mnemo::provider::LimitKind;
    match which {
        LimitKind::MaxTurns | LimitKind::MaxRequests => "request",
        LimitKind::MaxPromptTokens => "prompt-token",
        LimitKind::MaxCompletionTokens => "completion-token",
        LimitKind::MaxCostMicros => "cost",
        LimitKind::WallClock => "wall-clock",
        LimitKind::Idle => "idle",
    }
}
