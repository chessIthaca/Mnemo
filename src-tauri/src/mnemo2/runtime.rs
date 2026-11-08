// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The headless-engine-backed lead runtime for Mnemo2.
//!
//! This module owns one live lead conversation: engine startup, the
//! `mnemo.workflow` policy, session spawn/events, transcript persistence, and
//! resume-based model changes. It deliberately has no Tauri types so the same
//! runtime is testable against a scripted LLM.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use mnemo::app::plan_board::PlanBoard;
use mnemo::app::policy_factory::WorkflowPolicyFactory;
use mnemo::config::{Config, SafetyMode};
use mnemo::headless::{
    AgentPlan, AgentSession, Engine, EngineOptions, EventReceiver, MemoryBackend,
    PendingInteraction, SessionCommand, SessionEvent, SessionId, SpawnOptions, Transcript,
};
use mnemo::laya::LayaManager;
use mnemo::provider::LlmClient;
use mnemo::tool::ToolResult;
use mnemo::workspace::Workspace;
use mnemo::{Approval, PermissionPolicy, QuestionOption, SerializableAgentEvent, UserAnswer};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::ledger::LeadTeamLedger;
use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
use crate::mnemo2::team::{BoardSignal, MessageSender, TeamRuntime, TeamRuntimeOptions};
use crate::mnemo2::trace_view::TraceSupport;
use crate::mnemo2::usage::event_session;

/// The Tauri event channel name used by the IPC layer.
pub const LEAD_EVENT_CHANNEL: &str = "mnemo2://lead-event";

/// Attribution shared with the lead transcript renderer for worker-originated notices.
pub(crate) const TEAM_NOTICE_PREFIX: &str = "[TeamManager notice — not the human]";

/// The persisted lead file inside the runtime's state directory.
const LEAD_STATE_FILE: &str = "lead.json";

/// How many transient lead events the broadcast bus keeps for a slow consumer.
const EVENT_BUFFER: usize = 512;

/// Runtime startup options.
#[derive(Clone, Debug)]
pub struct LeadRuntimeOptions {
    /// The selected project root.
    pub workspace_root: PathBuf,
    /// The Mnemo config directory; `None` resolves `MNEMO_HOME` / `~/.mnemo`.
    pub config_dir: Option<PathBuf>,
    /// Mnemo2's app-owned state directory (transcript + engine workspace).
    pub state_dir: PathBuf,
    /// An explicit lead model (`endpoint/model`); `None` restores the saved
    /// model, then the configured default, then the first configured model.
    pub model: Option<String>,
}

/// One configured model that the lead may run on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LeadModel {
    /// The endpoint's config name.
    pub endpoint: String,
    /// The actual configured model id.
    pub model: String,
    /// The `endpoint/model` string accepted by the engine spawn.
    pub spec: String,
    /// Whether this is the configured default.
    pub is_default: bool,
    /// The reasoning efforts this model accepts (`"off"` included); empty
    /// when its endpoint does not take a reasoning effort at all.
    pub reasoning_efforts: Vec<String>,
    /// The effort the model runs at when none is chosen (its configured
    /// default); `None` when it takes no reasoning effort.
    pub default_effort: Option<String>,
}

/// The lead's current public state.
#[derive(Clone, Debug, Serialize)]
pub struct LeadSnapshot {
    /// Monotonic identity of the live session; stale event envelopes carry an
    /// older generation and are ignored by the frontend.
    pub generation: u64,
    /// The model currently selected for the lead (`endpoint/model`).
    pub model: Option<String>,
    /// The actual model id reported by the running session, when known.
    pub model_id: Option<String>,
    /// The reasoning effort chosen for the lead; `None` = the model's
    /// configured default (`LeadModel::default_effort`).
    pub reasoning_effort: Option<String>,
    /// Every configured model the lead may switch to.
    pub models: Vec<LeadModel>,
    /// The configured default model spec, when one exists.
    pub default_model: Option<String>,
    /// The conversation so far.
    pub transcript: Transcript,
    /// This lead session's active plan, if it has one.
    pub plan: Option<AgentPlan>,
    /// Approximate prompt-token usage.
    pub used_tokens: u32,
    /// The context window.
    pub max_tokens: u32,
    /// The interaction the session is blocked on, if any.
    pub pending: Option<LeadPending>,
    /// Whether a turn is currently running.
    pub busy: bool,
}

/// The lead's pending interaction plus the details its event carried. The
/// engine snapshot keeps only the id, so without these a restored or
/// refreshed UI could show a "waiting for you" banner with no question.
#[derive(Clone, Debug, Serialize)]
pub struct LeadPending {
    /// The engine's interaction (`kind` + its id).
    #[serde(flatten)]
    pub interaction: PendingInteraction,
    /// The `ask_user` question text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// The question's choices; empty means a free-form answer.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<QuestionOption>,
    /// The tool awaiting approval (or the host tool being executed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

/// Event details for the lead's open interactions, keyed by interaction id.
type PendingDetails = Arc<Mutex<std::collections::HashMap<String, PendingDetail>>>;

#[derive(Clone, Debug, Default)]
struct PendingDetail {
    question: Option<String>,
    options: Vec<QuestionOption>,
    tool_name: Option<String>,
}

fn interaction_id(interaction: &PendingInteraction) -> &str {
    match interaction {
        PendingInteraction::Approval { request_id } => request_id,
        PendingInteraction::Question { question_id } => question_id,
        PendingInteraction::HostToolCall { call_id } => call_id,
        PendingInteraction::PolicyHook { hook_id } => hook_id,
    }
}

/// One event forwarded from the live lead session.
#[derive(Clone, Debug, Serialize)]
pub struct LeadEvent {
    /// The live session generation that emitted the event.
    pub generation: u64,
    /// The underlying headless event.
    pub event: SessionEvent,
}

/// A saved lead conversation, restored only for the same canonical project.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedLead {
    model: Option<String>,
    #[serde(default)]
    reasoning_effort: Option<String>,
    transcript: Transcript,
    #[serde(default)]
    workspace_root: Option<String>,
    #[serde(default)]
    updated_at_ms: u128,
}

/// The backup-recoverable, project-scoped lead transcript store.
#[derive(Clone, Debug)]
struct LeadPersistence {
    path: PathBuf,
    workspace_root: String,
}

impl LeadPersistence {
    fn new(state_dir: &Path, workspace_root: &Path) -> Self {
        let identity = std::fs::canonicalize(workspace_root)
            .unwrap_or_else(|_| workspace_root.to_path_buf())
            .to_string_lossy()
            .into_owned();
        Self {
            path: state_dir.join(LEAD_STATE_FILE),
            workspace_root: identity,
        }
    }

    fn load(&self) -> Result<PersistedLead, String> {
        let Some(lead) = crate::mnemo2::project::read_json::<PersistedLead>(&self.path)? else {
            return Ok(PersistedLead::default());
        };
        if lead.workspace_root.as_deref() != Some(self.workspace_root.as_str()) {
            return Err(
                "saved lead state belongs to another project or lacks a project identity"
                    .to_string(),
            );
        }
        Ok(lead)
    }

    fn save(
        &self,
        model: Option<String>,
        reasoning_effort: Option<String>,
        transcript: Transcript,
    ) -> Result<(), String> {
        let lead = PersistedLead {
            model,
            reasoning_effort,
            transcript,
            workspace_root: Some(self.workspace_root.clone()),
            updated_at_ms: crate::mnemo2::phase3::now_ms(),
        };
        crate::mnemo2::project::write_json(&self.path, &lead)
    }
}

struct LiveLead {
    generation: u64,
    session: AgentSession,
    busy: Arc<AtomicBool>,
    model: Arc<Mutex<Option<String>>>,
    reasoning_effort: Option<String>,
    observed_model: Arc<Mutex<Option<String>>>,
    details: PendingDetails,
    pump: JoinHandle<()>,
    notifications: Option<JoinHandle<()>>,
}

