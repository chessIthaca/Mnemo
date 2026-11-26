// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The worker event pump: one tokio task per live worker that folds
//! session events into the record, serves host tools, notices cost and
//! releases a finished audit.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use mnemo::{SerializableAgentEvent, route_question};
use mnemo::app::plan_board::PlanBoard;
use mnemo::headless::{
    EventReceiver, PendingInteraction, SessionCommand, SessionEvent, SessionId, Transcript,
};
use mnemo::tool::ToolResult;
use mnemo::tool::agent::shell::validated_check_command;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast};

use crate::mnemo2::phase3;
use crate::mnemo2::dispatch_routing::{DispatchRoutingRow, RoutingMode, RoutingSettings};
use crate::mnemo2::pricing::usage_model_key;
use crate::mnemo2::runtime::LeadModel;
use crate::mnemo2::usage::{ProjectUsage, event_session};

use super::{BoardSignal, LiveWorker, TeamEvent, TeamMessage, now_key};
use super::escalation::{AutoEscalation, auto_escalate_idle};
use super::nudge::{StrikeNudges, note_tool_outcome, try_strike_nudge};
use super::persistence::{LEDGER_FLUSH_DELAY, PersistedTeam, TeamPersistence};
use super::record::{
    InboxEntry, InboxKind, MessageSender, WorkerRecord, WorkerStatus, append_inbox, kickoff_message,
};
use super::review::{CheckRecorder, clear_review_for_a_newer_tip};
use super::routing::{logs_label, outcome_row, ModelInventory, UsageLimits};
use super::worker_session::SharedWorkerSession;

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
///
/// `plan_read` with no `capability` returns the worker's current plan view.
/// With `capability`, it returns that OpenSpec capability's spec; a capability
/// name that is not one of [`PlanBoard::capability_names`] is an error naming
/// the real capabilities, because a success answer ("No current spec for …")
/// reads as "nothing to see" and sends the worker back to re-reading invented
/// names instead of its plan.
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
                Ok(None) => match board.capability_names() {
                    // A real capability folder whose spec.md does not exist
                    // yet keeps a success answer, now pointing back at the plan.
                    Ok(names) if names.iter().any(|name| name == capability) => {
                        ToolResult::success(format!(
                            "No current spec for '{capability}' yet. To re-read your plan, call plan_read with no arguments."
                        ))
                    }
                    Ok(names) => ToolResult::error(unknown_capability_error(capability, &names)),
                    Err(error) => ToolResult::error(error),
                },
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

/// The error a `plan_read` capability that names no OpenSpec capability gets:
/// it lists the real capabilities (or "none") and sends the worker back to its
/// plan, so an invented name cannot read as "there is nothing to read".
fn unknown_capability_error(capability: &str, known: &[String]) -> String {
    let list = if known.is_empty() {
        "none".to_string()
    } else {
        known.join(", ")
    };
    format!(
        "There is no OpenSpec capability named '{capability}'. Known capabilities: {list}. To re-read your plan, call plan_read with no arguments."
    )
}

/// The result a repeated `message_parent` report gets (plan 9a6cffc1):
/// the first report already reached the lead, so this copy is neither
/// recorded nor forwarded. It names the required next action — end the
/// turn now, with no further tool calls.
const REPEATED_REPORT_REFUSED: &str =
    "not sent to the lead: this report already reached it. End your turn now, with no further tool calls, so the lead can review your work.";

/// Send a host-tool result back to the worker session.
async fn send_worker_host_result(
    session: &SharedWorkerSession,
    call_id: String,
    result: ToolResult,
) {
    let _ = session
        .send(SessionCommand::HostToolResult {
            call_id,
            result: Some(result),
            error: None,
        })
        .await;
}

/// Record an audit's final assistant text as its report (bug 2026-11-16):
/// the `TeamMessage` a `message_parent` call records, the forwarded call the
/// lead's "<name> reports: …" notice comes from, and `report_delivered` on
/// the record — so the caller releases the audit in the same pass. Returns
/// whether the record was marked, i.e. whether the audit may be released.
async fn deliver_plain_text_report(
    messages: &Arc<Mutex<Vec<TeamMessage>>>,
    worker_id: &str,
    generation: u64,
    session_id: SessionId,
    text: &str,
    events: &broadcast::Sender<TeamEvent>,
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
) -> bool {
    let args = serde_json::json!({ "text": text });
    let _ = worker_host_tool_result(messages, worker_id, "message_parent", &args, None, None).await;
    // The same forwarded call a worker's own message_parent makes: the
    // runtime turns it into the lead's report notice.
    let _ = events.send(TeamEvent {
        worker_id: worker_id.to_string(),
        generation,
        event: SessionEvent {
            agent: session_id.agent_id(),
            event: SerializableAgentEvent::HostToolCall {
                call_id: format!("{worker_id}:{generation}:plain-text-report"),
                name: "message_parent".to_string(),
                args,
            },
            rates: None,
        },
    });
    let mut records = records.lock().await;
    let Some(worker) = records.iter_mut().find(|worker| {
        worker.id == worker_id && (worker.generation == 0 || worker.generation == generation)
    }) else {
        return false;
    };
    worker.report_delivered = true;
    true
}

