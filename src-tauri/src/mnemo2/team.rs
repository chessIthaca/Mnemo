// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The Mnemo2 team runtime: tracked workers, review, integration, and recovery.
//!
//! One [`TeamRuntime`] shares the lead's [`Engine`] and owns every real worker
//! session: its worktree, branch, model, brief, transcript, status, pending
//! interaction, and event pump. It deliberately has no Tauri types so the
//! worker lifecycle is testable against a scripted LLM.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use mnemo::app::plan_board::{BoardPlan, BoardStatus, BoardStep, CloseOutcome, PlanBoard};
use mnemo::app::team_ledger::TeamLedger;
use mnemo::app::worktrees::{
    agent_branch, agent_short_id, current_branch, remove_agent_worktree,
};
use mnemo::route_question;
use mnemo::headless::{
    AgentSession, Engine, SessionEvent, SessionId,
};
use mnemo::EmbedderGetter;
use mnemo::RouteKnn;
use mnemo::memory::classifier::ClassifierSlot;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::dispatch_routing::{
    load_routing_settings, RoutingMode, RoutingSettings,
};
use crate::mnemo2::ledger::{self, RecordsLedger};
use crate::mnemo2::lock_holders;
use crate::mnemo2::phase3::{self, MergeRecord};
use crate::mnemo2::runtime::LeadModel;
use crate::mnemo2::pricing::usage_model_key;
use crate::mnemo2::usage::{ProjectUsage, ProjectUsageView};

mod audit;
mod control;
mod dispatch;
mod escalation;
mod external_lead;
mod guardrails;
mod lead_tools;
mod nudge;
mod persistence;
mod pump;
mod record;
mod review;
mod routing;
mod spawn;

use audit::validate_audit_linkage;
use control::recover_worker;
use external_lead::ExternalLead;
use nudge::nudge_ignored_row;
use persistence::TeamPersistence;
use pump::{close_worker_session, save_workers};
use record::validate_worker_id;
use routing::{logs_label, logs_routing, outcome_row};

#[cfg(test)]
pub(crate) use guardrails::{audit_prompt, executor_prompt};
#[cfg(test)]
pub(crate) use record::{KICKOFF, READ_BACK_KICKOFF};
pub(crate) use nudge::tool_errors_text;
pub(crate) use spawn::WorkerSessionSpec;
pub use external_lead::{
    ExternalWait, ExternalWaitStatus, EXTERNAL_LEAD_IDLE_MS, EXTERNAL_LEAD_MEMORY_REMINDER,
};
pub use record::{
    ChangeSource, InboxEntry, InboxKind, MessageSender, ToolErrorNote, WorkerCheck, WorkerRecord,
    WorkerRouting, WorkerStatus,
};

/// The Tauri event channel emitted by the team runtime.
pub const TEAM_EVENT_CHANNEL: &str = "mnemo2://team-event";

/// The Tauri event channel streaming per-worker deletion-flow progress
/// (`DeletionProgress`) while `team_delete_preview` or `team_delete` runs.
pub const DELETION_EVENT_CHANNEL: &str = "mnemo2://worker-deletion";

/// How many transient team events the broadcast bus keeps for a slow consumer.
const EVENT_BUFFER: usize = 1024;

/// The default number of concurrently live worker sessions.
pub const DEFAULT_SLOTS: usize = 3;

/// A plan-board signal for the host: the UI refreshes on `Changed`, the lead
/// is told about a `Notice`.
#[derive(Clone, Debug)]
pub enum BoardSignal {
    /// The board (or the workers stamped on it) may have changed.
    Changed,
    /// A deduplicated TeamManager notice for the lead.
    Notice {
        /// Dedup key.
        key: String,
        /// The notice body.
        text: String,
    },
}

/// A worker on a board step, as the Plan tab shows it.
#[derive(Clone, Debug, Serialize)]
pub struct BoardWorker {
    /// The worker id.
    pub id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's lifecycle status.
    pub status: WorkerStatus,
}

/// One board step plus the workers stamped with it.
#[derive(Clone, Debug, Serialize)]
pub struct BoardStepView {
    /// The step itself.
    #[serde(flatten)]
    pub step: BoardStep,
    /// The workers stamped with this plan and step.
    pub workers: Vec<BoardWorker>,
}

/// One board plan as the UI shows it: the report path is project-relative.
#[derive(Clone, Debug, Serialize)]
pub struct BoardPlanView {
    /// The plan id.
    pub id: String,
    /// The plan title.
    pub title: String,
    /// The plan goal.
    pub goal: String,
    /// `implementation`, `research`, or `bug_fixing`.
    pub kind: String,
    /// Open, closed, or abandoned.
    pub status: BoardStatus,
    /// Epoch milliseconds the plan closed or was abandoned.
    pub closed_at_ms: Option<u64>,
    /// The closing report, relative to the project root.
    pub report_path: Option<String>,
    /// The steps with their workers.
    pub steps: Vec<BoardStepView>,
}

/// The plan board's public snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct BoardSnapshot {
    /// Every board plan.
    pub plans: Vec<BoardPlanView>,
}

/// The team's public snapshot.
#[derive(Clone, Debug, Serialize)]
pub struct TeamSnapshot {
    /// Every worker, including completed/stopped ones.
    pub workers: Vec<WorkerRecord>,
    /// The configured live-session cap.
    pub slots: usize,
    /// The number of currently live worker sessions.
    pub active: usize,
    /// Durable per-project usage and estimated cost across the lead and
    /// every worker; totals survive restarts and worker deletion.
    pub usage: ProjectUsageView,
}

/// One event forwarded from a worker session, tagged with its worker id.
#[derive(Clone, Debug, Serialize)]
pub struct TeamEvent {
    /// The worker that emitted the event.
    pub worker_id: String,
    /// The worker session generation that emitted it.
    pub generation: u64,
    /// The underlying headless event.
    pub event: SessionEvent,
}

/// A worker-to-lead message routed through the `message_parent` host tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TeamMessage {
    /// The worker that sent it.
    pub worker_id: String,
    /// The message text.
    pub text: String,
    /// Unix epoch milliseconds.
    pub at_ms: u128,
}

/// Team startup options. The engine is the lead's shared handle.
#[derive(Clone)]
pub struct TeamRuntimeOptions {
    /// The shared headless engine.
    pub engine: Arc<Engine>,
    /// The selected project root (worker worktrees fork from its branch).
    pub workspace_root: PathBuf,
    /// Mnemo2's app-owned state directory (the `team.json` ledger).
    pub state_dir: PathBuf,
    /// The lead's config directory, which holds Mnemo2's `mnemo2.toml`.
    pub config_dir: PathBuf,
    /// The engine's live Laya classifier slot, shared with every session.
    pub classifier: Option<ClassifierSlot>,
    /// Test-only routing-log override; production uses the global training log.
    pub routing_log_path: Option<PathBuf>,
    /// Explicit cap override; `None` restores the saved cap or the default.
    pub slots: Option<usize>,
    /// The configured model inventory: explicit worker models (and their
    /// reasoning efforts) must belong to it.
    pub models: Vec<LeadModel>,
    /// Configured per-model prices used to estimate real worker cost.
    pub pricing: Vec<mnemo::config::PricingEntry>,
}

struct LiveWorker {
    session: AgentSession,
    busy: Arc<AtomicBool>,
    pump: JoinHandle<()>,
    /// A failed close aborted the pump; only cleanup retries may use this handle.
    cleanup_retry_only: bool,
    /// The nudge strike counter (design §4.6), shared with the pump: three
    /// consecutive failed or verbatim-repeated tool calls nudge the lead.
    /// Beside the session handle, never persisted; a reroute resets it
    /// ([`TeamRuntime::change_model`]).
    strikes: Arc<Mutex<route_question::Strikes>>,
    /// A steer was sent and its follow-up turn has not started: set before the
    /// `SessionCommand::Steer` goes out, cleared by the follow-up's `Started`,
    /// by `Exited`, by a final `Error`, and by the follow-up deadline in
    /// [`pump_worker`]. A steered turn end is not a finished turn, so the pump
    /// holds it until the follow-up starts or the deadline settles it.
    steer_pending: Arc<AtomicBool>,
}

impl LiveWorker {
    fn abort(&self) {
        self.pump.abort();
    }

    /// Admit a non-cleanup session command only when the event pump is live.
    fn admit_session_command(&self, worker_id: &str) -> Result<(), String> {
        if self.cleanup_retry_only {
            return Err(format!(
                "worker '{worker_id}' session cleanup must be retried before sending commands"
            ));
        }
        Ok(())
    }
}

/// Unix epoch milliseconds now.
fn unix_ms() -> u64 {
    u64::try_from(phase3::now_ms()).unwrap_or(u64::MAX)
}