impl LiveLead {
    fn abort(&self) {
        self.pump.abort();
        if let Some(notifications) = &self.notifications {
            notifications.abort();
        }
    }
}

/// The one live lead conversation.
pub struct LeadRuntime {
    engine: Arc<Mutex<Option<Arc<Engine>>>>,
    live: Mutex<Option<LiveLead>>,
    generation: Arc<AtomicU64>,
    events: broadcast::Sender<LeadEvent>,
    persistence: LeadPersistence,
    config_dir: PathBuf,
    workspace_root: PathBuf,
    state_dir: PathBuf,
    models: std::sync::RwLock<Vec<LeadModel>>,
    default_model: std::sync::RwLock<Option<String>>,
    policy: Option<String>,
    team: crate::mnemo2::ledger::TeamSlot,
    /// The project's one plan board: the lead's board tools and the team's
    /// dispatch validation, tick, and close all use this instance.
    board: std::sync::Mutex<Option<Arc<PlanBoard>>>,
    /// The engine's memory, knowledge and code-graph handles, for the
    /// knowledge tools an external lead gets over MCP.
    stores: std::sync::Mutex<Option<ProjectStores>>,
    /// The engine's startup code-index and memory passes.
    indexing: Arc<crate::mnemo2::indexing::IndexingTracker>,
    /// The managed Laya runtime the engine attached at start (`None`
    /// while `[general.laya]` is disabled or uninstalled, and after
    /// [`LeadRuntime::shutdown`]). The running `laya-serve` child lives
    /// inside this manager, so the runtime holds the Arc for the
    /// engine's whole lifetime — dropping it would kill the child; the
    /// explicit stop in [`LeadRuntime::shutdown`] is the primary path,
    /// the manager's Drop is only the safety net.
    laya: std::sync::Mutex<Option<Arc<LayaManager>>>,
    /// The trace backend for the current engine (plan c3ba2b58 step B):
    /// the ring raised to 128 records, the metadata journal at
    /// `.coding/logs/`, and its background writer. `None` before the first
    /// engine start and after [`LeadRuntime::shutdown`].
    trace_support: std::sync::Mutex<Option<Arc<TraceSupport>>>,
}

/// The project stores the engine wired into its own agent registries. The
/// external lead's knowledge tools run over these same handles, so a record
/// either lead writes is found by the other and no second database
/// connection is opened.
struct ProjectStores {
    /// The project's memory store (`.coding/memory.db`).
    memory: Option<Arc<dyn mnemo::memory::MemoryStoreTrait>>,
    /// The knowledge-file store (`.coding/knowledge/**`): the sandbox every
    /// memory write goes through.
    knowledge: Option<Arc<mnemo::memory::KnowledgeStore>>,
    /// The code graph (`.coding/codegraph.db`); `None` when the project's
    /// config turns code indexing off.
    codegraph: Option<Arc<mnemo::codegraph::CodeGraph>>,
    /// `.coding/plans`, which the knowledge tools reindex against.
    plans_dir: PathBuf,
}

impl LeadRuntime {
    /// Start the real engine and open the saved (or fresh) lead session.
    pub async fn open(options: LeadRuntimeOptions) -> Result<Self, String> {
        Self::open_inner(options, None).await
    }

    async fn open_inner(
        options: LeadRuntimeOptions,
        provider: Option<Arc<dyn LlmClient>>,
    ) -> Result<Self, String> {
        Self::open_inner_with_policy(options, provider, Some("mnemo.workflow".to_string())).await
    }