/// Ask an audit that ended its turn with no text for its report (bug
/// 2026-11-16): one automatic host message, delivered like any other message
/// — an inbox receipt plus the prompt that starts a new turn — so the audit
/// stays live and available to answer. Returns whether the ask was sent.
async fn ask_audit_for_its_report(
    session: &SharedWorkerSession,
    worker_id: &str,
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    busy: &Arc<AtomicBool>,
) -> bool {
    let ask = "Your audit report did not reach the lead. Send your complete findings with message_parent now, then end your turn.";
    // A closed session takes no ask: nothing is written to the inbox for it.
    if !session.is_open().await {
        return false;
    }
    {
        let mut records = records.lock().await;
        let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) else {
            return false;
        };
        append_inbox(
            worker,
            InboxEntry {
                sender: MessageSender::Mnemo,
                kind: InboxKind::Prompt,
                text: ask.to_string(),
                at_ms: now_key(),
            },
        );
        worker.status = WorkerStatus::Working;
        worker.pending = None;
        worker.error = None;
    }
    busy.store(true, Ordering::SeqCst);
    let text = format!("{}\n{ask}", MessageSender::Mnemo.header());
    if session
        .send(SessionCommand::Prompt {
            text,
            images: Vec::new(),
        })
        .await
        .is_err()
    {
        // The ask never reached the session: put the finished status back, so
        // the silent audit reads finished instead of running a turn that does
        // not exist.
        busy.store(false, Ordering::SeqCst);
        if let Some(worker) = records
            .lock()
            .await
            .iter_mut()
            .find(|worker| worker.id == worker_id)
        {
            worker.status = WorkerStatus::Completed;
        }
        return false;
    }
    true
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

/// Where a counted check ran: the hub's own checkout, or a worker node's.
pub(super) enum CheckoutRoot<'a> {
    /// A checkout on this machine.
    Local(&'a Path),
    /// A checkout on a worker node, named by its path on that host
    /// ([`super::record::RemoteCheckout::path`]).
    Remote(&'a str),
}

impl CheckoutRoot<'_> {
    /// Whether a tool's reported `cwd` is this checkout's root.
    ///
    /// A local path is canonicalized on both sides, exactly like the counting
    /// rule always did. A node path cannot be canonicalized from the hub, so
    /// both strings are normalised instead: a `\\?\` prefix dropped, `\` as
    /// `/`, one trailing `/` dropped, compared ASCII case-insensitively (a
    /// Windows node may report either case). An empty or `.` cwd is the root;
    /// any other relative path names a subfolder and never matches.
    fn cwd_is_root(&self, cwd: &str) -> bool {
        match self {
            CheckoutRoot::Local(checkout) => {
                let Ok(root) = checkout.canonicalize() else {
                    return false;
                };
                let cwd = cwd.trim();
                if cwd.is_empty() || cwd == "." {
                    return true;
                }
                let cwd = Path::new(cwd);
                let cwd = if cwd.is_absolute() {
                    cwd.to_path_buf()
                } else {
                    root.join(cwd)
                };
                cwd.canonicalize().ok().as_deref() == Some(root.as_path())
            }
            CheckoutRoot::Remote(checkout) => {
                let cwd = cwd.trim();
                if cwd.is_empty() || cwd == "." {
                    return true;
                }
                let normalize = |path: &str| {
                    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
                    let mut path = path.replace('\\', "/");
                    if path.len() > 1 && path.ends_with('/') {
                        path.pop();
                    }
                    path
                };
                normalize(checkout).eq_ignore_ascii_case(&normalize(cwd))
            }
        }
    }
}

/// Return an allow-listed passing shell command and its unchanged tree.
pub(super) fn counted_check(
    tool: &str,
    args: &Value,
    result: &ToolResult,
    checkout: &Path,
    check_commands: &[String],
) -> Option<(String, String)> {
    // A checkout that cannot be resolved counts nothing, whatever the cwd.
    checkout.canonicalize().ok()?;
    counted_check_at(
        tool,
        args,
        result,
        &CheckoutRoot::Local(checkout),
        check_commands,
    )
}