/// A worker status as the short string the routing rows log (`working`,
/// `stopped`, …) — the serde tag of [`WorkerStatus`].
fn status_name(status: WorkerStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The shared team runtime: one engine, many real worker sessions.
pub struct TeamRuntime {
    engine: Arc<Engine>,
    workspace_root: PathBuf,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    live: Arc<Mutex<HashMap<String, LiveWorker>>>,
    messages: Arc<Mutex<Vec<TeamMessage>>>,
    events: broadcast::Sender<TeamEvent>,
    generation: Arc<AtomicU64>,
    next_worker: AtomicU64,
    lead_busy: AtomicBool,
    /// Transition tests flip this to inject a worker-session close
    /// failure; production never sets it. Shared with every worker pump
    /// so the audit release closes sessions exactly like `stop_inner`.
    fail_next_close: Arc<AtomicBool>,
    lead_session: Mutex<Option<SessionId>>,
    merge_lock: Mutex<()>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    config_dir: PathBuf,
    /// The dispatch-routing settings, shared with every worker pump so a
    /// failure's `outcome` row follows the mode in force when it happens.
    routing: Arc<std::sync::RwLock<RoutingSettings>>,
    classifier: Option<ClassifierSlot>,
    /// The online-learning overlay over `dispatch_routing.jsonl`, present when
    /// the engine exposes its shared embedder. Consulted before Laya.
    route_knn: Option<Arc<RouteKnn>>,
    /// One classifier call at a time, shared with the drift checks.
    routing_permit: Arc<tokio::sync::Semaphore>,
    routing_log_path: PathBuf,
    /// The model catalog and its tier assignment, shared with the worker
    /// pumps so a nudge suggests a tier from the catalog in force when the
    /// worker stuck (design §4.6).
    models: Arc<std::sync::RwLock<Vec<LeadModel>>>,
    tiers: Arc<std::sync::RwLock<mnemo::headless::TiersDto>>,
    persistence: TeamPersistence,
    /// The durable per-project usage accumulator (lead + workers).
    usage: Arc<Mutex<ProjectUsage>>,
    /// Serializes every `team.json` write across worker pumps, the lead's
    /// usage recording, and cleanup so no usage report can be lost to a
    /// stale interleaving of concurrent ledger saves.
    save_lock: Arc<Mutex<()>>,
    board: std::sync::Mutex<Option<Arc<PlanBoard>>>,
    signals: broadcast::Sender<BoardSignal>,
    /// The external lead's attach flag, notice queue and activity clock.
    external: std::sync::Mutex<ExternalLead>,
    /// Wakes [`TeamRuntime::wait_external_notices`] on a push or a detach.
    external_wake: tokio::sync::Notify,
}

/// Upper bound for concurrently live worker sessions in one project.
pub const MAX_WORKER_SLOTS: usize = 32;

impl TeamRuntime {
    async fn close_session_for_cleanup(&self, session: &AgentSession) -> Result<(), String> {
        close_worker_session(session, &self.fail_next_close).await
    }

    #[cfg(test)]
    fn fail_next_close_for_test(&self) {
        self.fail_next_close
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Open the team ledger, recover interrupted workers, and prepare to spawn.
    pub async fn open(options: TeamRuntimeOptions) -> Result<Self, String> {
        let config_dir = options.config_dir.clone();
        let persistence = TeamPersistence::new(&options.state_dir);
        let persisted = persistence.load()?;
        let slots = options
            .slots
            .or_else(|| persisted.as_ref().and_then(|team| team.slots))
            .unwrap_or(DEFAULT_SLOTS)
            .clamp(1, MAX_WORKER_SLOTS);
        persistence.slots.store(slots, Ordering::SeqCst);
        let usage = persisted
            .as_ref()
            .map(|team| team.usage.clone())
            .unwrap_or_default();
        let mut workers = persisted.map(|team| team.workers).unwrap_or_default();
        for worker in &mut workers {
            recover_worker(&options.workspace_root, worker).await?;
        }
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (signals, _) = broadcast::channel(EVENT_BUFFER);
        let routing_log_path = options.routing_log_path.clone().unwrap_or_else(
            crate::mnemo2::dispatch_routing::dispatch_routing_log_path,
        );
        // The kNN overlay shares the app's live embedder (never a second
        // backend). With no own memory store there is nothing to embed with,
        // so the overlay is absent and Laya decides every dispatch.
        let route_knn = options.engine.embedder_handle().map(|initial| {
            let engine = Arc::clone(&options.engine);
            let getter: EmbedderGetter =
                Arc::new(move || engine.embedder_handle().unwrap_or_else(|| initial.clone()));
            Arc::new(RouteKnn::new(getter, routing_log_path.clone()))
        });
        let runtime = Self {
            engine: options.engine,
            workspace_root: options.workspace_root,
            records: Arc::new(Mutex::new(workers)),
            live: Arc::new(Mutex::new(HashMap::new())),
            messages: Arc::new(Mutex::new(Vec::new())),
            events,
            generation: Arc::new(AtomicU64::new(0)),
            next_worker: AtomicU64::new(1),
            lead_busy: AtomicBool::new(false),
            fail_next_close: Arc::new(AtomicBool::new(false)),
            lead_session: Mutex::new(None),
            merge_lock: Mutex::new(()),
            pricing: Arc::new(std::sync::RwLock::new(options.pricing)),
            config_dir: config_dir.clone(),
            routing: Arc::new(std::sync::RwLock::new(load_routing_settings(&config_dir))),
            classifier: options.classifier,
            route_knn,
            routing_permit: Arc::new(tokio::sync::Semaphore::new(1)),
            routing_log_path,
            models: Arc::new(std::sync::RwLock::new(options.models)),
            tiers: Arc::new(std::sync::RwLock::new(mnemo::headless::TiersDto::default())),
            persistence,
            usage: Arc::new(Mutex::new(usage)),
            save_lock: Arc::new(Mutex::new(())),
            board: std::sync::Mutex::new(None),
            signals,
            external: std::sync::Mutex::new(ExternalLead::default()),
            external_wake: tokio::sync::Notify::new(),
        };
        runtime.persist().await?;
        // App start: sweep checkouts a previous run renamed into the
        // trash folder but could not remove yet (something held them).
        schedule_trash_sweep(&runtime.workspace_root);
        // App start: a checkout a previous run could not remove (something
        // held it) is retried now, and the branch it was blocking goes with it.
        runtime.retry_leftover_cleanups().await;
        Ok(runtime)
    }

    /// Subscribe to every worker event.
    pub fn subscribe(&self) -> broadcast::Receiver<TeamEvent> {
        self.events.subscribe()
    }

    /// Subscribe to plan-board signals (UI refreshes and lead notices).
    pub fn subscribe_board(&self) -> broadcast::Receiver<BoardSignal> {
        self.signals.subscribe()
    }

    /// Tell subscribers the board may have changed.
    pub(crate) fn signal_board_changed(&self) {
        let _ = self.signals.send(BoardSignal::Changed);
    }

    fn notify_lead(&self, key: String, text: String) {
        let _ = self.signals.send(BoardSignal::Notice { key, text });
    }

    /// Install a stub classifier for runtime integration tests.
    #[cfg(test)]
    pub(crate) fn set_classifier_for_test(&mut self, classifier: ClassifierSlot) {
        self.classifier = Some(classifier);
    }

    /// Inspect the classifier handle installed at open in integration tests.
    #[cfg(test)]
    pub(crate) fn classifier_slot_for_test(&self) -> Option<ClassifierSlot> {
        self.classifier.clone()
    }

    /// Inspect the routing settings loaded at open in integration tests.
    #[cfg(test)]
    pub(crate) fn routing_settings_for_test(&self) -> RoutingSettings {
        self.routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Share the lead's one plan board with the team and register the observer
    /// that tells working workers when the lead records something new.
    pub(crate) fn attach_board(self: &Arc<Self>, board: Arc<PlanBoard>) {
        board.set_external_lead_attached(self.external_lead_attached());
        let team = Arc::downgrade(self);
        let runtime = tokio::runtime::Handle::try_current().ok();
        board.set_edit_observer(Box::new(move |plan_id, kinds| {
            let Some(team) = team.upgrade() else {
                return;
            };
            let plan_id = plan_id.to_string();
            let text = format!(
                "Plan {plan_id} changed: new {}. Call plan_read to re-read it before you continue.",
                kinds.join(", ")
            );
            // The board calls this synchronously after releasing its own
            // guard, so the observer only hands the work to the runtime.
            let Some(runtime) = runtime.clone() else {
                eprintln!(
                    "team: no runtime is available to tell workers that plan {plan_id} changed"
                );
                return;
            };
            runtime.spawn(async move {
                let workers = {
                    let records = team.records.lock().await;
                    records
                        .iter()
                        .filter(|worker| worker.plan_id.as_deref() == Some(plan_id.as_str()))
                        .filter(|worker| {
                            matches!(
                                worker.status,
                                WorkerStatus::Working
                                    | WorkerStatus::Dispatching
                                    | WorkerStatus::Fixing
                            )
                        })
                        .map(|worker| worker.id.clone())
                        .collect::<Vec<_>>()
                };
                for worker_id in workers {
                    // Mnemo2 writes this notice and cannot tell who edited the plan, so it signs it itself.
                    if let Err(error) = team
                        .message(&worker_id, text.clone(), MessageSender::Mnemo)
                        .await
                    {
                        eprintln!(
                            "team: could not tell worker '{worker_id}' that plan {plan_id} changed: {error}"
                        );
                    }
                }
            });
        }));
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = Some(board);
    }

    fn board(&self) -> Option<Arc<PlanBoard>> {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The markdown of the board plan `plan_id`: what the high-risk gate
    /// scans for acceptance scenarios, what an audit brief carries, and what
    /// the Plans tab expands into (through `board_plan_document`).
    pub(crate) fn plan_document(&self, plan_id: &str) -> Result<String, String> {
        self.board()
            .ok_or_else(|| "the plan board is not available".to_string())?
            .read_markdown(plan_id)
    }

    /// Every board plan with the workers stamped on each step. Report paths
    /// are relative to the project root.
    pub async fn board_snapshot(&self) -> Result<BoardSnapshot, String> {
        let board = self
            .board()
            .ok_or_else(|| "the plan board is not available".to_string())?;
        let plans = board.list()?;
        let workers = self.records.lock().await.clone();
        let plans = plans
            .into_iter()
            .map(|plan: BoardPlan| BoardPlanView {
                report_path: plan
                    .report_path
                    .as_deref()
                    .map(|path| project_relative(&self.workspace_root, path)),
                steps: plan
                    .steps
                    .into_iter()
                    .map(|step| {
                        let on_step = workers
                            .iter()
                            .filter(|w| {
                                w.plan_id.as_deref() == Some(plan.id.as_str())
                                    && w.plan_step == step.number.checked_sub(1)
                            })
                            .map(|w| BoardWorker {
                                id: w.id.clone(),
                                name: w.name.clone(),
                                status: w.status,
                            })
                            .collect();
                        BoardStepView {
                            step,
                            workers: on_step,
                        }
                    })
                    .collect(),
                id: plan.id,
                title: plan.title,
                goal: plan.goal,
                kind: plan.kind,
                status: plan.status,
                closed_at_ms: plan.closed_at_ms,
            })
            .collect();
        Ok(BoardSnapshot { plans })
    }

    /// Test seam for the lost-follow-up path: mark a live worker as waiting for
    /// a steered turn's follow-up without sending a steer.
    #[cfg(test)]
    pub(crate) async fn set_steer_pending_for_test(&self, worker_id: &str, pending: bool) {
        if let Some(live) = self.live.lock().await.get(worker_id) {
            live.steer_pending.store(pending, Ordering::SeqCst);
        }
    }

    /// How many worker turns are running now (dispatching, working, fixing or
    /// merging).
    pub async fn live_worker_turns(&self) -> usize {
        ledger::live_turns(&self.records.lock().await)
    }

    /// Persist a new live-session cap without dropping active workers.
    pub async fn set_slots(&self, slots: usize) -> Result<TeamSnapshot, String> {
        if !(1..=MAX_WORKER_SLOTS).contains(&slots) {
            return Err(format!(
                "worker slots must be between 1 and {MAX_WORKER_SLOTS}"
            ));
        }
        let _operation = self.merge_lock.lock().await;
        let active = self.live.lock().await.len();
        if slots < active {
            return Err(format!(
                "stop {} worker(s) before reducing slots to {slots}",
                active - slots
            ));
        }
        let previous = self.persistence.slots.swap(slots, Ordering::SeqCst);
        if let Err(error) = self.persist().await {
            self.persistence.slots.store(previous, Ordering::SeqCst);
            let _ = self.persist().await;
            return Err(error);
        }
        Ok(self.snapshot().await)
    }

    /// Clean inactive clean checkouts and refresh this runtime's worker records.
    pub async fn cleanup(&self) -> Result<CleanupReport, String> {
        let _operation = self.merge_lock.lock().await;
        // Serialize with concurrent usage recording (and pump saves) so
        // cleanup's own ledger rewrite cannot drop a usage report that
        // lands mid-cleanup; the in-memory accumulator stays authoritative.
        let _ledger_save = self.save_lock.lock().await;
        if !self.live.lock().await.is_empty() {
            return Err("stop all workers before cleaning their worktrees".to_string());
        }
        let state_dir = self
            .persistence
            .path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| "the team ledger has no parent directory".to_string())?;
        let report = cleanup_workspace(&self.workspace_root, &state_dir, &self.routing_log_path)
            .await?;
        let restored = self.persistence.load()?.unwrap_or_default();
        *self.records.lock().await = restored.workers;
        Ok(report)
    }

    /// The raw durable per-model usage accumulator behind
    /// [`TeamSnapshot::usage`] (read-only; the Cost tab prices its rows per
    /// model).
    pub async fn usage(&self) -> ProjectUsage {
        self.usage.lock().await.clone()
    }

    /// The current team state.
    pub async fn snapshot(&self) -> TeamSnapshot {
        let workers = self.records.lock().await.clone();
        let active = self.live.lock().await.len();
        let usage = self.usage.lock().await.clone();
        let pricing = self
            .pricing
            .read()
            .expect("pricing lock poisoned")
            .clone();
        TeamSnapshot {
            workers,
            slots: self.persistence.slots.load(Ordering::SeqCst),
            active,
            usage: usage.view(&pricing),
        }
    }

    /// Fold one live usage report (lead or worker session) into the durable
    /// per-project accumulator and persist the ledger.
    ///
    /// The observed model id (already keyed through [`usage_model_key`])
    /// attributes the row; a report with no observed model lands on the
    /// literal `"unknown"` row so it still shows up in totals and in
    /// `unpriced_models` instead of being silently dropped. Raw token counts
    /// are stored (not costs), so the totals can be repriced whenever the
    /// pricing tables change.
    pub async fn record_usage(
        &self,
        model: Option<&str>,
        prompt_tokens: u64,
        cached_tokens: u64,
        completion_tokens: u64,
    ) -> Result<(), String> {
        let key = model
            .map(usage_model_key)
            .unwrap_or("unknown")
            .to_string();
        {
            let mut usage = self.usage.lock().await;
            usage.record(&key, prompt_tokens, cached_tokens, completion_tokens);
        }
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
    }

    /// One worker by id.
    pub async fn worker(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.records
            .lock()
            .await
            .iter()
            .find(|worker| worker.id == worker_id)
            .cloned()
            .ok_or_else(|| format!("unknown worker '{worker_id}'"))
    }

    /// Worker-to-lead messages received through `message_parent`.
    pub async fn messages(&self) -> Vec<TeamMessage> {
        self.messages.lock().await.clone()
    }

    async fn update_worker<F>(&self, worker_id: &str, update: F)
    where
        F: FnOnce(&mut WorkerRecord),
    {
        let mut records = self.records.lock().await;
        if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
            update(worker);
        }
    }

    async fn persist(&self) -> Result<(), String> {
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
    }
    /// Merge one reviewed worker into the clean, idle, non-main lead branch.
    pub async fn merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.merge_as(worker_id, false).await
    }

    /// `by_lead` is true when the lead itself asked, from inside its turn.
    async fn merge_as(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let worker = self.merge_locked(worker_id, by_lead).await?;
        self.after_merge(&worker).await;
        self.signal_board_changed();
        Ok(worker)
    }

    async fn merge_locked(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        // An audit worker is a read-only reviewer: it is never mergeable in ANY
        // state, so this refusal comes before every other gate. A still-running
        // audit (or a differently-named lead branch) must not mask the reason.
        let worker = self.worker(worker_id).await?;
        if worker.kind == "audit" {
            return Err(
                "an audit worker is a read-only reviewer and is never merged; adjudicate it on its implementation worker's review".to_string(),
            );
        }
        if !by_lead && self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before merge".to_string());
        }
        if self.worker_is_busy(worker_id).await {
            return Err("the worker must be idle before merge".to_string());
        }
        let target = current_branch(&self.workspace_root)
            .map_err(|err| format!("could not resolve the lead branch: {err}"))?;
        if target == "main" {
            return Err("Mnemo2 never merges a worker directly into main".to_string());
        }
        phase3::require_clean_lead(&self.workspace_root, "lead").await?;
        phase3::require_clean(&worker.worktree, "worker").await?;
        let tip = phase3::tip(&worker.worktree).await?;
        let records = self.records.lock().await.clone();
        validate_merge(&worker, &tip, &records)?;
        let previous = worker.clone();
        self.update_worker(worker_id, |worker| worker.status = WorkerStatus::Merging)
            .await;
        if let Err(err) = self.persist().await {
            self.update_worker(worker_id, |current| *current = previous.clone())
                .await;
            let _ = self.persist().await;
            return Err(err);
        }
        let message = format!("team: merge {} ({})", worker.name, worker.id);
        let output = match phase3::git(
            &self.workspace_root,
            &["merge", "--no-ff", &worker.branch, "-m", &message],
        )
        .await
        {
            Ok(output) => output,
            Err(err) => {
                self.update_worker(worker_id, |current| *current = previous.clone())
                    .await;
                let _ = self.persist().await;
                return Err(err);
            }
        };
        if !output.status.success() {
            let diagnostic = phase3::git_error(
                &["merge", "--no-ff", &worker.branch, "-m", &message],
                &output,
            );
            let merge_head = phase3::git(
                &self.workspace_root,
                &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
            )
            .await?;
            if !merge_head.status.success() {
                self.update_worker(worker_id, |current| {
                    *current = previous.clone();
                    current.error = Some(diagnostic.clone());
                })
                .await;
                self.persist().await?;
                return Err(diagnostic);
            }
            let abort = match phase3::git(&self.workspace_root, &["merge", "--abort"]).await {
                Ok(output) => output,
                Err(err) => {
                    let diagnostic = format!("{diagnostic}\n{err}");
                    self.update_worker(worker_id, |current| {
                        current.error = Some(diagnostic.clone())
                    })
                    .await;
                    let _ = self.persist().await;
                    return Err(diagnostic);
                }
            };
            if !abort.status.success() {
                let diagnostic = format!(
                    "{diagnostic}\n{}",
                    phase3::git_error(&["merge", "--abort"], &abort)
                );
                self.update_worker(worker_id, |current| {
                    current.error = Some(diagnostic.clone())
                })
                .await;
                let _ = self.persist().await;
                return Err(diagnostic);
            }
            if let Err(err) =
                phase3::require_clean_lead(&self.workspace_root, "lead after conflict").await
            {
                self.update_worker(worker_id, |current| current.error = Some(err.clone()))
                    .await;
                let _ = self.persist().await;
                return Err(err);
            }
            self.update_worker(worker_id, |worker| {
                worker.status = WorkerStatus::Conflict;
                worker.error = Some(diagnostic.clone());
                worker.review = None;
                worker.checks.clear();
                worker.fixer_finding_id = None;
            })
            .await;
            self.persist().await?;
            return Err(diagnostic);
        }
        let commit = phase3::tip(&self.workspace_root).await?;
        let record = merge_record_from_git(&self.workspace_root, &worker, commit, target).await?;
        self.update_worker(worker_id, |worker| {
            worker.status = WorkerStatus::Merged;
            worker.merge = Some(record);
            worker.error = None;
        })
        .await;
        self.persist().await?;
        let live = self.live.lock().await.remove(worker_id);
        if let Some(mut live) = live {
            live.abort();
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(worker_id.to_string(), live);
                self.update_worker(worker_id, |worker| {
                    // The Git merge landed, but keep the worker retryable:
                    // `team.merge` skips Merged records and would otherwise
                    // never retry session shutdown or checkout cleanup.
                    worker.status = WorkerStatus::InReview;
                    worker.merge = None;
                    worker.error = Some(format!(
                        "merge was committed, but cleanup needs attention; retry merge after closing the worker session: {error}"
                    ));
                })
                .await;
                self.persist().await?;
                return self.worker(worker_id).await;
            }
        }
        match cleanup_agent_worktree(&self.workspace_root, &worker).await {
            Err(err) => {
                self.update_worker(worker_id, |worker| {
                    worker.error = Some(format!("{WORKTREE_CLEANUP_ERROR}: {err}"));
                })
                .await;
                self.persist().await?;
            }
            Ok(()) => {
                // The checkout is gone, so Git no longer holds the branch. The
                // merge commit keeps the worker's tip as its second parent, so
                // deleting the branch loses no work.
                let shared = self
                    .records
                    .lock()
                    .await
                    .iter()
                    .any(|other| other.id != worker.id && other.branch == worker.branch);
                if !shared {
                    if let Err(err) = delete_merged_branch(&self.workspace_root, &worker).await {
                        self.update_worker(worker_id, |worker| {
                            worker.error = Some(format!("{BRANCH_CLEANUP_ERROR}: {err}"));
                        })
                        .await;
                        self.persist().await?;
                    }
                }
            }
        }
        self.retry_checkout_cleanups_locked(worker_id).await;
        self.sweep_merged_branches(worker_id).await;
        let record = self.worker(worker_id).await?;
        if record.status == WorkerStatus::Merged {
            // The one terminal end a worker is labelled at (design §4.7):
            // the merge landed and the record is Merged. The retryable
            // InReview cleanup path above logs nothing; its successful retry
            // arrives here. The outcome row follows `logs_label` — a hand
            // reroute is a learning in every mode — while the nudge row is
            // Laya-derived and keeps the `logs_routing` gate.
            if logs_label(&record, self.routing_mode()) {
                if let Some(row) = outcome_row(&record, "merged") {
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

    /// Retry the checkout removal for merged workers whose earlier removal
    /// failed: a folder something held at merge time, typically a shell whose
    /// working folder was inside it. Runs with the merge lock held. A removal
    /// that works (or finds the folder already gone) clears the note; one
    /// that still fails leaves the record as it was. Deleting the branch of a
    /// worker whose checkout is now gone is `sweep_merged_branches`'s job.
    async fn retry_checkout_cleanups_locked(&self, skip: &str) {
        let candidates: Vec<WorkerRecord> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|candidate| {
                    candidate.id != skip
                        && candidate.status == WorkerStatus::Merged
                        && candidate
                            .error
                            .as_deref()
                            .is_some_and(is_worktree_cleanup_error)
                })
                .cloned()
                .collect()
        };
        let mut cleared = false;
        for candidate in candidates {
            if cleanup_agent_worktree(&self.workspace_root, &candidate)
                .await
                .is_err()
            {
                continue;
            }
            self.update_worker(&candidate.id, |worker| {
                if worker
                    .error
                    .as_deref()
                    .is_some_and(is_worktree_cleanup_error)
                {
                    worker.error = None;
                }
            })
            .await;
            cleared = true;
        }
        if cleared {
            let _ = self.persist().await;
        }
    }

    /// App start: retry earlier stuck checkout removals, then the branch
    /// deletes they were blocking. Takes the merge lock itself.
    async fn retry_leftover_cleanups(&self) {
        let _guard = self.merge_lock.lock().await;
        self.retry_checkout_cleanups_locked("").await;
        self.sweep_merged_branches("").await;
    }

    /// Retry the branch delete for earlier merged workers. Runs inside
    /// `merge_locked`, so the merge lock is held. Best effort and silent: a
    /// branch Git refuses to delete stays until the next merge.
    async fn sweep_merged_branches(&self, just_merged: &str) {
        let candidates: Vec<WorkerRecord> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|candidate| {
                    candidate.id != just_merged
                        && ledger::merged(candidate)
                        && owns_its_branch(candidate)
                        && !records.iter().any(|other| {
                            other.id != candidate.id && other.branch == candidate.branch
                        })
                })
                .cloned()
                .collect()
        };
        let mut cleared = false;
        for candidate in candidates {
            // A checkout folder that is still there means its cleanup failed;
            // leave the whole worker alone.
            if std::fs::symlink_metadata(worker_checkout(&self.workspace_root, &candidate.id))
                .is_ok()
            {
                continue;
            }
            if let Ok(true) = delete_merged_branch(&self.workspace_root, &candidate).await {
                self.update_worker(&candidate.id, |worker| {
                    if worker
                        .error
                        .as_deref()
                        .is_some_and(|error| error.starts_with(BRANCH_CLEANUP_ERROR))
                    {
                        worker.error = None;
                    }
                })
                .await;
                cleared = true;
            }
        }
        if cleared {
            let _ = self.persist().await;
        }
    }

    /// Revert a worker merge on the lead branch before it reaches main.
    pub async fn revert_merge(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.revert_merge_as(worker_id, false).await
    }

    async fn revert_merge_as(
        &self,
        worker_id: &str,
        by_lead: bool,
    ) -> Result<WorkerRecord, String> {
        let worker = self.revert_locked(worker_id, by_lead).await?;
        self.after_revert(&worker).await;
        self.signal_board_changed();
        Ok(worker)
    }

    async fn revert_locked(&self, worker_id: &str, by_lead: bool) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        if !by_lead && self.lead_busy.load(Ordering::SeqCst) {
            return Err("the lead must be idle before revert".to_string());
        }
        phase3::require_clean_lead(&self.workspace_root, "lead").await?;
        let worker = self.worker(worker_id).await?;
        if worker.status != WorkerStatus::Merged {
            return Err("only a merged worker can be reverted".to_string());
        }
        let merge = worker
            .merge
            .clone()
            .ok_or_else(|| "worker has no merge record".to_string())?;
        if merge.revert_commit.is_some() {
            return Err("this worker merge was already reverted".to_string());
        }
        let target = current_branch(&self.workspace_root)?;
        if target == "main" || target != merge.target {
            return Err("revert requires the original non-main lead branch".to_string());
        }
        let in_history = phase3::git(
            &self.workspace_root,
            &["merge-base", "--is-ancestor", &merge.commit, "HEAD"],
        )
        .await?;
        if !in_history.status.success() {
            return Err("the original merge is not in the current lead history".to_string());
        }
        let landed = phase3::git(
            &self.workspace_root,
            &["merge-base", "--is-ancestor", &merge.commit, "main"],
        )
        .await?;
        if landed.status.success() {
            return Err("the worker merge has already reached main".to_string());
        }
        let output = phase3::git(
            &self.workspace_root,
            &["revert", "--no-edit", "-m", "1", &merge.commit],
        )
        .await?;
        if !output.status.success() {
            let _ = phase3::git(&self.workspace_root, &["revert", "--abort"]).await;
            return Err(phase3::git_error(
                &["revert", "--no-edit", "-m", "1", &merge.commit],
                &output,
            ));
        }
        let revert = phase3::tip(&self.workspace_root).await?;
        self.update_worker(worker_id, |worker| {
            if let Some(merge) = worker.merge.as_mut() {
                merge.revert_commit = Some(revert);
            }
            worker.error = None;
        })
        .await;
        self.persist().await?;
        self.worker(worker_id).await
    }

    /// After a merge: tick the worker's plan step once its tasks are all
    /// closed with at least one merged, then try to close the plan. Board
    /// errors never fail the merge; they go to the lead in the notice.
    async fn after_merge(&self, worker: &WorkerRecord) {
        let (Some(plan_id), Some(step)) = (worker.plan_id.as_deref(), worker.plan_step) else {
            return;
        };
        let Some(board) = self.board() else {
            return;
        };
        let _guard = self.merge_lock.lock().await;
        let tasks: Vec<WorkerRecord> = self
            .records
            .lock()
            .await
            .iter()
            .filter(|w| w.plan_id.as_deref() == Some(plan_id) && w.plan_step == Some(step))
            .cloned()
            .collect();
        let number = step + 1;
        let mut lines = Vec::new();
        if !tasks.iter().any(ledger::merged) {
            return;
        }
        if tasks.iter().any(|w| !ledger::is_closed(w)) {
            return;
        }
        let outcome = merge_outcome_line(number, worker);
        let mut notice = None;
        match board.tick(plan_id, step, &outcome) {
            Ok(true) => {
                lines.push(format!("Plan {plan_id} step {number} is done: {outcome}"));
                notice = Some(format!(
                    "tick:{}:{}",
                    worker.id,
                    worker.merge.as_ref().map_or("", |m| m.commit.as_str())
                ));
            }
            Ok(false) => {}
            Err(err) => {
                lines.push(format!(
                    "Plan {plan_id} step {number} could not be ticked: {err}"
                ));
                notice = Some(format!("tick-error:{}:{}", worker.id, now_key()));
            }
        }
        let ledger = RecordsLedger::new(self.records.clone());
        // `tick` released the board lock; closing reads the records again.
        match board.try_close(plan_id, &ledger as &dyn TeamLedger).await {
            CloseOutcome::Closed {
                report_path,
                retired_notes,
            } => {
                let relative = project_relative(&self.workspace_root, &report_path);
                lines.push(format!(
                    "Plan {plan_id} is closed. Review report: {relative}"
                ));
                if retired_notes > 0 {
                    lines.push(format!(
                        "{retired_notes} plan-scoped memory note(s) were retired into that report. Read them and re-store any durable fact without plan or branch references."
                    ));
                }
                if self.external_lead_attached() {
                    lines.push(EXTERNAL_LEAD_MEMORY_REMINDER.to_string());
                }
                notice = Some(format!("closed:{plan_id}:{relative}"));
            }
            CloseOutcome::Blocked { reasons } => {
                lines.push(format!(
                    "Plan {plan_id} cannot close yet: {}",
                    reasons.join("; ")
                ));
                notice = Some(format!("blocked:{plan_id}:{}", reasons.join("|")));
            }
            CloseOutcome::NotReady { .. } => {}
        }
        if let Some(key) = notice {
            self.notify_lead(
                format!("board:{key}"),
                format!(
                    "{}\nUse plan_show for the plan's current state.",
                    lines.join("\n")
                ),
            );
        }
    }

    /// After a revert: reopen the step when no other merged task remains.
    async fn after_revert(&self, worker: &WorkerRecord) {
        let (Some(plan_id), Some(step)) = (worker.plan_id.as_deref(), worker.plan_step) else {
            return;
        };
        let Some(board) = self.board() else {
            return;
        };
        let _guard = self.merge_lock.lock().await;
        let still_merged = self.records.lock().await.iter().any(|w| {
            w.plan_id.as_deref() == Some(plan_id) && w.plan_step == Some(step) && ledger::merged(w)
        });
        if still_merged {
            return;
        }
        let number = step + 1;
        let note = format!(
            "{} ({}) was reverted, so no merged task remains for it",
            worker.name, worker.id
        );
        let text = match board.untick(plan_id, step, &note) {
            Ok(true) => format!("Plan {plan_id} step {number} is reopened: {note}"),
            Ok(false) => return,
            Err(err) => format!("Plan {plan_id} step {number} could not be reopened: {err}"),
        };
        self.notify_lead(
            format!("board:untick:{}:{}", worker.id, now_key()),
            format!("{text}\nUse plan_show for the plan's current state."),
        );
    }

    /// Stop every live worker while preserving merged audit records.
    pub async fn stop_all(&self) -> Result<(), String> {
        let _guard = self.merge_lock.lock().await;
        let ids: Vec<String> = self.live.lock().await.keys().cloned().collect();
        for id in ids {
            self.stop_inner(&id).await?;
        }
        Ok(())
    }

    /// Describe what deleting these workers would remove, changing nothing.
    ///
    /// The preview is read-only, so it does not take the merge lock:
    /// `delete_workers` re-checks every worker under the lock before
    /// changing anything. Batch-wide Git facts are gathered once and the
    /// workers are inspected concurrently, so a large batch costs about
    /// the same as a small one.
    pub async fn deletion_preview(
        &self,
        worker_ids: &[String],
    ) -> Result<Vec<WorkerDeletion>, String> {
        self.deletion_preview_streaming(worker_ids, |_| {}).await
    }

    /// `deletion_preview` with per-worker progress: `on_preview` receives
    /// each worker's preview the moment its checks finish, so the open
    /// sheet can fill its rows in as they arrive.
    pub async fn deletion_preview_streaming(
        &self,
        worker_ids: &[String],
        on_preview: impl Fn(WorkerDeletion),
    ) -> Result<Vec<WorkerDeletion>, String> {
        self.inspect_batch(worker_ids, on_preview).await
    }

    /// Delete workers and their history: the ledger record (brief,
    /// transcript, telemetry, review, findings), inbox messages, checkout,
    /// and `wt/agent-*` branch. A live session is stopped first. Every
    /// worker is checked before any is deleted; work the lead branch lacks
    /// (uncommitted changes, unmerged commits) is discarded only when
    /// `discard_work` confirms it. A worker that cannot be deleted is
    /// reported with its error and stays listed — every other worker is
    /// still deleted.
    pub async fn delete_workers(
        &self,
        worker_ids: &[String],
        discard_work: bool,
    ) -> Result<TeamDeleteOutcome, String> {
        self.delete_workers_streaming(worker_ids, discard_work, |_| {}).await
    }

    /// `delete_workers` with per-worker progress: `on_result` receives
    /// each worker's outcome the moment it is known, so the open sheet
    /// can show each row's result as it happens.
    pub async fn delete_workers_streaming(
        &self,
        worker_ids: &[String],
        discard_work: bool,
        on_result: impl Fn(WorkerDeletionResult),
    ) -> Result<TeamDeleteOutcome, String> {
        let _guard = self.merge_lock.lock().await;
        // One result per requested worker, in request order: a failure
        // sets its worker's error and never stops the rest of the batch.
        let mut outcomes: Vec<WorkerDeletionResult> = Vec::with_capacity(worker_ids.len());
        for worker_id in worker_ids {
            outcomes.push(WorkerDeletionResult {
                worker_id: worker_id.clone(),
                name: self
                    .worker(worker_id)
                    .await
                    .map(|record| record.name)
                    .unwrap_or_else(|_| worker_id.clone()),
                error: None,
            });
        }
        // Phase A: check every worker before any is deleted.
        let previews = self.inspect_batch(worker_ids, |_| {}).await?;
        let mut stopped: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (position, preview) in previews.into_iter().enumerate() {
            match Self::deletion_decision(&preview, discard_work) {
                Ok(()) => stopped.push((position, preview)),
                Err(error) => {
                    outcomes[position].error = Some(error.clone());
                    on_result(outcomes[position].clone());
                }
            }
        }
        // Phase B: stop every live session.
        let mut pending: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (position, deletion) in stopped {
            if self.live.lock().await.contains_key(&deletion.worker_id) {
                match self.stop_inner(&deletion.worker_id).await {
                    Ok(_stopped) => pending.push((position, deletion)),
                    Err(error) => {
                        outcomes[position].error = Some(error.clone());
                        on_result(outcomes[position].clone());
                    }
                }
            } else {
                pending.push((position, deletion));
            }
        }
        // Phase C: re-check after stopping — a session may have written
        // more work between the preview and its stop. The batch for this
        // pass is the set that still proceeds, so Git state shared with a
        // worker that failed a check or a stop is never touched.
        let survivor_ids: Vec<String> = pending
            .iter()
            .map(|(_, deletion)| deletion.worker_id.clone())
            .collect();
        let rechecked = self.inspect_batch(&survivor_ids, |_| {}).await?;
        let mut deletions: Vec<(usize, WorkerDeletion)> = Vec::new();
        for (index, preview) in rechecked.into_iter().enumerate() {
            let (position, _) = pending[index];
            match Self::deletion_decision(&preview, discard_work) {
                Ok(()) => deletions.push((position, preview)),
                Err(error) => {
                    outcomes[position].error = Some(error.clone());
                    on_result(outcomes[position].clone());
                }
            }
        }
        // Phase D: delete each worker — one failure never stops the rest.
        for (position, deletion) in deletions {
            if let Err(error) = self.delete_one(&deletion).await {
                outcomes[position].error = Some(error.clone());
            }
            on_result(outcomes[position].clone());
        }
        Ok(TeamDeleteOutcome {
            results: outcomes,
            snapshot: self.snapshot().await,
        })
    }

    /// The decision for one worker's preview: `Ok` deletes it, an `Err`
    /// reports why it must stay — blocked, or work the lead branch lacks
    /// that the user did not confirm discarding.
    fn deletion_decision(deletion: &WorkerDeletion, discard_work: bool) -> Result<(), String> {
        if let Some(reason) = &deletion.blocked {
            return Err(format!("{} cannot be deleted: {reason}", deletion.name));
        }
        if deletion.discards_work() && !discard_work {
            return Err(format!(
                "{} has unmerged or uncommitted work on {}; confirm discarding it to delete the worker",
                deletion.name, deletion.branch
            ));
        }
        Ok(())
    }

    /// Inspect every worker of a deletion batch, streaming each preview
    /// as soon as its checks finish. Batch-wide Git facts are gathered
    /// with two Git runs and the per-worker checkout checks run
    /// `DELETION_CHECK_CONCURRENCY` at a time, so a large batch costs
    /// about the same as a small one. Git state shared with a record
    /// outside the batch is never counted or touched.
    async fn inspect_batch(
        &self,
        batch: &[String],
        on_preview: impl Fn(WorkerDeletion),
    ) -> Result<Vec<WorkerDeletion>, String> {
        let facts = DeletionGitFacts::gather(&self.workspace_root).await?;
        let mut cores = Vec::with_capacity(batch.len());
        for worker_id in batch {
            cores.push(self.inspect_core(worker_id, batch, &facts).await?);
        }
        let mut previews: Vec<Option<WorkerDeletion>> = vec![None; cores.len()];
        let mut pending = Vec::new();
        for (index, core) in cores.iter().enumerate() {
            if core.needs_check {
                pending.push((index, core.checkout.clone(), core.branch.clone()));
            }
        }
        // The Git-heavy half runs as one task per worker, bounded: every
        // worker in a chunk is checked concurrently while the chunks run
        // in order. A preview is finished — and emitted — the moment its
        // check lands.
        for chunk in pending.chunks(DELETION_CHECK_CONCURRENCY) {
            let mut handles = Vec::with_capacity(chunk.len());
            for (index, checkout, branch) in chunk {
                handles.push((
                    *index,
                    tokio::spawn(inspect_checkout(checkout.clone(), branch.clone())),
                ));
            }
            for (index, handle) in handles {
                let check = handle
                    .await
                    .map_err(|error| format!("a checkout check task failed: {error}"))??;
                let preview =
                    self.finish_preview(&cores[index], Some(check), &facts).await?;
                on_preview(preview.clone());
                previews[index] = Some(preview);
            }
        }
        // Workers whose Git state needs no per-worker check (no checkout,
        // unregistered, shared branch, merging) finish immediately.
        for index in 0..cores.len() {
            if previews[index].is_none() {
                let preview = self.finish_preview(&cores[index], None, &facts).await?;
                on_preview(preview.clone());
                previews[index] = Some(preview);
            }
        }
        Ok(previews
            .into_iter()
            .map(|preview| preview.expect("every preview finished"))
            .collect())
    }

    /// The in-memory half of one worker's preview: everything except its
    /// per-worker Git checks. Git state shared with a record outside the
    /// batch is never counted or touched.
    async fn inspect_core(
        &self,
        worker_id: &str,
        batch: &[String],
        facts: &DeletionGitFacts,
    ) -> Result<DeletionCore, String> {
        let record = self.worker(worker_id).await?;
        let shared_branch = self.records.lock().await.iter().any(|other| {
            other.id != record.id
                && other.branch == record.branch
                && !batch.iter().any(|id| id == &other.id)
        });
        // Informative in every state, blocked included: another worker may
        // have landed this record's plan step through its own commits.
        let superseded_by = {
            let records = self.records.lock().await;
            landing_worker_name(&records, &record)
        };
        let mut core = DeletionCore {
            worker_id: record.id.clone(),
            name: record.name.clone(),
            branch: record.branch.clone(),
            live: self.live.lock().await.contains_key(worker_id),
            checkout: worker_checkout(&self.workspace_root, &record.id),
            checkout_exists: false,
            superseded_by,
            shared_branch,
            blocked: None,
            needs_check: false,
        };
        if record.status == WorkerStatus::Merging {
            core.blocked = Some("a merge is in progress".to_string());
            return Ok(core);
        }
        if core.shared_branch || !owns_its_branch(&record) {
            // A shared or malformed legacy record: only the record itself is
            // removed; no Git state another record (or no record) owns is
            // touched.
            return Ok(core);
        }
        core.checkout_exists = std::fs::symlink_metadata(&core.checkout).is_ok();
        // Never probe an unregistered folder with Git: without its own
        // `.git`, Git walks up and answers for the main checkout instead.
        core.needs_check = core.checkout_exists && facts.lists(&core.checkout);
        Ok(core)
    }

    /// Merge a core and its finished checkout check into the worker's
    /// preview. The unmerged count comes from the batch's ahead counts; a
    /// Git without the `%(ahead-behind)` format atom falls back to
    /// counting that one branch with its own `rev-list` call.
    async fn finish_preview(
        &self,
        core: &DeletionCore,
        check: Option<CheckoutCheck>,
        facts: &DeletionGitFacts,
    ) -> Result<WorkerDeletion, String> {
        let (uncommitted, blocked) = match check {
            Some(CheckoutCheck { uncommitted, blocked }) => (uncommitted, core.blocked.clone().or(blocked)),
            None => (false, core.blocked.clone()),
        };
        let unmerged_commits = match &facts.ahead_counts {
            Some(counts) => counts
                .get(&format!("refs/heads/{}", core.branch))
                .copied()
                .unwrap_or(0),
            None => {
                if branch_exists(&self.workspace_root, &core.branch).await {
                    let range = format!("HEAD..refs/heads/{}", core.branch);
                    phase3::git_value(&self.workspace_root, &["rev-list", "--count", &range])
                        .await?
                        .parse()
                        .unwrap_or(0)
                } else {
                    0
                }
            }
        };
        Ok(WorkerDeletion {
            worker_id: core.worker_id.clone(),
            name: core.name.clone(),
            branch: core.branch.clone(),
            live: core.live,
            checkout: core.checkout_exists,
            uncommitted,
            unmerged_commits,
            superseded_by: core.superseded_by.clone(),
            shared_branch: core.shared_branch,
            blocked,
        })
    }

    /// Remove one checked worker. Git state goes first and the record last,
    /// so a failure leaves a record that a retry can finish deleting.
    async fn delete_one(&self, deletion: &WorkerDeletion) -> Result<(), String> {
        let record = self.worker(&deletion.worker_id).await?;
        let live = self.live.lock().await.remove(&record.id);
        if let Some(mut live) = live {
            live.abort();
            if let Err(error) = self.close_session_for_cleanup(&live.session).await {
                let message = format!("could not close {}'s session: {error}", record.name);
                live.cleanup_retry_only = true;
                self.live.lock().await.insert(record.id.clone(), live);
                return Err(message);
            }
        }
        if !deletion.shared_branch && owns_its_branch(&record) {
            self.remove_checkout(&record).await?;
            if branch_exists(&self.workspace_root, &record.branch).await {
                phase3::git_value(&self.workspace_root, &["branch", "-D", &record.branch])
                    .await
                    .map_err(|error| {
                        format!("could not delete {}'s branch: {error}", record.name)
                    })?;
            }
        }
        self.records
            .lock()
            .await
            .retain(|worker| worker.id != record.id);
        self.messages
            .lock()
            .await
            .retain(|message| message.worker_id != record.id);
        self.persist().await
    }

    /// Remove the worker's checkout folder from `.worktrees` and deregister
    /// its worktree. The folder is first renamed into `.worktrees/.trash/`
    /// with one cheap filesystem move — the user never waits for the slow
    /// file-by-file delete of a large checkout; the trash folder is
    /// removed in the background instead. When the rename is refused (on
    /// Windows: something holds the folder or a file in it), the removal
    /// falls back to the foreground retry path, which names the holder.
    async fn remove_checkout(&self, worker: &WorkerRecord) -> Result<(), String> {
        let Some(canonical) = validated_checkout(&self.workspace_root, worker)? else {
            return Ok(());
        };
        match move_checkout_to_trash(&self.workspace_root, &canonical).await {
            Ok(()) => {
                // The renamed checkout no longer sits at its registered
                // path: sweep the trash folder in the background and drop
                // the stale worktree registration, so the branch delete
                // below cannot fail on it.
                schedule_trash_sweep(&self.workspace_root);
                let arguments = ["worktree", "prune"];
                let prune = phase3::git(&self.workspace_root, &arguments).await?;
                if !prune.status.success() {
                    return Err(phase3::git_error(&arguments, &prune));
                }
            }
            Err(_held) => {
                // The move was refused — the folder is held. Fall back to
                // the foreground removal, which waits for holders and
                // reports the one it finds.
                self.remove_checkout_in_front(worker, &canonical).await?;
            }
        }
        Ok(())
    }

    /// The foreground checkout removal, used when the rename into trash
    /// was refused. Runs `git worktree remove --force` for a registered
    /// checkout (discarding was confirmed before) and filesystem removal
    /// for a leftover, then names the holder when the folder refuses to
    /// go.
    async fn remove_checkout_in_front(
        &self,
        worker: &WorkerRecord,
        checkout: &Path,
    ) -> Result<(), String> {
        let mut last_error = String::new();
        for _ in 0..12 {
            if std::fs::symlink_metadata(checkout).is_err() {
                break;
            }
            if !checkout_is_registered(&self.workspace_root, checkout).await? {
                remove_leftover_checkout(&self.workspace_root, worker).await?;
                break;
            }
            match remove_agent_worktree(self.workspace_root.clone(), worker.id.clone()).await {
                Ok(()) if !checkout.exists() => break,
                Ok(()) => {
                    last_error =
                        "Git deregistered the checkout but left its folder".to_string()
                }
                Err(error) => last_error = error,
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        if checkout.exists() {
            // Name what still holds the folder, so the user knows what to
            // close. The probe is best effort and never closes anything.
            let probe = checkout.to_path_buf();
            let holders =
                tokio::task::spawn_blocking(move || lock_holders::holders(&[probe]))
                    .await
                    .unwrap_or_default();
            return Err(format!(
                "could not remove {}'s checkout: {last_error}{}",
                worker.name,
                lock_holders::held_by_suffix(&holders)
            ));
        }
        Ok(())
    }

    /// Stop every live worker. The shared engine is owned by the lead and is
    /// not shut down here.
    pub async fn shutdown(&self) {
        *self.lead_session.lock().await = None;
        let live = {
            let mut map = self.live.lock().await;
            std::mem::take(&mut *map)
        };
        for worker in live.into_values() {
            worker.abort();
            let _ = worker.session.close().await;
        }
        let _ = self.persist().await;
    }
}

/// Whether a record's branch is exactly the one its id names, so Git
/// cleanup can never touch a branch the worker does not own.
fn owns_its_branch(record: &WorkerRecord) -> bool {
    !record.id.is_empty()
        && record
            .id
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
        && record.branch == agent_branch(&record.id)
}

/// The merged worker that landed this record's plan step, when the ledger
/// knows one: another implementation record on the same plan step was
/// merged and never reverted, so the step's work reached the lead branch
/// through that worker's commits — typically a replay worker that
/// re-applied this record's conflicted step by hand. Git cannot prove the
/// two commits are the same work; the ledger can. Purely informative: the
/// caller keeps counting this record's own unmerged commits.
fn landing_worker_name(records: &[WorkerRecord], record: &WorkerRecord) -> Option<String> {
    let (plan_id, plan_step) = (record.plan_id.as_deref()?, record.plan_step?);
    records
        .iter()
        .find(|other| {
            other.id != record.id
                && other.kind == "implementation"
                && other.plan_id.as_deref() == Some(plan_id)
                && other.plan_step == Some(plan_step)
                && ledger::merged(other)
        })
        .map(|other| other.name.clone())
}

/// The worker's registered checkout path inside the project.
fn worker_checkout(root: &Path, worker_id: &str) -> PathBuf {
    root.join(".worktrees")
        .join(format!("agent-{}", agent_short_id(worker_id)))
}

async fn branch_exists(root: &Path, branch: &str) -> bool {
    let revision = format!("refs/heads/{branch}^{{commit}}");
    phase3::git_value(root, &["rev-parse", "--verify", "--quiet", &revision])
        .await
        .is_ok()
}

/// How many workers a deletion preview checks concurrently. Each checked
/// worker runs two short Git calls; a small bound keeps a large batch
/// from fighting the live sessions it lists for the CPU.
const DELETION_CHECK_CONCURRENCY: usize = 4;

/// The in-memory half of one worker's deletion preview: everything the
/// [`WorkerDeletion`] reports before its per-worker Git checks run.
struct DeletionCore {
    /// The worker's id.
    worker_id: String,
    /// The worker's display name.
    name: String,
    /// The worker's `wt/agent-*` branch.
    branch: String,
    /// The worker's session is live; deleting stops it first.
    live: bool,
    /// The worker's expected checkout path.
    checkout: PathBuf,
    /// The checkout still exists at that path.
    checkout_exists: bool,
    /// The merged worker that landed this record's plan step through its
    /// own commits, when the ledger knows one.
    superseded_by: Option<String>,
    /// Another worker record outside the batch uses the same branch.
    shared_branch: bool,
    /// Why the worker cannot be deleted right now, when already known.
    blocked: Option<String>,
    /// The checkout exists and is registered, so its branch and
    /// uncommitted state still need to be read.
    needs_check: bool,
}

/// One worker's per-worker Git checks: whether its checkout holds
/// anything uncommitted, and why it is blocked when its branch moved.
struct CheckoutCheck {
    /// The checkout holds uncommitted changes.
    uncommitted: bool,
    /// Why the checkout blocks the worker's deletion, when it does.
    blocked: Option<String>,
}

/// Batch-wide, read-only Git facts for one deletion preview. One
/// `git worktree list` run and one `git for-each-ref` run serve the whole
/// batch instead of each worker spawning its own Git processes (each Git
/// call costs about 0.1 s on this codebase).
struct DeletionGitFacts {
    /// Every checkout path Git's worktree registry lists for this repo.
    registered_checkouts: Vec<PathBuf>,
    /// `<full refname> -> commits ahead of the lead branch's HEAD` for
    /// every local branch. `None` on a Git without the `%(ahead-behind)`
    /// format atom — the preview then counts each branch with its own
    /// `rev-list --count` call.
    ahead_counts: Option<HashMap<String, u32>>,
}

impl DeletionGitFacts {
    /// Gather the batch's facts with two Git runs.
    async fn gather(root: &Path) -> Result<Self, String> {
        let registered_checkouts = registered_checkouts(root).await?;
        let ahead_counts = match phase3::git_value(
            root,
            &[
                "for-each-ref",
                "--format=%(refname) %(ahead-behind:HEAD)",
                "refs/heads/",
            ],
        )
        .await
        {
            Ok(list) => parse_ahead_counts(&list),
            Err(_) => None,
        };
        Ok(Self {
            registered_checkouts,
            ahead_counts,
        })
    }

    /// Whether Git's worktree registry lists this checkout path.
    fn lists(&self, checkout: &Path) -> bool {
        let expected = normalized_checkout_path(checkout);
        self.registered_checkouts
            .iter()
            .any(|listed| same_checkout_path(listed, &expected))
    }
}

/// Every local branch's commits ahead of HEAD, read from one
/// `git for-each-ref --format='%(refname) %(ahead-behind:HEAD)'` run.
/// `None` when a line fails to parse — the preview falls back to
/// counting that branch with its own `rev-list` call.
fn parse_ahead_counts(list: &str) -> Option<HashMap<String, u32>> {
    let mut counts = HashMap::with_capacity(list.lines().count());
    for line in list.lines() {
        let mut fields = line.split_whitespace();
        let refname = fields.next()?;
        let ahead = fields.next()?;
        counts.insert(refname.to_string(), ahead.parse().ok()?);
    }
    Some(counts)
}

/// Every checkout path Git's worktree registry lists, in one
/// `git worktree list` run instead of one per worker.
async fn registered_checkouts(root: &Path) -> Result<Vec<PathBuf>, String> {
    let arguments = ["worktree", "list", "--porcelain", "-z"];
    let output = phase3::git(root, &arguments).await?;
    if !output.status.success() {
        return Err(phase3::git_error(&arguments, &output));
    }
    worktree_list_paths(&output.stdout)
}

/// One worker's per-worker Git checks. Untracked files are listed without
/// descending into untracked directories (`--untracked-files=normal`): the
/// preview only needs to know that *something* is uncommitted, not
/// enumerate a `node_modules` tree.
async fn inspect_checkout(checkout: PathBuf, branch: String) -> Result<CheckoutCheck, String> {
    let current = checkout_branch(&checkout).await?;
    if current != branch {
        return Ok(CheckoutCheck {
            uncommitted: false,
            blocked: Some(format!("its checkout is on '{current}', not {branch}")),
        });
    }
    let status = phase3::git_value(
        &checkout,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .await?;
    Ok(CheckoutCheck {
        uncommitted: !status.is_empty(),
        blocked: None,
    })
}

/// A checkout's current branch, without blocking the async executor (the
/// shared [`current_branch`] helper runs Git synchronously).
async fn checkout_branch(checkout: &Path) -> Result<String, String> {
    let branch = phase3::git_value(checkout, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let branch = branch.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return Err("the checkout is on a detached HEAD".to_string());
    }
    Ok(branch)
}

/// Whether Git's main-checkout worktree registry still lists this exact path.
/// Never probe an unregistered folder with Git: without its own `.git`, Git
/// walks up and answers for the main checkout instead.
async fn checkout_is_registered(root: &Path, checkout: &Path) -> Result<bool, String> {
    let expected = normalized_checkout_path(checkout);
    Ok(registered_checkouts(root)
        .await?
        .iter()
        .any(|listed| same_checkout_path(listed, &expected)))
}

/// Decode NUL-delimited porcelain worktree paths without replacement
/// characters. Unix paths remain arbitrary bytes; platforms whose paths are
/// Unicode reject invalid UTF-8 instead of failing open as an unregistered
/// checkout.
fn worktree_list_paths(stdout: &[u8]) -> Result<Vec<PathBuf>, String> {
    stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_prefix(b"worktree "))
        .map(|path| {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;

                Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
            }
            #[cfg(not(unix))]
            {
                std::str::from_utf8(path)
                    .map(PathBuf::from)
                    .map_err(|error| format!("undecodable worktree path: {error}"))
            }
        })
        .collect()
}

