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

/// The Tauri event channel name used by the IPC layer.
pub const LEAD_EVENT_CHANNEL: &str = "mnemo2://lead-event";

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

    /// Start a turn with a user prompt.
    pub async fn send_prompt(&self, text: String) -> Result<(), String> {
        if text.trim().is_empty() {
            return Err("the prompt is empty".to_string());
        }
        if let Some(team) = self.team.lock().await.clone() {
            team.begin_lead_turn();
        }
        let live = self.live.lock().await;
        let live = live
            .as_ref()
            .ok_or_else(|| "the lead session is not open".to_string())?;
        if live.busy.load(Ordering::SeqCst) {
            return Err("the lead is still working on the previous turn".to_string());
        }
        live.busy.store(true, Ordering::SeqCst);
        live.session
            .send(SessionCommand::Prompt {
                text,
                images: Vec::new(),
            })
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
            *self.live.lock().await = Some(old);
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
            team,
        ));
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
                *observed_model.lock().await = Some(changed.clone());
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
        Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice,
        ToolSchema,
    };
    use tempfile::TempDir;

    struct ScriptedLlm {
        responses: std::sync::Mutex<VecDeque<Vec<LlmEvent>>>,
        calls: std::sync::atomic::AtomicUsize,
        caps: Capabilities,
    }

    impl ScriptedLlm {
        fn new(responses: Vec<Vec<LlmEvent>>) -> Self {
            Self {
                responses: std::sync::Mutex::new(responses.into()),
                calls: std::sync::atomic::AtomicUsize::new(0),
                caps: Capabilities::openai(),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
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
            "scripted-a"
        }

        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let next = self
                .responses
                .lock()
                .expect("script lock poisoned")
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter(next)))
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
                pricing,
            })
            .await
            .expect("team opens");
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
        for _ in 0..100 {
            let worker = team.worker(worker_id).await.expect("worker record");
            if worker.status == status {
                return worker;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let worker = team.worker(worker_id).await.expect("worker record");
        panic!("worker did not reach {status:?}: {worker:?}");
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
            .send_prompt("hi".to_string())
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
            .send_prompt("remember this".to_string())
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
            .send_prompt("first turn".to_string())
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
            .send_prompt("write the note".to_string())
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
            .send_prompt("ask me".to_string())
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
            .send_prompt("take your time".to_string())
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
        team.review(&worker.id, "initial review".into(), vec![finding])
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
        team.review(&worker.id, "delta review".into(), fixed.findings.clone())
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
        team.review(&worker.id, "initial review".into(), vec![finding])
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
            .review(&worker.id, "delta review".into(), resolved.findings.clone())
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
        team.review(&worker.id, "reviewed".into(), Vec::new())
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
        team.review(&worker.id, "reviewed".into(), Vec::new())
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
        team.review(&worker.id, "reviewed".into(), Vec::new())
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
        team.review(&worker.id, "reviewed".into(), Vec::new())
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
        team.review(&worker.id, "reviewed".into(), Vec::new())
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
    async fn token_budget_pause_raise_stop_all_and_restart_use_real_usage() {
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
            usage_turn(80, 20, 10),
            usage_turn(30, 20, 5),
            text_turn(&["continued"]),
        ]));
        let (lead, team) = open_git_team_with_pricing(&root, &config, &state, llm, pricing).await;
        let team = team.with_worker_token_budget(150);
        let worker = team
            .dispatch("usage task".into(), None)
            .await
            .expect("dispatch");
        let first = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
        assert_eq!(first.total_tokens, 100);
        assert_eq!(first.cost_micros, Some(350));
        team.message(&worker.id, "second usage turn".into())
            .await
            .expect("second turn");
        let exhausted = wait_for_worker_status(&team, &worker.id, WorkerStatus::NeedsYou).await;
        assert_eq!(exhausted.total_tokens, 150);
        assert_eq!(exhausted.prompt_tokens, 110);
        assert_eq!(exhausted.completion_tokens, 40);
        assert_eq!(exhausted.cached_tokens, 15);
        assert_eq!(exhausted.requests, 2);
        assert_eq!(exhausted.cost_micros, Some(605));
        assert!(exhausted
            .error
            .as_deref()
            .is_some_and(|error| error.contains("increase the budget")));
        let blocked = team
            .message(&worker.id, "continue".into())
            .await
            .expect_err("budget blocks continuation");
        assert!(
            blocked.contains("increase the worker token budget"),
            "{blocked}"
        );

        let raised = team
            .set_token_budget(&worker.id, 160)
            .await
            .expect("raise budget");
        assert_eq!(raised.token_budget, 160);
        assert!(raised.error.is_none());
        team.message(&worker.id, "continue".into())
            .await
            .expect("continue after raise");
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
            pricing: Vec::new(),
        })
        .await
        .expect("reopen");
        let restored = reopened.worker(&worker.id).await.expect("restored worker");
        assert_eq!(restored.total_tokens, 150);
        assert_eq!(restored.requests, 2);
        assert_eq!(restored.cost_micros, Some(605));
        assert_eq!(restored.token_budget, 160);

        team.shutdown().await;
        reopened.shutdown().await;
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
            .review(&worker.id, "reviewed retained branch".to_string(), Vec::new())
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
}