    pub(crate) async fn open_inner_with_policy(
        options: LeadRuntimeOptions,
        provider: Option<Arc<dyn LlmClient>>,
        policy: Option<String>,
    ) -> Result<Self, String> {
        if !options.workspace_root.is_dir() {
            return Err(format!(
                "the selected project root does not exist: {}",
                options.workspace_root.display()
            ));
        }
        let config_dir = mnemo::config::resolve_config_dir(options.config_dir.clone());
        let (models, configured_default) = load_models(&config_dir)?;
        let persistence = LeadPersistence::new(&options.state_dir, &options.workspace_root);
        let persisted = persistence.load()?;
        let persisted_model = persisted
            .model
            .clone()
            .filter(|spec| models.iter().any(|model| &model.spec == spec));
        if let Some(spec) = &options.model {
            if !models.is_empty() && !models.iter().any(|model| &model.spec == spec) {
                return Err(format!("'{spec}' is not a configured model"));
            }
        }
        let selected = options
            .model
            .clone()
            .or_else(|| persisted_model.clone())
            .or_else(|| configured_default.clone())
            .or_else(|| models.first().map(|model| model.spec.clone()));
        let spawn_model = options.model.clone().or(persisted_model.clone());
        // A saved effort belongs to the saved model; it is restored only with
        // that model and only while the model still accepts it.
        let spawn_effort = if options.model.is_none() {
            persisted.reasoning_effort.clone().filter(|effort| {
                persisted_model.as_deref().is_some_and(|spec| {
                    models
                        .iter()
                        .any(|model| model.spec == spec && model.reasoning_efforts.contains(effort))
                })
            })
        } else {
            None
        };
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let runtime = Self {
            engine: Arc::new(Mutex::new(None)),
            live: Mutex::new(None),
            generation: Arc::new(AtomicU64::new(1)),
            events,
            persistence,
            config_dir,
            workspace_root: options.workspace_root,
            state_dir: options.state_dir,
            models: std::sync::RwLock::new(models),
            default_model: std::sync::RwLock::new(configured_default),
            policy,
            team: Arc::new(Mutex::new(None)),
            board: std::sync::Mutex::new(None),
            stores: std::sync::Mutex::new(None),
            indexing: crate::mnemo2::indexing::IndexingTracker::new(),
            laya: std::sync::Mutex::new(None),
            trace_support: std::sync::Mutex::new(None),
        };
        runtime.start_engine(provider).await?;
        let engine = runtime.engine_handle().await?;
        let classifier = mnemo::headless::Setup::open(Some(runtime.config_dir.clone()))
            .map_err(|error| format!("open the lead settings: {error}"))?
            .for_engine(&engine)
            .classifier_slot();
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine,
            workspace_root: runtime.workspace_root.clone(),
            state_dir: runtime.state_dir.clone(),
            config_dir: runtime.config_dir.clone(),
            classifier,
            routing_log_path: cfg!(test)
                .then(|| runtime.state_dir.join("dispatch_routing.jsonl")),
            slots: None,
            models: runtime.models(),
            pricing: runtime.pricing()?,
        })
        .await?;
        let team = Arc::new(team);
        team.set_tiers(runtime.tiers());
        if let Some(board) = runtime.board_handle() {
            team.attach_board(board);
        }
        *runtime.team.lock().await = Some(team);
        let live = runtime
            .spawn_live(1, selected, spawn_model, spawn_effort, persisted.transcript)
            .await?;
        *runtime.live.lock().await = Some(live);
        Ok(runtime)
    }

    /// Subscribe to the live lead event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<LeadEvent> {
        self.events.subscribe()
    }

    /// The configured model inventory.
    pub fn models(&self) -> Vec<LeadModel> {
        self.models
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The config directory this lead reads (`None` options resolve to the
    /// install default).
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The configured model tiers, read fresh from disk.
    pub(crate) fn tiers(&self) -> mnemo::headless::TiersDto {
        mnemo::headless::Setup::open(Some(self.config_dir.clone()))
            .and_then(|setup| setup.tiers())
            .unwrap_or_default()
    }

    /// Re-read endpoints, models, prices and tiers after a settings save, so
    /// new dispatches (and the next lead session's tool schema) see them.
    /// The running lead session keeps its already-advertised tool schema.
    pub async fn refresh_catalog(&self) -> Result<(), String> {
        let (models, default) = load_models(&self.config_dir)?;
        *self.models.write().unwrap_or_else(|e| e.into_inner()) = models.clone();
        *self
            .default_model
            .write()
            .unwrap_or_else(|e| e.into_inner()) = default;
        let team = self.team.lock().await.clone();
        if let Some(team) = team {
            team.set_catalog(models, self.tiers(), self.pricing()?);
        }
        Ok(())
    }

    /// The configured default model spec.
    pub fn default_model(&self) -> Option<String> {
        self.default_model
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The shared engine handle (the team runtime spawns workers on the same
    /// engine as the lead — never a second engine).
    pub(crate) async fn engine_handle(&self) -> Result<Arc<Engine>, String> {
        self.engine
            .lock()
            .await
            .clone()
            .ok_or_else(|| "the headless engine is not running".to_string())
    }

    /// The shared plan board, once the engine has started.
    pub(crate) fn board_handle(&self) -> Option<Arc<PlanBoard>> {
        self.board.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The plan-board tools (`plan_create`, `plan_update`, `plan_show`,
    /// `plan_list`, `plan_step_done`, `plan_abandon`) over this lead's board,
    /// for an external lead that has no engine session to carry them. Empty
    /// until the engine has started.
    pub(crate) fn board_tools(&self) -> Vec<Box<dyn mnemo::tool::Tool>> {
        use mnemo::app::tool::workflow::board::{
            PlanAbandonTool, PlanCreateTool, PlanListTool, PlanShowTool, PlanStepDoneTool,
            PlanUpdateTool,
        };

        let Some(board) = self.board_handle() else {
            return Vec::new();
        };
        let ledger = Arc::new(LeadTeamLedger::new(self.team.clone()));
        vec![
            Box::new(PlanCreateTool::new(board.clone())),
            Box::new(PlanUpdateTool::new(board.clone())),
            Box::new(PlanShowTool::new(board.clone())),
            Box::new(PlanListTool::new(board.clone())),
            Box::new(PlanStepDoneTool::new(board.clone(), ledger)),
            Box::new(PlanAbandonTool::new(board)),
        ]
    }

    /// The knowledge tools the in-app lead has through its engine session,
    /// for an external lead that has none: `memory_search`, `memory_write`,
    /// `memory_amend`, `memory_supersede` over the project's memory store and
    /// `graph_search`, `graph_context`, `graph_impact`, `graph_path` over its
    /// code graph.
    ///
    /// Built exactly as the engine's agent factory builds them (same structs,
    /// same shared handles, same knowledge-file backing), so schemas and
    /// descriptions are the engine's own and memory writes land in
    /// `.coding/knowledge/**` through the knowledge store's own path rules.
    /// Left out: the Laya auto-typing gate (these tool instances are built
    /// without it; the engine's OWN agent tools, by contrast, auto-type
    /// through the classifier slot `start_engine`'s attach fills when
    /// `[general.laya]` is enabled and installed) and the engine's
    /// agent-root note (the lead works in the main tree). Memory tools
    /// are absent until the engine has started; graph tools also when
    /// code indexing is off, as in the engine.
    pub(crate) fn knowledge_tools(&self) -> Vec<Box<dyn mnemo::tool::Tool>> {
        use mnemo::tool::agent::codegraph::{
            GraphContextTool, GraphImpactTool, GraphPathTool, GraphSearchTool,
        };
        use mnemo::tool::memory::retrieval::MemorySearchTool;
        use mnemo::tool::memory::{
            MemoryAmendTool, MemorySupersedeTool, MemoryUpdateTool, MemoryWriteTool,
        };

        let guard = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        let Some(stores) = guard.as_ref() else {
            return Vec::new();
        };
        let mut tools: Vec<Box<dyn mnemo::tool::Tool>> = Vec::new();
        if let Some(memory) = &stores.memory {
            tools.push(Box::new(MemorySearchTool::new(memory.clone())));
            match &stores.knowledge {
                Some(knowledge) => {
                    tools.push(Box::new(MemoryWriteTool::with_knowledge(
                        memory.clone(),
                        knowledge.clone(),
                        stores.plans_dir.clone(),
                    )));
                    // File-only, so it exists only with the knowledge backing.
                    tools.push(Box::new(MemoryAmendTool::new(
                        memory.clone(),
                        knowledge.clone(),
                        stores.plans_dir.clone(),
                    )));
                    // The amend/supersede descriptions point to memory_update.
                    tools.push(Box::new(MemoryUpdateTool::with_knowledge(
                        memory.clone(),
                        knowledge.clone(),
                        stores.plans_dir.clone(),
                    )));
                    tools.push(Box::new(MemorySupersedeTool::with_knowledge(
                        memory.clone(),
                        knowledge.clone(),
                        stores.plans_dir.clone(),
                    )));
                }
                None => {
                    tools.push(Box::new(MemoryWriteTool::new(memory.clone())));
                    tools.push(Box::new(MemoryUpdateTool::new(memory.clone())));
                    tools.push(Box::new(MemorySupersedeTool::new(memory.clone())));
                }
            }
        }
        if let Some(graph) = &stores.codegraph {
            // The stale-index log lives in the config dir, like the engine's.
            tools.push(Box::new(
                GraphSearchTool::new(graph.clone()).with_staleness_log(
                    mnemo::index_staleness::StalenessLog::at(self.config_dir.clone()),
                ),
            ));
            tools.push(Box::new(GraphContextTool::new(graph.clone())));
            tools.push(Box::new(GraphImpactTool::new(graph.clone())));
            tools.push(Box::new(GraphPathTool::new(graph.clone())));
        }
        tools
    }

    /// The selected project root (for worker worktree provisioning).
    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Load configured model prices for team telemetry.
    pub(crate) fn pricing(&self) -> Result<Vec<mnemo::config::PricingEntry>, String> {
        Config::load(&self.config_dir)
            .map(|config| config.pricing)
            .map_err(|err| format!("could not load model pricing: {err}"))
    }

    /// The trace backend for the current engine — the raised ring, the
    /// metadata journal, and its writer. `None` when no engine is running.
    pub(crate) fn trace_support(&self) -> Option<Arc<TraceSupport>> {
        self.trace_support
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The shared team runtime for this lead.
    /// The shared team runtime for this lead.
    pub(crate) async fn team_handle(&self) -> Result<Arc<TeamRuntime>, String> {
        self.team
            .lock()
            .await
            .clone()
            .ok_or_else(|| "the team runtime is not open".to_string())
    }

    /// The current lead state.
    pub async fn snapshot(&self) -> Result<LeadSnapshot, String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        let snapshot = live
            .session
            .snapshot()
            .await
            .map_err(|err| format!("could not read the lead snapshot: {err}"))?;
        let selected = live.model.lock().await.clone();
        let observed = live
            .observed_model
            .lock()
            .await
            .clone()
            .or(snapshot.model);
        let model_id = observed.or_else(|| {
            selected
                .as_deref()
                .and_then(|spec| self.models().into_iter().find(|model| model.spec == spec))
                .map(|model| model.model)
        });
        let pending = match snapshot.pending {
            Some(interaction) => {
                let detail = live
                    .details
                    .lock()
                    .await
                    .get(interaction_id(&interaction))
                    .cloned()
                    .unwrap_or_default();
                Some(LeadPending {
                    interaction,
                    question: detail.question,
                    options: detail.options,
                    tool_name: detail.tool_name,
                })
            }
            None => None,
        };
        Ok(LeadSnapshot {
            generation: live.generation,
            model: selected,
            model_id,
            reasoning_effort: live.reasoning_effort.clone(),
            models: self.models(),
            default_model: self.default_model(),
            transcript: snapshot.transcript,
            plan: live
                .session
                .plan()
                .await
                .map_err(|err| format!("could not read the lead plan: {err}"))?,
            used_tokens: snapshot.used_tokens,
            max_tokens: snapshot.max_tokens,
            pending,
            busy: live.busy.load(Ordering::SeqCst),
        })
    }

    /// Start a turn with a user prompt — or, while the lead is busy,
    /// queue the same text and images as a steer the kernel injects at
    /// the next safe turn boundary. Image-only submissions are accepted;
    /// only a wholly empty one (no text and no images) is rejected.
    pub async fn send_prompt(&self, text: String, images: Vec<String>) -> Result<(), String> {
        if text.trim().is_empty() && images.is_empty() {
            return Err("the prompt is empty".to_string());
        }
        // Take the team handle first, then the live lock — the same
        // team→live order `shutdown` uses, so the two never deadlock.
        let team = self.team.lock().await.clone();
        let live_guard = self.live.lock().await;
        let live = live_guard
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        // Mark the team's lead busy up front on both paths. The pump
        // clears the team gate WITHOUT the live lock, so a terminal
        // event processed while we hold this lock can still reopen the
        // gate; the busy branch therefore re-arms after its send lands.
        if let Some(team) = &team {
            team.begin_lead_turn();
        }
        if live.busy.load(Ordering::SeqCst) {
            // Busy: not an error and not a second prompt — a queued
            // steer injected at the next safe boundary. The busy flag
            // stays with the running turn.
            let result = live
                .session
                .send(SessionCommand::Steer { text, images })
                .await;
            // Re-arm AFTER a successful send: the running turn may have
            // just finished at the kernel (the busy flag above was
            // stale), and the pump processes that terminal event
            // without the live lock — including during this send —
            // clearing the gate while the steer's follow-up turn is
            // already queued. The up-front arm cannot cover a clear that
            // lands inside the send; this re-arm does. On failure the
            // gate stays armed: the running turn is still live and its
            // terminal event clears it through the pump.
            if result.is_ok() {
                if let Some(team) = &team {
                    team.begin_lead_turn();
                }
            }
            return result.map_err(|err| format!("could not steer the busy lead: {err}"));
        }
        live.busy.store(true, Ordering::SeqCst);
        let result = live
            .session
            .send(SessionCommand::Prompt { text, images })
            .await;
        if let Err(err) = result {
            // The rejected prompt started no turn, so no terminal
            // event will ever arrive to clear the team gate — a parked
            // or closed session rejects the prompt right here, and the
            // up-front begin_lead_turn already armed the gate. Clear it
            // on this arm or every later merge stays blocked.
            if let Some(team) = &team {
                team.set_lead_busy(false);
            }
            live.busy.store(false, Ordering::SeqCst);
            return Err(format!("could not send the prompt: {err}"));
        }
        Ok(())
    }

    /// Cancel the current generation; the session remains open.
    pub async fn interrupt(&self) -> Result<(), String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        live.session
            .send(SessionCommand::Interrupt)
            .await
            .map_err(|err| format!("could not interrupt the lead: {err}"))
    }

    /// Compact the lead's context window; the session stays open. The kernel
    /// summarizes the older history between turns and reports the outcome as
    /// `compact_started` / `compacted` lead events (the compacted event
    /// carries the token count before and after). A busy lead queues the
    /// compaction behind the running turn.
    pub async fn compact(&self) -> Result<(), String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        live.session
            .send(SessionCommand::Compact)
            .await
            .map_err(|err| format!("could not compact the lead context: {err}"))
    }

    /// Resolve a pending approval.
    pub async fn approve(&self, request_id: String, decision: Approval) -> Result<(), String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        live.session
            .send(SessionCommand::Approve {
                request_id: request_id.clone(),
                decision,
            })
            .await
            .map_err(|err| format!("could not answer the approval: {err}"))?;
        live.details.lock().await.remove(&request_id);
        Ok(())
    }

    /// Resolve a pending `ask_user` question.
    pub async fn answer(&self, question_id: String, answer: UserAnswer) -> Result<(), String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        live.session
            .send(SessionCommand::Answer {
                question_id: question_id.clone(),
                answer,
            })
            .await
            .map_err(|err| format!("could not answer the question: {err}"))?;
        live.details.lock().await.remove(&question_id);
        Ok(())
    }

    /// Change the lead's model and reasoning effort while it is idle.
    ///
    /// The headless command surface has no `SetModel`, so this saves the
    /// transcript, closes the old session, and resumes a fresh one on the new
    /// model. The generation advances so stale events cannot touch the new UI.
    /// `reasoning_effort: None` runs the model at its configured default.
    pub async fn set_model(
        &self,
        spec: String,
        reasoning_effort: Option<String>,
    ) -> Result<LeadSnapshot, String> {
        let model = self
            .models()
            .into_iter()
            .find(|model| model.spec == spec)
            .ok_or_else(|| format!("'{spec}' is not a configured model"))?;
        if let Some(effort) = &reasoning_effort {
            if !model.reasoning_efforts.contains(effort) {
                return Err(if model.reasoning_efforts.is_empty() {
                    format!("'{spec}' does not take a reasoning effort")
                } else {
                    format!(
                        "'{spec}' does not support reasoning effort '{effort}'; choose one of: {}",
                        model.reasoning_efforts.join(", ")
                    )
                });
            }
        }
        let team = self.team_handle().await?;
        let old = {
            let mut guard = self.live.lock().await;
            let live = guard
                .as_ref()
                .ok_or_else(|| "the lead session is not open".to_string())?;
            if live.busy.load(Ordering::SeqCst) {
                return Err("stop the current turn before changing models".to_string());
            }
            guard.take().expect("live session checked above")
        };
        let snapshot = match old.session.snapshot().await {
            Ok(snapshot) => snapshot,
            Err(err) => {
                *self.live.lock().await = Some(old);
                return Err(format!(
                    "could not save the lead before switching models: {err}"
                ));
            }
        };
        let generation = self.generation.load(Ordering::SeqCst) + 1;
        let live = match self
            .spawn_live(
                generation,
                Some(spec.clone()),
                Some(spec.clone()),
                reasoning_effort.clone(),
                snapshot.transcript.clone(),
            )
            .await
        {
            Ok(live) => live,
            Err(err) => {
                *self.live.lock().await = Some(old);
                return Err(err);
            }
        };
        if let Err(err) = self
            .persistence
            .save(Some(spec), reasoning_effort, snapshot.transcript)
        {
            live.abort();
            let _ = live.session.send(SessionCommand::Interrupt).await;
            let previous_session = old.session.id();
            *self.live.lock().await = Some(old);
            team.attach_lead(previous_session).await;
            return Err(err);
        }
        self.generation.store(generation, Ordering::SeqCst);
        old.abort();
        let _ = old.session.send(SessionCommand::Interrupt).await;
        *self.live.lock().await = Some(live);
        self.snapshot().await
    }

    /// Hand the project back to the in-app lead: detach any external lead and
    /// give the in-app lead session the notices the external lead never read
    /// (a steer while it works, a prompt when it idles; all in one message).
    /// If the in-app session cannot take them they stay on the team's
    /// undelivered list (logged), for the next in-app notice or external
    /// lead. Returns `true` when an external lead was attached.
    pub(crate) async fn release_external_lead(&self) -> bool {
        let Ok(team) = self.team_handle().await else {
            return false;
        };
        let was_attached = team.detach_external_lead();
        let held = team.current_notices(team.take_undelivered_notices()).await;
        if held.is_empty() {
            return was_attached;
        }
        let session_id = self.live.lock().await.as_ref().map(|live| live.session.id());
        let delivered = match session_id {
            Some(session_id) => {
                send_to_in_app_lead(&self.engine, session_id, &team, held.join("\n\n")).await
            }
            None => false,
        };
        if !delivered {
            eprintln!(
                "mnemo2: the in-app lead could not take {} notice(s) from the external lead; keeping them queued",
                held.len()
            );
            team.restore_undelivered_notices(held);
        }
        was_attached
    }

    /// Stop the session and discard the engine handles.
    pub async fn shutdown(&self) {
        self.indexing.stop();
        if let Some(team) = self.team.lock().await.take() {
            team.shutdown().await;
        }
        // Stop the trace journal's background writer before the engine
        // drops: it polls the engine's shared trace log (plan c3ba2b58
        // step B).
        self.trace_support.lock().unwrap_or_else(|e| e.into_inner()).take();
        // Stop the managed Laya sidecar BEFORE the engine drops: every
        // classifier consumer (routing, auto-typing, triage) reads the
        // shared slot at call time, and the manager's Drop kill is only
        // the safety net — this explicit stop is the primary path.
        if let Some(laya) = self.laya.lock().unwrap_or_else(|e| e.into_inner()).take() {
            laya.stop();
        }
        let live = self.live.lock().await.take();
        if let Some(live) = live {
            live.abort();
            let _ = live.session.send(SessionCommand::Interrupt).await;
        }
        if let Some(engine) = self.engine.lock().await.take() {
            let _ = engine.shutdown().await;
        }
    }

    async fn start_engine(&self, provider: Option<Arc<dyn LlmClient>>) -> Result<(), String> {
        // The engine's state dir is the project's own `.coding/`, the same one
        // Mnemo1 uses: memory.db, knowledge/, codegraph.db, plans/ and reviews/
        // are one shared set of project stores. (Mnemo2's own state stays in
        // `.coding/mnemo2/`.)
        // Best effort, as before: this only keeps the shared caches out of
        // `git status`, so a failed or cancelled task is ignored.
        let root = self.workspace_root.clone();
        let _ = tokio::task::spawn_blocking(move || ignore_project_caches(&root)).await;
        std::fs::create_dir_all(self.workspace_root.join(".coding"))
            .map_err(|err| format!("could not create the project's .coding directory: {err}"))?;
        let workspace = Workspace::new(self.workspace_root.clone());
        // Mnemo1 indexes the graph unless the config turns it off.
        let index_codegraph = Config::load(&self.config_dir)
            .map(|config| config.general.general.codegraph)
            .unwrap_or(true);
        // No code index yet: the first pass parses the whole tree, and the
        // UI holds the project until it ends.
        let first_run = index_codegraph
            && !self
                .workspace_root
                .join(".coding")
                .join("codegraph.db")
                .exists();
        let options = EngineOptions {
            workspace,
            config_dir: Some(self.config_dir.clone()),
            permissions: PermissionPolicy::ApproveViaApi {
                safety_mode: SafetyMode::ApproveEachAction,
            },
            memory: MemoryBackend::Sqlite { path: None },
            index_codegraph,
            require_git: false,
            auto_install_embedder: false,
        };
        let mut builder = Engine::builder(options);
        if let Some(provider) = provider {
            builder = builder.provider(provider);
        }
        let engine = builder
            .start()
            .await
            .map_err(|err| format!("could not start the headless engine: {err}"))?;
        let ledger = Arc::new(LeadTeamLedger::new(self.team.clone()));
        let reviews_dir = engine.reviews_dir();
        let plans_dir = reviews_dir
            .parent()
            .map(|coding| coding.join("plans"))
            .unwrap_or_default();
        let mut board = PlanBoard::new(plans_dir.clone(), reviews_dir);
        if let Some(memory) = engine.memory_store_handle() {
            board = board.with_memory(memory);
        }
        if let Some(knowledge) = engine.knowledge_store_handle() {
            board = board.with_knowledge(knowledge);
        }
        if let Some(codegraph) = engine.codegraph_handle() {
            board = board.with_codegraph(codegraph);
        }
        let board = Arc::new(board);
        *self.board.lock().unwrap_or_else(|e| e.into_inner()) = Some(board.clone());
        *self.stores.lock().unwrap_or_else(|e| e.into_inner()) = Some(ProjectStores {
            memory: engine.memory_store_handle(),
            knowledge: engine.knowledge_store_handle(),
            codegraph: engine.codegraph_handle(),
            plans_dir: plans_dir.clone(),
        });
        engine.register_policy(
            "mnemo.workflow",
            Arc::new(
                WorkflowPolicyFactory::new(
                    engine.memory_store_handle(),
                    engine.knowledge_store_handle(),
                    engine.codegraph_handle(),
                    engine.reviews_dir(),
                    None,
                )
                .with_team_lead(ledger, board),
            ),
        );
        let engine = Arc::new(engine);
        if index_codegraph {
            self.indexing.begin_code_pass(first_run);
        }
        self.indexing.watch(engine.clone());
        // The classifier attach is last and never gates the start: the
        // fill is I/O-free and every failure only logs.
        self.attach_laya(&engine).await;
        // Plan c3ba2b58 step B: the trace backend per engine — the ring
        // raised to 128 records and the metadata journal's background
        // writer. A restarted engine replaces the slot (dropping the old
        // support aborts its writer).
        *self.trace_support.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Arc::new(TraceSupport::start(&engine)));
        *self.engine.lock().await = Some(engine);
        Ok(())
    }

    /// Attach the managed Laya classifier to the just-started engine —
    /// the headless twin of Mnemo1's startup attach (main.rs:78-127):
    /// when the live `[general.laya]` config enables Laya and the
    /// English checkpoint is already installed, the engine-backed
    /// `Setup` fills the brain's classifier slot and parks the sidecar
    /// start (status `Starting` until the probe answers); the workers
    /// share the engine's brain, so this one fill covers the lead and
    /// every worker session. A disabled or uninstalled Laya changes
    /// nothing — the slot stays empty, a byte-identical pre-Laya
    /// startup — and no error here ever fails the engine start. The
    /// manager Arc is held on the runtime: dropping it would kill the
    /// `laya-serve` child, so it must outlive this call.
    async fn attach_laya(&self, engine: &Arc<Engine>) {
        let setup = match mnemo::headless::Setup::open(Some(self.config_dir.clone())) {
            Ok(setup) => setup.for_engine(engine),
            Err(err) => {
                eprintln!(
                    "mnemo2: the Laya classifier could not attach — the settings \
                     setup failed to open: {err}"
                );
                return;
            }
        };
        if setup.attach_laya().await.is_none() {
            return;
        }
        *self.laya.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Arc::clone(setup.laya_manager()));
        eprintln!("mnemo2: the managed Laya classifier attached to the lead engine");
        // TODO(plan e02de747): the design (§2) spawns the startup
        // fine-tune gate right here — `spawn_startup_finetune`
        // (src-tauri/src/ipc/finetune.rs:585) on this manager plus the
        // slot (`Setup::classifier_slot`), the status Arc and the live
        // config (`Setup::live_config`). It is NOT spawned: that
        // function hangs off main.rs's binary-only `mod ipc;` tree —
        // this lib-compiled module (src-tauri/src/lib.rs) has no `ipc`
        // module, so the call does not even resolve for the mnemo2
        // binary. Lifting the gate into the lib is a crate-layout
        // change outside this step's single-file ownership; until it
        // lands, Mnemo2 skips the startup fine-tune (the classifier
        // serves whatever checkpoint is installed).
    }

    /// The project's startup indexing status.
    pub fn indexing(&self) -> &Arc<crate::mnemo2::indexing::IndexingTracker> {
        &self.indexing
    }

    /// The managed Laya runtime this lead attached at engine start, when
    /// `[general.laya]` was enabled and the checkpoint already installed
    /// (`None` otherwise, and after [`LeadRuntime::shutdown`]). Rewire-style
    /// callers must reuse THIS Arc: a fresh `for_engine` `Setup` builds its
    /// own manager, and only this instance owns the running sidecar child.
    pub fn laya_manager(&self) -> Option<Arc<LayaManager>> {
        self.laya
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Hand a settings-installed Laya manager (the manager a
    /// `laya_settings::laya_setup` pipeline ran on) to this session, so
    /// [`LeadRuntime::shutdown`] stops its sidecar exactly like the
    /// attach-time one — a static hold is never dropped, and without
    /// this hand-off a settings-installed `laya-serve` would outlive the
    /// session and the app (review finding heldManagerOrphan). Any
    /// manager the session already holds is stopped first: its child is
    /// superseded by the live-slot swap that landed with the new
    /// sidecar. Only `laya_settings`' pipeline-end settlement calls
    /// this, and only when the manager it hands over actually serves.
    pub fn set_laya_manager(&self, manager: Arc<LayaManager>) {
        let previous = self
            .laya
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(manager);
        if let Some(previous) = previous {
            previous.stop();
        }
    }

    /// Run the automatic Laya check (plan 8106337a) on this lead's engine:
    /// hand [`crate::mnemo2::laya_check::spawn_laya_check`] this runtime's
    /// indexing tracker (the pass shows on the project's status line),
    /// the open team (so the gates it applies take effect without a
    /// restart) and the real record path. Unforced, the check itself
    /// skips silently when routing is off, Laya is down or the record's
    /// fingerprint still matches. Never called from `start_engine` or
    /// `open` — the project-open path in `ipc` owns that trigger, after
    /// its indexing forwarder exists.
    pub async fn start_laya_check(&self, force: bool) -> Result<(), String> {
        let engine = self.engine_handle().await?;
        let team = self.team_handle().await.ok();
        let setup = mnemo::headless::Setup::open(Some(self.config_dir.clone()))
            .map_err(|error| error.to_string())?
            .for_engine(&engine);
        crate::mnemo2::laya_check::spawn_laya_check(
            crate::mnemo2::laya_check::LayaCheck {
                setup,
                config_dir: self.config_dir.clone(),
                team,
                tracker: self.indexing.clone(),
                record_path: crate::mnemo2::route_eval::route_eval_path(),
            },
            force,
        )
    }

    async fn spawn_live(
        &self,
        generation: u64,
        selected_model: Option<String>,
        spawn_model: Option<String>,
        reasoning_effort: Option<String>,
        resume: Transcript,
    ) -> Result<LiveLead, String> {
        // An effort applies to an explicitly chosen model only.
        let reasoning_effort = reasoning_effort.filter(|_| spawn_model.is_some());
        let engine = self
            .engine
            .lock()
            .await
            .clone()
            .ok_or_else(|| "the headless engine is not running".to_string())?;
        let team = self.team.lock().await.clone();
        let host_tools = team
            .as_ref()
            .map(|team| team.lead_host_tools())
            .unwrap_or_default();
        let session = engine
            .spawn(SpawnOptions {
                name: Some("Lead".to_string()),
                model: spawn_model.clone(),
                reasoning_effort: reasoning_effort.clone(),
                worktree: None,
                permissions: None,
                policy: self.policy.clone(),
                resume: Some(resume),
                limits: None,
                host_tools,
                external_policy: None,
                guardrails: None,
                // Plan c3ba2b58 step B: the lead's trace identity — the
                // spawn path fills the allocated agent/session ids and
                // stamps it on the loop, so every lead call's record
                // carries `actor_kind: lead` and the display name.
                trace_identity: Some(mnemo::provider::trace::TraceIdentity {
                    actor_kind: mnemo::provider::trace::TraceActorKind::Lead,
                    agent_id: String::new(),
                    session_id: String::new(),
                    agent_name: "Lead".to_string(),
                    worker_id: None,
                    plan_id: None,
                    plan_step: None,
                }),
                // Plan 40eef714 step 1: the lead session carries no
                // pinned task.
                pinned_task: None,
                // Plan 2ba2168a step 1: the lead session's turns are never
                // classified by the per-prompt gate — Mnemo2 routes at
                // dispatch instead.
                skip_turn_routing: true,
            })
            .await
            .map_err(|err| format!("could not spawn the lead session: {err}"))?;
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(false));
        let observed_model = Arc::new(Mutex::new(
            selected_model
                .as_deref()
                .and_then(|spec| self.models().into_iter().find(|entry| entry.spec == spec))
                .map(|entry| entry.model),
        ));
        let model = Arc::new(Mutex::new(selected_model));
        let details = PendingDetails::default();
        let pump = tokio::spawn(pump_events(
            generation,
            self.engine.clone(),
            session_id,
            session.events(),
            self.events.clone(),
            self.persistence.clone(),
            busy.clone(),
            model.clone(),
            reasoning_effort.clone(),
            observed_model.clone(),
            details.clone(),
            self.generation.clone(),
            team.clone(),
        ));
        let notifications = if let Some(team) = team {
            team.attach_lead(session_id).await;
            Some(tokio::spawn(pump_team_notifications(
                self.engine.clone(),
                session_id,
                Arc::downgrade(&team),
                team.subscribe(),
                team.subscribe_board(),
            )))
        } else {
            None
        };
        Ok(LiveLead {
            generation,
            session,
            busy,
            model,
            reasoning_effort,
            observed_model,
            details,
            pump,
            notifications,
        })
    }
}