/// [`counted_check`] against a hub or node checkout.
pub(super) fn counted_check_at(
    tool: &str,
    args: &Value,
    result: &ToolResult,
    checkout: &CheckoutRoot<'_>,
    check_commands: &[String],
) -> Option<(String, String)> {
    if tool != "shell" || !result.success {
        return None;
    }
    let data = result.data.as_ref()?;
    if data.get("exit_code")?.as_i64()? != 0 {
        return None;
    }
    let command = validated_check_command(args.get("command")?.as_str()?)?;
    if !check_commands.iter().any(|allowed| allowed == command) {
        return None;
    }
    if let Some(cwd) = args.get("cwd") {
        if !checkout.cwd_is_root(cwd.as_str()?) {
            return None;
        }
    }
    let tree_before = data.get("tree_before")?.as_str()?;
    let tree_after = data.get("tree_after")?.as_str()?;
    if tree_before.is_empty() || tree_before != tree_after {
        return None;
    }
    Some((command.to_string(), tree_before.to_string()))
}

/// Every allow-listed passing check a tool result carries, with the unchanged
/// content tree it measured (plan 5b85ab61 step 3).
///
/// `shell` yields at most one run ([`counted_check`]); `run_checks` yields one
/// per entry of `data.checks` that exited 0, ran the exact allow-listed command
/// from the checkout root, and measured a tree equal before and after — the
/// same rules a counted `shell` run obeys, so both tools record identically.
pub(super) fn counted_checks(
    tool: &str,
    args: &Value,
    result: &ToolResult,
    checkout: &Path,
    check_commands: &[String],
) -> Vec<(String, String)> {
    if tool == "shell" {
        return counted_check(tool, args, result, checkout, check_commands)
            .into_iter()
            .collect();
    }
    if checkout.canonicalize().is_err() {
        return Vec::new();
    }
    counted_checks_at(
        tool,
        args,
        result,
        &CheckoutRoot::Local(checkout),
        check_commands,
    )
}

/// [`counted_checks`] against a hub or node checkout.
pub(super) fn counted_checks_at(
    tool: &str,
    args: &Value,
    result: &ToolResult,
    checkout: &CheckoutRoot<'_>,
    check_commands: &[String],
) -> Vec<(String, String)> {
    if tool == "shell" {
        return counted_check_at(tool, args, result, checkout, check_commands)
            .into_iter()
            .collect();
    }
    if tool != "run_checks" || !result.success {
        return Vec::new();
    }
    let Some(entries) = result
        .data
        .as_ref()
        .and_then(|data| data.get("checks"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            if entry.get("exit_code")?.as_i64()? != 0 {
                return None;
            }
            let command = validated_check_command(entry.get("command")?.as_str()?)?;
            if !check_commands.iter().any(|allowed| allowed == command) {
                return None;
            }
            // `run_checks` always runs from the checkout root; an entry naming
            // anything else is not this worker's checkout.
            if !checkout.cwd_is_root(entry.get("cwd")?.as_str()?) {
                return None;
            }
            let tree_before = entry.get("tree_before")?.as_str()?;
            let tree_after = entry.get("tree_after")?.as_str()?;
            if tree_before.is_empty() || tree_before != tree_after {
                return None;
            }
            Some((command.to_string(), tree_before.to_string()))
        })
        .collect()
}

/// Bring a remote worker's hub shadow up to its node before the turn-end
/// bookkeeping reads it.
///
/// A local worker is left alone. A link error is logged rather than failing
/// the pump: the shadow keeps its last synced tip, and the next entry point
/// (a check, a review, a merge) syncs again and reports the host there.
async fn sync_shadow_quietly(
    proof: &super::proving::ProofCtx,
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    worker_id: &str,
    generation: u64,
) {
    let record = {
        let records = records.lock().await;
        records
            .iter()
            .find(|worker| {
                worker.id == worker_id && (worker.generation == 0 || worker.generation == generation)
            })
            .cloned()
    };
    let Some(record) = record.filter(|record| record.remote.is_some()) else {
        return;
    };
    if let Err(error) = proof.remote.sync_shadow(&record, true).await {
        eprintln!(
            "team: could not sync the shadow of {worker_id} before its turn-end bookkeeping: {error}"
        );
    }
}

