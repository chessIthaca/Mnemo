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

use mnemo::app::policy_factory::WorkflowPolicyFactory;
use mnemo::config::{Config, SafetyMode};
use mnemo::headless::{
    AgentPlan, AgentSession, Engine, EngineOptions, EventReceiver, MemoryBackend,
    PendingInteraction, SessionCommand, SessionEvent, SessionId, SpawnOptions, Transcript,
};
use mnemo::provider::LlmClient;
use mnemo::tool::ToolResult;
use mnemo::workspace::Workspace;
use mnemo::{Approval, PermissionPolicy, SerializableAgentEvent, UserAnswer};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, DEFAULT_SPAWNS_PER_TURN};
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
    pub pending: Option<PendingInteraction>,
    /// Whether a turn is currently running.
    pub busy: bool,
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

    fn save(&self, model: Option<String>, transcript: Transcript) -> Result<(), String> {
        let lead = PersistedLead {
            model,
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
    observed_model: Arc<Mutex<Option<String>>>,
    pump: JoinHandle<()>,
}

impl LiveLead {
    fn abort(&self) {
        self.pump.abort();
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
    models: Vec<LeadModel>,
    default_model: Option<String>,
    policy: Option<String>,
    team: Mutex<Option<Arc<TeamRuntime>>>,
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
        let spawn_model = options.model.clone().or(persisted_model);
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
            models,
            default_model: configured_default,
            policy,
            team: Mutex::new(None),
        };
        runtime.start_engine(provider).await?;
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: runtime.engine_handle().await?,
            workspace_root: runtime.workspace_root.clone(),
            state_dir: runtime.state_dir.clone(),
            slots: None,
            max_spawns_per_turn: DEFAULT_SPAWNS_PER_TURN,
            model_specs: runtime.models.iter().map(|model| model.spec.clone()).collect(),
            pricing: runtime.pricing()?,
        })
        .await?;
        *runtime.team.lock().await = Some(Arc::new(team));
        let live = runtime
            .spawn_live(1, selected, spawn_model, persisted.transcript)
            .await?;
        *runtime.live.lock().await = Some(live);
        Ok(runtime)
    }

    /// Subscribe to the live lead event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<LeadEvent> {
        self.events.subscribe()
    }

    /// The configured model inventory.
    pub fn models(&self) -> &[LeadModel] {
        &self.models
    }

    /// The configured default model spec.
    pub fn default_model(&self) -> Option<String> {
        self.default_model.clone()
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
            .or_else(|| snapshot.model);
        let model_id = observed.or_else(|| {
            selected
                .as_deref()
                .and_then(|spec| self.models.iter().find(|model| model.spec == spec))
                .map(|model| model.model.clone())
        });
        Ok(LeadSnapshot {
            generation: live.generation,
            model: selected,
            model_id,
            models: self.models.clone(),
            default_model: self.default_model.clone(),
            transcript: snapshot.transcript,
            plan: live
                .session
                .plan()
                .await
                .map_err(|err| format!("could not read the lead plan: {err}"))?,
            used_tokens: snapshot.used_tokens,
            max_tokens: snapshot.max_tokens,
            pending: snapshot.pending,
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
        if live.busy.load(Ordering::SeqCst) {
            // Busy: not an error and not a second prompt — a queued
            // steer. `begin_lead_turn` is skipped (the lead is already
            // busy, so the merge gate is already closed) and the busy
            // flag stays with the running turn.
            return live
                .session
                .send(SessionCommand::Steer { text, images })
                .await
                .map_err(|err| format!("could not steer the busy lead: {err}"));
        }
        if let Some(team) = team {
            team.begin_lead_turn();
        }
        live.busy.store(true, Ordering::SeqCst);
        live.session
            .send(SessionCommand::Prompt { text, images })
            .await
            .map_err(|err| {
                live.busy.store(false, Ordering::SeqCst);
                format!("could not send the prompt: {err}")
            })
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

    /// Resolve a pending approval.
    pub async fn approve(&self, request_id: String, decision: Approval) -> Result<(), String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        live.session
            .send(SessionCommand::Approve {
                request_id,
                decision,
            })
            .await
            .map_err(|err| format!("could not answer the approval: {err}"))
    }

    /// Resolve a pending `ask_user` question.
    pub async fn answer(&self, question_id: String, answer: UserAnswer) -> Result<(), String> {
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        live.session
            .send(SessionCommand::Answer {
                question_id,
                answer,
            })
            .await
            .map_err(|err| format!("could not answer the question: {err}"))
    }

    /// Change the lead's model while it is idle.
    ///
    /// The headless command surface has no `SetModel`, so this saves the
    /// transcript, closes the old session, and resumes a fresh one on the new
    /// model. The generation advances so stale events cannot touch the new UI.
    pub async fn set_model(&self, spec: String) -> Result<LeadSnapshot, String> {
        if !self.models.iter().any(|model| model.spec == spec) {
            return Err(format!("'{spec}' is not a configured model"));
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
            let old = guard.take().expect("live session checked above");
            old
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
            .save(Some(spec), snapshot.transcript)
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

    /// Stop the session and discard the engine handles.
    pub async fn shutdown(&self) {
        if let Some(team) = self.team.lock().await.take() {
            team.shutdown().await;
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
        let workspace = Workspace::with_state_dir(
            self.workspace_root.clone(),
            self.state_dir.join("workspace"),
        );
        let options = EngineOptions {
            workspace,
            config_dir: Some(self.config_dir.clone()),
            permissions: PermissionPolicy::ApproveViaApi {
                safety_mode: SafetyMode::ApproveEachAction,
            },
            memory: MemoryBackend::Disabled,
            index_codegraph: false,
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
        engine.register_policy(
            "mnemo.workflow",
            Arc::new(
                WorkflowPolicyFactory::default()
                    .with_blocked_tools(vec!["spawn_agent".to_string()]),
            ),
        );
        *self.engine.lock().await = Some(Arc::new(engine));
        Ok(())
    }

    async fn spawn_live(
        &self,
        generation: u64,
        selected_model: Option<String>,
        spawn_model: Option<String>,
        resume: Transcript,
    ) -> Result<LiveLead, String> {
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
                worktree: None,
                permissions: None,
                policy: self.policy.clone(),
                resume: Some(resume),
                limits: None,
                host_tools,
                external_policy: None,
                guardrails: None,
            })
            .await
            .map_err(|err| format!("could not spawn the lead session: {err}"))?;
        let session_id = session.id();
        let busy = Arc::new(AtomicBool::new(false));
        let observed_model = Arc::new(Mutex::new(
            selected_model
                .as_deref()
                .and_then(|spec| self.models.iter().find(|entry| entry.spec == spec))
                .map(|entry| entry.model.clone()),
        ));
        let model = Arc::new(Mutex::new(selected_model));
        let pump = tokio::spawn(pump_events(
            generation,
            self.engine.clone(),
            session_id,
            session.events(),
            self.events.clone(),
            self.persistence.clone(),
            busy.clone(),
            model.clone(),
            observed_model.clone(),
            self.generation.clone(),
            team.clone(),
        ));
        if let Some(team) = team {
            team.attach_lead(session_id).await;
            tokio::spawn(pump_team_notifications(
                self.engine.clone(),
                session_id,
                Arc::downgrade(&team),
                team.subscribe(),
            ));
        }
        Ok(LiveLead {
            generation,
            session,
            busy,
            model,
            observed_model,
            pump,
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
) {
    let mut notified = std::collections::HashSet::new();
    while let Ok(event) = receiver.recv().await {
        let Some(team) = team.upgrade() else {
            return;
        };
        if !team.lead_is_current(session_id).await {
            return;
        }
        let Ok(worker) = team.worker(&event.worker_id).await else {
            continue;
        };
        let (key, detail) = match &event.event.event {
            SerializableAgentEvent::HostToolCall { call_id, name, args }
                if name == "message_parent" =>
            {
                let text = args
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("sent an update");
                (
                    format!("{}:{}:message:{call_id}", worker.id, event.generation),
                    format!("{} reports: {text}", worker.name),
                )
            }
            SerializableAgentEvent::ApprovalRequest { tool_call_id, .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::NeedsYou =>
            {
                (
                    format!("{}:{}:approval:{tool_call_id}", worker.id, event.generation),
                    format!("{} is blocked waiting for an approval.", worker.name),
                )
            }
            SerializableAgentEvent::UserQuestion { question_id, .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::NeedsYou =>
            {
                (
                    format!("{}:{}:question:{question_id}", worker.id, event.generation),
                    format!("{} is blocked waiting for an answer.", worker.name),
                )
            }
            SerializableAgentEvent::Parked { .. }
                if worker.status == crate::mnemo2::team::WorkerStatus::NeedsYou =>
            {
                (
                    format!("{}:{}:parked:{}", worker.id, event.generation, worker.last_activity_ms),
                    format!("{} is blocked by an execution limit; review its actual state.", worker.name),
                )
            }
            SerializableAgentEvent::Error { error, retrying: false }
                if worker.status == crate::mnemo2::team::WorkerStatus::Failed =>
            {
                (
                    format!("{}:{}:error:{}", worker.id, event.generation, worker.last_activity_ms),
                    format!("{} failed: {error}", worker.name),
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
                )
            }
            _ => continue,
        };
        if notified.contains(&key) {
            continue;
        }
        let text = format!(
            "{TEAM_NOTICE_PREFIX} worker_id={}\n{detail}\nUse team_status with this exact worker_id. Reply with team_message, or use team_stop/team_compact as appropriate. Never infer lifecycle from Git, files, timestamps, or team.json.",
            worker.id
        );
        let engine = engine.lock().await.clone();
        let Some(session) = engine.as_ref() else {
            continue;
        };
        let Some(session) = session.session(&session_id).await else {
            continue;
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
            Ok(()) => {
                notified.insert(key);
                if notified.len() > 1024 {
                    notified.clear();
                }
            }
            Err(error) => eprintln!(
                "mnemo2: could not deliver worker '{}' notice to the lead: {error}",
                worker.id
            ),
        }
    }
}

async fn pump_events(
    generation: u64,
    engine: Arc<Mutex<Option<Arc<Engine>>>>,
    session_id: SessionId,
    mut receiver: EventReceiver<SessionEvent>,
    events: broadcast::Sender<LeadEvent>,
    persistence: LeadPersistence,
    busy: Arc<AtomicBool>,
    model: Arc<Mutex<Option<String>>>,
    observed_model: Arc<Mutex<Option<String>>>,
    active_generation: Arc<AtomicU64>,
    team: Option<Arc<TeamRuntime>>,
) {
    while let Ok(event) = receiver.recv().await {
        if let SerializableAgentEvent::HostToolCall {
            call_id,
            name,
            args,
        } = &event.event
        {
            let result = match &team {
                Some(team) => team.handle_lead_host_tool(name, args.clone()).await,
                None => ToolResult::error("the team runtime is not available"),
            };
            send_lead_host_result(&engine, session_id, call_id.clone(), result).await;
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
                        let _ = persistence.save(model, history);
                    }
                }
            });
        }
    }
}

fn load_models(config_dir: &Path) -> Result<(Vec<LeadModel>, Option<String>), String> {
    let config = Config::load(config_dir)
        .map_err(|err| format!("could not load the Mnemo configuration: {err}"))?;
    let default_spec = configured_default(&config);
    let mut models = Vec::new();
    for endpoint in &config.endpoints {
        for model in &endpoint.models {
            let spec = format!("{}/{}", endpoint.name, model.id);
            models.push(LeadModel {
                endpoint: endpoint.name.clone(),
                model: model.id.clone(),
                is_default: default_spec.as_deref() == Some(spec.as_str()),
                spec,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use mnemo::provider::{
        Capabilities, FinishReason, LlmClient, LlmEvent, Message, MessageContent, ProviderKind,
        ToolChoice, ToolSchema,
    };
    use tempfile::TempDir;

    /// One scripted turn: the events to emit, optionally held inside
    /// `complete` until the test releases a gate.
    struct ScriptedTurn {
        gate: Option<tokio::sync::oneshot::Receiver<()>>,
        events: Vec<LlmEvent>,
    }

    struct ScriptedLlm {
        responses: std::sync::Mutex<VecDeque<ScriptedTurn>>,
        calls: std::sync::atomic::AtomicUsize,
        advertised_tools: std::sync::Mutex<Vec<Vec<String>>>,
        recorded_messages: std::sync::Mutex<Vec<Vec<Message>>>,
        caps: Capabilities,
        models: Vec<std::sync::Arc<str>>,
        active_model: std::sync::atomic::AtomicUsize,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<Vec<LlmEvent>>) -> Self {
            Self::new_with_model("scripted-a", responses)
        }

        /// A scripted provider that reports the given model id — what a
        /// session's ModelChanged events and per-model usage rows observe.
        fn new_with_model(model: &str, responses: Vec<Vec<LlmEvent>>) -> Self {
            Self::new_gated(vec![model], responses.into_iter().map(|events| (None, events)).collect())
        }

        /// A scripted provider over a table of model names (see
        /// [`ScriptedLlm::set_model`]) whose turns can be held inside
        /// `complete` until the test releases them — the interleaving seam
        /// for cross-session model-attribution tests.
        fn new_gated(
            models: Vec<&str>,
            turns: Vec<(Option<tokio::sync::oneshot::Receiver<()>>, Vec<LlmEvent>)>,
        ) -> Self {
            Self {
                responses: std::sync::Mutex::new(
                    turns
                        .into_iter()
                        .map(|(gate, events)| ScriptedTurn { gate, events })
                        .collect(),
                ),
                calls: std::sync::atomic::AtomicUsize::new(0),
                advertised_tools: std::sync::Mutex::new(Vec::new()),
                recorded_messages: std::sync::Mutex::new(Vec::new()),
                caps: Capabilities::openai(),
                models: models.into_iter().map(std::sync::Arc::from).collect(),
                active_model: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        /// Point `model()` at table entry `index` — the next request's
        /// ModelChanged carries the new name.
        fn set_model(&self, index: usize) {
            self.active_model
                .store(index, std::sync::atomic::Ordering::SeqCst);
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }

        /// Report a multimodal capability set — the flag image tests
        /// need so pasted images ride the request as image blocks
        /// instead of the vision-model fallback.
        fn multimodal(mut self) -> Self {
            self.caps.multimodal = true;
            self
        }

        /// The full conversation each `complete` call saw, oldest first —
        /// how tests assert an image block reached the provider.
        fn recorded_messages(&self) -> Vec<Vec<Message>> {
            self.recorded_messages
                .lock()
                .expect("recorded message lock poisoned")
                .clone()
        }

        fn advertised_tools(&self) -> Vec<Vec<String>> {
            self.advertised_tools
                .lock()
                .expect("advertised tool lock poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedLlm {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }

        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }

        fn model(&self) -> &str {
            &self.models[self.active_model.load(std::sync::atomic::Ordering::SeqCst)]
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
            self.advertised_tools
                .lock()
                .expect("advertised tool lock poisoned")
                .push(_tools.iter().map(|tool| tool.name.clone()).collect());
            self.recorded_messages
                .lock()
                .expect("recorded message lock poisoned")
                .push(_messages.to_vec());
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let next = self
                .responses
                .lock()
                .expect("script lock poisoned")
                .pop_front()
                .map(|turn| (turn.gate, turn.events))
                .unwrap_or((None, Vec::new()));
            let (gate, events) = next;
            if let Some(gate) = gate {
                // Hold the request mid-turn: the session's ModelChanged is
                // already on the bus, but its usage events are not.
                let _ = gate.await;
            }
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    struct SlowLlm {
        caps: Capabilities,
    }

    impl SlowLlm {
        fn new() -> Self {
            Self {
                caps: Capabilities::openai(),
            }
        }
    }

    #[async_trait]
    impl LlmClient for SlowLlm {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }

        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }

        fn model(&self) -> &str {
            "slow"
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
            Ok(Box::pin(futures::stream::unfold(0u8, |step| async move {
                match step {
                    0 => Some((
                        LlmEvent::TextDelta {
                            text: "slow".into(),
                        },
                        1,
                    )),
                    1 => {
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        Some((
                            LlmEvent::Finish {
                                reason: FinishReason::Stop,
                            },
                            2,
                        ))
                    }
                    _ => None,
                }
            })))
        }
    }

    fn text_turn(texts: &[&str]) -> Vec<LlmEvent> {
        let mut events = texts
            .iter()
            .map(|text| LlmEvent::TextDelta {
                text: (*text).into(),
            })
            .collect::<Vec<_>>();
        events.push(LlmEvent::Finish {
            reason: FinishReason::Stop,
        });
        events
    }

    /// Complete structured review evidence for lead-review call sites.
    fn review_evidence() -> crate::mnemo2::phase3::ReviewEvidence {
        crate::mnemo2::phase3::ReviewEvidence {
            unanchored_pass: true,
            intent: "the worker implements the scoped brief".into(),
            execution: "the change flows through the documented path".into(),
            integration: "the module fits the surrounding runtime".into(),
            test_quality: "the regression reproduces the defect before the fix".into(),
            residual_risk: "no residual risk beyond the reviewed diff".into(),
            independent_reviewer_id: None,
            audit_adjudication: None,
        }
    }

    fn usage_turn(prompt_tokens: u32, completion_tokens: u32, cached_tokens: u32) -> Vec<LlmEvent> {
        vec![
            LlmEvent::Usage {
                prompt_tokens,
                completion_tokens,
                reasoning_tokens: 0,
                cached_tokens,
                ttft_ms: None,
                generation_ms: None,
            },
            LlmEvent::Finish {
                reason: FinishReason::Stop,
            },
        ]
    }

    /// A usage turn that also carries assistant text — the shape a real
    /// response has, so a persisted-then-resumed lead transcript ends with a
    /// well-formed assistant reply.
    fn usage_text_turn(
        prompt_tokens: u32,
        completion_tokens: u32,
        cached_tokens: u32,
        text: &str,
    ) -> Vec<LlmEvent> {
        let mut events = usage_turn(prompt_tokens, completion_tokens, cached_tokens);
        events.insert(
            events.len() - 1,
            LlmEvent::TextDelta {
                text: text.to_string(),
            },
        );
        events
    }

    fn tool_turn(name: &str, args: &str) -> Vec<LlmEvent> {
        vec![
            LlmEvent::ToolCallStart {
                index: 0,
                id: "call_1".into(),
                name: name.into(),
            },
            LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: args.into(),
            },
            LlmEvent::Finish {
                reason: FinishReason::ToolCalls,
            },
        ]
    }

    fn write_config(dir: &Path, models: &[&str]) {
        std::fs::create_dir_all(dir).expect("config dir");
        std::fs::write(
            dir.join("config.toml"),
            format!(
                "[general]\ndefault_provider = \"test\"\ndefault_model = \"{}\"\n",
                models[0]
            ),
        )
        .expect("config.toml");
        let models = models
            .iter()
            .map(|model| format!("\"{model}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            dir.join("endpoints.toml"),
            format!(
                "[[endpoint]]\nname = \"test\"\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [{models}]\n"
            ),
        )
        .expect("endpoints.toml");
    }

    fn options(root: &TempDir, config: &TempDir, state: &TempDir) -> LeadRuntimeOptions {
        LeadRuntimeOptions {
            workspace_root: root.path().to_path_buf(),
            config_dir: Some(config.path().to_path_buf()),
            state_dir: state.path().to_path_buf(),
            model: None,
        }
    }

    async fn open_git_team(
        root: &TempDir,
        config: &TempDir,
        state: &TempDir,
        llm: Arc<dyn LlmClient>,
    ) -> (LeadRuntime, crate::mnemo2::team::TeamRuntime) {
        open_git_team_with_pricing(root, config, state, llm, Vec::new()).await
    }

    async fn open_git_team_with_pricing(
        root: &TempDir,
        config: &TempDir,
        state: &TempDir,
        llm: Arc<dyn LlmClient>,
        pricing: Vec<mnemo::config::PricingEntry>,
    ) -> (LeadRuntime, crate::mnemo2::team::TeamRuntime) {
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(options(root, config, state), Some(llm))
            .await
            .expect("lead opens");
        let team =
            crate::mnemo2::team::TeamRuntime::open(crate::mnemo2::team::TeamRuntimeOptions {
                engine: lead.engine_handle().await.expect("engine"),
                workspace_root: root.path().to_path_buf(),
                state_dir: state.path().to_path_buf(),
                slots: Some(1),
                max_spawns_per_turn: 1,
                model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
                pricing,
            })
            .await
            .expect("team opens");
        (lead, team)
    }

    /// [`open_git_team`] with configurable concurrency: audit flows need
    /// more than one delegated spawn per lead turn.
    async fn open_git_team_spawns(
        root: &TempDir,
        config: &TempDir,
        state: &TempDir,
        llm: Arc<dyn LlmClient>,
        slots: usize,
        max_spawns_per_turn: usize,
    ) -> (LeadRuntime, crate::mnemo2::team::TeamRuntime) {
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(options(root, config, state), Some(llm))
            .await
            .expect("lead opens");
        let team =
            crate::mnemo2::team::TeamRuntime::open(crate::mnemo2::team::TeamRuntimeOptions {
                engine: lead.engine_handle().await.expect("engine"),
                workspace_root: root.path().to_path_buf(),
                state_dir: state.path().to_path_buf(),
                slots: Some(slots),
                max_spawns_per_turn,
                model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
                pricing: Vec::new(),
            })
            .await
            .expect("team opens");
        (lead, team)
    }

    /// Write a config whose `endpoints.toml` also carries explicit
    /// `[[pricing]]` rows: `(model, input, cached, output)` per 1M tokens.
    fn write_config_with_pricing(
        dir: &Path,
        models: &[&str],
        pricing: &[(&str, f64, f64, f64)],
    ) {
        write_config(dir, models);
        let rows = pricing
            .iter()
            .map(|(model, input, cached, output)| {
                format!(
                    "\n[[pricing]]\nmodel = \"{model}\"\ninput_per_1m = {input}\ncached_per_1m = {cached}\noutput_per_1m = {output}\n"
                )
            })
            .collect::<String>();
        let endpoints = dir.join("endpoints.toml");
        let base = std::fs::read_to_string(&endpoints).expect("endpoints.toml");
        std::fs::write(&endpoints, format!("{base}\n{rows}")).expect("endpoints pricing");
    }

    /// Open a lead together with its own internal team — the single
    /// TeamRuntime the IPC layer drives in production — on a fresh git root.
    /// The config must already be written (see [`write_config`]).
    async fn open_git_lead(
        root: &TempDir,
        config: &TempDir,
        state: &TempDir,
        llm: Arc<dyn LlmClient>,
    ) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        let lead = LeadRuntime::open_inner(options(root, config, state), Some(llm))
            .await
            .expect("lead opens");
        let team = lead.team_handle().await.expect("team handle");
        (lead, team)
    }

    fn commit_worker_file(
        worker: &crate::mnemo2::team::WorkerRecord,
        relative: &str,
        contents: &str,
        message: &str,
    ) {
        let path = worker.worktree.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("worker file directory");
        }
        std::fs::write(path, contents).expect("worker file");
        run_git(&worker.worktree, &["add", "--", relative]);
        run_git(&worker.worktree, &["commit", "-m", message]);
    }

    async fn wait_for_worker_status(
        team: &crate::mnemo2::team::TeamRuntime,
        worker_id: &str,
        status: crate::mnemo2::team::WorkerStatus,
    ) -> crate::mnemo2::team::WorkerRecord {
        // Generous budget: under the parallel test-suite load a spawned
        // session's first provider request can take seconds to start.
        for _ in 0..500 {
            let worker = team.worker(worker_id).await.expect("worker record");
            if worker.status == status {
                return worker;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let worker = team.worker(worker_id).await.expect("worker record");
        panic!("worker did not reach {status:?}: {worker:?}");
    }

    /// Poll until the worker's record carries the given observed model id
    /// (the ModelChanged may land on the bus shortly after the turn ends).
    async fn wait_for_worker_model(
        team: &crate::mnemo2::team::TeamRuntime,
        worker_id: &str,
        model: &str,
    ) -> crate::mnemo2::team::WorkerRecord {
        for _ in 0..250 {
            let worker = team.worker(worker_id).await.expect("worker record");
            if worker.model_id.as_deref() == Some(model) {
                return worker;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let worker = team.worker(worker_id).await.expect("worker record");
        panic!("worker did not observe '{model}': {worker:?}");
    }
    async fn wait_for_calls(llm: &Arc<ScriptedLlm>, expected: usize) {
        for _ in 0..500 {
            if llm.calls() >= expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "the provider only served {} of {expected} requests",
            llm.calls()
        );
    }

    /// One raw model row of the persisted usage accumulator, read from the
    /// project ledger: `(prompt_tokens, cached_tokens, output_tokens)`.
    fn ledger_usage_row(state_dir: &Path, model: &str) -> Option<(u64, u64, u64)> {
        let ledger: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(state_dir.join("team.json")).expect("ledger"),
        )
        .expect("ledger JSON");
        let row = ledger["usage"]["models"][model].as_object()?;
        Some((
            row["prompt_tokens"].as_u64()?,
            row["cached_tokens"].as_u64()?,
            row["output_tokens"].as_u64()?,
        ))
    }

    /// Poll until the persisted ledger carries the given raw model row —
    /// the terminal refresh can surface a worker's Completed status a beat
    /// before its own trailing usage report is folded and saved.
    async fn wait_for_ledger_row(
        state_dir: &Path,
        model: &str,
        expected: (u64, u64, u64),
    ) {
        for _ in 0..250 {
            if ledger_usage_row(state_dir, model) == Some(expected) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "ledger row '{model}' never reached {expected:?}: {:?}",
            ledger_usage_row(state_dir, model)
        );
    }

    async fn next_event(rx: &mut broadcast::Receiver<LeadEvent>) -> LeadEvent {
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .expect("the lead emitted an event")
            .expect("the lead event bus stayed open")
    }

    async fn drain_turn(rx: &mut broadcast::Receiver<LeadEvent>) -> Vec<LeadEvent> {
        let mut events = Vec::new();
        loop {
            let event = next_event(rx).await;
            let finished = matches!(
                &event.event.event,
                SerializableAgentEvent::Finished { .. } | SerializableAgentEvent::Exited
            );
            events.push(event);
            if finished {
                return events;
            }
        }
    }

    /// Poll until the lead reports idle again (a queued steer's follow-up
    /// turn can outlive the interrupt that flushed it).
    async fn wait_until_idle(runtime: &LeadRuntime) {
        for _ in 0..500 {
            if !runtime.snapshot().await.expect("snapshot").busy {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the lead never returned to idle");
    }

    async fn wait_for_persisted(path: &Path) -> PersistedLead {
        for _ in 0..100 {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(saved) = serde_json::from_slice::<PersistedLead>(&bytes) {
                    if !saved.transcript.messages.is_empty() {
                        return saved;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the lead transcript was not persisted: {}", path.display());
    }

    #[tokio::test]
    async fn streams_a_real_engine_turn_and_persists_the_transcript() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["hello ", "world"])]));
        let runtime = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("hi".to_string(), Vec::new())
            .await
            .expect("the prompt is accepted");
        let events = drain_turn(&mut rx).await;
        let text = events
            .iter()
            .filter_map(|event| match &event.event.event {
                SerializableAgentEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "hello world");
        assert_eq!(llm.calls(), 1);
        let snapshot = runtime.snapshot().await.expect("snapshot");
        assert_eq!(snapshot.model.as_deref(), Some("test/scripted-a"));
        assert_eq!(snapshot.model_id.as_deref(), Some("scripted-a"));
        assert!(!snapshot.busy, "a finished turn is idle");
        let saved = wait_for_persisted(&state.path().join(LEAD_STATE_FILE)).await;
        assert_eq!(saved.model.as_deref(), Some("test/scripted-a"));
        assert_eq!(
            saved.transcript.messages.len(),
            snapshot.transcript.messages.len()
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn restores_the_saved_transcript_and_model_on_reopen() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let first = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![text_turn(&["saved"])]))),
        )
        .await
        .expect("the first lead opens");
        let mut rx = first.subscribe();
        first
            .send_prompt("remember this".to_string(), Vec::new())
            .await
            .expect("prompt");
        drain_turn(&mut rx).await;
        wait_for_persisted(&state.path().join(LEAD_STATE_FILE)).await;
        first.shutdown().await;

        let second = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(Vec::new()))),
        )
        .await
        .expect("the lead reopens");
        let snapshot = second.snapshot().await.expect("snapshot");
        assert_eq!(snapshot.model.as_deref(), Some("test/scripted-a"));
        assert!(
            !snapshot.transcript.messages.is_empty(),
            "the saved conversation is restored"
        );
        second.shutdown().await;
    }

    #[tokio::test]
    async fn changes_model_only_while_idle_and_resumes_the_transcript() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a", "scripted-b"]);
        let runtime = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![text_turn(&["first"])]))),
        )
        .await
        .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("first turn".to_string(), Vec::new())
            .await
            .expect("prompt");
        drain_turn(&mut rx).await;
        let before = runtime.snapshot().await.expect("snapshot");
        let switched = runtime
            .set_model("test/scripted-b".to_string())
            .await
            .expect("the model switch succeeds");
        assert_eq!(switched.model.as_deref(), Some("test/scripted-b"));
        assert_eq!(switched.model_id.as_deref(), Some("scripted-b"));
        assert!(switched.generation > before.generation);
        assert_eq!(
            switched.transcript.messages.len(),
            before.transcript.messages.len()
        );
        assert!(runtime
            .set_model("test/not-configured".to_string())
            .await
            .is_err());
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn busy_submission_becomes_a_queued_steer_with_images() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let llm = Arc::new(ScriptedLlm::new_gated(
            vec!["scripted-a"],
            vec![
                (Some(gate_rx), text_turn(&["turn one"])),
                (None, text_turn(&["turn two"])),
            ],
        ));
        let runtime = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("first".to_string(), Vec::new())
            .await
            .expect("the first prompt is accepted");
        assert!(
            runtime.snapshot().await.expect("snapshot").busy,
            "the gated turn is busy"
        );

        // The busy submission: not an error and not a second prompt — a
        // queued steer carrying both the text and the images.
        runtime
            .send_prompt(
                "a steer".to_string(),
                vec!["data:image/png;base64,AAA".to_string()],
            )
            .await
            .expect("a busy submission becomes a queued steer");
        assert!(
            runtime.snapshot().await.expect("snapshot").busy,
            "a queued steer leaves the running turn's busy flag alone"
        );
        assert_eq!(llm.calls(), 1, "the steer waits at the turn boundary");

        // Release the first turn: the steer is injected with its images
        // and runs as the follow-up turn.
        gate_tx.send(()).expect("the first turn is released");
        let mut saw_injected = false;
        let mut finished_turns = 0;
        while finished_turns < 2 {
            let event = next_event(&mut rx).await;
            match event.event.event {
                SerializableAgentEvent::SuggestionInjected { text, images } => {
                    assert_eq!(text, "a steer");
                    assert_eq!(
                        images,
                        vec!["data:image/png;base64,AAA".to_string()],
                        "the injected steer keeps the busy submission's images"
                    );
                    saw_injected = true;
                }
                SerializableAgentEvent::Finished { .. } => finished_turns += 1,
                _ => {}
            }
        }
        assert!(saw_injected, "the queued steer was injected at the boundary");
        wait_until_idle(&runtime).await;
        assert_eq!(llm.calls(), 2, "the injected steer ran a follow-up turn");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn image_only_prompt_is_accepted_and_rides_the_request() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["saw it"])]).multimodal());
        let runtime = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt(
                "   ".to_string(),
                vec!["data:image/png;base64,AAA".to_string()],
            )
            .await
            .expect("an image-only submission is accepted");
        drain_turn(&mut rx).await;
        let recorded = llm.recorded_messages();
        assert_eq!(recorded.len(), 1, "the image-only prompt ran one turn");
        assert!(
            recorded[0].iter().any(|message| match &message.content {
                MessageContent::Parts(parts) => parts.iter().any(|part| {
                    matches!(
                        part,
                        mnemo::provider::ContentPart::ImageUrl { image_url }
                            if image_url.url == "data:image/png;base64,AAA"
                    )
                }),
                _ => false,
            }),
            "the pasted image must ride the request as an image block: {:?}",
            recorded[0]
        );
        assert!(!runtime.snapshot().await.expect("snapshot").busy);
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn a_wholly_empty_submission_is_rejected_without_starting_a_turn() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(Vec::new()));
        let runtime = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("the lead opens");
        let err = runtime
            .send_prompt("   ".to_string(), Vec::new())
            .await
            .expect_err("blank text with no images is rejected");
        assert_eq!(err, "the prompt is empty");
        assert!(!runtime.snapshot().await.expect("snapshot").busy);
        assert_eq!(llm.calls(), 0, "the rejection never reaches the engine");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn stop_remains_independently_available_with_a_queued_steer() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
        let llm = Arc::new(ScriptedLlm::new_gated(
            vec!["scripted-a"],
            vec![
                (Some(gate_rx), text_turn(&["turn one"])),
                (None, text_turn(&["steer turn"])),
                (None, text_turn(&["after the stop"])),
            ],
        ));
        let runtime = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm))
            .await
            .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("first".to_string(), Vec::new())
            .await
            .expect("the first prompt is accepted");
        runtime
            .send_prompt("a steer".to_string(), Vec::new())
            .await
            .expect("the busy submission queues as a steer");
        // Stop stays independently available while a steer is queued —
        // it interrupts the active turn without submitting the queued
        // steer through the prompt path.
        runtime
            .interrupt()
            .await
            .expect("stop interrupts the running turn with a steer queued");
        gate_tx.send(()).expect("the first turn is released");
        // Whatever the kernel does with the flushed steer, the lead
        // must return to idle and accept a fresh prompt.
        wait_until_idle(&runtime).await;
        runtime
            .send_prompt("after the stop".to_string(), Vec::new())
            .await
            .expect("a fresh prompt is accepted after stop");
        drain_turn(&mut rx).await;
        wait_until_idle(&runtime).await;
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn approval_turn_surfaces_and_resolves_by_tool_call_id() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let runtime = LeadRuntime::open_inner_with_policy(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![
                tool_turn("shell", r#"{"command":"echo approved"}"#),
                text_turn(&["done"]),
            ]))),
            None,
        )
        .await
        .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("write the note".to_string(), Vec::new())
            .await
            .expect("prompt");

        let mut approval_id = None;
        for _ in 0..64 {
            let event = next_event(&mut rx).await;
            if let SerializableAgentEvent::ApprovalRequest { tool_call_id, .. } = &event.event.event
            {
                approval_id = Some(tool_call_id.clone());
                break;
            }
        }
        let request_id = approval_id.expect("the gated write surfaces an approval");
        let pending = runtime.snapshot().await.expect("snapshot").pending;
        match pending {
            Some(PendingInteraction::Approval { request_id: id }) => {
                assert_eq!(id, request_id);
            }
            other => panic!("expected a pending approval, got {other:?}"),
        }
        runtime
            .approve(request_id, Approval::Approve)
            .await
            .expect("the approval resolves");

        let mut finished = false;
        for _ in 0..64 {
            let event = next_event(&mut rx).await;
            if matches!(
                event.event.event,
                SerializableAgentEvent::Finished { .. } | SerializableAgentEvent::Exited
            ) {
                finished = true;
                break;
            }
        }
        assert!(finished, "the approved turn completes");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn question_turn_surfaces_and_resolves_by_question_id() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let runtime = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![
                tool_turn(
                    "ask_user",
                    r#"{"question":"Which branch?","options":[{"label":"main"},{"label":"wt/mnemo"}]}"#,
                ),
                text_turn(&["done"]),
            ]))),
        )
        .await
        .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("ask me".to_string(), Vec::new())
            .await
            .expect("prompt");

        let mut question_id = None;
        for _ in 0..64 {
            let event = next_event(&mut rx).await;
            if let SerializableAgentEvent::UserQuestion {
                question_id: id, ..
            } = &event.event.event
            {
                question_id = Some(id.clone());
                break;
            }
        }
        let id = question_id.expect("the question surfaces");
        let pending = runtime.snapshot().await.expect("snapshot").pending;
        match pending {
            Some(PendingInteraction::Question { question_id }) => {
                assert_eq!(question_id, id);
            }
            other => panic!("expected a pending question, got {other:?}"),
        }
        runtime
            .answer(id, UserAnswer::Choice { index: 1 })
            .await
            .expect("the question resolves");

        let mut finished = false;
        for _ in 0..64 {
            let event = next_event(&mut rx).await;
            if matches!(
                event.event.event,
                SerializableAgentEvent::Finished { .. } | SerializableAgentEvent::Exited
            ) {
                finished = true;
                break;
            }
        }
        assert!(finished, "the answered turn completes");
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn interrupt_stops_a_slow_turn() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let runtime = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(SlowLlm::new())),
        )
        .await
        .expect("the lead opens");
        let mut rx = runtime.subscribe();
        runtime
            .send_prompt("take your time".to_string(), Vec::new())
            .await
            .expect("prompt");
        let mut started = false;
        for _ in 0..32 {
            let event = next_event(&mut rx).await;
            if matches!(event.event.event, SerializableAgentEvent::Started) {
                started = true;
                break;
            }
        }
        assert!(started, "the slow turn starts");
        runtime.interrupt().await.expect("interrupt is accepted");
        let events = drain_turn(&mut rx).await;
        assert!(events.iter().any(|event| matches!(
            event.event.event,
            SerializableAgentEvent::Finished { .. } | SerializableAgentEvent::Exited
        )));
        assert!(!runtime.snapshot().await.expect("snapshot").busy);
        runtime.shutdown().await;
    }

    fn run_git(root: &std::path::Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git(root: &std::path::Path) {
        run_git(root, &["init", "-b", "main"]);
        run_git(root, &["config", "user.email", "test@example.com"]);
        run_git(root, &["config", "user.name", "Mnemo2 Test"]);
        std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
        std::fs::write(
            root.join(".gitignore"),
            ".worktrees/\n.coding/memory.db*\n.coding/codegraph.db*\n.coding/logs/\n.coding/plans/stack.json\n.coding/instance.json\n.coding/tmp/\n",
        )
        .expect("worktree ignore");
        run_git(root, &["add", "."]);
        run_git(root, &["commit", "-m", "seed"]);
    }

    #[tokio::test]
    async fn team_runs_two_workers_concurrently_without_cross_talk() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        write_config(config.path(), &["scripted-a", "scripted-b"]);
        let llm = Arc::new(ScriptedLlm::new(vec![
            text_turn(&["first worker done"]),
            text_turn(&["second worker done"]),
        ]));
        let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("the lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("the shared engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(2),
            max_spawns_per_turn: 2,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("the team opens");
        let mut rx = team.subscribe();
        let first = team
            .dispatch("first task".to_string(), None)
            .await
            .expect("the first worker dispatches");
        let second = team
            .dispatch("second task".to_string(), None)
            .await
            .expect("the second worker dispatches");
        let expected = [first.id.clone(), second.id.clone()]
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let mut finished = std::collections::HashSet::new();
        let mut texts = std::collections::HashMap::<String, String>::new();
        for _ in 0..100 {
            let event = match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(event)) => event,
                other => {
                    let snapshot = team.snapshot().await;
                    panic!(
                        "a team event arrives: {other:?}; llm_calls={}; snapshot={snapshot:#?}",
                        llm.calls()
                    );
                }
            };
            assert!(
                expected.contains(&event.worker_id),
                "unexpected worker id {}",
                event.worker_id
            );
            match &event.event.event {
                SerializableAgentEvent::TextDelta { text } => {
                    texts
                        .entry(event.worker_id.clone())
                        .or_default()
                        .push_str(text);
                }
                SerializableAgentEvent::Finished { .. } | SerializableAgentEvent::Exited => {
                    finished.insert(event.worker_id.clone());
                    if finished.len() == expected.len() {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(finished, expected, "both workers finish");
        assert_eq!(llm.calls(), 2, "each worker made its own LLM call");
        let mut text_values = texts.values().cloned().collect::<Vec<_>>();
        text_values.sort();
        assert_eq!(
            text_values,
            vec![
                "first worker done".to_string(),
                "second worker done".to_string()
            ],
            "each worker kept its own response"
        );

        let mut snapshot = team.snapshot().await;
        for _ in 0..100 {
            let ready = snapshot.workers.len() == 2
                && snapshot.workers.iter().all(|worker| {
                    worker.status == WorkerStatus::Completed
                        && !worker.transcript.messages.is_empty()
                });
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            snapshot = team.snapshot().await;
        }
        assert_eq!(snapshot.workers.len(), 2);
        assert_eq!(snapshot.active, 2, "both worker sessions are live");
        for worker in &snapshot.workers {
            assert_eq!(worker.status, WorkerStatus::Completed, "{}", worker.id);
            assert!(
                !worker.transcript.messages.is_empty(),
                "{} has its own transcript",
                worker.id
            );
            assert!(worker.branch.starts_with("wt/agent-"), "{}", worker.branch);
            assert!(worker.worktree.starts_with(root.path().join(".worktrees")));
        }
        assert_eq!(first.model, None);
        assert_eq!(second.model, None);

        let reopened = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("the shared engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(2),
            max_spawns_per_turn: 2,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("the team ledger reopens");
        let recovered = reopened.snapshot().await;
        assert_eq!(recovered.workers.len(), 2);
        assert!(recovered
            .workers
            .iter()
            .all(|worker| worker.status == WorkerStatus::Completed));

        team.shutdown().await;
        reopened.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn project_usage_records_lead_and_worker_usage_and_survives_restart_and_resume() {
        use crate::mnemo2::team::WorkerStatus;
        use crate::mnemo2::usage::ProjectUsageView;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config_with_pricing(
            config.path(),
            &["scripted-a"],
            &[("scripted-a", 2.0, 1.0, 10.0)],
        );
        let (lead, team) = open_git_lead(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new(vec![
                usage_turn(100_000, 20_000, 30_000),
                usage_turn(40_000, 10_000, 0),
                usage_text_turn(60_000, 10_000, 5_000, "lead reply"),
            ])),
        )
        .await;

        // Worker usage is folded into the durable project accumulator.
        let worker = team
            .dispatch("usage task".into(), None)
            .await
            .expect("dispatch");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        assert_eq!(
            team.snapshot().await.usage,
            ProjectUsageView {
                input_tokens: 70_000,
                cached_tokens: 30_000,
                output_tokens: 20_000,
                cost_micros: 370_000,
                unpriced_models: Vec::new(),
            }
        );

        // A second worker turn accumulates on the same model row.
        team.message(&worker.id, "second usage turn".into())
            .await
            .expect("second turn");
        let worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        assert_eq!(worker.cost_micros, Some(550_000));
        let after_worker = team.snapshot().await.usage;
        assert_eq!(after_worker.input_tokens, 110_000);
        assert_eq!(after_worker.cached_tokens, 30_000);
        assert_eq!(after_worker.output_tokens, 30_000);
        assert_eq!(after_worker.cost_micros, 550_000);

        // The lead's own usage is recorded on the same accumulator.
        let mut events = lead.subscribe();
        lead.send_prompt("lead usage turn".into(), Vec::new())
            .await
            .expect("lead prompt");
        drain_turn(&mut events).await;
        let after_lead = team.snapshot().await.usage;
        assert_eq!(after_lead.input_tokens, 165_000);
        assert_eq!(after_lead.cached_tokens, 35_000);
        assert_eq!(after_lead.output_tokens, 40_000);
        assert_eq!(after_lead.cost_micros, 765_000);
        assert!(after_lead.unpriced_models.is_empty());

        // Restart: a fresh runtime reloads the accumulator exactly once —
        // the resumed transcripts do NOT replay their historical usage.
        lead.shutdown().await;
        let reopened = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![usage_turn(
                10_000, 2_000, 3_000,
            )]))),
        )
        .await
        .expect("lead reopens");
        let reopened_team = reopened.team_handle().await.expect("team handle");
        assert_eq!(reopened_team.snapshot().await.usage, after_lead);

        // Resuming the stopped worker recovers its saved session snapshot
        // and records only the NEW turn's usage — no double count from the
        // recovered session or the reloaded ledger.
        reopened_team.resume(&worker.id).await.expect("resume worker");
        reopened_team
            .message(&worker.id, "resume usage turn".into())
            .await
            .expect("post-restart worker turn");
        wait_for_worker_status(&reopened_team, &worker.id, WorkerStatus::Completed).await;
        let final_usage = reopened_team.snapshot().await.usage;
        assert_eq!(final_usage.input_tokens, 172_000);
        assert_eq!(final_usage.cached_tokens, 38_000);
        assert_eq!(final_usage.output_tokens, 42_000);
        assert_eq!(final_usage.cost_micros, 802_000);

        // A second restart persists the same totals.
        reopened.shutdown().await;
        let reopened_again = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(Vec::new()))),
        )
        .await
        .expect("second reopen");
        assert_eq!(
            reopened_again
                .team_handle()
                .await
                .expect("team")
                .snapshot()
                .await
                .usage,
            final_usage
        );
        reopened_again.shutdown().await;
    }

    #[tokio::test]
    async fn project_usage_is_isolated_per_project_and_survives_worker_record_pruning() {
        use crate::mnemo2::team::WorkerStatus;
        use crate::mnemo2::usage::ProjectUsageView;

        let root_a = tempfile::tempdir().expect("workspace A");
        let config_a = tempfile::tempdir().expect("config A");
        let state_a = tempfile::tempdir().expect("state A");
        let root_b = tempfile::tempdir().expect("workspace B");
        let config_b = tempfile::tempdir().expect("config B");
        let state_b = tempfile::tempdir().expect("state B");
        write_config_with_pricing(
            config_a.path(),
            &["scripted-a"],
            &[("scripted-a", 2.0, 1.0, 10.0)],
        );
        write_config(config_b.path(), &["scripted-a"]);

        let (lead_a, team_a) = open_git_lead(
            &root_a,
            &config_a,
            &state_a,
            Arc::new(ScriptedLlm::new(vec![usage_turn(100_000, 20_000, 30_000)])),
        )
        .await;
        let (lead_b, team_b) = open_git_lead(
            &root_b,
            &config_b,
            &state_b,
            Arc::new(ScriptedLlm::new(vec![text_turn(&["unused"])])),
        )
        .await;

        let worker = team_a
            .dispatch("usage task".into(), None)
            .await
            .expect("dispatch");
        wait_for_worker_status(&team_a, &worker.id, WorkerStatus::Completed).await;
        let project_a = team_a.snapshot().await.usage;
        assert_eq!(
            project_a,
            ProjectUsageView {
                input_tokens: 70_000,
                cached_tokens: 30_000,
                output_tokens: 20_000,
                cost_micros: 370_000,
                unpriced_models: Vec::new(),
            }
        );

        // Project B has its own state dir: A's usage never leaks into it.
        assert_eq!(team_b.snapshot().await.usage, ProjectUsageView::default());
        lead_b.shutdown().await;

        // Deleting the worker's record (the frontend prunes records from
        // its derived store) must not remove its usage contribution.
        lead_a.shutdown().await;
        let ledger_path = state_a.path().join("team.json");
        let mut ledger: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&ledger_path).expect("ledger"))
                .expect("ledger JSON");
        ledger["workers"] = serde_json::json!([]);
        std::fs::write(&ledger_path, ledger.to_string()).expect("pruned ledger");
        let reopened =
            LeadRuntime::open_inner(options(&root_a, &config_a, &state_a), Some(Arc::new(ScriptedLlm::new(Vec::new()))))
                .await
                .expect("reopen");
        let reopened_team = reopened.team_handle().await.expect("team handle");
        let snapshot = reopened_team.snapshot().await;
        assert!(snapshot.workers.is_empty(), "the worker record was pruned");
        assert_eq!(snapshot.usage, project_a, "usage survives record deletion");
        reopened.shutdown().await;
    }

    /// Open a fresh lead whose scripted provider reports `model` as the
    /// observed model id, dispatch one usage turn, and return the project
    /// usage view plus the completed worker record.
    async fn dispatch_usage_under_model(
        model: &str,
        config_models: &[&str],
        turn: Vec<LlmEvent>,
    ) -> (
        crate::mnemo2::usage::ProjectUsageView,
        crate::mnemo2::team::WorkerRecord,
    ) {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), config_models);
        let (lead, team) = open_git_lead(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new_with_model(model, vec![turn])),
        )
        .await;
        let worker = team
            .dispatch("usage task".into(), None)
            .await
            .expect("dispatch");
        let worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        let usage = team.snapshot().await.usage;
        lead.shutdown().await;
        (usage, worker)
    }

    #[tokio::test]
    async fn vendor_default_pricing_prices_configured_models_without_explicit_rows() {
        // glm-5.3 via the official Ollama table, with no configured pricing:
        // 70k * 1.40 + 30k * 0.26 + 20k * 4.40 = 193,800 micro-USD.
        let (glm_usage, glm_worker) = dispatch_usage_under_model(
            "glm-5.3",
            &["glm-5.3"],
            usage_turn(100_000, 20_000, 30_000),
        )
        .await;
        assert_eq!(glm_worker.model_id.as_deref(), Some("glm-5.3"));
        assert_eq!(
            glm_worker.cost_micros,
            None,
            "per-worker telemetry still needs explicit pricing"
        );
        assert_eq!(glm_usage.input_tokens, 70_000);
        assert_eq!(glm_usage.cached_tokens, 30_000);
        assert_eq!(glm_usage.output_tokens, 20_000);
        assert_eq!(glm_usage.cost_micros, 193_800);
        assert!(glm_usage.unpriced_models.is_empty());

        // The configured Ollama tag `deepseek-v4.1-flash:1017` resolves the
        // vendor row after stripping the tag:
        // 5k * 0.30 + 5k * 0.006 + 1k * 1.20 = 2,730 micro-USD.
        let (tagged_usage, tagged_worker) = dispatch_usage_under_model(
            "deepseek-v4.1-flash:1017",
            &["deepseek-v4.1-flash"],
            usage_turn(10_000, 1_000, 5_000),
        )
        .await;
        assert_eq!(
            tagged_worker.model_id.as_deref(),
            Some("deepseek-v4.1-flash:1017")
        );
        assert_eq!(tagged_usage.input_tokens, 5_000);
        assert_eq!(tagged_usage.cached_tokens, 5_000);
        assert_eq!(tagged_usage.output_tokens, 1_000);
        assert_eq!(tagged_usage.cost_micros, 2_730);
        assert!(tagged_usage.unpriced_models.is_empty());

        // A model with neither configured nor vendor pricing keeps its
        // tokens in the totals but is excluded from the cost and listed
        // so the UI can mark the estimate partial.
        let (mystery_usage, mystery_worker) =
            dispatch_usage_under_model("scripted-b", &["scripted-b"], usage_turn(1_000, 0, 0))
                .await;
        assert_eq!(mystery_worker.model_id.as_deref(), Some("scripted-b"));
        assert_eq!(mystery_usage.input_tokens, 1_000);
        assert_eq!(mystery_usage.cached_tokens, 0);
        assert_eq!(mystery_usage.output_tokens, 0);
        assert_eq!(mystery_usage.cost_micros, 0);
        assert_eq!(
            mystery_usage.unpriced_models,
            vec!["scripted-b".to_string()]
        );
    }

    #[tokio::test]
    async fn interleaved_model_changes_stay_with_their_own_session() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        // Two real vendor-priced models so the per-model cost proves the
        // attribution: glm-5.3 (1.40/0.26/4.40) and deepseek-v4.1-flash:1017
        // (0.30/0.006/1.20), no explicit pricing. The workers share one
        // configured spec; their OBSERVED ids come from what the provider
        // serves, which the test switches mid-run.
        write_config(config.path(), &["scripted-a"]);
        let (release_a1, held_a1) = tokio::sync::oneshot::channel::<()>();
        let (release_a2, held_a2) = tokio::sync::oneshot::channel::<()>();
        let (release_b1, held_b1) = tokio::sync::oneshot::channel::<()>();
        // Index 0 = deepseek-v4.1-flash:1017, index 1 = glm-5.3; the test
        // switches the shared provider between them mid-run so each
        // worker's SECOND request resolves under a model that differs from
        // its session's serve seed — the only way the engine emits a
        // ModelChanged in a scripted-provider setup.
        let llm = Arc::new(ScriptedLlm::new_gated(
            vec!["deepseek-v4.1-flash:1017", "glm-5.3"],
            vec![
                // A's first request: a kernel-tool call keeps A's turn alive
                // for a second (held) request.
                (Some(held_a1), tool_turn("read_file", r#"{"path":"README.md"}"#)),
                // B's first request, held inside the provider.
                (Some(held_b1), tool_turn("read_file", r#"{"path":"README.md"}"#)),
                // A's second request: resolves under glm-5.3 (emitting A's
                // own ModelChanged) and then holds while B's turn continues.
                (Some(held_a2), usage_turn(100_000, 20_000, 30_000)),
                // B's second request: resolves under deepseek-v4.1-flash:1017
                // (emitting B's own ModelChanged) and completes.
                (None, usage_turn(50_000, 10_000, 20_000)),
            ],
        ));
        let (lead, team) = open_git_lead(&root, &config, &state, llm.clone()).await;

        // A starts under deepseek (seeded and served — suppressed), then
        // holds inside its first request.
        let a = team
            .dispatch("held task".into(), None)
            .await
            .expect("dispatch A");
        wait_for_calls(&llm, 1).await;
        // Switch the serving model BEFORE B starts, so B seeds glm-5.3.
        llm.set_model(1);
        let b = team
            .dispatch("quick task".into(), None)
            .await
            .expect("dispatch B");
        wait_for_calls(&llm, 2).await;

        // A's second request resolves under glm-5.3 — emitting A's own
        // ModelChanged, which sets A's observed model — and then holds, so
        // A's usage stays pending while B's turn continues.
        release_a1.send(()).expect("release A request 1");
        wait_for_calls(&llm, 3).await;

        // Switch back, then let B's turn continue: B's second request
        // resolves under deepseek-v4.1-flash:1017 and its ModelChanged
        // rides the engine-wide bus into A's pump — it must not re-point
        // A's row.
        llm.set_model(0);
        release_b1.send(()).expect("release B request 1");
        wait_for_worker_status(&team, &b.id, WorkerStatus::Completed).await;
        wait_for_worker_model(&team, &b.id, "deepseek-v4.1-flash:1017").await;

        // Release A: its held usage must still land under its own model.
        release_a2.send(()).expect("release A request 2");
        wait_for_worker_status(&team, &a.id, WorkerStatus::Completed).await;
        let a = team.worker(&a.id).await.expect("worker A");
        assert_eq!(
            a.model_id.as_deref(),
            Some("glm-5.3"),
            "A's observed model must survive B's concurrent ModelChanged"
        );

        // Per-model raw rows in the persisted ledger prove the attribution:
        // A's 100k/30k/20k under glm-5.3, B's 50k/20k/10k under the tag.
        wait_for_ledger_row(state.path(), "glm-5.3", (100_000, 30_000, 20_000)).await;
        wait_for_ledger_row(state.path(), "deepseek-v4.1-flash:1017", (50_000, 20_000, 10_000)).await;

        // Aggregate cost: A at glm rates (70k*1.40 + 30k*0.26 + 20k*4.40 =
        // 193,800) + B at deepseek rates (30k*0.30 + 20k*0.006 + 10k*1.20
        // = 21,120). If A were misattributed to the deepseek row the total
        // would be 66,300 instead.
        let view = team.snapshot().await.usage;
        assert_eq!(view.input_tokens, 100_000);
        assert_eq!(view.cached_tokens, 50_000);
        assert_eq!(view.output_tokens, 30_000);
        assert_eq!(view.cost_micros, 193_800 + 21_120);
        assert!(view.unpriced_models.is_empty());
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn team_preserves_distinct_model_specs_on_dispatch() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        write_config(config.path(), &["scripted-a", "scripted-b"]);
        let lead = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(Vec::new()))),
        )
        .await
        .expect("the lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("the shared engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(2),
            max_spawns_per_turn: 2,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("the team opens");
        let first = team
            .dispatch(
                "first task".to_string(),
                Some("test/scripted-a".to_string()),
            )
            .await
            .expect("the first worker dispatches");
        let second = team
            .dispatch(
                "second task".to_string(),
                Some("test/scripted-b".to_string()),
            )
            .await
            .expect("the second worker dispatches");
        assert_eq!(first.model.as_deref(), Some("test/scripted-a"));
        assert_eq!(second.model.as_deref(), Some("test/scripted-b"));
        assert_ne!(first.branch, second.branch);
        assert_ne!(first.worktree, second.worktree);
        let _ = team.stop(&first.id).await;
        let _ = team.stop(&second.id).await;
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn team_enforces_limits_and_resumes_a_stopped_worker() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]))),
        )
        .await
        .expect("the lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("the shared engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("the team opens");

        let first = team
            .dispatch("first task".to_string(), None)
            .await
            .expect("the first worker dispatches");
        let cap = team
            .dispatch("second task".to_string(), None)
            .await
            .expect_err("the per-turn cap refuses the second delegation");
        assert!(cap.contains("already delegated"), "{cap}");
        team.begin_lead_turn();
        let full = team
            .dispatch("second task".to_string(), None)
            .await
            .expect_err("the slot cap refuses the second delegation");
        assert!(full.contains("slots are busy"), "{full}");

        let stopped = team.stop(&first.id).await.expect("stop");
        assert_eq!(stopped.status, WorkerStatus::Stopped);
        let resumed = team.resume(&first.id).await.expect("resume");
        assert_eq!(resumed.status, WorkerStatus::Idle);
        let _ = team.stop(&first.id).await;
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn non_git_project_explains_how_to_enable_tracked_dispatch() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(Vec::new()))),
        )
        .await
        .expect("the lead opens without Git");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
        let host_tools = team.lead_host_tools();
        assert!(host_tools.iter().any(|tool| tool.name == "team_dispatch"));
        assert!(host_tools.iter().all(|tool| tool.name != "spawn_agent"));
        let error = team
            .dispatch("task".into(), None)
            .await
            .expect_err("non-Git dispatch fails safely");
        assert!(error.contains("folder containing .git"), "{error}");
        assert!(team.snapshot().await.workers.is_empty());
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn restart_downgrades_a_live_worker_to_interrupted() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(SlowLlm::new())),
        )
        .await
        .expect("lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
        let worker = team
            .dispatch("slow task".into(), None)
            .await
            .expect("dispatch");
        assert_eq!(worker.status, WorkerStatus::Working);
        team.shutdown().await;

        let reopened = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team reopens");
        let recovered = reopened.worker(&worker.id).await.expect("recovered worker");
        assert_eq!(recovered.status, WorkerStatus::Interrupted);
        assert_eq!(recovered.branch, worker.branch);
        assert_eq!(
            recovered.transcript.messages.len(),
            worker.transcript.messages.len()
        );

        reopened.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn review_fix_preserves_scope_evidence_and_requires_delta_review() {
        use crate::mnemo2::phase3::Finding;
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]))),
        )
        .await
        .expect("lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        team.stop(&worker.id).await.expect("stop");
        std::fs::create_dir_all(worker.worktree.join("src")).expect("source dir");
        std::fs::write(worker.worktree.join("src/lib.rs"), "pub fn old() {}\n").expect("source");
        run_git(&worker.worktree, &["add", "src/lib.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "implementation"]);
        let finding = Finding {
            id: "F-1".into(),
            severity: "low".into(),
            title: "Rename the function".into(),
            detail: "The public name should describe the new behavior".into(),
            file: "src/lib.rs".into(),
            line: 1,
            status: "open".into(),
            fix_commit: None,
            evidence: None,
            justification: None,
        };
        team.review(&worker.id, "initial review".into(), vec![finding], review_evidence())
            .await
            .expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("old-tip check");
        let evidence = team
            .resolve_finding(&worker.id, "F-1", "fixed", None, None)
            .await
            .expect_err("fixed findings need evidence");
        assert!(evidence.contains("evidence"), "{evidence}");

        std::fs::write(
            worker.worktree.join("unrelated.rs"),
            "pub fn unrelated() {}\n",
        )
        .expect("unrelated source");
        run_git(&worker.worktree, &["add", "unrelated.rs"]);
        let dirty = team
            .review_fix(&worker.id, "F-1", "src/lib.rs", "old", "new")
            .await
            .expect_err("a dirty worker cannot be review-fixed");
        assert!(dirty.contains("clean"), "{dirty}");
        run_git(&worker.worktree, &["reset", "--", "unrelated.rs"]);
        std::fs::remove_file(worker.worktree.join("unrelated.rs")).expect("remove fixture");

        let fixed = team
            .review_fix(&worker.id, "F-1", "src/lib.rs", "old", "new")
            .await
            .expect("review fix");
        let fix_tip = crate::mnemo2::phase3::tip(&worker.worktree)
            .await
            .expect("fix tip");
        assert_eq!(fixed.findings[0].status, "fixed");
        assert_eq!(
            fixed.findings[0].fix_commit.as_deref(),
            Some(fix_tip.as_str())
        );
        assert!(fixed.findings[0].evidence.is_some());
        assert!(fixed.review.is_none());
        assert!(fixed.checks.is_empty());
        assert_eq!(
            std::fs::read_to_string(worker.worktree.join("src/lib.rs")).expect("fixed source"),
            "pub fn new() {}\n"
        );
        assert!(!worker.worktree.join("unrelated.rs").exists());
        let second = team
            .review_fix(&worker.id, "F-1", "src/lib.rs", "new", "other")
            .await
            .expect_err("a second fix needs delta review");
        assert!(second.contains("fresh delta review"), "{second}");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("new-tip check");
        let stale = team
            .merge(&worker.id)
            .await
            .expect_err("merge needs delta review");
        assert!(stale.contains("has not been reviewed"), "{stale}");
        team.review(&worker.id, "delta review".into(), fixed.findings.clone(), review_evidence())
            .await
            .expect("delta review");
        let merged = team.merge(&worker.id).await.expect("reviewed merge");
        assert_eq!(merged.status, WorkerStatus::Merged);

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn larger_findings_route_to_the_tracked_worker_for_delta_review() {
        use crate::mnemo2::phase3::Finding;
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(vec![
            text_turn(&["implementation done"]),
            text_turn(&["fix done"]),
        ]));
        let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        for _ in 0..100 {
            if team.worker(&worker.id).await.expect("worker").status == WorkerStatus::Completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        std::fs::create_dir_all(worker.worktree.join("src")).expect("source dir");
        std::fs::write(worker.worktree.join("src/lib.rs"), "pub fn old() {}\n").expect("source");
        run_git(&worker.worktree, &["add", "src/lib.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "implementation"]);
        let finding = Finding {
            id: "F-large".into(),
            severity: "high".into(),
            title: "Rework the implementation".into(),
            detail: "The change exceeds the tiny review-fix scope".into(),
            file: "src/lib.rs".into(),
            line: 1,
            status: "open".into(),
            fix_commit: None,
            evidence: None,
            justification: None,
        };
        team.review(&worker.id, "initial review".into(), vec![finding], review_evidence())
            .await
            .expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("check");
        let routed = team
            .route_fix(&worker.id, "F-large")
            .await
            .expect("route fixer");
        assert_eq!(routed.fixer_finding_id.as_deref(), Some("F-large"));
        assert!(routed.review.is_none());
        assert!(routed.checks.is_empty());
        for _ in 0..100 {
            if llm.calls() >= 2
                && team.worker(&worker.id).await.expect("worker").status == WorkerStatus::Completed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(llm.calls(), 2, "the tracked worker received the fix brief");
        std::fs::write(worker.worktree.join("src/lib.rs"), "pub fn new() {}\n")
            .expect("fixed source");
        run_git(&worker.worktree, &["add", "src/lib.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "fix review finding"]);
        let resolved = team
            .resolve_finding(
                &worker.id,
                "F-large",
                "fixed",
                Some("worker commit and affected checks verified".into()),
                None,
            )
            .await
            .expect("close with evidence");
        let fix_tip = crate::mnemo2::phase3::tip(&worker.worktree)
            .await
            .expect("tip");
        assert_eq!(
            resolved.findings[0].fix_commit.as_deref(),
            Some(fix_tip.as_str())
        );
        let reviewed = team
            .review(&worker.id, "delta review".into(), resolved.findings.clone(), review_evidence())
            .await
            .expect("delta review");
        assert_eq!(reviewed.review.expect("review record").tip, fix_tip);

        let _ = team.stop(&worker.id).await;
        team.shutdown().await;
        lead.shutdown().await;
    }

    fn mark_merge_interrupted_for_test(state: &TempDir) {
        let path = state.path().join("team.json");
        let mut ledger: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("team ledger"))
                .expect("team ledger JSON");
        ledger["workers"][0]["status"] = serde_json::json!("merging");
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&ledger).expect("ledger JSON"),
        )
        .expect("interrupted ledger");
    }

    #[tokio::test]
    async fn merge_requires_an_idle_worker() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(&root, &config, &state, Arc::new(SlowLlm::new())).await;
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        team.stop(&worker.id).await.expect("stop initial turn");
        commit_worker_file(&worker, "src/lib.rs", "pub fn feature() {}\n", "feature");
        team.review(&worker.id, "reviewed".into(), Vec::new(), review_evidence())
            .await
            .expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("check");
        team.resume(&worker.id).await.expect("resume");
        team.message(&worker.id, "continue working".into())
            .await
            .expect("new turn");

        let error = team
            .merge(&worker.id)
            .await
            .expect_err("busy worker cannot merge");
        assert!(error.contains("worker must be idle"), "{error}");

        let _ = team.stop(&worker.id).await;
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn merge_detaches_the_live_session_before_worktree_cleanup() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]));
        let (lead, team) = open_git_team(&root, &config, &state, llm).await;
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        commit_worker_file(&worker, "src/lib.rs", "pub fn feature() {}\n", "feature");
        team.review(&worker.id, "reviewed".into(), Vec::new(), review_evidence())
            .await
            .expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("check");
        assert_eq!(team.snapshot().await.active, 1);

        let merged = team.merge(&worker.id).await.expect("merge");
        assert_eq!(merged.status, WorkerStatus::Merged);
        assert_eq!(team.snapshot().await.active, 0);
        assert!(!worker.worktree.exists());
        assert!(
            team.resume(&worker.id).await.is_err(),
            "merged workers stay closed"
        );

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn restart_reconstructs_a_completed_merge_from_git_evidence() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]));
        let (lead, team) = open_git_team(&root, &config, &state, llm).await;
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        commit_worker_file(&worker, "src/lib.rs", "pub fn feature() {}\n", "feature");
        team.review(&worker.id, "reviewed".into(), Vec::new(), review_evidence())
            .await
            .expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("check");
        team.shutdown().await;
        run_git(
            root.path(),
            &[
                "merge",
                "--no-ff",
                &worker.branch,
                "-m",
                "completed before crash",
            ],
        );
        let worker_tip = crate::mnemo2::phase3::tip(&worker.worktree)
            .await
            .expect("worker tip");
        std::fs::write(
            root.path().join(".git/MERGE_HEAD"),
            format!("{worker_tip}\n"),
        )
        .expect("stale merge head");
        mark_merge_interrupted_for_test(&state);

        let reopened = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team reopens");
        let recovered = reopened.worker(&worker.id).await.expect("recovered worker");
        let lead_tip = crate::mnemo2::phase3::tip(root.path())
            .await
            .expect("lead tip");
        assert_eq!(recovered.status, WorkerStatus::Merged);
        assert_eq!(recovered.merge.expect("merge record").commit, lead_tip);
        assert!(!worker.worktree.exists());
        crate::mnemo2::phase3::require_clean(root.path(), "lead")
            .await
            .expect("clean lead");

        reopened.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn restart_aborts_an_interrupted_conflict_and_keeps_the_branch() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]));
        let (lead, team) = open_git_team(&root, &config, &state, llm).await;
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        commit_worker_file(&worker, "README.md", "worker\n", "worker edit");
        std::fs::write(root.path().join("README.md"), "lead\n").expect("lead edit");
        run_git(root.path(), &["add", "README.md"]);
        run_git(root.path(), &["commit", "-m", "lead edit"]);
        team.review(&worker.id, "reviewed".into(), Vec::new(), review_evidence())
            .await
            .expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("check");
        team.shutdown().await;
        let conflict = std::process::Command::new("git")
            .args(["merge", "--no-ff", &worker.branch])
            .current_dir(root.path())
            .output()
            .expect("conflicting merge");
        assert!(!conflict.status.success());
        mark_merge_interrupted_for_test(&state);

        let reopened = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team reopens");
        let recovered = reopened.worker(&worker.id).await.expect("recovered worker");
        assert_eq!(recovered.status, WorkerStatus::Conflict);
        assert!(recovered.review.is_none());
        assert!(recovered.checks.is_empty());
        assert!(recovered.error.is_some());
        assert!(worker.worktree.exists());
        run_git(
            root.path(),
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", worker.branch),
            ],
        );
        crate::mnemo2::phase3::require_clean(root.path(), "lead")
            .await
            .expect("clean lead");
        let merge_head =
            crate::mnemo2::phase3::git(root.path(), &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
                .await
                .expect("merge-head probe");
        assert!(!merge_head.status.success());

        reopened.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn recovery_preserves_unrelated_lead_changes_during_a_conflict() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])])),
        )
        .await;
        let worker = team.dispatch("implement".into(), None).await.expect("dispatch");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        commit_worker_file(&worker, "README.md", "worker\n", "worker edit");
        std::fs::write(root.path().join("README.md"), "lead\n").expect("lead edit");
        run_git(root.path(), &["add", "README.md"]);
        run_git(root.path(), &["commit", "-m", "lead edit"]);
        team.review(&worker.id, "reviewed".into(), Vec::new(), review_evidence()).await.expect("review");
        team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
            .await
            .expect("check");
        team.shutdown().await;
        let conflict = std::process::Command::new("git")
            .args(["merge", "--no-ff", &worker.branch])
            .current_dir(root.path())
            .output()
            .expect("conflicting merge");
        assert!(!conflict.status.success());
        std::fs::write(root.path().join("lead-note.txt"), "unrelated work\n").expect("unrelated work");
        mark_merge_interrupted_for_test(&state);

        let reopened = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team reopens without changing the lead");
        let recovered = reopened.worker(&worker.id).await.expect("recovered worker");
        assert_eq!(recovered.status, WorkerStatus::Conflict);
        assert!(recovered.error.as_deref().is_some_and(|error| error.contains("unrelated changes")));
        assert_eq!(std::fs::read_to_string(root.path().join("lead-note.txt")).expect("preserved file"), "unrelated work\n");
        let merge_head = crate::mnemo2::phase3::git(
            root.path(),
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .await
        .expect("merge-head probe");
        assert!(merge_head.status.success(), "an unsafe automatic abort must not run");
        assert!(worker.worktree.exists());

        reopened.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn pre_delivery_spawn_failure_retries_the_same_tracked_worker() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(Vec::new()));
        let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: vec!["test/scripted-a".into(), "broken".into()],
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
        let worker_id = "failure001";
        let error = team
            .dispatch_with_id(
                "retry without duplication".into(),
                Some("broken".into()),
                Some(worker_id.into()),
                None,
            )
            .await
            .expect_err("the engine rejects an invalid forced model spec");
        assert!(error.contains("retains its checkout"), "{error}");
        let failed = team.worker(worker_id).await.expect("tracked failed dispatch");
        assert_eq!(failed.status, WorkerStatus::Failed);
        assert!(!failed.brief_sent);
        assert!(failed.worktree.exists());

        let retry = team
            .dispatch_with_id(
                "retry without duplication".into(),
                Some("broken".into()),
                Some(worker_id.into()),
                None,
            )
            .await
            .expect_err("retry still surfaces the host failure");
        assert!(retry.contains("retains its checkout"), "{retry}");
        let retried = team.worker(worker_id).await.expect("same tracked worker");
        assert_eq!(retried.branch, failed.branch);
        assert_eq!(retried.worktree, failed.worktree);
        assert_eq!(team.snapshot().await.workers.len(), 1);
        assert_eq!(llm.calls(), 0, "no provider turn was delivered");
        assert!(crate::mnemo2::phase3::git_value(
            root.path(),
            &["rev-parse", "--verify", &format!("refs/heads/{}", failed.branch)],
        )
        .await
        .is_ok());

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn dispatch_rollback_removes_only_untouched_fresh_worktrees() {
        let root = tempfile::tempdir().expect("workspace");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        std::fs::write(root.path().join("lead-note.txt"), "keep lead work\n").expect("lead work");
        let (checkout, branch) = mnemo::app::worktrees::provision_agent_worktree(
            root.path().to_path_buf(),
            "rollback001".into(),
            "wt/lead".into(),
        )
        .await
        .expect("fresh checkout");
        let base_tip = crate::mnemo2::phase3::tip(&checkout).await.expect("base tip");
        mnemo::app::worktrees::rollback_agent_worktree(
            root.path().to_path_buf(),
            "rollback001".into(),
            base_tip.clone(),
        )
        .await
        .expect("untouched fresh checkout rolls back");
        assert!(!checkout.exists());
        assert!(crate::mnemo2::phase3::git_value(root.path(), &["rev-parse", &branch])
            .await
            .is_err());

        let (checkout, branch) = mnemo::app::worktrees::provision_agent_worktree(
            root.path().to_path_buf(),
            "rollback001".into(),
            "wt/lead".into(),
        )
        .await
        .expect("new checkout after rollback");
        std::fs::write(checkout.join("feature.txt"), "useful commit\n").expect("feature");
        run_git(&checkout, &["add", "feature.txt"]);
        run_git(&checkout, &["commit", "-m", "useful worker commit"]);
        let preserved = mnemo::app::worktrees::rollback_agent_worktree(
            root.path().to_path_buf(),
            "rollback001".into(),
            base_tip.clone(),
        )
        .await
        .expect_err("advanced worker commits are never deleted");
        assert!(preserved.contains("advanced"), "{preserved}");
        assert!(crate::mnemo2::phase3::git_value(root.path(), &["rev-parse", &branch])
            .await
            .is_ok());
        assert_eq!(std::fs::read_to_string(checkout.join("feature.txt")).expect("worker file"), "useful commit\n");

        let (dirty, _) = mnemo::app::worktrees::provision_agent_worktree(
            root.path().to_path_buf(),
            "dirty00002".into(),
            "wt/lead".into(),
        )
        .await
        .expect("dirty-test checkout");
        let dirty_tip = crate::mnemo2::phase3::tip(&dirty).await.expect("dirty base");
        std::fs::write(dirty.join("untracked.txt"), "keep worker work\n").expect("dirty work");
        let preserved = mnemo::app::worktrees::rollback_agent_worktree(
            root.path().to_path_buf(),
            "dirty00002".into(),
            dirty_tip,
        )
        .await
        .expect_err("dirty work is never deleted");
        assert!(preserved.contains("uncommitted"), "{preserved}");
        assert_eq!(std::fs::read_to_string(dirty.join("untracked.txt")).expect("dirty file"), "keep worker work\n");
        assert_eq!(std::fs::read_to_string(root.path().join("lead-note.txt")).expect("lead file"), "keep lead work\n");
    }

    #[tokio::test]
    async fn merge_requires_tip_checks_and_supports_local_revert() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let lead = LeadRuntime::open_inner(
            options(&root, &config, &state),
            Some(Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]))),
        )
        .await
        .expect("lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
        let worker = team
            .dispatch("implement".into(), None)
            .await
            .expect("dispatch");
        team.stop(&worker.id).await.expect("stop");
        std::fs::write(worker.worktree.join("feature.rs"), "pub fn feature() {}\n")
            .expect("worker source");
        run_git(&worker.worktree, &["add", "feature.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "feature"]);
        team.review(&worker.id, "reviewed".into(), Vec::new(), review_evidence())
            .await
            .expect("review");
        let empty = team
            .merge(&worker.id)
            .await
            .expect_err("empty checks refuse merge");
        assert!(empty.contains("no checks"), "{empty}");
        team.record_check(
            &worker.id,
            "cargo test --workspace".into(),
            "passed".into(),
            None,
        )
        .await
        .expect("check");
        let merged = team.merge(&worker.id).await.expect("merge");
        assert_eq!(merged.status, WorkerStatus::Merged);
        let record = merged.merge.as_ref().expect("merge record");
        assert_eq!(record.target, "wt/lead");
        assert_eq!(
            (record.files, record.additions, record.deletions),
            (1, 1, 0)
        );
        assert!(
            !worker.worktree.exists(),
            "successful merge removes the worktree: {:?}",
            merged.error
        );
        let merged_source = std::fs::read_to_string(root.path().join("feature.rs"))
            .expect("merged source")
            .replace("\r\n", "\n");
        assert_eq!(merged_source, "pub fn feature() {}\n");
        run_git(
            root.path(),
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", worker.branch),
            ],
        );
        let main_tip = crate::mnemo2::phase3::git_value(root.path(), &["rev-parse", "main"])
            .await
            .expect("main tip");
        run_git(root.path(), &["branch", "-f", "main", &record.commit]);
        let landed = team
            .revert_merge(&worker.id)
            .await
            .expect_err("landed merge cannot revert");
        assert!(landed.contains("already landed"), "{landed}");
        run_git(root.path(), &["branch", "-f", "main", &main_tip]);
        let reverted = team.revert_merge(&worker.id).await.expect("revert");
        assert!(reverted
            .merge
            .is_some_and(|record| record.revert_commit.is_some()));
        assert!(
            !root.path().join("feature.rs").exists(),
            "revert restores the lead tree"
        );
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn usage_above_former_budget_never_pauses_and_survives_restart() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let pricing = vec![mnemo::config::PricingEntry {
            model: "scripted-a".into(),
            input_per_1m: 2.0,
            output_per_1m: 10.0,
            cached_per_1m: 1.0,
        }];
        let llm = Arc::new(ScriptedLlm::new(vec![
            usage_turn(180_000, 20_000, 120_000),
            usage_turn(50_000, 25_000, 40_000),
            text_turn(&["continued"]),
        ]));
        let (lead, team) = open_git_team_with_pricing(&root, &config, &state, llm, pricing).await;
        let worker = team
            .dispatch("usage task".into(), None)
            .await
            .expect("dispatch");
        let first = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        assert_eq!(first.total_tokens, 200_000);
        assert_eq!(first.cost_micros, Some(440_000));
        team.message(&worker.id, "second usage turn".into())
            .await
            .expect("second turn");
        let continued = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        assert_eq!(continued.total_tokens, 275_000);
        assert_eq!(continued.prompt_tokens, 230_000);
        assert_eq!(continued.completion_tokens, 45_000);
        assert_eq!(continued.cached_tokens, 160_000);
        assert_eq!(continued.requests, 2);
        assert_eq!(continued.cost_micros, Some(750_000));
        assert!(continued.error.is_none());

        team.message(&worker.id, "continue without a budget".into())
            .await
            .expect("usage never blocks continuation");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        team.stop_all().await.expect("stop all");
        let stopped = team.snapshot().await;
        assert_eq!(stopped.active, 0);
        assert_eq!(stopped.workers[0].status, WorkerStatus::Stopped);

        let reopened = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("reopen");
        let restored = reopened.worker(&worker.id).await.expect("restored worker");
        assert_eq!(restored.total_tokens, 275_000);
        assert_eq!(restored.requests, 2);
        assert_eq!(restored.cost_micros, Some(750_000));
        assert!(restored.brief_sent);

        team.shutdown().await;
        reopened.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn dispatch_validates_models_and_retries_one_worker_id() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])])),
        )
        .await;
        let worker_id = "worker0001";
        let invalid = team
            .handle_lead_host_tool(
                "team_dispatch",
                serde_json::json!({
                    "worker_id": worker_id,
                    "brief": "invalid model task",
                    "model": "test/missing"
                }),
            )
            .await;
        assert!(!invalid.success);
        assert_eq!(invalid.data.as_ref().and_then(|data| data.get("error_kind")).and_then(|value| value.as_str()), Some("domain"));
        assert!(team.snapshot().await.workers.is_empty());
        assert!(!root.path().join(".worktrees/agent-worker00").exists());
        assert!(crate::mnemo2::phase3::git_value(
            root.path(),
            &["rev-parse", "--verify", "refs/heads/wt/agent-worker00"],
        )
        .await
        .is_err());

        let args = serde_json::json!({"worker_id": worker_id, "brief": "retry this task"});
        let first = team.handle_lead_host_tool("team_dispatch", args.clone()).await;
        assert!(first.success, "{}", first.output);
        let receipt: serde_json::Value = serde_json::from_str(&first.output).expect("JSON receipt");
        assert_eq!(receipt.pointer("/result/worker/id").and_then(|value| value.as_str()), Some(worker_id));
        assert!(receipt.pointer("/result/worker/status").is_some());
        assert_eq!(receipt.pointer("/result/reused").and_then(|value| value.as_bool()), Some(false));
        let worker = team.worker(worker_id).await.expect("tracked worker");
        assert!(worker.brief_sent);

        let retried = team.handle_lead_host_tool("team_dispatch", args).await;
        let receipt: serde_json::Value = serde_json::from_str(&retried.output).expect("retry receipt");
        assert_eq!(receipt.pointer("/result/reused").and_then(|value| value.as_bool()), Some(true));
        assert_eq!(team.snapshot().await.workers.len(), 1);
        assert_eq!(team.snapshot().await.active, 1);
        let conflict = team
            .dispatch_with_id(
                "a different brief".into(),
                None,
                Some(worker_id.to_string()),
                None,
            )
            .await
            .expect_err("same ID cannot change the logical task");
        assert!(conflict.contains("different brief or model"), "{conflict}");
        let model_conflict = team
            .dispatch_with_id(
                "retry this task".into(),
                Some("test/scripted-a".into()),
                Some(worker_id.to_string()),
                None,
            )
            .await
            .expect_err("a retry cannot silently change the worker model");
        assert!(model_conflict.contains("different brief or model"), "{model_conflict}");
        let cap = team
            .dispatch("a genuinely different task".into(), None)
            .await
            .expect_err("an idempotent retry did not consume the one spawn allowed this turn");
        assert!(cap.contains("already delegated"), "{cap}");

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn high_risk_dispatch_and_review_require_substantive_evidence() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team_spawns(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new(vec![
                text_turn(&["done"]),
                text_turn(&["audit findings: none"]),
                text_turn(&["acknowledged"]),
            ])),
            2,
            4,
        )
        .await;

        // An unknown risk classification is rejected outright.
        let invalid = team
            .handle_lead_host_tool(
                "team_dispatch",
                serde_json::json!({
                    "worker_id": "hiresk0001",
                    "brief": "high-risk task",
                    "review_risk": "extreme"
                }),
            )
            .await;
        assert!(!invalid.success);
        assert!(
            invalid.output.contains("review_risk must be 'standard' or 'high'"),
            "{}",
            invalid.output
        );

        // High-risk dispatch requires the active plan's acceptance scenarios:
        // with no active plan the dispatch is refused before provisioning.
        let no_plan = team
            .handle_lead_host_tool(
                "team_dispatch",
                serde_json::json!({
                    "worker_id": "hiresk0001",
                    "brief": "high-risk task",
                    "review_risk": "high"
                }),
            )
            .await;
        assert!(!no_plan.success);
        assert!(no_plan.output.contains("stack.json"), "{}", no_plan.output);
        assert!(team.worker("hiresk0001").await.is_err());
        assert!(team.snapshot().await.workers.is_empty());

        // A plan without the section still blocks…
        let plans = root.path().join(".coding/plans");
        std::fs::create_dir_all(&plans).expect("plans dir");
        std::fs::write(
            plans.join("stack.json"),
            r#"{"stack":["planid01"],"reviewed":false}"#,
        )
        .expect("stack");
        std::fs::write(plans.join("planid01.md"), "# Plan\n\n## Goal\nShip it.\n")
            .expect("plan doc");
        run_git(root.path(), &["add", ".coding/plans"]);
        run_git(root.path(), &["commit", "-m", "plan without scenarios"]);
        let no_section = team
            .handle_lead_host_tool(
                "team_dispatch",
                serde_json::json!({
                    "worker_id": "hiresk0001",
                    "brief": "high-risk task",
                    "review_risk": "high"
                }),
            )
            .await;
        assert!(!no_section.success);
        assert!(
            no_section.output.contains("does not declare"),
            "{}",
            no_section.output
        );

        // …while observable bullets unblock it, and the risk is recorded.
        std::fs::write(
            plans.join("planid01.md"),
            "# Plan\n\n## Acceptance scenarios\n\n- A high-risk worker cannot merge without structured evidence.\n",
        )
        .expect("plan doc");
        run_git(root.path(), &["add", ".coding/plans"]);
        run_git(root.path(), &["commit", "-m", "plan acceptance scenarios"]);
        let dispatched = team
            .handle_lead_host_tool(
                "team_dispatch",
                serde_json::json!({
                    "worker_id": "hiresk0001",
                    "brief": "high-risk task",
                    "review_risk": "high"
                }),
            )
            .await;
        assert!(dispatched.success, "{}", dispatched.output);
        let worker = team.worker("hiresk0001").await.expect("high-risk worker");
        assert_eq!(worker.review_risk, "high");
        team.stop(&worker.id).await.expect("stop");
        std::fs::write(worker.worktree.join("feature.rs"), "pub fn feature() {}\n")
            .expect("worker source");
        run_git(&worker.worktree, &["add", "feature.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "feature"]);

        // team_review without an evidence object is refused.
        let unevidenced = team
            .handle_lead_host_tool(
                "team_review",
                serde_json::json!({
                    "worker_id": worker.id,
                    "summary": "reviewed",
                    "findings": []
                }),
            )
            .await;
        assert!(!unevidenced.success);
        assert!(
            unevidenced
                .output
                .contains("requires a complete 'evidence' object"),
            "{}",
            unevidenced.output
        );

        // An anchored-only pass and an empty assessment are refused.
        let anchored = team
            .handle_lead_host_tool(
                "team_review",
                serde_json::json!({
                    "worker_id": worker.id,
                    "summary": "reviewed",
                    "findings": [],
                    "evidence": {
                        "unanchored_pass": false,
                        "intent": "the worker implements the scoped brief",
                        "execution": "the change flows through the documented path",
                        "integration": "the module fits the surrounding runtime",
                        "test_quality": "the regression reproduces the defect before the fix",
                        "residual_risk": "no residual risk beyond the reviewed diff"
                    }
                }),
            )
            .await;
        assert!(!anchored.success);
        assert!(anchored.output.contains("unanchored"), "{}", anchored.output);
        let empty = team
            .handle_lead_host_tool(
                "team_review",
                serde_json::json!({
                    "worker_id": worker.id,
                    "summary": "reviewed",
                    "findings": [],
                    "evidence": {
                        "unanchored_pass": true,
                        "intent": "  ",
                        "execution": "the change flows through the documented path",
                        "integration": "the module fits the surrounding runtime",
                        "test_quality": "the regression reproduces the defect before the fix",
                        "residual_risk": "no residual risk beyond the reviewed diff"
                    }
                }),
            )
            .await;
        assert!(!empty.success);
        assert!(empty.output.contains("intent"), "{}", empty.output);

        // Complete evidence alone no longer merges high-risk work: the
        // review is refused until a fresh independent audit is linked.
        let unaudited = team
            .handle_lead_host_tool(
                "team_review",
                serde_json::json!({
                    "worker_id": worker.id,
                    "summary": "reviewed",
                    "findings": [],
                    "evidence": review_evidence()
                }),
            )
            .await;
        assert!(!unaudited.success);
        assert!(
            unaudited
                .output
                .contains("high-risk reviews require an independent reviewer"),
            "{}",
            unaudited.output
        );

        // team_audit_dispatch provisions a read-only audit at the worker's
        // exact current tip, with plan context and no implementer claims.
        let audited = team
            .handle_lead_host_tool(
                "team_audit_dispatch",
                serde_json::json!({
                    "worker_id": worker.id,
                    "reviewer_id": "auditor01",
                    "reasoning_effort": "high",
                    "brief": "focus on concurrency"
                }),
            )
            .await;
        assert!(audited.success, "{}", audited.output);
        let receipt: serde_json::Value =
            serde_json::from_str(&audited.output).expect("JSON receipt");
        assert_eq!(receipt.pointer("/result/reused").and_then(|value| value.as_bool()), Some(false));
        let audit = team.worker("auditor01").await.expect("audit record");
        assert_eq!(audit.kind, "audit");
        assert_eq!(audit.audit_target.as_deref(), Some(worker.id.as_str()));
        assert!(audit.audit_tip.is_some(), "the audit records the inspected tip");
        assert!(
            !audit
                .brief
                .contains("reviewed")
                || audit.brief.contains("not a summary of what was done"),
            "the audit brief re-frames the intent instead of echoing implementer claims"
        );

        // Retrying the same reviewer for the same worker and tip is an
        // idempotent reuse; the audit is otherwise left to finish.
        let retry = team
            .handle_lead_host_tool(
                "team_audit_dispatch",
                serde_json::json!({
                    "worker_id": worker.id,
                    "reviewer_id": "auditor01"
                }),
            )
            .await;
        assert!(retry.success, "{}", retry.output);
        let retry_receipt: serde_json::Value =
            serde_json::from_str(&retry.output).expect("JSON receipt");
        assert_eq!(retry_receipt.pointer("/result/reused").and_then(|value| value.as_bool()), Some(true));

        // The audit never merges and is never itself reviewable.
        let merge_audit = team.merge("auditor01").await;
        assert!(merge_audit.is_err(), "an audit worker must never merge");
        assert!(
            merge_audit
                .err()
                .unwrap_or_default()
                .contains("never merged"),
            "the audit merge refusal names the reviewer"
        );
        let review_audit = team
            .review(
                "auditor01",
                "reviewed".into(),
                Vec::new(),
                review_evidence(),
            )
            .await;
        assert!(review_audit.is_err(), "an audit worker is not reviewable");
        assert!(
            review_audit
                .err()
                .unwrap_or_default()
                .contains("read-only reviewer"),
            "the audit review refusal names the reviewer"
        );

        // Once the audit completes, linking it (with a written
        // adjudication) completes the high-risk review.
        wait_for_worker_status(&team, "auditor01", WorkerStatus::Completed).await;
        let reviewed = team
            .handle_lead_host_tool(
                "team_review",
                serde_json::json!({
                    "worker_id": worker.id,
                    "summary": "reviewed",
                    "findings": [],
                    "evidence": {
                        "unanchored_pass": true,
                        "intent": "the worker implements the scoped brief",
                        "execution": "the change flows through the documented path",
                        "integration": "the module fits the surrounding runtime",
                        "test_quality": "the regression reproduces the defect before the fix",
                        "residual_risk": "no residual risk beyond the reviewed diff",
                        "independent_reviewer_id": "auditor01",
                        "audit_adjudication": "the independent audit found no additional defects; accepted"
                    }
                }),
            )
            .await;
        assert!(reviewed.success, "{}", reviewed.output);
        let review_receipt: serde_json::Value =
            serde_json::from_str(&reviewed.output).expect("JSON receipt");
        assert_eq!(
            review_receipt
                .pointer("/result/review/evidence/unanchored_pass")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            review_receipt
                .pointer("/result/review/evidence/independent_reviewer_id")
                .and_then(|value| value.as_str()),
            Some("auditor01")
        );
        team.record_check(
            &worker.id,
            "cargo test --workspace".into(),
            "passed".into(),
            None,
        )
        .await
        .expect("check");
        let merged = team.merge(&worker.id).await.expect("adjudicated merge");
        assert_eq!(merged.status, WorkerStatus::Merged);
        let record = merged.merge.as_ref().expect("merge record");
        assert_eq!(record.review_risk.as_deref(), Some("high"));
        assert!(
            record
                .review_evidence
                .as_ref()
                .is_some_and(|evidence| evidence.unanchored_pass),
            "the durable merge record captures the evidence"
        );
        assert!(
            record
                .review_evidence
                .as_ref()
                .is_some_and(|evidence| evidence.independent_reviewer_id.as_deref() == Some("auditor01")),
            "the durable merge record captures the audit linkage"
        );

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn stale_audit_blocks_high_risk_merge_until_a_fresh_audit() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team_spawns(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new(vec![
                text_turn(&["done"]),
                text_turn(&["audit one complete"]),
                text_turn(&["audit two complete"]),
            ])),
            2,
            4,
        )
        .await;
        let plans = root.path().join(".coding/plans");
        std::fs::create_dir_all(&plans).expect("plans dir");
        std::fs::write(
            plans.join("stack.json"),
            r#"{"stack":["planid02"],"reviewed":false}"#,
        )
        .expect("stack");
        std::fs::write(
            plans.join("planid02.md"),
            "# Plan\n\n## Acceptance scenarios\n\n- A fix commit invalidates the prior audit.\n",
        )
        .expect("plan doc");
        run_git(root.path(), &["add", ".coding/plans"]);
        run_git(root.path(), &["commit", "-m", "plan acceptance scenarios"]);

        let worker = team
            .dispatch_with_id(
                "high-risk feature".into(),
                None,
                Some("stalesk01".into()),
                Some("high".into()),
            )
            .await
            .expect("dispatch");
        team.stop(&worker.id).await.expect("stop");
        std::fs::write(worker.worktree.join("feature.rs"), "pub fn feature() {}\n")
            .expect("worker source");
        run_git(&worker.worktree, &["add", "feature.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "feature"]);

        // First audit at the first tip.
        team.audit_dispatch(&worker.id, "audita001", None, None, None)
            .await
            .expect("first audit dispatch");
        wait_for_worker_status(&team, "audita001", WorkerStatus::Completed).await;

        // A fix commit moves the tip: the recorded audit is now stale.
        std::fs::write(worker.worktree.join("fix.rs"), "pub fn fix() {}\n")
            .expect("worker fix");
        run_git(&worker.worktree, &["add", "fix.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "fix"]);

        let stale = team
            .review(
                &worker.id,
                "reviewed".into(),
                Vec::new(),
                crate::mnemo2::phase3::ReviewEvidence {
                    independent_reviewer_id: Some("audita001".into()),
                    audit_adjudication: Some("accepted".into()),
                    ..review_evidence()
                },
            )
            .await;
        assert!(stale.is_err(), "a stale audit must not link to the new tip");
        assert!(
            stale.err().unwrap_or_default().contains("stale"),
            "the stale refusal names the staleness"
        );

        // Reusing the stale reviewer identity is refused; a fresh reviewer
        // at the new tip is required.
        let reuse = team
            .audit_dispatch(&worker.id, "audita001", None, None, None)
            .await
            .expect_err("the moved tip requires a fresh reviewer");
        assert!(reuse.contains("older tip"), "{reuse}");
        team.audit_dispatch(&worker.id, "auditb002", None, None, None)
            .await
            .expect("fresh audit dispatch at the new tip");
        wait_for_worker_status(&team, "auditb002", WorkerStatus::Completed).await;
        let fresh = team.worker("auditb002").await.expect("fresh audit");
        let tip = crate::mnemo2::phase3::git_value(&worker.worktree, &["rev-parse", "HEAD"])
            .await
            .expect("current tip");
        assert_eq!(fresh.audit_tip.as_deref(), Some(tip.as_str()));

        team.review(
            &worker.id,
            "reviewed after the fix".into(),
            Vec::new(),
            crate::mnemo2::phase3::ReviewEvidence {
                independent_reviewer_id: Some("auditb002".into()),
                audit_adjudication: Some("the fresh audit covered the fix; accepted".into()),
                ..review_evidence()
            },
        )
        .await
        .expect("fresh review with a fresh audit");
        team.record_check(&worker.id, "cargo test".into(), "passed".into(), None)
            .await
            .expect("check");
        let merged = team.merge(&worker.id).await.expect("merge behind the fresh audit");
        assert_eq!(merged.status, WorkerStatus::Merged);

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn audit_workers_are_read_only_and_unanchored_from_implementer_claims() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let llm = Arc::new(ScriptedLlm::new(vec![
            text_turn(&["implementation done"]),
            text_turn(&["audit findings: none"]),
        ]));
        let (lead, team) =
            open_git_team_spawns(&root, &config, &state, llm.clone(), 2, 4).await;
        let plans = root.path().join(".coding/plans");
        std::fs::create_dir_all(&plans).expect("plans dir");
        std::fs::write(
            plans.join("stack.json"),
            r#"{"stack":["planid03"],"reviewed":false}"#,
        )
        .expect("stack");
        std::fs::write(
            plans.join("planid03.md"),
            "# Plan\n\n## Goal\nShip PLANID03-GOAL-MARKER.\n\n## Acceptance scenarios\n\n- The audit receives the plan.\n",
        )
        .expect("plan doc");
        run_git(root.path(), &["add", ".coding/plans"]);
        run_git(root.path(), &["commit", "-m", "plan"]);

        // One implementation worker with a distinctive intent brief.
        let worker = team
            .dispatch_with_id(
                "INTENT-MARKER-77 implement the flux capacitor".into(),
                None,
                Some("auditsk01".into()),
                None,
            )
            .await
            .expect("dispatch");
        wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        std::fs::write(worker.worktree.join("feature.rs"), "pub fn feature() {}\n")
            .expect("worker source");
        run_git(&worker.worktree, &["add", "feature.rs"]);
        run_git(&worker.worktree, &["commit", "-m", "feature"]);

        // A lead review whose summary and findings the audit must never see.
        team.review(
            &worker.id,
            "SUMMARY-MARKER-42 looks complete".into(),
            vec![crate::mnemo2::phase3::Finding {
                id: "find01".into(),
                severity: "low".into(),
                title: "FINDING-MARKER-99".into(),
                detail: "naming drift".into(),
                file: "src/feature.rs".into(),
                line: 0,
                status: "open".into(),
                fix_commit: None,
                evidence: None,
                justification: None,
            }],
            review_evidence(),
        )
        .await
        .expect("lead review");

        let (audit, reused) = team
            .audit_dispatch(
                &worker.id,
                "auditor07",
                None,
                Some("high".into()),
                Some("AUDIT-BRIEF-MARKER check concurrency".into()),
            )
            .await
            .expect("audit dispatch");
        assert!(!reused, "a fresh reviewer id starts a fresh audit");
        assert_eq!(audit.kind, "audit");
        assert_eq!(audit.name, "Audit ".to_string() + &audit.id[..8]);
        assert_eq!(audit.audit_target.as_deref(), Some(worker.id.as_str()));
        let tip = crate::mnemo2::phase3::git_value(&worker.worktree, &["rev-parse", "HEAD"])
            .await
            .expect("worker tip");
        assert_eq!(audit.audit_tip.as_deref(), Some(tip.as_str()));

        // The audit brief carries plan, intent, diff, and effort — and
        // never the implementer's summary, findings, or review.
        let brief = &audit.brief;
        for needle in [
            "PLANID03-GOAL-MARKER",
            "INTENT-MARKER-77",
            "Target diff",
            "AUDIT-BRIEF-MARKER",
            "reasoning depth: high",
            "feature.rs",
        ] {
            assert!(brief.contains(needle), "the audit brief is missing {needle}");
        }
        for leaked in ["SUMMARY-MARKER-42", "FINDING-MARKER-99", "naming drift"] {
            assert!(
                !brief.contains(leaked),
                "the audit brief leaks the implementer's claims: {leaked}"
            );
        }

        // The audit worktree sits at the audited tip and the reviewer's
        // advertised tools exclude every mutation surface.
        assert!(
            audit.worktree.join("feature.rs").is_file(),
            "the audit checks out the audited tip"
        );
        wait_for_worker_status(&team, "auditor07", WorkerStatus::Completed).await;
        let advertised = llm.advertised_tools();
        let audit_tools = advertised
            .iter()
            .rev()
            .find(|tools| !tools.iter().any(|name| name == "file_write"))
            .expect("the audit session advertised a read-only tool set");
        for name in ["read_files", "git_read", "search", "message_parent"] {
            assert!(
                audit_tools.iter().any(|tool| tool == name),
                "the audit lost its read tool {name}: {audit_tools:?}"
            );
        }
        for name in [
            "file_edit",
            "file_write",
            "multi_edit",
            "convert_line_endings",
            "shell",
            "git",
            "spawn_agent",
            "ask_user",
            "finish",
            "create_plan",
            "update_plan",
            "complete_step",
            "abandon_plan",
            "memory_write",
            "memory_supersede",
            "memory_consolidate",
        ] {
            assert!(
                !audit_tools.iter().any(|tool| tool == name),
                "the audit can mutate through {name}: {audit_tools:?}"
            );
        }

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn worker_message_parent_round_trips_to_the_lead_inbox() {
        use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions};

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(vec![
            tool_turn("message_parent", r#"{"text":"need the API key"}"#),
            text_turn(&["done"]),
        ]));
        let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("the lead opens");
        let team = TeamRuntime::open(TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("the shared engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: Some(1),
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("the team opens");
        let worker = team
            .dispatch("ask for the key".to_string(), None)
            .await
            .expect("the worker dispatches");
        let mut delivered = None;
        for _ in 0..100 {
            if let Some(message) = team.messages().await.into_iter().next() {
                delivered = Some(message);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let message = delivered.expect("the worker message reaches the lead inbox");
        assert_eq!(message.worker_id, worker.id);
        assert_eq!(message.text, "need the API key");
        assert!(
            llm.calls() >= 2,
            "the host-tool result drove a follow-up turn"
        );
        let _ = team.stop(&worker.id).await;
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn blocked_worker_notice_reaches_the_actual_lead_once() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let llm = Arc::new(ScriptedLlm::new(vec![
            tool_turn("message_parent", r#"{"text":"need the API key"}"#),
            text_turn(&["worker done"]),
            text_turn(&["lead acknowledged"]),
        ]));
        let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("lead opens");
        let team = lead.team_handle().await.expect("lead's actual team");
        let worker = team
            .dispatch_with_id(
                "ask for the key".into(),
                None,
                Some("notice0001".into()),
                None,
            )
            .await
            .expect("worker dispatches");
        let mut notified = false;
        for _ in 0..100 {
            let snapshot = lead.snapshot().await.expect("lead snapshot");
            let notices = snapshot
                .transcript
                .messages
                .iter()
                .filter(|message| message.content.as_text().starts_with(TEAM_NOTICE_PREFIX))
                .count();
            if notices == 1 && !snapshot.busy && llm.calls() >= 3 {
                notified = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(notified, "the worker notice must drive one real lead turn");
        let snapshot = lead.snapshot().await.expect("notified lead snapshot");
        let notice = snapshot
            .transcript
            .messages
            .iter()
            .find(|message| message.content.as_text().starts_with(TEAM_NOTICE_PREFIX))
            .expect("TeamManager-attributed notice");
        let text = notice.content.as_text();
        assert!(text.contains(&format!("worker_id={}", worker.id)), "{text}");
        assert!(text.contains("need the API key"), "{text}");
        assert!(text.contains("team_status"), "{text}");
        assert!(team.messages().await.iter().any(|message| {
            message.worker_id == worker.id && message.text == "need the API key"
        }));

        lead.shutdown().await;
    }

    #[tokio::test]
    async fn audit_completion_notice_asks_the_lead_to_adjudicate() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        init_git(root.path());
        run_git(root.path(), &["checkout", "-b", "wt/lead"]);
        write_config(config.path(), &["scripted-a"]);
        let plans = root.path().join(".coding/plans");
        std::fs::create_dir_all(&plans).expect("plans dir");
        std::fs::write(
            plans.join("stack.json"),
            r#"{"stack":["planid04"],"reviewed":false}"#,
        )
        .expect("stack");
        std::fs::write(plans.join("planid04.md"), "# Plan\n\n## Goal\nAudited change.\n")
            .expect("plan doc");
        run_git(root.path(), &["add", ".coding/plans"]);
        run_git(root.path(), &["commit", "-m", "plan"]);
        let llm = Arc::new(ScriptedLlm::new(vec![
            text_turn(&["worker done"]),
            text_turn(&["audit complete"]),
            text_turn(&["lead acknowledged"]),
        ]));
        let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
            .await
            .expect("lead opens");
        let team = lead.team_handle().await.expect("lead's actual team");
        let worker = team
            .dispatch_with_id(
                "implement the audited change".into(),
                None,
                Some("notice0002".into()),
                None,
            )
            .await
            .expect("worker dispatches");
        wait_for_worker_status(&team, &worker.id, crate::mnemo2::team::WorkerStatus::Completed)
            .await;
        let (audit, _reused) = team
            .audit_dispatch(&worker.id, "auditnot01", None, None, None)
            .await
            .expect("audit dispatches");
        wait_for_worker_status(&team, &audit.id, crate::mnemo2::team::WorkerStatus::Completed)
            .await;
        let mut notified = false;
        for _ in 0..100 {
            let snapshot = lead.snapshot().await.expect("lead snapshot");
            let notices = snapshot
                .transcript
                .messages
                .iter()
                .filter(|message| message.content.as_text().starts_with(TEAM_NOTICE_PREFIX))
                .count();
            if notices == 1 && !snapshot.busy && llm.calls() >= 3 {
                notified = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(notified, "the audit completion must drive one real lead turn");
        let snapshot = lead.snapshot().await.expect("notified lead snapshot");
        let notice = snapshot
            .transcript
            .messages
            .iter()
            .find(|message| message.content.as_text().starts_with(TEAM_NOTICE_PREFIX))
            .expect("TeamManager-attributed audit notice");
        let text = notice.content.as_text();
        assert!(text.contains(&format!("worker_id={}", audit.id)), "{text}");
        assert!(
            text.contains(&format!(
                "at tip {}.",
                audit.audit_tip.as_deref().unwrap_or("")
            )),
            "{text}"
        );
        assert!(text.contains("Adjudicate the audit"), "{text}");
        assert!(
            text.contains(&format!("independent_reviewer_id='{}'", audit.id)),
            "{text}"
        );
        assert!(text.contains("Never merge the audit worker itself"), "{text}");

        lead.shutdown().await;
    }

    #[tokio::test]
    async fn stopping_an_active_worker_preserves_its_brief() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(&root, &config, &state, Arc::new(SlowLlm::new())).await;
        let worker = team
            .dispatch("retain the running brief".into(), None)
            .await
            .expect("dispatch");
        let stopped = team.stop(&worker.id).await.expect("stop active worker");
        assert_eq!(stopped.status, crate::mnemo2::team::WorkerStatus::Stopped);
        assert!(stopped.transcript.messages.iter().any(|message| {
            message.content.as_text() == "retain the running brief"
        }));

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn compact_resumes_one_stopped_worker_and_persists_the_real_outcome() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let llm = Arc::new(ScriptedLlm::new(vec![
            text_turn(&["implemented"]),
            text_turn(&["compact summary"]),
        ]));
        let (lead, team) = open_git_team(&root, &config, &state, llm.clone()).await;
        let mut events = team.subscribe();
        let worker = team.dispatch("retain this work".into(), None).await.expect("dispatch");
        let mut worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        for _ in 0..100 {
            worker = team.worker(&worker.id).await.expect("worker");
            if worker.transcript.messages.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(worker.transcript.messages.len() >= 2, "the completed transcript is saved before stopping");
        team.stop(&worker.id).await.expect("stop before compaction");
        let requested = team.compact(&worker.id).await.expect("compaction accepted");
        assert_eq!(requested.id, worker.id);
        assert!(requested.generation > worker.generation);
        let compacted = wait_for_worker_status(&team, &worker.id, WorkerStatus::Idle).await;
        assert!(!compacted.compacting);
        assert!(compacted.error.is_none(), "{:?}", compacted.error);
        let observed: Vec<String> = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| format!("{:?}", event.event.event))
            .collect();
        assert_eq!(
            compacted.turns, worker.turns,
            "events: {observed:?}; before: {worker:?}; after: {compacted:?}"
        );
        assert!(observed.iter().any(|event| event.starts_with("CompactStarted")), "{observed:?}");
        assert!(observed.iter().any(|event| event.starts_with("Compacted { before:")), "{observed:?}");
        assert!(compacted.transcript.messages.len() >= 2);
        assert!(compacted.used_tokens > 0);

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[test]
    fn lead_state_rejects_a_different_project_identity() {
        let first = tempfile::tempdir().expect("first project");
        let second = tempfile::tempdir().expect("second project");
        let state = tempfile::tempdir().expect("state");
        LeadPersistence::new(state.path(), first.path())
            .save(Some("test/model".to_string()), Transcript::default())
            .expect("save lead");
        assert!(LeadPersistence::new(state.path(), second.path()).load().is_err());
    }

    #[tokio::test]
    async fn slots_restore_from_the_ledger_without_an_explicit_override() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(&root, &config, &state, Arc::new(ScriptedLlm::new(vec![]))).await;
        team.set_slots(5).await.expect("set slots");
        assert_eq!(team.snapshot().await.slots, 5);
        team.shutdown().await;
        let restored = crate::mnemo2::team::TeamRuntime::open(crate::mnemo2::team::TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            slots: None,
            max_spawns_per_turn: 1,
            model_specs: lead.models().iter().map(|model| model.spec.clone()).collect(),
            pricing: Vec::new(),
        })
        .await
        .expect("restore slots");
        assert_eq!(restored.snapshot().await.slots, 5);
        restored.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn cleaned_branch_can_be_reviewed_but_not_merged() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(
            &root,
            &config,
            &state,
            Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])])),
        )
        .await;
        let worker = team.dispatch("small retained task".to_string(), None).await.expect("dispatch");
        commit_worker_file(&worker, "retained.txt", "retained work\n", "retained change");
        let worker_tip = crate::mnemo2::phase3::tip(&worker.worktree).await.expect("worker tip");
        team.stop(&worker.id).await.expect("stop worker");
        let report = team.cleanup()
            .await
            .expect("clean checkout");
        assert!(!worker.worktree.exists(), "cleanup did not remove the checkout: {report:?}");
        let reviewed = team
            .review(&worker.id, "reviewed retained branch".to_string(), Vec::new(), review_evidence())
            .await
            .expect("review retained branch");
        assert_eq!(reviewed.status, crate::mnemo2::team::WorkerStatus::Cancelled);
        assert_eq!(reviewed.review.as_ref().expect("review").tip, worker_tip);
        let checked = team
            .record_check(&worker.id, "workspace tests".to_string(), "passed".to_string(), None)
            .await
            .expect("check retained branch");
        assert_eq!(checked.status, crate::mnemo2::team::WorkerStatus::Cancelled);
        assert_eq!(checked.checks[0].tip, worker_tip);
        let lead_tip = crate::mnemo2::phase3::tip(root.path()).await.expect("lead tip");
        assert!(team.merge(&worker.id).await.is_err());
        assert_eq!(crate::mnemo2::phase3::tip(root.path()).await.expect("unchanged lead"), lead_tip);
        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn executor_advertises_and_executes_coding_tools() {
        use crate::mnemo2::team::WorkerStatus;

        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let llm = Arc::new(ScriptedLlm::new(vec![
            tool_turn(
                "file_write",
                r#"{"path":"executor-proof.txt","content":"worker wrote this\n"}"#,
            ),
            tool_turn(
                "shell",
                r#"{"command":"echo worker-shell","purpose":"verify worker execution"}"#,
            ),
            tool_turn(
                "git",
                r#"{"subcommand":"commit","message":"executor proof"}"#,
            ),
            text_turn(&["done"]),
        ]));
        let (lead, team) = open_git_team(&root, &config, &state, llm.clone()).await;
        let worker = team
            .dispatch("write and commit a proof file".into(), None)
            .await
            .expect("dispatch");
        let mut worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        for _ in 0..100 {
            if worker.transcript.messages.iter().any(|message| {
                message.name.as_deref() == Some("git")
            }) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            worker = team.worker(&worker.id).await.expect("worker");
        }
        let advertised = llm.advertised_tools();
        let first = advertised.first().expect("worker request");
        for name in ["file_edit", "file_write", "shell", "git", "git_read"] {
            assert!(first.iter().any(|tool| tool == name), "{name} is not advertised to the worker: {first:?}");
        }
        assert_eq!(
            std::fs::read_to_string(worker.worktree.join("executor-proof.txt"))
                .expect("worker-written file"),
            "worker wrote this\n"
        );
        assert!(worker.transcript.messages.iter().any(|message| {
            message.name.as_deref() == Some("shell")
                && message.content.as_text().contains("worker-shell")
                && !message.tool_is_error
        }), "shell did not execute: {:?}", worker.transcript);
        for name in ["file_write", "git"] {
            assert!(worker.transcript.messages.iter().any(|message| {
                message.name.as_deref() == Some(name) && !message.tool_is_error
            }), "{name} did not execute successfully: {:?}", worker.transcript);
        }
        assert_eq!(
            crate::mnemo2::phase3::git_value(&worker.worktree, &["log", "-1", "--pretty=%s"])
                .await
                .expect("worker commit"),
            "executor proof"
        );

        team.shutdown().await;
        lead.shutdown().await;
    }

    #[tokio::test]
    async fn executor_profile_uses_mnemo2_tools_not_mnemo_workflow_reviewer() {
        let root = tempfile::tempdir().expect("workspace");
        let config = tempfile::tempdir().expect("config");
        let state = tempfile::tempdir().expect("state");
        let (lead, team) = open_git_team(&root, &config, &state, Arc::new(ScriptedLlm::new(vec![]))).await;
        let session = team
            .spawn_worker_session("Executor test".to_string(), None, root.path().to_path_buf(), None, "implementation")
            .await
            .expect("spawn executor");
        let tools = session.tool_names().await.expect("registered tool names");
        for name in ["file_edit", "file_write", "shell", "git", "git_read"] {
            assert!(tools.iter().any(|tool| tool == name), "missing coding tool {name}");
        }
        for name in ["create_plan", "update_plan", "finish", "spawn_agent", "ask_user"] {
            assert!(!tools.iter().any(|tool| tool == name), "unexpected workflow tool {name}");
        }
        assert!(tools.iter().any(|tool| tool == "message_parent"));

        let head = mnemo::headless::assemble_spec_head(
            &crate::mnemo2::team::executor_prompt(),
            &mnemo::headless::Constitution::default(),
        );
        assert!(head.contains("Mnemo2 executor worker"));
        assert!(head.to_ascii_lowercase().contains("file editing"));
        assert!(head.contains("Git commits"));
        assert!(head.contains("lead reviews your work on its main thread"));
        assert!(!head.contains("spawn_agent"));

        // The audit profile is the read-only counterpart: every mutation
        // surface is gone, and the identity forbids lifecycle actions.
        let audit = team
            .spawn_worker_session("Audit test".to_string(), None, root.path().to_path_buf(), None, "audit")
            .await
            .expect("spawn audit");
        let audit_tools = audit.tool_names().await.expect("registered audit tools");
        for name in ["read_files", "search", "git_read", "graph_search", "message_parent"] {
            assert!(
                audit_tools.iter().any(|tool| tool == name),
                "the audit lost its read tool {name}"
            );
        }
        for name in [
            "file_edit",
            "file_write",
            "multi_edit",
            "convert_line_endings",
            "shell",
            "git",
            "create_plan",
            "update_plan",
            "complete_step",
            "abandon_plan",
            "finish",
            "spawn_agent",
            "ask_user",
            "memory_write",
            "memory_consolidate",
        ] {
            assert!(
                !audit_tools.iter().any(|tool| tool == name),
                "the audit must not mutate through {name}: {audit_tools:?}"
            );
        }
        let audit_head = mnemo::headless::assemble_spec_head(
            &crate::mnemo2::team::audit_prompt(),
            &mnemo::headless::Constitution::default(),
        );
        assert!(audit_head.contains("independent audit worker"));
        assert!(audit_head.contains("read-only by construction"));
        assert!(audit_head.contains("Never merge"));
        team.shutdown().await;
        lead.shutdown().await;
    }
}
