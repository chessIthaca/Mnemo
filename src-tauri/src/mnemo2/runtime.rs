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

use mnemo::config::{Config, SafetyMode};
use mnemo::headless::{
    AgentSession, Engine, EngineOptions, EventReceiver, MemoryBackend, PendingInteraction,
    SessionCommand, SessionEvent, SessionId, SpawnOptions, Transcript,
};
use mnemo::provider::LlmClient;
use mnemo::workspace::Workspace;
use mnemo::app::policy_factory::WorkflowPolicyFactory;
use mnemo::{Approval, PermissionPolicy, SerializableAgentEvent, UserAnswer};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

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

/// A saved lead conversation, restored on the next launch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedLead {
    model: Option<String>,
    transcript: Transcript,
}

/// The small, atomic-ish state file for the lead transcript.
#[derive(Clone, Debug)]
struct LeadPersistence {
    path: PathBuf,
}

impl LeadPersistence {
    fn new(state_dir: &Path) -> Self {
        Self {
            path: state_dir.join(LEAD_STATE_FILE),
        }
    }

    fn load(&self) -> Option<PersistedLead> {
        let bytes = std::fs::read(&self.path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn save(&self, lead: &PersistedLead) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("could not create Mnemo2 state dir: {err}"))?;
        }
        let bytes = serde_json::to_vec_pretty(lead)
            .map_err(|err| format!("could not encode the lead transcript: {err}"))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)
            .map_err(|err| format!("could not write the lead transcript: {err}"))?;
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
        }
        std::fs::rename(&temp, &self.path)
            .map_err(|err| format!("could not replace the lead transcript: {err}"))
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
        let persistence = LeadPersistence::new(&options.state_dir);
        let persisted = persistence.load().unwrap_or_default();
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
        };
        runtime.start_engine(provider).await?;
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
        if let Err(err) = self.persistence.save(&PersistedLead {
            model: Some(spec),
            transcript: snapshot.transcript,
        }) {
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
        engine.register_policy("mnemo.workflow", Arc::new(WorkflowPolicyFactory::default()));
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
        let session = engine
            .spawn(SpawnOptions {
                    name: Some("Lead".to_string()),
                    model: spawn_model.clone(),
                    worktree: None,
                    permissions: None,
                    policy: self.policy.clone(),
                    resume: Some(resume),
                    limits: None,
                    host_tools: Vec::new(),
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
) {
    while let Ok(event) = receiver.recv().await {
        let terminal = match &event.event {
            SerializableAgentEvent::Started => {
                busy.store(true, Ordering::SeqCst);
                false
            }
            SerializableAgentEvent::Finished { .. }
            | SerializableAgentEvent::Exited
            | SerializableAgentEvent::Parked { .. } => {
                busy.store(false, Ordering::SeqCst);
                true
            }
            SerializableAgentEvent::Error { retrying, .. } => {
                if !retrying {
                    busy.store(false, Ordering::SeqCst);
                }
                false
            }
            SerializableAgentEvent::ModelChanged {
                model: changed, ..
            } => {
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
                        let _ = persistence.save(&PersistedLead {
                            model,
                            transcript: history,
                        });
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
                    0 => Some((LlmEvent::TextDelta { text: "slow".into() }, 1)),
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

    fn init_git(root: &std::path::Path) {
        let run = |args: &[&str]| {
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
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Mnemo2 Test"]);
        std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
        run(&["add", "."]);
        run(&["commit", "-m", "seed"]);
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
            slots: 2,
            max_spawns_per_turn: 2,
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
                    texts.entry(event.worker_id.clone()).or_default().push_str(text);
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
            vec!["first worker done".to_string(), "second worker done".to_string()],
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
            slots: 2,
            max_spawns_per_turn: 2,
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
            slots: 2,
            max_spawns_per_turn: 2,
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
            slots: 1,
            max_spawns_per_turn: 1,
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
}