async fn record_turn_checks(
    worker_id: &str,
    generation: u64,
    observations: &mut HashMap<String, String>,
    ledger: &PumpLedger,
) {
    CheckRecorder::new(
        &ledger.records,
        &ledger.usage,
        &ledger.save_lock,
        &ledger.persistence,
    )
    .record_worker_checks(&ledger.merge_lock, worker_id, generation, observations)
    .await;
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

/// Fire the turn-end proofs off the pump: the trigger never delays the event
/// loop behind a proof decision, and the pump keeps folding this worker's
/// events while the proof runs. Every path that settles a turn as Completed
/// calls this — the natural finished turn, a parked steered turn end, and the
/// follow-up deadline below (F4/F5).
/// The trigger is tracked in the team's task register, so a shutdown waits
/// for it instead of leaving it running in the worker's checkout.
async fn fire_turn_end_proofs(
    proof: &super::proving::ProofCtx,
    worker_id: &str,
    tasks: &mnemo::headless::BackgroundTasks,
) {
    let proof = proof.clone();
    let proved_worker = worker_id.to_string();
    tasks.register(tokio::spawn(async move {
        super::proving::spawn_turn_proofs(&proof, proved_worker).await;
    }));
}

// Each parameter is one shared worker handle — the pump's wiring stays flat.
#[allow(clippy::too_many_arguments)]
pub(super) async fn pump_worker(
    worker_id: String,
    generation: u64,
    session: SharedWorkerSession,
    mut receiver: EventReceiver<SessionEvent>,
    events: broadcast::Sender<TeamEvent>,
    busy: Arc<AtomicBool>,
    messages: Arc<Mutex<Vec<TeamMessage>>>,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    merge_lock: Arc<Mutex<()>>,
    check_commands: Arc<Vec<String>>,
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
    proof: super::proving::ProofCtx,
    usage_limits: UsageLimits,
) {
    let session_id = session.id();
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
    let mut check_observations: HashMap<String, String> = HashMap::new();
    let ledger = PumpLedger {
        records: records.clone(),
        usage: usage.clone(),
        save_lock: save_lock.clone(),
        persistence: persistence.clone(),
        merge_lock,
    };
    // The report streak (plan 9a6cffc1): consecutive `message_parent`
    // calls with no other tool call in between, within one worker turn.
    // The 2nd report in a row is refused — not recorded, no lead notice
    // — and the 3rd is refused and the worker's turn is ended for it.
    // Any other tool call resets it, except `memory_*` calls; a new turn
    // (`Started`) starts it over.
    let mut report_streak: usize = 0;
    // The audit turn already answered with the automatic "send your report"
    // ask (bug 2026-11-16): one ask per turn, never a second for the same
    // turn's finish.
    let mut silent_audit_asked_turn: Option<u32> = None;
    // The assistant text streamed in the worker's current turn (bug
    // 2026-11-16): an audit's silent plain-text end is decided on this, never
    // on the transcript, whose repaired empty turn reads "(no output)".
    let mut turn_text = String::new();
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
                    if held.is_some() {
                        sync_shadow_quietly(&proof, &records, &worker_id, generation).await;
                        record_turn_checks(
                            &worker_id,
                            generation,
                            &mut check_observations,
                            &ledger,
                        )
                        .await;
                    }
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
                    if completed {
                        // F5 (steerTimeoutTrigger): this deadline settled the
                        // steered turn end as this worker's finished turn, so
                        // it owes its proofs like any other turn end — this
                        // branch continues and never reaches the trigger
                        // below.
                        fire_turn_end_proofs(&proof, &worker_id, &persistence.tasks).await;
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
            report_streak = 0;
            check_observations.clear();
            turn_text.clear();
        }
        // A suppressed repeated report (plan 9a6cffc1): answered with
        // [`REPEATED_REPORT_REFUSED`] below, and kept so the TeamEvent
        // forward at the bottom of this iteration skips it — one source
        // of truth for "suppressed", before the runtime could ever turn
        // the call into a lead notice.
        let mut suppressed_report = false;
        if let SerializableAgentEvent::HostToolCall {
            call_id,
            name,
            args,
        } = &event.event
        {
            if name == "plan_read" {
                plan_stamp = worker_plan_stamp(&records, &worker_id).await;
            }
            if name == "message_parent" {
                report_streak += 1;
                if report_streak == 1 {
                    // The first report in a turn is forwarded unchanged
                    // (plan 9a6cffc1): recorded as a TeamMessage, forwarded
                    // as a TeamEvent, normal result text.
                    let result = worker_host_tool_result(
                        &messages,
                        &worker_id,
                        name,
                        args,
                        board.as_ref(),
                        plan_stamp.as_ref(),
                    )
                    .await;
                    send_worker_host_result(&session, call_id.clone(), result).await;
                    // An audit's report is the message_parent call that
                    // delivered it (plan 5779641): mark the record the
                    // moment the message is recorded, so the turn's
                    // Finished event below can end the session and free
                    // the slot it holds.
                    let mut records = records.lock().await;
                    if let Some(worker) = records.iter_mut().find(|worker| {
                        worker.id == worker_id
                            && (worker.generation == 0 || worker.generation == generation)
                            && worker.kind == "audit"
                    }) {
                        worker.report_delivered = true;
                    }
                } else {
                    // A repeated report (plan 9a6cffc1): the first one
                    // already reached the lead, so this copy is neither
                    // recorded nor forwarded — no TeamMessage, no lead
                    // notice. The worker is told to end its turn now.
                    suppressed_report = true;
                    send_worker_host_result(
                        &session,
                        call_id.clone(),
                        ToolResult::success(REPEATED_REPORT_REFUSED),
                    )
                    .await;
                    if report_streak >= 3 {
                        // The third report in a row: the worker keeps
                        // re-reporting after being told to stop, so the
                        // host ends the turn for it. The interrupt stops
                        // the turn at its next break (the provider
                        // request or an in-flight wait), which lands in
                        // the normal `Finished` path below — the lead's
                        // "finished its turn" notice, and the worker
                        // left idle for review.
                        let _ = session.send(SessionCommand::Interrupt).await;
                    }
                }
            } else {
                // Another host tool (plan_read) is a tool call like any
                // other (plan 9a6cffc1): it resets the report streak.
                report_streak = 0;
                let result = worker_host_tool_result(
                    &messages,
                    &worker_id,
                    name,
                    args,
                    board.as_ref(),
                    plan_stamp.as_ref(),
                )
                .await;
                send_worker_host_result(&session, call_id.clone(), result).await;
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
        // The audit turn that ended with no message_parent report (bug
        // 2026-11-16): its final assistant text is delivered as the report
        // once the record block below releases its lock, before the turn's
        // Finished event is forwarded.
        let mut plain_text_audit_turn: Option<u32> = None;
        // The finished tool call this event (if any) carries, captured beside
        // the strike counter so the record block below can fold it into the
        // worker's current failing streak.
        let mut tool_outcome: Option<(String, bool, String)> = None;
        match &event.event {
            SerializableAgentEvent::ToolCallStart { index, id, name } => {
                // The text collected so far belongs to the assistant message
                // that calls this tool, not to the turn's last message (bug
                // 2026-11-16): the audit report fallback keeps the last one.
                turn_text.clear();
                // A provider may supersede an unexecuted call at the same
                // index; drop the old entry so the buffers stay bounded.
                if let Some(superseded) = call_ids.insert(*index, id.clone()) {
                    pending_calls.remove(&superseded);
                }
                pending_calls.insert(id.clone(), (name.clone(), String::new()));
                // The report streak (plan 9a6cffc1): a `message_parent`
                // call starts with this event too, so it must not reset its
                // own streak, and a `memory_*` call is not progress between
                // reports — every other tool call resets it.
                if name != "message_parent" && !name.starts_with("memory_") {
                    report_streak = 0;
                }
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
                    // A steering redirect (the stale-read gate's EDIT
                    // INTERCEPTED, an unknown read receipt) is neither a
                    // failure nor a success (plan 5b85ab61 step 4): it leaves
                    // the strike count exactly as it was, so following the
                    // gate's "re-read, then retry" advice cannot push the
                    // worker toward the stuck nudge. An exact repeat of the
                    // previous call still strikes.
                    let steering = route_question::is_steering_redirect(result);
                    let fired = if steering {
                        strikes.lock().await.observe_neutral(signature)
                    } else {
                        strikes.lock().await.observe(signature, failed)
                    };
                    if fired {
                        strike_fired = Some(repeated);
                    }
                    // Only a real outcome feeds the nudge's error list: a
                    // redirect adds nothing and clears nothing.
                    if !steering {
                        tool_outcome =
                            Some((name.clone(), result.success, result.output.clone()));
                    }
                    let checkout = {
                        let records = records.lock().await;
                        records
                            .iter()
                            .find(|worker| {
                                worker.id == worker_id
                                    && (worker.generation == 0
                                        || worker.generation == generation)
                            })
                            .map(|worker| (worker.worktree.clone(), worker.remote.clone()))
                    };
                    if let Some((worktree, remote)) = checkout {
                        // A remote worker's tool results name the node's
                        // checkout, not the hub shadow; a local worker's name
                        // its worktree. The counting rule reads the same
                        // either way.
                        let counted = match &remote {
                            Some(remote) => counted_checks_at(
                                &name,
                                &parsed,
                                result,
                                &CheckoutRoot::Remote(&remote.path),
                                check_commands.as_slice(),
                            ),
                            None => counted_checks(
                                &name,
                                &parsed,
                                result,
                                &worktree,
                                check_commands.as_slice(),
                            ),
                        };
                        for (check, tree) in counted {
                            check_observations.insert(check, tree);
                        }
                    }
                }
            }
            // The audit report fallback (bug 2026-11-16) reads the turn's
            // streamed assistant text, not the transcript: a silent turn's
            // repaired history reads "(no output)" instead of empty.
            SerializableAgentEvent::TextDelta { text } => {
                turn_text.push_str(text);
            }
            _ => {}
        }
        let usage_model = if matches!(&event.event, SerializableAgentEvent::Usage { .. })
            && worker_lacks_model_id(&records, &worker_id, generation).await
        {
            session
                .snapshot()
                .await
                .ok()
                .and_then(|snapshot| snapshot_model(&snapshot))
        } else {
            None
        };
        let mut terminal = false;
        // Whether the event left the worker's record reading Completed (F4
        // parkedTrigger): only a turn that ended Completed has finished work
        // to prove. Read under the records lock below.
        let mut completed_turn_end = false;
        let mut refresh = false;
        let mut changed = false;
        let mut telemetry = false;
        // Whether this event ended a reported audit's turn (plan 5779641):
        // its live session is released below, once the save persists the
        // report and the `Completed` status.
        let mut audit_release = false;
        // The model this event's failure took out of the tiers for a
        // provider usage limit (plan e5d93df3 decision 8); in routing mode
        // auto the worker is resumed on its tier's fold target below.
        let mut usage_limited: Option<String> = None;
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
                        // An audit that ended its turn without calling
                        // message_parent keeps its findings as the turn's
                        // final assistant text (bug 2026-11-16): remember
                        // the turn, so that text is delivered as its report
                        // — or the audit is asked for the report — once this
                        // record block releases its lock.
                        if worker.kind == "audit"
                            && !worker.report_delivered
                            && matches!(worker.status, WorkerStatus::Completed)
                        {
                            plain_text_audit_turn = Some(worker.turns);
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
                    // Mnemo2 routes at dispatch; the kernel's per-turn routing
                    // notice cannot alter worker status, busy, or compaction
                    // state.
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
                                // Marked before the event is forwarded, so
                                // the lead's failure notice already reads the
                                // model as out.
                                usage_limited = usage_limits.mark_failure(worker, error);
                            }
                            terminal = true;
                        }
                    }
                    _ => {}
                }
                completed_turn_end = matches!(worker.status, WorkerStatus::Completed);
            }
        }
        // An audit that ended its turn without a message_parent report (bug
        // 2026-11-16): its findings are the turn's final assistant text.
        // Deliver that text exactly as a report — the recorded TeamMessage,
        // the call the lead's "<name> reports: …" notice comes from, and
        // `report_delivered` — before the turn's Finished event below is
        // forwarded, so the lead reads the report before the adjudication
        // notice and the released audit frees its slot in this same pass.
        // Text that is empty after trimming has nothing to deliver: the
        // audit is asked for its report, once, and keeps its session.
        if let Some(turn) = plain_text_audit_turn {
            let text = turn_text.trim().to_string();
            if text.is_empty() {
                // One ask per audit session: the ask itself starts a new
                // turn, so a per-turn guard would ask a silent audit forever.
                if silent_audit_asked_turn.is_none()
                    && ask_audit_for_its_report(&session, &worker_id, &records, &busy)
                        .await
                {
                    silent_audit_asked_turn = Some(turn);
                }
            } else {
                audit_release = deliver_plain_text_report(
                    &messages,
                    &worker_id,
                    generation,
                    session_id,
                    &text,
                    &events,
                    &records,
                )
                .await;
            }
        }
        if (terminal && matches!(&event.event, SerializableAgentEvent::Finished { .. }))
            || settled_parked_finish.is_some()
        {
            sync_shadow_quietly(&proof, &records, &worker_id, generation).await;
            record_turn_checks(
                &worker_id,
                generation,
                &mut check_observations,
                &ledger,
            )
            .await;
            // The turn is over: prove what it owes while nothing else may
            // touch the checkout — but only when the turn ended Completed (F4
            // parkedTrigger): a LimitReached park or an interrupted exit has
            // not finished the work, and its resume would run under the
            // proof's checkout.
            if completed_turn_end {
                fire_turn_end_proofs(&proof, &worker_id, &persistence.tasks).await;
            }
        }
        // A suppressed repeated report is not forwarded (plan 9a6cffc1):
        // no TeamEvent means the runtime never turns it into a lead
        // notice — the pump is the single source of truth for the
        // suppression.
        if !suppressed_report {
            let _ = events.send(TeamEvent {
                worker_id: worker_id.clone(),
                generation,
                event,
            });
        }
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
                ledger.clone(),
                &fail_next_close,
            )
            .await;
            break;
        }
        if let Some(row) = pending_outcome.take() {
            mnemo::model_routing::append_row(&routing_log_path, &row);
        }
        if let Some(spec) = usage_limited.take() {
            let mode = routing.read().unwrap_or_else(|e| e.into_inner()).mode;
            if mode == RoutingMode::Auto {
                // Its own task: the resume stops this session, which aborts
                // this pump.
                if let Some(team) = usage_limits.team() {
                    let worker_id = worker_id.clone();
                    persistence.tasks.register(tokio::spawn(async move {
                        team.resume_usage_limited(&worker_id, generation, &spec)
                            .await;
                    }));
                }
            }
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
                        models: ModelInventory::new(&models, &usage_limits),
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
        // A nudge onto a tier that runs as a Claude Code agent while Claude
        // Code leads hands the step to the lead's agent (plan e52e0be1
        // decision 5), in its own task: the hand-off stops this session,
        // which aborts this pump.
        let to_agent = if settings.nudges && settings.mode == RoutingMode::Auto {
            match usage_limits.team() {
                Some(team) if team.nudged_to_agent(&worker_id).await => Some(team),
                _ => None,
            }
        } else {
            None
        };
        if let Some(team) = to_agent {
            let worker_id = worker_id.clone();
            persistence.tasks.register(tokio::spawn(async move {
                let _ = team.escalate_to_agent(&worker_id).await;
            }));
        } else if settings.nudges && settings.mode == RoutingMode::Auto {
            // A strike fires during a busy turn. Trying on every event makes
            // the first idle event apply the pending nudge without a restart.
            let _ = auto_escalate_idle(
                &worker_id,
                AutoEscalation {
                    records: &records,
                    live: &live,
                    models: ModelInventory::new(&models, &usage_limits),
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
            let session = session.clone();
            let records = records.clone();
            let usage = usage.clone();
            let save_lock = save_lock.clone();
            let persistence = persistence.clone();
            let worker_id = worker_id.clone();
            let proofs = proof.remote.clone();
            // Tracked: the refresh runs `git rev-parse HEAD` in the checkout
            // and holds the session (and through it the engine), so the team's
            // shutdown waits for it.
            let tasks = persistence.tasks.clone();
            tasks.register(tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                // A session closed meanwhile (a stop, a release) already
                // folded its own snapshot: a closed session's empty answer
                // must not overwrite the record.
                if !session.is_open().await {
                    return;
                }
                let Ok(snapshot) = session.snapshot().await else {
                    return;
                };
                let record = {
                    let records = records.lock().await;
                    records
                        .iter()
                        .find(|worker| worker.id == worker_id && worker.generation == generation)
                        .cloned()
                };
                let Some(record) = record else {
                    return;
                };
                // A remote worker's latest commits live on its node: refresh the
                // hub shadow before the probe reads it. A link error leaves the
                // last synced tip in place; the next entry point retries.
                if record.remote.is_some() {
                    if let Err(error) = proofs.sync_shadow(&record, true).await {
                        eprintln!(
                            "team: could not sync the shadow of {} before the tip probe: {error}",
                            record.id
                        );
                    }
                }
                // The tip probe runs outside the ledger lock, so a review recorded
                // while it ran (the lead reviewing the commit it just made) is newer
                // than this observation and cannot be stale against it.
                let tip_read_ms = phase3::now_ms();
                let current_tip = phase3::tip(&record.worktree).await.ok();
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
            }));
        }
    }
}

