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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use mnemo::app::plan_board::{BoardPlan, BoardStatus, BoardStep, PlanBoard};
use mnemo::route_question;
use mnemo::headless::{
    Engine, SessionEvent, SessionId, Transcript,
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

mod agent_log;
mod agent_watch;
mod audit;
mod check_run;
pub mod claude_node;
mod control;
mod deletion;
mod dispatch;
mod escalation;
mod external_lead;
pub(crate) mod findings_ledger;
mod guardrails;
mod integrate;
mod lead_tools;
mod merge;
mod nudge;
mod persistence;
mod placement;
// Public: the proof engine's contract is part of the crate's API, so a step
// that adds it before the team calls it does not trip the dead-code lint.
pub mod proof;
mod proving;
mod pump;
mod record;
// Public, like `proof`: the remote seam is the contract steps 5-7 build on, so
// a step that adds it before the team calls every method does not trip the
// dead-code lint.
pub mod remote;
mod review;
mod routing;
mod spawn;
// Public for the same reason: the worker-session seam is the team's contract.
pub mod worker_session;

use control::recover_worker;
use deletion::schedule_trash_sweep;
use external_lead::ExternalLead;
use merge::{
    RECOVERED_WORKTREE_CLEANUP_ERROR, cleanup_agent_worktree, merge_conflicts_only,
    merge_record_from_git,
};
use persistence::TeamPersistence;
use pump::{close_worker_session, save_workers_now, save_workers_soon};
use remote::{RemoteWorkers, ShadowSync};
use worker_session::SharedWorkerSession;

pub use agent_watch::{AgentGateView, AgentGateWorker};
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
    ChangeSource, HandedBack, InboxEntry, InboxKind, MessageSender, RemoteCheckout, ToolErrorNote,
    WorkerCheck, WorkerRecord, WorkerRouting, WorkerStatus,
};
pub use remote::{HostDirectory, PairOffer};

/// The Tauri event channel emitted by the team runtime.
pub const TEAM_EVENT_CHANNEL: &str = "mnemo2://team-event";

/// The Tauri event channel streaming per-worker deletion-flow progress
/// (`DeletionProgress`) while `team_delete_preview` or `team_delete` runs.
pub const DELETION_EVENT_CHANNEL: &str = "mnemo2://worker-deletion";

/// How many transient team events the broadcast bus keeps for a slow consumer.
const EVENT_BUFFER: usize = 1024;

/// The default number of concurrently live worker sessions.
pub const DEFAULT_SLOTS: usize = 3;

/// How often the host monitor re-reads the node links when no change woke it.
const HOST_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// How often the host monitor reconciles sessions, pools and refs with a node.
const HOST_RECONCILE: std::time::Duration = std::time::Duration::from_secs(60);

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

/// The next per-worker transcript revision.
///
/// A revision names one immutable transcript value: a worker always replaces
/// its transcript with a fresh `Arc`, so a cached revision is reused only
/// while the pointer still matches. The counter is process-global and never
/// reset, so revisions stay unique across reopened runtimes.
static TRANSCRIPT_REVS: AtomicU64 = AtomicU64::new(1);

/// Each worker's current transcript value with the revision that describes it,
/// keyed by worker id. A sync reuses the revision only while the cached `Arc`
/// is the same allocation as the worker's transcript, so a replaced transcript
/// is detected by pointer identity alone.
type TranscriptRevisionCache = HashMap<String, (Arc<Transcript>, u64)>;

/// The team's public snapshot. Each serialized worker also carries its
/// derived `agent_phase` (see [`TeamSnapshot::agent_phases`]).
#[derive(Clone, Debug)]
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
    /// Each worker's transcript revision; it changes whenever that worker's
    /// transcript is replaced. The window sends back the revisions it holds.
    pub transcript_revs: HashMap<String, u64>,
    /// Workers whose transcript was left out (sent empty) because the caller
    /// already holds that revision.
    pub omitted_transcripts: Vec<String>,
    /// What Mnemo knows about each registered Claude Code agent, by worker
    /// id (the `agent` object of `team_status`). Such a worker's transcript
    /// above is the agent's own log, converted.
    pub agents: HashMap<String, serde_json::Value>,
    /// Each Claude Code agent worker's phase, by worker id: `awaiting_launch`,
    /// `running`, `stalled` or `finished`. Serialized onto the worker as
    /// `agent_phase`, never onto the record, so it never reaches team.json.
    pub agent_phases: HashMap<String, &'static str>,
}

/// One worker as the snapshot sends it: the record and its derived phase.
#[derive(Serialize)]
struct SnapshotWorker<'a> {
    #[serde(flatten)]
    record: &'a WorkerRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_phase: Option<&'static str>,
}

impl Serialize for TeamSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            workers: Vec<SnapshotWorker<'a>>,
            slots: usize,
            active: usize,
            usage: &'a ProjectUsageView,
            transcript_revs: &'a HashMap<String, u64>,
            #[serde(skip_serializing_if = "<[String]>::is_empty")]
            omitted_transcripts: &'a [String],
            #[serde(skip_serializing_if = "HashMap::is_empty")]
            agents: &'a HashMap<String, serde_json::Value>,
        }
        Wire {
            workers: self
                .workers
                .iter()
                .map(|record| SnapshotWorker {
                    record,
                    agent_phase: self.agent_phases.get(&record.id).copied(),
                })
                .collect(),
            slots: self.slots,
            active: self.active,
            usage: &self.usage,
            transcript_revs: &self.transcript_revs,
            omitted_transcripts: &self.omitted_transcripts,
            agents: &self.agents,
        }
        .serialize(serializer)
    }
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
    session: SharedWorkerSession,
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

