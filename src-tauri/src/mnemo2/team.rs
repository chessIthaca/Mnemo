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

use mnemo::app::plan_board::{BoardPlan, BoardStatus, BoardStep, PlanBoard};
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

use crate::mnemo2::dispatch_routing::{load_routing_settings, RoutingSettings};
use crate::mnemo2::ledger;
use crate::mnemo2::phase3;
use crate::mnemo2::runtime::LeadModel;
use crate::mnemo2::pricing::usage_model_key;
use crate::mnemo2::usage::{ProjectUsage, ProjectUsageView};

mod audit;
mod control;
mod deletion;
mod dispatch;
mod escalation;
mod external_lead;
mod guardrails;
mod lead_tools;
mod merge;
mod nudge;
mod persistence;
mod pump;
mod record;
mod review;
mod routing;
mod spawn;

use control::recover_worker;
use deletion::schedule_trash_sweep;
use external_lead::ExternalLead;
use merge::{
    RECOVERED_WORKTREE_CLEANUP_ERROR, cleanup_agent_worktree, merge_conflicts_only,
    merge_record_from_git,
};
use persistence::TeamPersistence;
use pump::{close_worker_session, save_workers};

pub use deletion::{
    CleanupReport, DeletionPhase, DeletionProgress, TeamDeleteOutcome, WorkerDeletion,
    WorkerDeletionResult, cleanup_workspace,
};
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

    /// Stop every live worker while preserving merged audit records.
    pub async fn stop_all(&self) -> Result<(), String> {
        let _guard = self.merge_lock.lock().await;
        let ids: Vec<String> = self.live.lock().await.keys().cloned().collect();
        for id in ids {
            self.stop_inner(&id).await?;
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

/// A bare worker record for tests in sibling modules (`notice_tag`): the
/// shape the team test fixture builds, without a checkout on disk.
#[cfg(test)]
pub(crate) fn test_worker_record(id: &str) -> WorkerRecord {
    tests::worker(std::path::Path::new("."), id)
}

#[cfg(test)]
#[path = "team/tests.rs"]
mod tests;