/// Close a worker session for cleanup, honoring the transition tests'
/// injected close failure. Shared by [`TeamRuntime::stop_inner`] and the
/// audit release below so both paths behave identically.
pub(super) async fn close_worker_session(
    session: &SharedWorkerSession,
    fail_next_close: &Arc<AtomicBool>,
) -> Result<(), String> {
    if fail_next_close.swap(false, Ordering::SeqCst) {
        return Err("injected close failure for transition tests".to_string());
    }
    session.close_and_wait().await
}

/// The shared ledger handles a worker pump holds: the record store plus
/// everything [`save_workers`] needs. [`release_finished_audit`] takes
/// them as one group because the pump hands them over on its way out.
#[derive(Clone)]
struct PumpLedger {
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    persistence: TeamPersistence,
    merge_lock: Arc<Mutex<()>>,
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
    save_workers_now(records, usage, save_lock, persistence, None).await
}

/// [`save_workers`] without the shutdown check: the closing runtime's final
/// persist. `snapshot_override` replaces one worker in the saved snapshot,
/// which lets a persistence regression force a transcript write without
/// changing the runtime's in-memory record.
pub(super) async fn save_workers_now(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    usage: &Arc<Mutex<ProjectUsage>>,
    save_lock: &Arc<Mutex<()>>,
    persistence: &TeamPersistence,
    snapshot_override: Option<WorkerRecord>,
) -> Result<(), String> {
    let save_guard = save_lock.lock().await;
    save_workers_now_locked(records, usage, persistence, snapshot_override, &save_guard).await
}