/// A commit sha as a person reads it: the first seven characters.
fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
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
    /// The lead's shared engine, borrowed until [`TeamRuntime::shutdown`]
    /// releases it: a shut-down team may still be referenced (by the MCP
    /// host, a pending tool call) and must not keep the engine — and with it
    /// the project's SQLite stores — alive.
    engine: std::sync::RwLock<Option<Arc<Engine>>>,
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
    merge_lock: Arc<Mutex<()>>,
    /// Serializes pool selection through insertion of the new worker record.
    checkout_pool_lock: Mutex<()>,
    pricing: Arc<std::sync::RwLock<Vec<mnemo::config::PricingEntry>>>,
    config_dir: PathBuf,
    /// The dispatch-routing settings, shared with every worker pump so a
    /// failure's `outcome` row follows the mode in force when it happens.
    routing: Arc<std::sync::RwLock<RoutingSettings>>,
    /// The models a provider usage limit took out, with the time each serves
    /// again, and the runtime's own handle (plan e5d93df3 decision 8). Shared
    /// with every worker pump, which marks the model a usage-limited worker
    /// ran on and, in routing mode auto, resumes that worker. In memory only.
    usage_limits: routing::UsageLimits,
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
    /// The transcript revision handed to each worker's current transcript
    /// value, cached beside the `Arc` it describes so a sync can tell which
    /// transcripts the window already holds.
    transcript_revs: Arc<std::sync::Mutex<TranscriptRevisionCache>>,
    board: std::sync::Mutex<Option<Arc<PlanBoard>>>,
    /// The workers whose proofs are running. Every entry point that would move
    /// a proved checkout refuses while the worker's id is in here.
    proving: proving::Proving,
    /// The workers (or the lead checkout) whose Mnemo-run check runs are in
    /// progress; the same entry points refuse them as refuse a proof.
    check_runs: check_run::CheckRuns,
    /// The registered Claude Code agents' transcript readings and their
    /// watcher task.
    agent_watch: agent_watch::AgentWatch,
    /// The two proof commands, behind a seam a test can answer without a
    /// twenty-minute build.
    proofs: std::sync::RwLock<Arc<dyn proving::ProofRunner>>,
    /// What this session already mutated and already reported: the tip each
    /// worker's last automatic mutation run reached the runner at, and the
    /// missing mutation tools it named (decision 2026-11-19). In memory only.
    mutation_memory: Arc<std::sync::Mutex<proving::MutationMemory>>,
    signals: broadcast::Sender<BoardSignal>,
    /// The remote-worker seam: host lookup, the per-node event demux, the git
    /// bundle round trips, node-side checks and host suitability. With no
    /// [`HostDirectory`] attached every worker is local.
    remote: Arc<RemoteWorkers>,
    /// The app's host directory once [`TeamRuntime::attach_hosts`] ran; the
    /// `team_hosts` tool pairs, lists, and revokes hosts through it.
    hosts: std::sync::RwLock<Option<Arc<dyn HostDirectory>>>,
    /// The external lead's attach flag, notice queue and activity clock.
    external: std::sync::Mutex<ExternalLead>,
    /// Wakes [`TeamRuntime::wait_external_notices`] on a push or a detach.
    external_wake: tokio::sync::Notify,
}

/// Upper bound for concurrently live worker sessions in one project.
pub const MAX_WORKER_SLOTS: usize = 32;

impl TeamRuntime {
    /// Every checkout currently held by a worker record.
    async fn held_checkouts(&self) -> Vec<PathBuf> {
        self.records
            .lock()
            .await
            .iter()
            .filter(|worker| !worker.worktree.as_os_str().is_empty())
            .map(|worker| worker.worktree.clone())
            .collect()
    }

    /// Give back the pool checkouts no worker needs any more (plan 4d920eec
    /// decision 8), so the next dispatch reuses them warm instead of making a
    /// cold pool: an audit that has delivered its report (it is locked and
    /// never builds again), and a Stopped or Failed worker whose plan step is
    /// already done on the board. Each is released the way a merge releases
    /// a pool (detached, build caches kept, the record's worktree cleared);
    /// one with a live session or uncommitted work keeps its pool. Runs
    /// right before a pool is handed out, so no background job is needed.
    pub(super) async fn release_idle_checkouts(&self) {
        let board = self.board();
        let step_done = |worker: &WorkerRecord| -> bool {
            let (Some(board), Some(plan_id), Some(step)) =
                (board.as_ref(), worker.plan_id.as_deref(), worker.plan_step)
            else {
                return false;
            };
            board
                .get(plan_id)
                .ok()
                .and_then(|plan| plan.steps.get(step).map(|board_step| board_step.done))
                .unwrap_or(false)
        };
        let candidates: Vec<(String, PathBuf, String)> = self
            .records
            .lock()
            .await
            .iter()
            .filter(|worker| mnemo::app::worktrees::is_pool_checkout(&worker.worktree))
            .filter(|worker| {
                (worker.kind == "audit" && worker.report_delivered)
                    || (matches!(worker.status, WorkerStatus::Stopped | WorkerStatus::Failed)
                        && step_done(worker))
            })
            .map(|worker| (worker.id.clone(), worker.worktree.clone(), worker.branch.clone()))
            .collect();
        let mut released = false;
        for (worker_id, checkout, branch) in candidates {
            if self.live.lock().await.contains_key(&worker_id) {
                continue;
            }
            match mnemo::app::worktrees::release_pool_checkout(
                self.workspace_root.clone(),
                checkout.clone(),
                branch,
            )
            .await
            {
                Ok(()) => {
                    self.update_worker(&worker_id, |worker| worker.worktree = PathBuf::new())
                        .await;
                    released = true;
                }
                Err(error) => eprintln!(
                    "team: kept the pool checkout {} of {worker_id}: {error}",
                    checkout.display()
                ),
            }
        }
        if released {
            if let Err(error) = self.persist().await {
                eprintln!("team: could not save the released pool checkouts: {error}");
            }
        }
    }

    /// Remove the FREE pool checkouts whose index is above the worker slot
    /// count (plan 2e15085a): the pools stop accumulating above the slots, and
    /// at rest the pool count is at most the slot count. The cap is SOFT —
    /// dispatch never waits for a trim, and a pool above the slot count may
    /// still be created while the lower ones are held. A pool is kept when a
    /// worker record holds it, when it is on a branch or has uncommitted work,
    /// when another instance claims it, and always when its index is at or
    /// below the slot count (those are the warm caches). The removal itself is
    /// cheap: the pool folder is renamed to a `trash-pool-*` name under
    /// `.worktrees` and its 10-15 GB of caches are deleted on a DETACHED thread
    /// (see `trim_pool_checkouts`), so a trim never deletes them while the
    /// caller holds the merge lock.
    ///
    /// Takes the checkout pool lock itself, so a caller that holds the merge
    /// lock may call it (merge and trim both lock `merge_lock ->
    /// checkout_pool_lock`); NEVER call it while the checkout pool lock is
    /// already held. Reads the held checkouts after taking the lock and never
    /// fails its caller: a listing or removal problem goes to stderr.
    pub(super) async fn trim_idle_pools(&self) {
        let _pools = self.checkout_pool_lock.lock().await;
        let keep = self.persistence.slots.load(Ordering::SeqCst);
        let held = self.held_checkouts().await;
        match mnemo::app::worktrees::trim_pool_checkouts(
            self.workspace_root.clone(),
            keep as u32,
            held,
        )
        .await
        {
            Ok(removed) => {
                for pool in removed {
                    eprintln!(
                        "team: removed idle pool checkout {} above the {keep} slots",
                        pool.display()
                    );
                }
            }
            Err(error) => eprintln!("team: could not trim the idle pool checkouts: {error}"),
        }
    }