/// Answer a lead host-tool call with its result.
async fn send_lead_host_result(
    engine: &Arc<Mutex<Option<Arc<Engine>>>>,
    session_id: SessionId,
    call_id: String,
    result: ToolResult,
) {
    let engine = engine.lock().await.clone();
    if let Some(engine) = engine {
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
}

async fn pump_team_notifications(
    engine: Arc<Mutex<Option<Arc<Engine>>>>,
    session_id: SessionId,
    team: std::sync::Weak<TeamRuntime>,
    mut receiver: broadcast::Receiver<crate::mnemo2::team::TeamEvent>,
    mut board_receiver: broadcast::Receiver<BoardSignal>,
) {
    let mut notified = std::collections::HashSet::new();
    loop {
        let event = tokio::select! {
            event = receiver.recv() => match event {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            signal = board_receiver.recv() => {
                match signal {
                    Ok(BoardSignal::Notice { key, text }) => {
                        let Some(team) = team.upgrade() else {
                            return;
                        };
                        if !team.lead_is_current(session_id).await {
                            return;
                        }
                        if notified.contains(&key) {
                            continue;
                        }
                        let text = format!("{TEAM_NOTICE_PREFIX}\n{text}");
                        if deliver_lead_notice(&engine, session_id, &team, text).await {
                            notified.insert(key);
                            if notified.len() > 1024 {
                                notified.clear();
                            }
                        }
                    }
                    Ok(BoardSignal::Changed)
                    | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
                continue;
            }
        };
        let Some(team) = team.upgrade() else {
            return;
        };
        if !team.lead_is_current(session_id).await {
            return;
        }
        let Ok(worker) = team.worker(&event.worker_id).await else {
            continue;
        };
        let (key, detail, state) = match &event.event.event {
            SerializableAgentEvent::HostToolCall {
                call_id,
                name,
                args,
            } if name == "message_parent" => {
                let text = args
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("sent an update");
                (
                    format!("{}:{}:message:{call_id}", worker.id, event.generation),
                    format!("{} reports: {text}", worker.name),
                    None,
                )
            }
            SerializableAgentEvent::ApprovalRequest { tool_call_id, .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::NeedsYou =>
            {
                (
                    format!("{}:{}:approval:{tool_call_id}", worker.id, event.generation),
                    format!("{} is blocked waiting for an approval.", worker.name),
                    Some(NoticeState::Blocked),
                )
            }
            SerializableAgentEvent::UserQuestion { question_id, .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::NeedsYou =>
            {
                (
                    format!("{}:{}:question:{question_id}", worker.id, event.generation),
                    format!("{} is blocked waiting for an answer.", worker.name),
                    Some(NoticeState::Blocked),
                )
            }
            SerializableAgentEvent::Parked { .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::NeedsYou =>
            {
                (
                    format!(
                        "{}:{}:parked:{}",
                        worker.id, event.generation, worker.last_activity_ms
                    ),
                    match worker.error.as_deref() {
                        Some(reason) => format!("{} is blocked: {reason}", worker.name),
                        None => format!(
                            "{} parked before finishing; review its actual state.",
                            worker.name
                        ),
                    },
                    Some(NoticeState::Blocked),
                )
            }
            SerializableAgentEvent::Error {
                error,
                retrying: false,
            } if worker.status == crate::mnemo2::team::WorkerStatus::Failed => (
                format!(
                    "{}:{}:error:{}",
                    worker.id, event.generation, worker.last_activity_ms
                ),
                format!(
                    "{} failed: {error}.{}\nTo continue on the same model, team_message it. To retry on another model, use team_resume with worker_id={} and a `tier` or `model`, then team_message it to continue.",
                    worker.name,
                    crate::mnemo2::team::tool_errors_text(&worker),
                    worker.id
                ),
                Some(NoticeState::Failed),
            ),
            // A turn that ended Completed: with the lead idling on the turn-end
            // Stop, this is what wakes it to review the work. The turn count
            // keeps each finished turn's notice distinct.
            SerializableAgentEvent::Finished { .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::Completed
                    && worker.kind != "audit" =>
            {
                (
                    format!(
                        "{}:{}:finished:{}",
                        worker.id, event.generation, worker.turns
                    ),
                    finished_notice(&worker),
                    Some(NoticeState::Finished),
                )
            }
            SerializableAgentEvent::Finished { .. } if worker.kind == "audit" => {
                // Audit completions surface as an explicit adjudication
                // request for the target review — implementation workers
                // keep the message-driven flow unchanged.
                let target = worker.audit_target.clone().unwrap_or_default();
                let tip = worker.audit_tip.as_deref().unwrap_or("(unknown)");
                (
                    format!("{}:{}:finished", worker.id, event.generation),
                    format!(
                        "{} ({}) finished its read-only audit of worker {} at tip {}. Adjudicate the audit: record team_review on {} with independent_reviewer_id='{}' and a written audit_adjudication. Never merge the audit worker itself.",
                        worker.name, worker.id, target, tip, target, worker.id
                    ),
                    None,
                )
            }
            _ => continue,
        };
        if notified.contains(&key) {
            continue;
        }
        let tag = state.map(|state| TurnTag::of(&worker, event.generation, state).suffix());
        let text = format!(
            "{TEAM_NOTICE_PREFIX} worker_id={}{}\n{detail}\nUse team_status with this exact worker_id. Reply with team_message, or use team_stop/team_compact as appropriate. Never infer lifecycle from Git, files, timestamps, or team.json.",
            worker.id,
            tag.unwrap_or_default(),
        );
        if deliver_lead_notice(&engine, session_id, &team, text).await {
            notified.insert(key);
            if notified.len() > 1024 {
                notified.clear();
            }
        }
    }
}

/// The lead notice for a worker turn that ended Completed: its plan step and
/// the next move toward a merge (review, close findings, or merge).
fn finished_notice(worker: &crate::mnemo2::team::WorkerRecord) -> String {
    let step = match (&worker.plan_id, worker.plan_step) {
        (Some(plan), Some(step)) => format!(" on plan {plan} step {}", step + 1),
        _ => String::new(),
    };
    let open = worker
        .findings
        .iter()
        .filter(|f| f.status == "open")
        .count();
    let next = if worker.review.is_none() {
        "Review it now: team_check, then team_review. When the review passes and no finding is open, team_merge it (the merge ticks its step).".to_string()
    } else if open > 0 {
        format!("Its tip is reviewed but {open} finding(s) are open: verify the fix and resolve them (team_resolve_finding), then team_merge.")
    } else {
        "Its tip is reviewed with no open findings: it is ready for team_merge (the merge ticks its step).".to_string()
    };
    format!("{} finished its turn{step}. {next}", worker.name)
}

/// Send a TeamManager notice to the lead: a steer while it works, a prompt
/// when it idles. `true` when the session accepted it. While an external lead
/// (Claude Code over MCP) leads the team, the notice joins its queue instead
/// and the in-app lead is left alone. The queue push decides under one lock
/// whether an external lead is attached, so a detach in between sends the
/// notice to the in-app lead rather than losing it.
async fn deliver_lead_notice(
    engine: &Arc<Mutex<Option<Arc<Engine>>>>,
    session_id: SessionId,
    team: &TeamRuntime,
    text: String,
) -> bool {
    if team.push_external_notice(text.clone()) {
        return true;
    }
    // Notices an earlier external lead never read go first, in one message.
    let held = team.current_notices(team.take_undelivered_notices()).await;
    if held.is_empty() {
        return send_to_in_app_lead(engine, session_id, team, text).await;
    }
    let combined = format!("{}\n\n{text}", held.join("\n\n"));
    if send_to_in_app_lead(engine, session_id, team, combined).await {
        return true;
    }
    team.restore_undelivered_notices(held);
    false
}

/// Give the in-app lead session a notice: a steer while it works, a prompt
/// when it idles. `true` when the session accepted it.
async fn send_to_in_app_lead(
    engine: &Arc<Mutex<Option<Arc<Engine>>>>,
    session_id: SessionId,
    team: &TeamRuntime,
    text: String,
) -> bool {
    let engine = engine.lock().await.clone();
    let Some(engine) = engine.as_ref() else {
        return false;
    };
    let Some(session) = engine.session(&session_id).await else {
        return false;
    };
    let command = if team.lead_is_busy() {
        SessionCommand::Steer {
            text,
            images: Vec::new(),
        }
    } else {
        SessionCommand::Prompt {
            text,
            images: Vec::new(),
        }
    };
    match session.send(command).await {
        Ok(()) => true,
        Err(error) => {
            eprintln!("mnemo2: could not deliver a notice to the lead: {error}");
            false
        }
    }
}

// Each parameter is one shared lead handle the pump observes — the wiring stays flat.
#[allow(clippy::too_many_arguments)]
async fn pump_events(
    generation: u64,
    engine: Arc<Mutex<Option<Arc<Engine>>>>,
    session_id: SessionId,
    mut receiver: EventReceiver<SessionEvent>,
    events: broadcast::Sender<LeadEvent>,
    persistence: LeadPersistence,
    busy: Arc<AtomicBool>,
    model: Arc<Mutex<Option<String>>>,
    reasoning_effort: Option<String>,
    observed_model: Arc<Mutex<Option<String>>>,
    details: PendingDetails,
    active_generation: Arc<AtomicU64>,
    team: Option<Arc<TeamRuntime>>,
) {
    // Board tool calls in flight (call id), so their results can refresh the
    // Plan tab.
    let mut board_calls = std::collections::HashSet::<String>::new();
    loop {
        let event = match receiver.recv().await {
            Ok(event) => event,
            // A slow consumer skips what it missed rather than ending the
            // lead's only event stream.
            Err(mnemo::headless::RecvError::Lagged(_)) => continue,
            Err(mnemo::headless::RecvError::Closed) => break,
        };
        // The engine bus is shared with every worker session: only the
        // lead's own events may drive its state, answer its host tools, or
        // reach the lead UI.
        if !event.is_from(session_id) {
            continue;
        }
        match &event.event {
            SerializableAgentEvent::ApprovalRequest {
                tool_call_id,
                tool_name,
                ..
            } => {
                details.lock().await.insert(
                    tool_call_id.clone(),
                    PendingDetail {
                        tool_name: Some(tool_name.clone()),
                        ..PendingDetail::default()
                    },
                );
            }
            SerializableAgentEvent::UserQuestion {
                question_id,
                question,
                options,
            } => {
                details.lock().await.insert(
                    question_id.clone(),
                    PendingDetail {
                        question: Some(question.clone()),
                        options: options.clone(),
                        tool_name: None,
                    },
                );
            }
            _ => {}
        }
        match &event.event {
            SerializableAgentEvent::ToolCallStart { id, name, .. } if name.starts_with("plan_") => {
                board_calls.insert(id.clone());
            }
            SerializableAgentEvent::ToolResult { tool_call_id, .. }
                if board_calls.remove(tool_call_id) =>
            {
                if let Some(team) = &team {
                    team.signal_board_changed();
                }
            }
            _ => {}
        }
        if let SerializableAgentEvent::HostToolCall {
            call_id,
            name,
            args,
        } = &event.event
        {
            let result = match &team {
                Some(team) => {
                    team.handle_lead_host_tool_from(name, args.clone(), MessageSender::Lead)
                        .await
                }
                None => ToolResult::error("the team runtime is not available"),
            };
            send_lead_host_result(&engine, session_id, call_id.clone(), result).await;
        }
        if matches!(
            &event.event,
            SerializableAgentEvent::Finished { .. }
                | SerializableAgentEvent::Exited
                | SerializableAgentEvent::Parked { .. }
                | SerializableAgentEvent::Error {
                    retrying: false,
                    ..
                }
        ) {
            details.lock().await.clear();
        }
        let terminal = match &event.event {
            SerializableAgentEvent::Started => {
                busy.store(true, Ordering::SeqCst);
                if let Some(team) = &team {
                    team.set_lead_busy(true);
                }
                false
            }
            SerializableAgentEvent::Finished { .. }
            | SerializableAgentEvent::Exited
            | SerializableAgentEvent::Parked { .. } => {
                busy.store(false, Ordering::SeqCst);
                if let Some(team) = &team {
                    team.set_lead_busy(false);
                }
                true
            }
            SerializableAgentEvent::Error { retrying, .. } => {
                if !retrying {
                    busy.store(false, Ordering::SeqCst);
                    if let Some(team) = &team {
                        team.set_lead_busy(false);
                    }
                }
                false
            }
            // A deferred compact-then-switch completed on the lead's own
            // session: informational — the turn continues on the new model, so
            // the lead stays busy and no error is recorded (bug 2026-11-02,
            // plan 1238e0de: this used to arrive as an `Error`).
            SerializableAgentEvent::ModelSwapSummarized { .. } => false,
            // Mnemo1-only informational notice; the lead pump ignores it.
            SerializableAgentEvent::RoutingNotice { .. } => false,
            SerializableAgentEvent::ModelChanged { model: changed, .. } => {
                // The engine's bus is engine-wide: only OUR session's
                // model change may re-point the lead's observed model
                // (and its next usage row) — a worker's ModelChanged must
                // not cross into it.
                if event_session(&event) == Some(session_id) {
                    *observed_model.lock().await = Some(changed.clone());
                }
                false
            }
            SerializableAgentEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                ..
            } => {
                // Fold the lead's own usage into the durable per-project
                // accumulator, keyed by the actual observed model id (the
                // spec-derived id until the session reports its own). The
                // engine's bus is engine-wide, so fold only OUR session's
                // report — worker events are folded by their own pumps.
                if event_session(&event) == Some(session_id) {
                    let model = observed_model.lock().await.clone();
                    if let Some(team) = &team {
                        let _ = team
                            .record_usage(
                                model.as_deref(),
                                *prompt_tokens as u64,
                                *cached_tokens as u64,
                                *completion_tokens as u64,
                            )
                            .await;
                    }
                }
                false
            }
            _ => false,
        };
        let _ = events.send(LeadEvent {
            generation,
            event: event.clone(),
        });
        if terminal {
            let engine = engine.clone();
            let persistence = persistence.clone();
            let model = model.clone();
            let reasoning_effort = reasoning_effort.clone();
            let active_generation = active_generation.clone();
            tokio::spawn(async move {
                if active_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let engine = engine.lock().await.clone();
                let handle = match engine {
                    Some(engine) => engine.session(&session_id).await,
                    None => None,
                };
                if let Some(handle) = handle {
                    if let Ok(history) = handle.history().await {
                        let model = model.lock().await.clone();
                        let _ = persistence.save(model, reasoning_effort, history);
                    }
                }
            });
        }
    }
}

/// Keep the shared project caches (`memory.db`, `codegraph.db`, logs, ...) out
/// of `git status` through the repository's local `info/exclude`, so the
/// lead's clean-checkout merge gate never trips on them. Mnemo1 gets the same
/// rules from the `.gitignore` its `Project::init` merges. Best effort: a
/// project that is not a Git checkout (or an unwritable exclude file) is left
/// alone.
fn ignore_project_caches(root: &Path) {
    let Ok(output) = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if reported.is_empty() {
        return;
    }
    let exclude = root.join(reported);
    let current = std::fs::read_to_string(&exclude).unwrap_or_default();
    // The one-time knowledge migration marker is also per-machine state; so
    // are the memory check pass's report and its rename cache (plan 373dec00
    // step 5) — generated per checkout, never committed.
    let missing: Vec<&str> = mnemo::project::scaffold::MANAGED_GITIGNORE_ENTRIES
        .iter()
        .copied()
        .chain([
            ".coding/knowledge/.migration-*",
            ".coding/knowledge/HEALTH.md",
            ".coding/knowledge/.health-state.json",
        ])
        .filter(|entry| !current.lines().any(|line| line.trim() == *entry))
        .collect();
    if missing.is_empty() {
        return;
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str("# Mnemo project caches\n");
    for entry in missing {
        next.push_str(entry);
        next.push('\n');
    }
    if let Some(parent) = exclude.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&exclude, next);
}

fn load_models(config_dir: &Path) -> Result<(Vec<LeadModel>, Option<String>), String> {
    let config = Config::load(config_dir)
        .map_err(|err| format!("could not load the Mnemo configuration: {err}"))?;
    let default_spec = configured_default(&config);
    let mut models = Vec::new();
    for endpoint in &config.endpoints {
        for model in &endpoint.models {
            let spec = format!("{}/{}", endpoint.name, model.id);
            let reasoning_efforts = endpoint.reasoning_efforts_for(Some(&model.id));
            let default_effort = (!reasoning_efforts.is_empty())
                .then(|| endpoint.display_reasoning_effort_for(Some(&model.id)));
            models.push(LeadModel {
                endpoint: endpoint.name.clone(),
                model: model.id.clone(),
                is_default: default_spec.as_deref() == Some(spec.as_str()),
                spec,
                reasoning_efforts,
                default_effort,
            });
        }
    }
    let default = default_spec.filter(|spec| models.iter().any(|model| &model.spec == spec));
    Ok((models, default))
}

fn configured_default(config: &Config) -> Option<String> {
    let model = config.general.general.default_model.clone()?;
    if model.contains('/') {
        return Some(model);
    }
    if let Some(provider) = config.general.general.default_provider.clone() {
        return Some(format!("{provider}/{model}"));
    }
    config
        .endpoints
        .iter()
        .find(|endpoint| endpoint.models.iter().any(|spec| spec.id == model))
        .map(|endpoint| format!("{}/{}", endpoint.name, model))
}
/// Trace identity tests (plan c3ba2b58 step B) — in their own module so the
/// append-heavy `mod tests` below stays untouched by concurrent work.
#[cfg(test)]
#[path = "runtime/trace_tests.rs"]
mod trace_tests;

#[cfg(test)]
#[path = "runtime/tests.rs"]
mod tests;