/// Persist with the save serialization guard already held. Holding that
/// guard across a record mutation and this write prevents every other saver
/// from cloning a transient in-memory state.
pub(super) async fn save_workers_now_locked(
    records: &Arc<Mutex<Vec<WorkerRecord>>>,
    usage: &Arc<Mutex<ProjectUsage>>,
    persistence: &TeamPersistence,
    snapshot_override: Option<WorkerRecord>,
    _save_guard: &tokio::sync::MutexGuard<'_, ()>,
) -> Result<(), String> {
    let mut workers = records.lock().await.clone();
    if let Some(snapshot) = snapshot_override {
        if let Some(worker) = workers.iter_mut().find(|worker| worker.id == snapshot.id) {
            *worker = snapshot;
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The `run_checks` result of one exit-0 command in `cwd`.
    fn result(cwd: &str) -> ToolResult {
        ToolResult {
            success: true,
            output: String::new(),
            data: Some(serde_json::json!({"checks": [{
                "command": "cargo test --workspace",
                "exit_code": 0,
                "tree_before": "tree",
                "tree_after": "tree",
                "cwd": cwd,
            }]})),
        }
    }

    /// A node checkout's own path is the checkout however the node spells it;
    /// a subfolder or another folder never is.
    #[test]
    fn counted_checks_at_remote_matches_node_path() {
        let commands = vec!["cargo test --workspace".to_string()];
        let checkout = CheckoutRoot::Remote(r"C:\Users\mnemo\checkouts\0123456789ab-1");
        for cwd in [
            r"C:\Users\mnemo\checkouts\0123456789ab-1",
            r"\\?\C:\Users\mnemo\checkouts\0123456789ab-1",
            "C:/Users/mnemo/checkouts/0123456789ab-1",
            "c:/users/MNEMO/checkouts/0123456789ab-1/",
            "",
            ".",
        ] {
            assert_eq!(
                counted_checks_at(
                    "run_checks",
                    &serde_json::json!({}),
                    &result(cwd),
                    &checkout,
                    &commands
                ),
                vec![("cargo test --workspace".to_string(), "tree".to_string())],
                "{cwd:?}"
            );
        }
        for cwd in [r"D:\elsewhere", "src", "./sub"] {
            assert!(
                counted_checks_at(
                    "run_checks",
                    &serde_json::json!({}),
                    &result(cwd),
                    &checkout,
                    &commands
                )
                .is_empty(),
                "{cwd:?}"
            );
        }
    }

    /// The local wrapper still counts exactly the checkout it always did: the
    /// root counts, a subfolder does not.
    #[test]
    fn counted_checks_local_wrapper_unchanged() {
        let dir = tempfile::tempdir().expect("checkout");
        let checkout = dir.path();
        let commands = vec!["cargo test --workspace".to_string()];
        assert_eq!(
            counted_checks(
                "run_checks",
                &serde_json::json!({}),
                &result(&checkout.display().to_string()),
                checkout,
                &commands
            ),
            vec![("cargo test --workspace".to_string(), "tree".to_string())]
        );
        let sub = checkout.join("src");
        std::fs::create_dir_all(&sub).expect("subfolder");
        assert!(
            counted_checks(
                "run_checks",
                &serde_json::json!({}),
                &result(&sub.display().to_string()),
                checkout,
                &commands
            )
            .is_empty(),
            "a subfolder is not the checkout"
        );
    }
}