    /// Every checkout held by a record other than `worker_id`.
    async fn other_held_checkouts(&self, worker_id: &str) -> Vec<PathBuf> {
        self.records
            .lock()
            .await
            .iter()
            .filter(|worker| worker.id != worker_id && !worker.worktree.as_os_str().is_empty())
            .map(|worker| worker.worktree.clone())
            .collect()
    }

    async fn close_session_for_cleanup(&self, session: &SharedWorkerSession) -> Result<(), String> {
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
        // Mnemo2 starts with no external lead, so the hand-back clock starts
        // now (plan ee73bd2f decision 1): an agent worker whose agent was
        // never launched, or has stalled, goes back to Mnemo only once the
        // grace period has passed with no lead attached. Recovery keeps
        // agent workers at work, and the agent watch follows them.
        let agent_watch = agent_watch::AgentWatch::default();
        let held: Vec<(String, PathBuf)> = workers
            .iter()
            .filter(|worker| !worker.worktree.as_os_str().is_empty())
            .map(|worker| (worker.id.clone(), worker.worktree.clone()))
            .collect();
        for worker in &mut workers {
            let other_worktrees: Vec<PathBuf> = held
                .iter()
                .filter(|(id, _)| id != &worker.id)
                .map(|(_, checkout)| checkout.clone())
                .collect();
            recover_worker(
                &options.workspace_root,
                worker,
                &other_worktrees,
                &options.state_dir,
            )
            .await?;
        }
        // A proof a hard kill interrupted leaves its checkout reverse-applied;
        // the claim marker names the tip to repair it back to (finding
        // restartWhileProving). Safe before anything else runs: no proof is
        // live at open.
        proving::repair_interrupted_proofs(&options.workspace_root, &workers).await;
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let (signals, _) = broadcast::channel(EVENT_BUFFER);
        let routing_log_path = options.routing_log_path.clone().unwrap_or_else(
            crate::mnemo2::dispatch_routing::dispatch_routing_log_path,
        );
        // The kNN overlay shares the app's live embedder (never a second
        // backend). With no own memory store there is nothing to embed with,
        // so the overlay is absent and Laya decides every dispatch.
        let route_knn = options.engine.embedder_handle().map(|initial| {
            // A back-reference only: the team's own handle owns the engine.
            let engine = Arc::downgrade(&options.engine);
            let getter: EmbedderGetter = Arc::new(move || {
                engine
                    .upgrade()
                    .and_then(|engine| engine.embedder_handle())
                    .unwrap_or_else(|| initial.clone())
            });
            Arc::new(RouteKnn::new(getter, routing_log_path.clone()))
        });
        let workspace_root = options.workspace_root;
        let records = Arc::new(Mutex::new(workers));
        let live: Arc<Mutex<HashMap<String, LiveWorker>>> = Arc::new(Mutex::new(HashMap::new()));
        // Remoting is off until a host directory is attached: the seam exists,
        // it just never reaches a node.
        let remote = Arc::new(RemoteWorkers::new(
            workspace_root.clone(),
            options.state_dir.clone(),
            records.clone(),
            live.clone(),
        ));
        let runtime = Self {
            engine: std::sync::RwLock::new(Some(options.engine)),
            workspace_root,
            records,
            live,
            messages: Arc::new(Mutex::new(Vec::new())),
            events,
            generation: Arc::new(AtomicU64::new(0)),
            next_worker: AtomicU64::new(1),
            lead_busy: AtomicBool::new(false),
            fail_next_close: Arc::new(AtomicBool::new(false)),
            lead_session: Mutex::new(None),
            merge_lock: Arc::new(Mutex::new(())),
            checkout_pool_lock: Mutex::new(()),
            pricing: Arc::new(std::sync::RwLock::new(options.pricing)),
            config_dir: config_dir.clone(),
            routing: Arc::new(std::sync::RwLock::new(load_routing_settings(&config_dir))),
            usage_limits: routing::UsageLimits::default(),
            classifier: options.classifier,
            route_knn,
            routing_permit: Arc::new(tokio::sync::Semaphore::new(1)),
            routing_log_path,
            models: Arc::new(std::sync::RwLock::new(options.models)),
            tiers: Arc::new(std::sync::RwLock::new(mnemo::headless::TiersDto::default())),
            persistence,
            usage: Arc::new(Mutex::new(usage)),
            save_lock: Arc::new(Mutex::new(())),
            transcript_revs: Arc::new(std::sync::Mutex::new(HashMap::new())),
            board: std::sync::Mutex::new(None),
            proving: proving::Proving::default(),
            check_runs: check_run::CheckRuns::default(),
            agent_watch,
            proofs: std::sync::RwLock::new(Arc::new(proving::RealProofs::new(config_dir.clone()))),
            mutation_memory: Arc::new(std::sync::Mutex::new(proving::MutationMemory::default())),
            signals,
            remote,
            hosts: std::sync::RwLock::new(None),
            external: std::sync::Mutex::new(ExternalLead::away_since(unix_ms())),
            external_wake: tokio::sync::Notify::new(),
        };
        runtime.persist().await?;
        // A grace of 0 hands back at once, as before the grace existed; the
        // in-app lead (or the next external lead) learns of each one.
        if let Err(error) = runtime.hand_back_agent_workers().await {
            eprintln!("team: could not hand back agent workers at start: {error}");
        }
        // App start: sweep checkouts a previous run renamed into the
        // trash folder but could not remove yet (something held them).
        schedule_trash_sweep(&runtime.workspace_root);
        // App start: a checkout a previous run could not remove (something
        // held it) is retried now, and the branch it was blocking goes with it.
        runtime.retry_leftover_cleanups().await;
        // Recovery released the pools of the workers it closed: drop the free
        // pools above the slot count, so a started app converges on the slots
        // (plan 2e15085a).
        runtime.trim_idle_pools().await;
        // An agent worker recovery kept at work is watched again: its
        // finish is processed, and once no lead has been attached for the
        // grace period an unlaunched or stalled one is handed back.
        runtime.ensure_agent_watch().await;
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

    /// The folder the team's ledger, transcripts and archived screenshots
    /// live in.
    pub(crate) fn state_dir(&self) -> &Path {
        self.persistence
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."))
    }

    /// Tell subscribers the board may have changed.
    pub(crate) fn signal_board_changed(&self) {
        let _ = self.signals.send(BoardSignal::Changed);
    }

    fn notify_lead(&self, key: String, text: String) {
        let _ = self.signals.send(BoardSignal::Notice { key, text });
    }

    /// Attach the app's host directory: from here on a dispatch may place a
    /// Mnemo-model worker on a paired node, and the host monitor runs.
    ///
    /// Remoting stays off until this is called with a directory that has a
    /// running hub, so an app whose user never paired a node behaves exactly as
    /// before.
    pub fn attach_hosts(self: &Arc<Self>, hosts: Arc<dyn HostDirectory>) {
        *self
            .hosts
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(hosts.clone());
        self.remote.attach(hosts.clone());
        let team = Arc::downgrade(self);
        tokio::spawn(async move {
            // The app attaches its directory before the hub's listener exists
            // (the first pairing starts it), so wait for a server rather than
            // giving up.
            let mut links: Option<tokio::sync::watch::Receiver<u64>> = None;
            let mut last_reconcile = std::time::Instant::now();
            loop {
                // A dropped team ends the monitor, also while it is still
                // waiting for the hub to appear.
                if team.upgrade().is_none() {
                    return;
                }
                if links.is_none() {
                    let Some(server) = hosts.server() else {
                        tokio::time::sleep(HOST_POLL).await;
                        continue;
                    };
                    links = Some(server.link_changes());
                    let Some(runtime) = team.upgrade() else {
                        return;
                    };
                    runtime.reconcile_hosts().await;
                    last_reconcile = std::time::Instant::now();
                    continue;
                }
                let Some(link) = links.as_mut() else {
                    continue;
                };
                tokio::select! {
                    changed = link.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    _ = tokio::time::sleep(HOST_POLL) => {}
                }
                let Some(runtime) = team.upgrade() else {
                    return;
                };
                runtime.poll_hosts().await;
                if last_reconcile.elapsed() >= HOST_RECONCILE {
                    runtime.reconcile_hosts().await;
                    last_reconcile = std::time::Instant::now();
                }
            }
        });
    }

    /// Bring the hub shadow checkout of `worker_id` up to the worker's latest
    /// tip. A local worker answers its worktree tip; remoting is not involved.
    pub async fn sync_remote(&self, worker_id: &str) -> Result<ShadowSync, String> {
        let record = {
            let records = self.records.lock().await;
            records
                .iter()
                .find(|worker| worker.id == worker_id)
                .cloned()
        }
        .ok_or_else(|| format!("unknown worker '{worker_id}'"))?;
        let sync = self.remote.sync_shadow(&record, true).await?;
        if record.remote.is_some() {
            if let Err(error) = self.persist().await {
                eprintln!("team: could not save the synced tip of {worker_id}: {error}");
            }
        }
        Ok(sync)
    }

    /// Notice a host going away, coming back, or staying away past the outage
    /// budget. Runs on the host monitor; one pass never holds a team lock
    /// across a link call.
    async fn poll_hosts(&self) {
        let Some(server) = self.remote.server() else {
            return;
        };
        let connected: HashSet<String> = server.connected().into_iter().collect();
        // One lock at a time: the delivery path holds `live` while it takes
        // `records`, so nesting them here in the other order would deadlock.
        let sessions: Vec<(String, SharedWorkerSession)> = {
            let live = self.live.lock().await;
            live.iter()
                .map(|(id, worker)| (id.clone(), worker.session.clone()))
                .collect()
        };
        let watching: Vec<(String, RemoteCheckout, SharedWorkerSession)> = {
            let records = self.records.lock().await;
            sessions
                .into_iter()
                .filter_map(|(id, session)| {
                    let record = records.iter().find(|record| record.id == id)?;
                    // A remote agent worker is followed by the second pass
                    // below: it is never failed for a lost link.
                    if agent_watch::runs_as_agent(record) {
                        return None;
                    }
                    let remote = record.remote.clone()?;
                    Some((id, remote, session))
                })
                .collect()
        };
        // Remote Claude Code agent workers hold no Mnemo session: copy their
        // checkouts under the records lock alone, then release it.
        let agents: Vec<(String, RemoteCheckout)> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|record| record.claude_agent.is_some() && agent_watch::runs_as_agent(record))
                .filter_map(|record| Some((record.id.clone(), record.remote.clone()?)))
                .collect()
        };
        let budget_ms = u64::try_from(self.remote.outage_budget().as_millis()).unwrap_or(u64::MAX);
        let now = unix_ms();
        for (id, remote) in agents {
            // The session runs over SSH and does not depend on the node link:
            // a lost link only marks the worker offline, and only a revoke
            // fails it.
            let linked = connected.contains(&remote.node_id);
            match remote.offline_since_ms {
                None if !linked => self.mark_host_offline(&id, &remote, true).await,
                Some(_) if linked => self.mark_host_back(&id, &remote).await,
                _ => {}
            }
        }
        for (id, remote, session) in watching {
            if !connected.contains(&remote.node_id) {
                match remote.offline_since_ms {
                    None => self.mark_host_offline(&id, &remote, false).await,
                    Some(since) if now.saturating_sub(since) > budget_ms => {
                        self.fail_lost_host(&id, &remote).await;
                    }
                    Some(_) => {}
                }
                continue;
            }
            if remote.offline_since_ms.is_none() {
                continue;
            }
            let node = server.node(&remote.node_id);
            match node.list_sessions().await {
                Ok(sessions) if sessions.iter().any(|info| info.session == session.id()) => {
                    self.mark_host_back(&id, &remote).await;
                }
                Ok(_) => self.mark_host_restarted(&id, &remote).await,
                // A flapping link: the next pass decides.
                Err(_) => {}
            }
        }
    }

    /// Mark a live remote worker's host as away and tell the lead. `agent` is a
    /// remote Claude Code agent worker, whose session keeps working over SSH.
    async fn mark_host_offline(&self, worker_id: &str, remote: &RemoteCheckout, agent: bool) {
        {
            let mut records = self.records.lock().await;
            let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) else {
                return;
            };
            let Some(state) = worker.remote.as_mut() else {
                return;
            };
            if state.offline_since_ms.is_some() {
                return;
            }
            state.offline_since_ms = Some(unix_ms());
        }
        if let Err(error) = self.persist().await {
            eprintln!("team: could not save the lost host of {worker_id}: {error}");
        }
        let text = if agent {
            format!(
                "Worker {worker_id} lost its host {}; its Claude Code session there may keep working. Mnemo follows it again when the host is back.",
                remote.node_name
            )
        } else {
            let budget = self.remote.outage_budget().as_secs();
            format!(
                "Worker {worker_id} lost its host {}; it continues when the host is back (up to {budget} s).",
                remote.node_name
            )
        };
        self.notify_lead(format!("host-offline:{worker_id}"), text);
    }

    /// A host that came back within the outage budget: clear the mark and tell
    /// the lead the worker continues there.
    async fn mark_host_back(&self, worker_id: &str, remote: &RemoteCheckout) {
        {
            let mut records = self.records.lock().await;
            let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) else {
                return;
            };
            let Some(state) = worker.remote.as_mut() else {
                return;
            };
            if state.offline_since_ms.take().is_none() {
                return;
            }
        }
        if let Err(error) = self.persist().await {
            eprintln!("team: could not save the returned host of {worker_id}: {error}");
        }
        self.notify_lead(
            format!("host-back:{worker_id}"),
            format!(
                "Host {} is back; worker {worker_id} continues.",
                remote.node_name
            ),
        );
    }

    /// A host that came back after its node process restarted: the session is
    /// gone, and `team_resume` starts it there again.
    async fn mark_host_restarted(&self, worker_id: &str, remote: &RemoteCheckout) {
        let active;
        {
            let mut records = self.records.lock().await;
            let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) else {
                return;
            };
            // Only a worker that was running a turn is interrupted; any other
            // worker (finished, in review, merged, stopped, already failed or
            // interrupted) keeps its status and error. The lost session is
            // dropped below either way.
            active = is_active(worker.status);
            if active {
                worker.status = WorkerStatus::Interrupted;
                worker.error = Some(format!(
                    "host '{}' restarted; its session ended. team_resume continues it there.",
                    remote.node_name
                ));
            }
            if let Some(state) = worker.remote.as_mut() {
                state.offline_since_ms = None;
            }
        }
        {
            // The session is gone with the node process: end its pump and free
            // the host slot now, as a lost host does.
            let mut live = self.live.lock().await;
            if let Some(worker) = live.remove(worker_id) {
                worker.pump.abort();
            }
        }
        if let Err(error) = self.persist().await {
            eprintln!("team: could not save the restarted host of {worker_id}: {error}");
        }
        if active {
            self.notify_lead(
                format!("host-back:{worker_id}"),
                format!(
                    "Host {} is back; worker {worker_id} continues.",
                    remote.node_name
                ),
            );
        }
        self.signal_board_changed();
    }

    /// A host that stayed away past the outage budget: the session there is
    /// lost, and the worker fails with a resumable error naming the host.
    async fn fail_lost_host(&self, worker_id: &str, remote: &RemoteCheckout) {
        let message = {
            let mut records = self.records.lock().await;
            let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) else {
                return;
            };
            // Only a worker that was running a turn fails; any other worker
            // keeps its status and error (the lost session is dropped below).
            if !is_active(worker.status) {
                None
            } else {
                let budget = self.remote.outage_budget().as_secs();
                let name = &remote.node_name;
                let base = worker
                    .base_commit
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("");
                let from = match worker
                    .remote
                    .as_ref()
                    .and_then(|state| state.synced_tip.as_deref())
                {
                    Some(tip) => format!(
                        " from its last synced commit {}",
                        short_sha(tip)
                    ),
                    None if !base.is_empty() => format!(
                        " from its starting commit {} (nothing was synced from {name})",
                        short_sha(base)
                    ),
                    None => String::new(),
                };
                let text = format!(
                    "host '{name}' has been offline for more than {budget} s; its session there is lost. team_resume continues worker '{worker_id}' on {name} once the host is back, or here with host \"local\"{from}."
                );
                worker.status = WorkerStatus::Failed;
                worker.error = Some(text.clone());
                Some(text)
            }
        };
        {
            let mut live = self.live.lock().await;
            if let Some(worker) = live.remove(worker_id) {
                worker.pump.abort();
            }
        }
        if let Err(error) = self.persist().await {
            eprintln!("team: could not save the failed worker {worker_id}: {error}");
        }
        if let Some(message) = message {
            self.notify_lead(
                format!("host-lost:{worker_id}"),
                format!(
                    "Host {} stayed away too long; worker {worker_id} failed. {message}",
                    remote.node_name
                ),
            );
        }
        self.signal_board_changed();
    }

    /// Fail every running worker of a host the lead just revoked.
    ///
    /// The session on that host is gone for good, but the record keeps its
    /// remote checkout so `team_resume` with `host "local"` can continue the
    /// worker from the hub's shadow checkout. A running remote agent worker
    /// fails too: its Claude Code session may still work there, so its error
    /// asks the lead to stop it first. Returns the failed worker ids.
    pub(super) async fn fail_revoked_host(&self, node_id: &str, name: &str) -> Vec<String> {
        let running: Vec<String> = {
            let live = self.live.lock().await;
            live.keys().cloned().collect()
        };
        let mut failed = Vec::new();
        let mut lost = Vec::new();
        {
            let mut records = self.records.lock().await;
            for worker in records.iter_mut() {
                let agent = worker.claude_agent.is_some() && agent_watch::runs_as_agent(worker);
                // A remote agent worker holds no Mnemo session, so it is not in
                // the live set; every other worker is only touched when running.
                if !agent && !running.contains(&worker.id) {
                    continue;
                }
                let Some(remote) = worker.remote.as_ref() else {
                    continue;
                };
                if remote.node_id != node_id {
                    continue;
                }
                // Only a worker that was running a turn fails; any other
                // worker keeps its status and error, and its lost session is
                // dropped below all the same.
                if !agent && !is_active(worker.status) {
                    lost.push(worker.id.clone());
                    continue;
                }
                let from = match remote.synced_tip.as_deref() {
                    Some(tip) => format!(" from its last synced commit {}", short_sha(tip)),
                    None => worker
                        .base_commit
                        .as_deref()
                        .map(str::trim)
                        .filter(|base| !base.is_empty())
                        .map(|base| {
                            format!(
                                " from its starting commit {} (nothing was synced from {name})",
                                short_sha(base)
                            )
                        })
                        .unwrap_or_default(),
                };
                worker.status = WorkerStatus::Failed;
                worker.error = Some(if agent {
                    format!(
                        "host '{name}' was revoked; Mnemo can no longer follow worker '{}'s Claude Code session there. Stop that session (SendMessage it to stop, or end it in the app), then team_resume continues the worker here with host \"local\"{from}.",
                        worker.id
                    )
                } else {
                    format!(
                        "host '{name}' was revoked; its session there is lost. team_resume continues worker '{}' here with host \"local\"{from}.",
                        worker.id
                    )
                });
                failed.push(worker.id.clone());
            }
        }
        if failed.is_empty() && lost.is_empty() {
            return failed;
        }
        {
            let mut live = self.live.lock().await;
            for id in failed.iter().chain(lost.iter()) {
                if let Some(worker) = live.remove(id) {
                    worker.pump.abort();
                }
            }
        }
        if let Err(error) = self.persist().await {
            eprintln!("team: could not save the workers of the revoked host {name}: {error}");
        }
        self.signal_board_changed();
        failed
    }

    /// Re-align the node side with the hub: close sessions the hub does not
    /// know, release pools no record holds, drop the refs of deleted workers,
    /// and start a qualification on every connected node that has none.
    ///
    /// A remote agent worker has no Mnemo session on its node, but its record
    /// names its checkout, so that checkout counts as referenced and is kept.
    async fn reconcile_hosts(&self) {
        let Some(server) = self.remote.server() else {
            return;
        };
        let project = self.remote.project().to_string();
        let nodes = server.nodes().list().unwrap_or_default();
        // One lock at a time (see `poll_hosts`): copy the live sessions, drop
        // `live`, then read the records.
        let live_ids: Vec<(String, mnemo::headless::SessionId)> = {
            let live = self.live.lock().await;
            live.iter()
                .map(|(id, worker)| (id.clone(), worker.session.id()))
                .collect()
        };
        let live_sessions: Vec<(String, mnemo::headless::SessionId)> = {
            let records = self.records.lock().await;
            live_ids
                .into_iter()
                .filter_map(|(id, session)| {
                    let record = records.iter().find(|record| record.id == id)?;
                    let remote = record.remote.as_ref()?;
                    Some((remote.node_id.clone(), session))
                })
                .collect()
        };
        let previously_orphaned = self.remote.orphans();
        let mut orphans_now: HashSet<(String, mnemo::headless::SessionId)> = HashSet::new();
        for node_record in nodes
            .iter()
            .filter(|node| server.is_connected(&node.node_id))
        {
            let node = server.node(&node_record.node_id);
            if let Ok(sessions) = node.list_sessions().await {
                for info in sessions {
                    let known = live_sessions
                        .iter()
                        .any(|(node_id, session)| node_id == &node_record.node_id && *session == info.session);
                    if known {
                        continue;
                    }
                    // A dispatch inserts its live entry only after the opening
                    // prompt, so a session seen for the first time may still be
                    // spawning: close it only when the previous pass saw it too.
                    let key = (node_record.node_id.clone(), info.session);
                    let seen_before = previously_orphaned.contains(&key);
                    orphans_now.insert(key);
                    if seen_before {
                        let _ = node.close(info.session).await;
                    }
                }
            }
            let referenced: HashSet<String> = {
                let records = self.records.lock().await;
                records
                    .iter()
                    .filter_map(|record| {
                        let remote = record.remote.as_ref()?;
                        (remote.node_id == node_record.node_id).then(|| remote.checkout.clone())
                    })
                    .collect()
            };
            let qualifying = self.remote.qualifying_checkout(&node_record.node_id);
            let listed = node
                .call(
                    mnemo::remote::repo::CHECKOUT_LIST,
                    serde_json::json!(mnemo::remote::repo::CheckoutList {
                        project: project.clone(),
                    }),
                    mnemo::remote::hub::CALL_TIMEOUT,
                )
                .await;
            match listed {
                Ok(value) => match serde_json::from_value::<
                    mnemo::remote::repo::CheckoutListAnswer,
                >(value)
                {
                    Ok(answer) => {
                        for info in answer.checkouts {
                            if referenced.contains(&info.checkout) {
                                continue;
                            }
                            if qualifying.as_deref() == Some(info.checkout.as_str()) {
                                continue;
                            }
                            // A detached pool is already released.
                            let Some(branch) = info.branch.as_deref() else {
                                continue;
                            };
                            if !branch.starts_with("wt/agent-") {
                                continue;
                            }
                            // A qualification holds (or is about to hold) its
                            // own hostcheck checkout: never release it under it.
                            if self
                                .remote
                                .qualification_protects(&node_record.node_id, branch)
                            {
                                continue;
                            }
                            // A dispatch is still preparing this checkout; its
                            // record only names it once `prepare_checkout`
                            // returned.
                            if self.remote.is_reserved(&node_record.node_id, branch) {
                                continue;
                            }
                            let released = node
                                .call(
                                    mnemo::remote::repo::CHECKOUT_RELEASE,
                                    serde_json::json!(mnemo::remote::repo::CheckoutRelease {
                                        checkout: info.checkout.clone(),
                                    }),
                                    mnemo::remote::hub::CALL_TIMEOUT,
                                )
                                .await;
                            if let Err(error) = released {
                                eprintln!(
                                    "team: could not release checkout {} on host '{}': {error}",
                                    info.checkout, node_record.name
                                );
                            }
                        }
                    }
                    Err(error) => eprintln!(
                        "team: bad checkout list from host '{}': {error}",
                        node_record.name
                    ),
                },
                Err(error) => eprintln!(
                    "team: could not list checkouts on host '{}': {error}",
                    node_record.name
                ),
            }
        }
        self.remote.replace_orphans(orphans_now);
        self.drop_stale_remote_refs().await;
        let commands = spawn::load_check_commands(&self.workspace_root);
        let hash = remote::checks_hash(&commands);
        for node_record in nodes
            .iter()
            .filter(|node| server.is_connected(&node.node_id))
        {
            if self.remote.qualification(&node_record.node_id, &hash)
                == remote::Qualification::Pending
            {
                self.remote.qualify(&node_record.node_id);
            }
        }
    }

    /// Delete the `refs/mnemo/remote/<id>` refs of workers that no longer
    /// exist.
    async fn drop_stale_remote_refs(&self) {
        let listing = match remote::git_value(
            &self.workspace_root,
            &["for-each-ref", "--format=%(refname)", "refs/mnemo/remote/"],
        )
        .await
        {
            Ok(text) => text,
            Err(_) => return,
        };
        if listing.is_empty() {
            return;
        }
        let known: HashSet<String> = self
            .records
            .lock()
            .await
            .iter()
            .map(|record| record.id.clone())
            .collect();
        for reference in listing.lines().map(str::trim).filter(|line| !line.is_empty()) {
            let Some(id) = reference.strip_prefix("refs/mnemo/remote/") else {
                continue;
            };
            if known.contains(id) {
                continue;
            }
            if let Err(error) = remote::git_value(
                &self.workspace_root,
                &["update-ref", "-d", reference],
            )
            .await
            {
                eprintln!("team: could not drop {reference}: {error}");
            }
        }
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
    pub fn attach_board(self: &Arc<Self>, board: Arc<PlanBoard>) {
        board.set_external_lead_attached(self.external_lead_attached());
        // The worker pumps resume a usage-limited worker through this handle;
        // a second attach finds it already set to this same runtime.
        let _ = self.usage_limits.team.set(Arc::downgrade(self));
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

    /// Everything a proof run reads from the runtime: the ledger handles it
    /// writes through, the plan board it reads the plan from, the claim set,
    /// and the runner.
    pub(super) fn proof_ctx(&self) -> proving::ProofCtx {
        proving::ProofCtx {
            workspace_root: self.workspace_root.clone(),
            records: self.records.clone(),
            usage: self.usage.clone(),
            save_lock: self.save_lock.clone(),
            persistence: self.persistence.clone(),
            signals: self.signals.clone(),
            proving: self.proving.clone(),
            check_runs: self.check_runs.clone(),
            board: self.board(),
            runner: Arc::clone(&*self.proofs.read().unwrap_or_else(|e| e.into_inner())),
            mutation_memory: self.mutation_memory.clone(),
            merge_lock: self.merge_lock.clone(),
            live: self.live.clone(),
            remote: self.remote.clone(),
        }
    }

    /// The refusal an entry point returns while a proof, or Mnemo's own check
    /// run, owns the worker's checkout. Every path that would move that checkout, or start a turn in
    /// it, must ask this **after** it takes the merge lock (finding
    /// claimRace): a claim holds the same lock while it decides, so the check
    /// and the claim are serialized and one of them always sees the other.
    pub(super) fn proving_guard(&self, worker_id: &str) -> Result<(), String> {
        if self.proving.is_active(worker_id) {
            return Err(proving::proving_error(worker_id));
        }
        if self.check_runs.is_active(worker_id) {
            return Err(check_run::check_run_error(worker_id));
        }
        Ok(())
    }

    /// Replace the proof runner. Tests only: a scripted outcome must not need a
    /// real build.
    #[cfg(test)]
    pub(crate) fn set_proof_runner_for_test(&self, runner: Arc<dyn proving::ProofRunner>) {
        *self.proofs.write().unwrap_or_else(|e| e.into_inner()) = runner;
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
        let plans = tokio::task::spawn_blocking({
            let board = board.clone();
            move || board.list()
        })
        .await
        .map_err(|err| format!("the plan board read failed: {err}"))??;
        // Only what the Plan tab stamps on a step is taken under the records
        // lock: a full worker record is large and nothing else here needs it.
        let workers: Vec<(Option<String> /*plan_id*/, Option<usize> /*plan_step*/, BoardWorker)> = {
            let records = self.records.lock().await;
            records
                .iter()
                .map(|w| {
                    (
                        w.plan_id.clone(),
                        w.plan_step,
                        BoardWorker {
                            id: w.id.clone(),
                            name: w.name.clone(),
                            status: w.status,
                        },
                    )
                })
                .collect()
        };
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
                            .filter(|(plan_id, plan_step, _)| {
                                plan_id.as_deref() == Some(plan.id.as_str())
                                    && *plan_step == step.number.checked_sub(1)
                            })
                            .map(|(_, _, worker)| worker)
                            .cloned()
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

    /// Active workers for every slot gate: live local Mnemo sessions plus
    /// session-free local Claude Code agents in Working or Interrupted.
    ///
    /// A remote worker does not use a local pool slot: its host has its own
    /// slots (`mnemo2-node run --slots`), so it is counted there, not here — a
    /// remote agent worker's session-free record included (plan 5e64ccd5).
    pub(super) async fn active_workers_for_slots(&self) -> usize {
        let active_agents = self
            .records
            .lock()
            .await
            .iter()
            .filter(|worker| {
                worker.claude_agent.is_some()
                    && worker.remote.is_none()
                    && matches!(
                        worker.status,
                        WorkerStatus::Working | WorkerStatus::Fixing | WorkerStatus::Interrupted
                    )
            })
            .count();
        // One lock at a time: copy the live ids, drop `live`, then read the
        // records (the delivery path nests them live-then-records).
        let live_ids: Vec<String> = self.live.lock().await.keys().cloned().collect();
        let local = {
            let records = self.records.lock().await;
            live_ids
                .iter()
                .filter(|id| {
                    records
                        .iter()
                        .any(|worker| &worker.id == *id && worker.remote.is_none())
                })
                .count()
        };
        local + active_agents
    }

    /// Persist a new live-session cap without dropping active workers.
    pub async fn set_slots(&self, slots: usize) -> Result<TeamSnapshot, String> {
        if !(1..=MAX_WORKER_SLOTS).contains(&slots) {
            return Err(format!(
                "worker slots must be between 1 and {MAX_WORKER_SLOTS}"
            ));
        }
        let _operation = self.merge_lock.lock().await;
        let active = self.active_workers_for_slots().await;
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
        // The lower cap is persisted: the free pools above it go now.
        self.trim_idle_pools().await;
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
        // The workers whose proofs are running right now (finding
        // cleanupDuringProof), read under this cleanup's merge lock: a
        // claim holds the same lock, so no proof can start while the list
        // is built or used.
        let proving: Vec<String> = {
            let records = self.records.lock().await;
            records
                .iter()
                .filter(|worker| self.proving.is_active(&worker.id))
                .map(|worker| worker.id.clone())
                .collect()
        };
        let report = cleanup_workspace(
            &self.workspace_root,
            &state_dir,
            &self.routing_log_path,
            &proving,
        )
        .await?;
        let restored = self.persistence.load()?.unwrap_or_default();
        *self.records.lock().await = restored.workers;
        // The cleanup released the pools whose records are gone; the free
        // pools above the slot count go with them (plan 2e15085a).
        self.trim_idle_pools().await;
        Ok(report)
    }

    /// The raw durable per-model usage accumulator behind
    /// [`TeamSnapshot::usage`] (read-only; the Cost tab prices its rows per
    /// model).
    pub async fn usage(&self) -> ProjectUsage {
        self.usage.lock().await.clone()
    }

    /// The current team state, every worker's transcript included.
    ///
    /// Callers that only show or report the ledger (set_slots, deletion, the
    /// lead's MCP tools) want this; the window's sync path calls
    /// [`TeamRuntime::snapshot_known`] so it never re-receives transcripts it
    /// already holds.
    pub async fn snapshot(&self) -> TeamSnapshot {
        self.snapshot_known(&HashMap::new()).await
    }

    /// How many workers are running and how many are blocked on the person,
    /// counted from the records alone.
    ///
    /// [`TeamRuntime::snapshot`] clones every worker's transcript (and the
    /// usage accumulator) for the same two numbers; the machine-wide
    /// instances summary, which every viewing window polls every 10 s, must
    /// not.
    pub async fn activity_counts(&self) -> (usize, usize) {
        let records = self.records.lock().await;
        (
            records
                .iter()
                .filter(|worker| worker.status.is_running())
                .count(),
            records
                .iter()
                .filter(|worker| worker.waiting_on_user())
                .count(),
        )
    }

    /// The current team state, without the transcripts the caller holds.
    ///
    /// `known` maps a worker id to the transcript revision the caller already
    /// received. A worker whose transcript is still that revision comes back
    /// with an empty transcript and its id in
    /// [`TeamSnapshot::omitted_transcripts`]; every other worker travels in
    /// full. Revisions are cached beside the `Arc` they describe, so an
    /// unchanged transcript costs nothing and a replaced one is detected by
    /// pointer identity alone.
    pub async fn snapshot_known(&self, known: &HashMap<String, u64>) -> TeamSnapshot {
        let mut transcript_revs = HashMap::new();
        let mut omitted_transcripts = Vec::new();
        let workers = {
            let records = self.records.lock().await;
            // The revision cache is a plain std mutex: held only for the
            // clone-and-store below, never across an await.
            let mut revisions = self
                .transcript_revs
                .lock()
                .expect("transcript revision lock poisoned");
            let workers: Vec<WorkerRecord> = records
                .iter()
                .map(|record| {
                    let mut record = record.clone();
                    // A registered agent's thread is its own log, read-only.
                    if let Some(log) = agent_log::agent_transcript(&self.agent_watch, &record) {
                        record.transcript = log;
                    }
                    let rev = match revisions.get(&record.id) {
                        Some((cached, rev)) if Arc::ptr_eq(cached, &record.transcript) => *rev,
                        _ => {
                            let rev = TRANSCRIPT_REVS.fetch_add(1, Ordering::SeqCst);
                            revisions
                                .insert(record.id.clone(), (Arc::clone(&record.transcript), rev));
                            rev
                        }
                    };
                    if known.get(&record.id) == Some(&rev) {
                        record.transcript = Arc::default();
                        omitted_transcripts.push(record.id.clone());
                    }
                    transcript_revs.insert(record.id.clone(), rev);
                    record
                })
                .collect();
            // Forget the revisions of workers the ledger no longer has.
            revisions.retain(|id, _| transcript_revs.contains_key(id));
            workers
        };
        let mut agents = HashMap::new();
        let mut agent_phases = HashMap::new();
        for worker in workers.iter().filter(|worker| worker.claude_agent.is_some()) {
            let view = if worker.claude_agent_id.is_some() {
                self.agent_view(worker).await
            } else {
                None
            };
            if let Some(view) = &view {
                agents.insert(worker.id.clone(), view.to_json());
            }
            if let Some(phase) = agent_watch::agent_phase(worker, view.as_ref()) {
                agent_phases.insert(worker.id.clone(), phase);
            }
        }
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
            transcript_revs,
            omitted_transcripts,
            agents,
            agent_phases,
        }
    }

    /// Fold one live usage report (lead or worker session) into the durable
    /// per-project accumulator and schedule a ledger save (see
    /// `save_workers_soon`).
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
        save_workers_soon(&self.records, &self.usage, &self.save_lock, &self.persistence);
        Ok(())
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
        self.persist_with_snapshot(None).await
    }

    /// Persist the ledger, optionally substituting one worker in the saved
    /// snapshot. The override is a regression seam for transcript write
    /// failures; normal lifecycle writes always pass `None`.
    async fn persist_with_snapshot(
        &self,
        snapshot_override: Option<WorkerRecord>,
    ) -> Result<(), String> {
        // The final write of a closing runtime, so it bypasses the shutdown
        // check that voids detached saves.
        save_workers_now(
            &self.records,
            &self.usage,
            &self.save_lock,
            &self.persistence,
            snapshot_override,
        )
        .await
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

    /// The shared engine, or an error once the team has shut down.
    pub(crate) fn engine(&self) -> Result<Arc<Engine>, String> {
        self.engine
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| "the team has shut down".to_string())
    }

    /// Stop every live worker and release the team's handles on the shared
    /// engine and the plan board. The engine is owned by the lead and is not
    /// shut down here.
    pub async fn shutdown(&self) {
        // Void a pending telemetry flush: the final persist is the last write.
        self.persistence.closed.store(true, Ordering::SeqCst);
        self.agent_watch.stop().await;
        *self.lead_session.lock().await = None;
        let live = {
            let mut map = self.live.lock().await;
            std::mem::take(&mut *map)
        };
        let live: Vec<LiveWorker> = live.into_values().collect();
        for worker in &live {
            worker.abort();
        }
        // Wait until each worker's agent family has released its loop: a
        // worktree session holds the checkout's code graph (an open SQLite
        // file), which must be closed once shutdown returns.
        for worker in &live {
            let _ = worker.session.close_and_wait().await;
        }
        // An aborted pump holds the engine until the runtime polls it again:
        // wait until each one is dropped.
        for worker in live {
            let _ = worker.pump.await;
        }
        // The pumps' detached tasks (a transcript refresh running Git in a
        // checkout, a turn-end proof trigger, the proof jobs it starts)
        // outlive the aborted pumps: wait for them, bounded, so no child
        // process is still working in a checkout once shutdown returns.
        let detached = self.persistence.tasks.join(PUMP_TASK_GRACE).await;
        if detached > 0 {
            eprintln!("team shutdown: {detached} worker task(s) still running; they finish in the background");
        }
        let _ = self.persist().await;
        // The board holds the engine's memory store and code graph.
        self.board.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.engine
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

/// How long [`TeamRuntime::shutdown`] waits for the worker pumps' detached
/// tasks. They are usually short (a `git rev-parse`, a ledger save); a long
/// proof job finishes detached. With the engine's own waits (5 s for the
/// agent loops, 3 s for its background tasks) this stays inside the app's
/// 10 s `SHUTDOWN_DEADLINE`.
const PUMP_TASK_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

fn now_key() -> u128 {
    phase3::now_ms()
}

/// A worker that is running a turn (or about to, or waiting on an answer in
/// the middle of one): the only workers a lost or restarted host changes. Every
/// other status keeps its status and error (plan 9a2f4b67, step 5).
fn is_active(status: WorkerStatus) -> bool {
    matches!(
        status,
        WorkerStatus::Dispatching
            | WorkerStatus::Working
            | WorkerStatus::NeedsYou
            | WorkerStatus::Fixing
    )
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