/// Normalize a checkout without following a symlink or junction at its final
/// component. Git prints absolute paths; canonicalizing the parent also
/// reconciles Windows short names and verbatim prefixes.
fn normalized_checkout_path(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => dunce::canonicalize(parent)
            .unwrap_or_else(|_| dunce::simplified(parent).to_path_buf())
            .join(name),
        _ => dunce::simplified(path).to_path_buf(),
    }
}

fn same_checkout_path(left: &Path, right: &Path) -> bool {
    let left = normalized_checkout_path(left);
    let right = normalized_checkout_path(right);
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
            == right
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

/// The checkout path after every ownership guard passes: the worker's own
/// direct-child folder of the canonical `.worktrees`, never a link or
/// junction, never anything else. `None` when it no longer exists.
fn validated_checkout(root: &Path, worker: &WorkerRecord) -> Result<Option<PathBuf>, String> {
    if !owns_its_branch(worker) {
        return Err("refusing to remove a checkout for an invalid worker identity".to_string());
    }
    let checkout = worker_checkout(root, &worker.id);
    let metadata = match std::fs::symlink_metadata(&checkout) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if is_link_or_junction(&metadata) {
        return Err(format!(
            "refusing to remove a symlink or junction at {}",
            checkout.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "refusing to remove a non-directory at {}",
            checkout.display()
        ));
    }
    let canonical_root = dunce::canonicalize(root).map_err(|error| error.to_string())?;
    let worktrees = root.join(".worktrees");
    let canonical_worktrees =
        dunce::canonicalize(&worktrees).map_err(|error| error.to_string())?;
    if !same_checkout_path(&canonical_worktrees, &canonical_root.join(".worktrees")) {
        return Err(format!(
            "refusing to remove {}: .worktrees is not a direct child of the workspace",
            checkout.display()
        ));
    }
    let canonical = dunce::canonicalize(&checkout).map_err(|error| error.to_string())?;
    let expected = canonical_worktrees.join(format!("agent-{}", agent_short_id(&worker.id)));
    if !same_checkout_path(&canonical, &expected)
        || canonical.parent() != Some(canonical_worktrees.as_path())
        || canonical.file_name() != expected.file_name()
    {
        return Err(format!(
            "refusing to remove {}: it is not the worker's own checkout",
            checkout.display()
        ));
    }
    Ok(Some(canonical))
}

/// Remove an unregistered checkout with filesystem operations only. The
/// canonical path must remain the expected direct child of this repository's
/// `.worktrees`; links and junctions are refused rather than traversed.
async fn remove_leftover_checkout(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    let Some(canonical) = validated_checkout(root, worker)? else {
        return Ok(());
    };

    let mut last_error = String::new();
    for attempt in 0..12 {
        // Revalidate immediately before every destructive call, then delete
        // only the canonical path that passed the direct-child guard above.
        let current = match dunce::canonicalize(&canonical) {
            Ok(current) => current,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        let metadata = match std::fs::symlink_metadata(&canonical) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if current != canonical || is_link_or_junction(&metadata) || !metadata.is_dir() {
            return Err(format!(
                "refusing to remove {}: the validated checkout changed",
                canonical.display()
            ));
        }
        let path = canonical.clone();
        match tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(Err(error)) => last_error = error.to_string(),
            Err(error) => last_error = error.to_string(),
        }
        if !canonical.exists() {
            return Ok(());
        }
        if attempt < 11 {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
    // Name what still holds the folder, so the user knows what to close. The
    // probe is best effort and never closes anything itself.
    let probe = canonical.clone();
    let holders = tokio::task::spawn_blocking(move || lock_holders::holders(&[probe]))
        .await
        .unwrap_or_default();
    Err(format!(
        "could not remove leftover checkout {}: {last_error}{}",
        canonical.display(),
        lock_holders::held_by_suffix(&holders)
    ))
}

/// The trash folder renamed-away checkouts wait in for background removal:
/// `.worktrees/.trash` — inside the gitignored `.worktrees`, never a
/// tracked path.
fn trash_root(root: &Path) -> PathBuf {
    root.join(".worktrees").join(".trash")
}

/// Every folder waiting in the trash.
fn trash_entries(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(trash_root(root)) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect()
}

/// Rename a validated checkout folder into the trash folder with one
/// `rename` (same volume, same parent tree): the delete flow waits for a
/// cheap move instead of the slow file-by-file removal of a large
/// checkout. Fails when something holds the folder — the caller then
/// falls back to the foreground removal path.
async fn move_checkout_to_trash(root: &Path, checkout: &Path) -> Result<(), String> {
    tokio::task::spawn_blocking({
        let root = root.to_path_buf();
        let checkout = checkout.to_path_buf();
        move || {
            let trash = trash_root(&root);
            std::fs::create_dir_all(&trash).map_err(|error| error.to_string())?;
            let name = checkout
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| "the checkout path has no folder name".to_string())?;
            let mut target = trash.join(format!("{name}-{}", phase3::now_ms()));
            let mut attempt = 0;
            while target.exists() {
                attempt += 1;
                target = trash.join(format!("{name}-{}-{attempt}", phase3::now_ms()));
            }
            std::fs::rename(checkout, &target).map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|error| format!("the checkout move task failed: {error}"))?
}

/// Sweep the trash folder in the background: try to remove every folder in
/// it, retrying for a bounded window — a file can be held for a moment by
/// an indexer or a scanner right after the rename. Anything still held
/// stays for the next sweep; every app start schedules one.
fn schedule_trash_sweep(root: &Path) {
    let root = root.to_path_buf();
    tokio::spawn(async move {
        for _ in 0..12 {
            if trash_entries(&root).is_empty() {
                return;
            }
            sweep_trash(&root).await;
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });
}

/// Remove every folder under `.worktrees/.trash/` once. Each folder is
/// removed independently — one locked folder never keeps the others.
async fn sweep_trash(root: &Path) {
    for entry in trash_entries(root) {
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(entry)).await;
    }
}

/// The durable error prefix a failed `git branch -d` leaves on a merged
/// worker: the merge landed and only the branch cleanup is outstanding.
const BRANCH_CLEANUP_ERROR: &str = "merged, but branch cleanup needs attention";

/// The durable notes a failed checkout removal leaves on a merged worker.
/// `merge_locked` writes the first, `recover_worker` the second; both are
/// retried by `retry_checkout_cleanups_locked`.
const WORKTREE_CLEANUP_ERROR: &str = "merged, but worktree cleanup needs attention";
const RECOVERED_WORKTREE_CLEANUP_ERROR: &str =
    "merge recovered, but worktree cleanup needs attention";

/// Whether an error is one of the checkout-cleanup notes above.
fn is_worktree_cleanup_error(error: &str) -> bool {
    error.starts_with(WORKTREE_CLEANUP_ERROR)
        || error.starts_with(RECOVERED_WORKTREE_CLEANUP_ERROR)
}

async fn cleanup_agent_worktree(root: &Path, worker: &WorkerRecord) -> Result<(), String> {
    validate_worker_id(&worker.id)?;
    let worktree = worker_checkout(root, &worker.id);
    match std::fs::symlink_metadata(&worktree) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    if !checkout_is_registered(root, &worktree).await? {
        return remove_leftover_checkout(root, worker).await;
    }
    if current_branch(&worktree)? != agent_branch(&worker.id) {
        return Err("worker checkout is on an unexpected branch; checkout retained".to_string());
    }
    let mut last_error = String::new();
    for _ in 0..12 {
        phase3::require_clean(&worktree, "worker before worktree cleanup").await?;
        match remove_agent_worktree(root.to_path_buf(), worker.id.clone()).await {
            Ok(()) if !worktree.exists() => return Ok(()),
            Ok(()) => last_error = "Git deregistered the checkout but left its folder".to_string(),
            Err(_) if !worktree.exists() => return Ok(()),
            Err(err) => last_error = err,
        }
        if !checkout_is_registered(root, &worktree).await? {
            return remove_leftover_checkout(root, worker).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(last_error)
}

/// Delete a merged worker's branch with Git's safe delete. `Ok(true)` when
/// the branch was deleted. `Ok(false)` when there is nothing for this worker
/// to delete: the branch is not the one its id names, or it is already gone.
/// `git branch -d` refuses a branch that is not fully merged into the
/// checked-out lead branch, or that a worktree still uses; that refusal comes
/// back as `Err` and the branch stays.
async fn delete_merged_branch(root: &Path, worker: &WorkerRecord) -> Result<bool, String> {
    if !owns_its_branch(worker) || !branch_exists(root, &worker.branch).await {
        return Ok(false);
    }
    phase3::git_value(root, &["branch", "-d", &worker.branch])
        .await
        .map(|_| true)
}

async fn merge_record_from_git(
    root: &std::path::Path,
    worker: &WorkerRecord,
    commit: String,
    target: String,
) -> Result<MergeRecord, String> {
    let review = worker
        .review
        .as_ref()
        .ok_or_else(|| "the merge has no durable review record".to_string())?;
    let (files, additions, deletions) = phase3::merge_diff_stats(root, &commit).await?;
    Ok(MergeRecord {
        commit,
        worker_tip: review.tip.clone(),
        branch: worker.branch.clone(),
        target,
        checks: worker
            .checks
            .iter()
            .map(|check| check.name.clone())
            .collect(),
        review_summary: review.summary.clone(),
        review_risk: Some(worker.review_risk.clone()),
        review_evidence: review.evidence.clone(),
        files,
        additions,
        deletions,
        worker_model: worker.model_id.clone().or_else(|| worker.model.clone()),
        worker_tokens: worker.total_tokens,
        cost_micros: worker.cost_micros,
        merged_at_ms: phase3::now_ms(),
        revert_commit: None,
    })
}

async fn merge_conflicts_only(root: &Path) -> Result<bool, String> {
    let status = phase3::git_value(root, &["status", "--porcelain", "-uall"]).await?;
    let mut conflicts = false;
    for line in status.lines().filter(|line| !line.is_empty()) {
        if line.len() < 3 || !matches!(&line[..2], "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU") {
            return Ok(false);
        }
        conflicts = true;
    }
    Ok(conflicts)
}

/// What deleting one worker removes, reported before anything is deleted so
/// the user can see exactly which unmerged work would be discarded.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkerDeletion {
    /// The worker's id.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's `wt/agent-*` branch.
    pub branch: String,
    /// The worker's session is live; deleting stops it first.
    pub live: bool,
    /// The worker's checkout still exists and will be removed.
    pub checkout: bool,
    /// That checkout has uncommitted changes.
    pub uncommitted: bool,
    /// Commits on the branch that the lead branch does not contain.
    pub unmerged_commits: u32,
    /// The merged worker that landed this record's plan step through its
    /// own commits, when the ledger knows one (a replay worker, for
    /// example): those commits re-apply work that already reached the lead
    /// branch, so the preview says whose instead of presenting them as the
    /// only copy. `None` — serialized as `null` — when no merged worker
    /// shares the step. Purely informative: the commit count and the
    /// discard check are unaffected.
    pub superseded_by: Option<String>,
    /// Another worker record (outside this deletion) uses the same branch,
    /// so the branch and checkout are kept and only this record goes.
    pub shared_branch: bool,
    /// Why the worker cannot be deleted right now, when it cannot.
    pub blocked: Option<String>,
}

impl WorkerDeletion {
    /// Whether deleting this worker discards work the lead branch lacks.
    pub fn discards_work(&self) -> bool {
        self.uncommitted || self.unmerged_commits > 0
    }
}

/// One worker's outcome of a completed deletion batch: `error` is `None`
/// exactly when the worker was deleted.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct WorkerDeletionResult {
    /// The worker the result describes.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// Why the worker could not be deleted, or `None` when it was deleted.
    pub error: Option<String>,
}

/// `team_delete`'s outcome: one result per requested worker, in request
/// order, plus the team snapshot after every deletion that succeeded.
#[derive(Clone, Debug, Serialize)]
pub struct TeamDeleteOutcome {
    /// One result per requested worker, in request order.
    pub results: Vec<WorkerDeletionResult>,
    /// The team after every deletion that succeeded.
    pub snapshot: TeamSnapshot,
}

/// One streamed deletion-flow update on [`DELETION_EVENT_CHANNEL`]: a
/// worker's finished preview (checking phase) or its deletion result
/// (deleting phase), emitted the moment it is known so the open sheet
/// can fill its rows in.
#[derive(Clone, Debug, Serialize)]
pub struct DeletionProgress {
    /// The phase the update belongs to.
    pub phase: DeletionPhase,
    /// The worker the update describes.
    pub worker_id: String,
    /// The worker's display name.
    pub name: String,
    /// The worker's finished preview; `None` in the deleting phase.
    pub preview: Option<WorkerDeletion>,
    /// Why the worker's deletion failed; `None` when it was deleted.
    pub error: Option<String>,
}

/// Which phase of a deletion flow an update belongs to.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeletionPhase {
    /// `team_delete_preview` streams a worker's finished preview.
    Checking,
    /// `team_delete` streams a worker's deletion result.
    Deleting,
}

/// Outcome of conservative start-screen cleanup.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct CleanupReport {
    /// Clean worktrees actually removed.
    pub removed: Vec<String>,
    /// Branches/worktrees retained, with the reason for each.
    pub skipped: Vec<String>,
}

/// Clean only inactive, clean worker checkouts; retain every worker branch.
pub async fn cleanup_workspace(
    root: &Path,
    state_dir: &Path,
    routing_log_path: &Path,
) -> Result<CleanupReport, String> {
    current_branch(root)?;
    let persistence = TeamPersistence::new(state_dir);
    let Some(mut ledger) = persistence.load()? else {
        return Ok(CleanupReport::default());
    };
    let mut report = CleanupReport::default();
    for index in 0..ledger.workers.len() {
        let worker = &mut ledger.workers[index];
        if worker.merge.is_some()
            || worker.worktree.as_os_str().is_empty()
            || matches!(
                worker.status,
                WorkerStatus::Dispatching
                    | WorkerStatus::Working
                    | WorkerStatus::NeedsYou
                    | WorkerStatus::Fixing
                    | WorkerStatus::Merging
                    | WorkerStatus::Conflict
            )
        {
            continue;
        }
        let id = worker.id.clone();
        let branch = worker.branch.clone();
        if branch != agent_branch(&id)
            || id.is_empty()
            || !id.chars().all(|c| c.is_ascii_alphanumeric())
        {
            report
                .skipped
                .push(format!("{branch}: invalid worker identity"));
            continue;
        }
        let checkout = root
            .join(".worktrees")
            .join(format!("agent-{}", agent_short_id(&id)));
        let recorded = match dunce::canonicalize(&worker.worktree) {
            Ok(path) => path,
            Err(_) if !worker.worktree.exists() => checkout.clone(),
            Err(error) => {
                report.skipped.push(format!("{branch}: {error}"));
                continue;
            }
        };
        let expected = if std::fs::symlink_metadata(&checkout).is_ok() {
            dunce::canonicalize(&checkout).map_err(|err| err.to_string())?
        } else {
            checkout.clone()
        };
        if recorded != expected {
            report.skipped.push(format!(
                "{branch}: checkout is outside its registered worktree"
            ));
            continue;
        }
        let revision = format!("refs/heads/{branch}^{{commit}}");
        if let Err(error) = phase3::git_value(root, &["rev-parse", "--verify", &revision]).await {
            report.skipped.push(format!(
                "{branch}: retained checkout because the branch cannot be verified: {error}"
            ));
            continue;
        }
        if std::fs::symlink_metadata(&checkout).is_ok() {
            let registered = checkout_is_registered(root, &checkout).await?;
            let mut removal_error = None;
            if registered {
                match phase3::git_value(&checkout, &["status", "--porcelain", "-uall"]).await {
                    Ok(status) if status.trim().is_empty() => {}
                    Ok(_) => {
                        report
                            .skipped
                            .push(format!("{branch}: uncommitted work is retained for review"));
                        continue;
                    }
                    Err(error) => {
                        report.skipped.push(format!(
                            "{branch}: cleanliness could not be verified: {error}"
                        ));
                        continue;
                    }
                }
                if let Err(error) = remove_agent_worktree(root.to_path_buf(), id.clone()).await {
                    removal_error = Some(error);
                }
            }
            if std::fs::symlink_metadata(&checkout).is_ok() {
                if checkout_is_registered(root, &checkout).await? {
                    report.skipped.push(format!(
                        "{branch}: cleanup failed: {}",
                        removal_error.unwrap_or_else(|| {
                            "the registered checkout folder remains".to_string()
                        })
                    ));
                    continue;
                }
                if let Err(error) = remove_leftover_checkout(root, worker).await {
                    report
                        .skipped
                        .push(format!("{branch}: cleanup failed: {error}"));
                    continue;
                }
            }
            report.removed.push(branch.clone());
        } else {
            report.skipped.push(format!(
                "{branch}: checkout was already absent; branch retained"
            ));
        }
        worker.status = WorkerStatus::Cancelled;
        worker.pending = None;
        worker.worktree = PathBuf::new();
        worker.error =
            Some("inactive checkout cleaned; the branch is retained for review".to_string());
        // Abandoned is terminal (design §4.7): the worker's `outcome` row
        // and, when its one nudge was never acted on, the `nudge_ignored`
        // row land beside the cleaned checkout — the outcome row for a
        // worker routing ever applied to or one that was rerouted by hand
        // (a hand reroute is a learning in every mode), the nudge row only
        // for a worker routing ever applied to (cleanup has no settings
        // handle, so the record decides).
        let outcome = outcome_row(worker, "abandoned")
            .filter(|_| logs_label(worker, RoutingMode::Off));
        let nudge_ignored = nudge_ignored_row(worker)
            .filter(|_| logs_routing(worker, RoutingMode::Off));
        let rows = [outcome, nudge_ignored]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        persistence.save(&ledger)?;
        for row in rows {
            mnemo::model_routing::append_row(routing_log_path, &row);
        }
    }
    Ok(report)
}

fn now_key() -> u128 {
    phase3::now_ms()
}

/// `path` relative to the project root with forward slashes; the path as-is
/// when it is not under the root.
fn project_relative(root: &Path, path: &str) -> String {
    let path = Path::new(path);
    let relative = path
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .ok()
        .or_else(|| {
            // The same location spelled differently (short names, `\\?\`).
            let root = std::fs::canonicalize(root).ok()?;
            let path = std::fs::canonicalize(path).ok()?;
            path.strip_prefix(root).map(Path::to_path_buf).ok()
        })
        .unwrap_or_else(|| path.to_path_buf());
    relative.to_string_lossy().replace('\\', "/")
}

/// One `## Progress` line for a merged task: step, worker, model, merge
/// commit, and the review summary when one was recorded.
fn merge_outcome_line(number: usize, worker: &WorkerRecord) -> String {
    let merge = worker.merge.as_ref();
    let model = merge
        .and_then(|m| m.worker_model.clone())
        .or_else(|| worker.model_id.clone())
        .or_else(|| worker.model.clone())
        .unwrap_or_else(|| "default model".to_string());
    let commit = merge.map_or("", |m| m.commit.as_str());
    let short: String = commit.chars().take(8).collect();
    let mut line = format!(
        "step {number} merged: {} ({}) on {model}, merge commit {short}",
        worker.name, worker.id
    );
    if let Some(summary) = merge
        .map(|m| m.review_summary.trim())
        .filter(|summary| !summary.is_empty())
    {
        line.push_str(&format!("; review: {summary}"));
    }
    line
}

/// Enforce every source/evidence precondition while the merge lock is held.
fn validate_merge(worker: &WorkerRecord, tip: &str, records: &[WorkerRecord]) -> Result<(), String> {
    if worker.status != WorkerStatus::InReview {
        return Err(format!("worker is {:?}, not in review", worker.status));
    }
    let review = worker
        .review
        .as_ref()
        .ok_or_else(|| "the current worker tip has not been reviewed".to_string())?;
    if review.tip != tip {
        return Err("the review is stale; review the current worker tip".to_string());
    }
    // A summary plus passing checks alone cannot merge: every review of the
    // merge-time tip must carry complete structured evidence. Legacy reviews
    // decoded without evidence fail here, so old records need a fresh review.
    let evidence = review.evidence.as_ref().ok_or_else(|| {
        "the review predates structured evidence; re-review the current worker tip with a complete evidence object"
            .to_string()
    })?;
    // High-risk merges additionally require a completed, fresh, read-only
    // audit linked to this exact tip plus a written lead adjudication. Any
    // fix commit moved the tip and made the previous audit stale — this gate
    // blocks the merge until a fresh audit ran at the new tip.
    let require_audit = worker.review_risk == "high";
    phase3::validate_review_evidence(evidence, require_audit)?;
    validate_audit_linkage(records, evidence, &worker.id, tip)?;
    if let Some(finding) = worker
        .findings
        .iter()
        .find(|finding| finding.status == "open")
    {
        return Err(format!("finding '{}' is still open", finding.id));
    }
    phase3::validate_findings(&worker.findings)?;
    if worker.checks.is_empty() {
        return Err("no checks have passed on the merge-time tip".to_string());
    }
    if let Some(check) = worker
        .checks
        .iter()
        .find(|check| check.status != "passed" || check.tip != tip)
    {
        return Err(format!(
            "check '{}' has not passed on the merge-time tip",
            check.name
        ));
    }
    Ok(())
}

/// A bare worker record for tests in sibling modules (`notice_tag`): the
/// shape the team test fixture builds, without a checkout on disk.
#[cfg(test)]
pub(crate) fn test_worker_record(id: &str) -> WorkerRecord {
    tests::worker(std::path::Path::new("."), id)
}

#[cfg(test)]
#[path = "team/tests.rs"]
mod tests;
