use super::*;

macro_rules! tiers_dto {
    (simple: $simple:expr, average: $average:expr, difficult: $difficult:expr $(,)?) => {{
        let mut dto = mnemo::headless::TiersDto::default();
        dto.tiers[0].models = $simple;
        dto.tiers[1].models = $average;
        dto.tiers[3].models = $difficult;
        dto
    }};
    (average: $average:expr, ..Default::default() $(,)?) => {{
        let mut dto = mnemo::headless::TiersDto::default();
        dto.tiers[1].models = $average;
        dto
    }};
}
use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, MessageContent, ProviderKind,
    ToolChoice, ToolSchema,
};
use tempfile::TempDir;

use crate::mnemo2::team::{KICKOFF, READ_BACK_KICKOFF, WorkerSessionSpec};

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

/// Append `[general.laya] enabled = true` to the fixture config — the
/// same flip the kernel seam's tests make (setup.rs's
/// `enable_laya_in_config`).
fn enable_laya_in_config(dir: &Path) {
    let path = dir.join("config.toml");
    let mut text = std::fs::read_to_string(&path).expect("fixture config.toml");
    text.push_str("\n[general.laya]\nenabled = true\n");
    std::fs::write(&path, text).expect("writing config.toml");
}

/// Append Mnemo1's per-state `[models]` section to the fixture config —
/// the shared-config shape plan 7f0b3848 tests against: the slots belong
/// to Mnemo1's workflow and live in the same config.toml Mnemo2 reads.
fn enable_per_state_models_in_config(dir: &Path) {
    let path = dir.join("config.toml");
    let mut text = std::fs::read_to_string(&path).expect("fixture config.toml");
    text.push_str("\n[models.executing]\nendpoint = \"test\"\nmodel = \"scripted-b\"\n");
    std::fs::write(&path, text).expect("writing config.toml");
}

/// Doctor a PRETEND-installed managed runtime under `<config_dir>/laya`
/// — the same fake the kernel seam's tests plant (setup.rs's
/// `doctor_installed_laya`): exactly the two files
/// `is_checkpoint_installed` probes (the marker + the venv's
/// `laya-serve`). The fake binary is deliberately NOT a valid
/// executable image: even if the fire-and-forget start task the
/// attach parks ever runs, the child spawn fails instantly — no real
/// sidecar, no network.
fn doctor_installed_laya(dir: &Path) {
    let base = dir.join("laya");
    std::fs::create_dir_all(base.join("markers")).expect("markers dir");
    std::fs::write(
        base.join("markers")
            .join(mnemo::laya::english_checkpoint().id),
        b"installed",
    )
    .expect("marker file");
    let serve = mnemo::laya::venv_bin(
        &base.join("venv"),
        std::env::consts::OS,
        "laya-serve",
    );
    std::fs::create_dir_all(serve.parent().expect("venv bin dir")).expect("venv bin dir");
    std::fs::write(&serve, b"not a real executable").expect("fake laya-serve");
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
) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
    open_git_team_with_pricing(root, config, state, llm, Vec::new()).await
}

async fn open_git_team_with_pricing(
    root: &TempDir,
    config: &TempDir,
    state: &TempDir,
    llm: Arc<dyn LlmClient>,
    pricing: Vec<mnemo::config::PricingEntry>,
) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
    open_git_team_slots(root, config, state, llm, pricing, 1).await
}

async fn open_git_team_slots(
    root: &TempDir,
    config: &TempDir,
    state: &TempDir,
    llm: Arc<dyn LlmClient>,
    pricing: Vec<mnemo::config::PricingEntry>,
    slots: usize,
) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
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
            config_dir: state.path().join("config"),
            classifier: None,
            routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
            slots: Some(slots),
            models: lead.models(),
            pricing,
        })
        .await
        .expect("team opens");
    let team = Arc::new(team);
    team.attach_board(lead.board_handle().expect("the lead's plan board"));
    (lead, team)
}

/// [`open_git_team`] with configurable concurrency: audit flows need
/// more than one concurrent worker.
async fn open_git_team_spawns(
    root: &TempDir,
    config: &TempDir,
    state: &TempDir,
    llm: Arc<dyn LlmClient>,
    slots: usize,
) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
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
            config_dir: state.path().join("config"),
            classifier: None,
            routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
            slots: Some(slots),
            models: lead.models(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
    let team = Arc::new(team);
    team.attach_board(lead.board_handle().expect("the lead's plan board"));
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

/// Create a board plan with the given steps and return its id.
fn new_plan(lead: &LeadRuntime, steps: &[&str]) -> String {
    lead.board_handle()
        .expect("the lead's plan board")
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Board plan".into(),
            goal: "Ship the change".into(),
            context:
                "verified: the host owns dispatch; src/lib.rs is the anchor file for the change"
                    .into(),
            steps: steps.iter().map(|step| step.to_string()).collect(),
            ..Default::default()
        })
        .expect("the plan is created")
        .id
}

/// A one-step board plan with `goal` and one acceptance scenario, so it
/// takes high-risk dispatches and audits.
fn new_audited_plan(lead: &LeadRuntime, goal: &str, scenario: &str) -> String {
    lead.board_handle()
        .expect("the lead's plan board")
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Audited plan".into(),
            goal: goal.into(),
            context:
                "verified: the host owns dispatch; src/lib.rs is the anchor file for the change"
                    .into(),
            steps: vec!["**Change** - implement src/lib.rs".into()],
            acceptance: vec![scenario.into()],
            ..Default::default()
        })
        .expect("the plan is created")
        .id
}

/// Dispatch through the lead tool under `plan`/`step`, then make the
/// worker mergeable: stopped, one committed file, reviewed, checked.
async fn mergeable_worker(
    team: &crate::mnemo2::team::TeamRuntime,
    worker_id: &str,
    plan: &str,
    step: u64,
    file: &str,
) -> crate::mnemo2::team::WorkerRecord {
    team.begin_lead_turn();
    team.set_lead_busy(false);
    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": worker_id,
                "plan_id": plan,
                "step": step,
                "brief": format!("write {file}"),
            }),
        )
        .await;
    assert!(dispatched.success, "{}", dispatched.output);
    let worker = team.worker(worker_id).await.expect("tracked worker");
    assert_eq!(worker.name, worker_id, "a worker the lead names keeps that name");
    team.stop(worker_id).await.expect("stop");
    commit_worker_file(&worker, file, "pub fn f() {}\n", "work");
    team.review(
        worker_id,
        "reviewed and fine".into(),
        Vec::new(),
        review_evidence(),
    )
        .await
        .expect("review");
    team.record_check(worker_id, "cargo test".into(), "passed".into(), None)
        .await
        .expect("check");
    worker
}


struct DispatchRoutingClassifier;

#[async_trait::async_trait]
impl mnemo::memory::Classifier for DispatchRoutingClassifier {
    async fn classify(
        &self,
        _state: &str,
        _question: &mnemo::memory::Question,
    ) -> Option<mnemo::memory::Answer> {
        Some(mnemo::memory::Answer::Choice {
            label: mnemo::agent::route_question::LABEL_HARD.into(),
            confidence: 0.71,
            probabilities: std::collections::BTreeMap::new(),
        })
    }
}

#[tokio::test]
async fn dispatch_routing_survives_a_real_worker_start() {
    let root = tempfile::tempdir().expect("workspace");
    let state = tempfile::tempdir().expect("state");
    let config = tempfile::tempdir().expect("config");
    init_git(root.path());
    crate::mnemo2::dispatch_routing::save_routing_settings(
        config.path(),
        &crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
    )
    .expect("save shadow routing mode");
    let mut server = StubServer::start(vec![StubTurn {
        body: stub_text_turn("routing worker started"),
        gate: None,
    }])
    .await;
    write_stub_config(config.path(), server.addr);
    let engine = std::sync::Arc::new(
        Engine::start(EngineOptions {
            workspace: mnemo::workspace::Workspace::new(root.path()),
            config_dir: Some(config.path().to_path_buf()),
            permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
            memory: mnemo::headless::MemoryBackend::Disabled,
            index_codegraph: false,
            require_git: true,
            auto_install_embedder: false,
        })
        .await
        .expect("start the engine"),
    );
    let mut team = TeamRuntime::open(TeamRuntimeOptions {
        engine,
        workspace_root: root.path().to_path_buf(),
        state_dir: state.path().to_path_buf(),
        config_dir: config.path().to_path_buf(),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: Vec::new(),
        pricing: Vec::new(),
    })
    .await
    .expect("open the team runtime");
    team.set_catalog(
        vec![LeadModel {
            endpoint: "test".into(),
            model: "model-a".into(),
            spec: "test/model-a".into(),
            is_default: true,
            reasoning_efforts: Vec::new(),
            default_effort: None,
        }],
        tiers_dto! {
            simple: vec!["test/model-a".into()],
            average: vec!["test/model-a".into()],
            difficult: vec!["test/model-a".into()],
        },
        Vec::new(),
    );
    let classifier: std::sync::Arc<dyn mnemo::memory::Classifier> =
        std::sync::Arc::new(DispatchRoutingClassifier);
    team.set_classifier_for_test(std::sync::Arc::new(std::sync::RwLock::new(Some(
        classifier,
    ))));
    let board = std::sync::Arc::new(mnemo::app::plan_board::PlanBoard::new(
        root.path().join(".coding/plans"),
        root.path().join(".coding/reviews"),
    ));
    let plan = board
        .create(mnemo::app::plan_board::PlanDraft {
            title: "Real dispatch routing".into(),
            goal: "Verify routing metadata survives an actual worker session start".into(),
            context: "The stub server must receive the first request from the routed worker.".into(),
            steps: vec!["Implement the complete routing behavior in src-tauri/src/mnemo2/team.rs and verify the result with focused tests".into()],
            ..mnemo::app::plan_board::PlanDraft::default()
        })
        .expect("create plan");
    let team = Arc::new(team);
    team.attach_board(board);

    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "routingworker",
                "plan_id": plan.id,
                "step": 1,
                "brief": "Implement the assigned step and report the checks that were run.",
                "tier": "routine",
            }),
        )
        .await;
    assert!(dispatched.success, "{}", dispatched.output);
    // The explicit tier never waits on the classification (plan
    // c74b1987): the dispatch answers while the label is deferred.
    assert!(dispatched.output.contains(
        "Laya: classifying in the background; the label will land on the routing record."
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), server.next_arrival())
        .await
        .expect("the worker's first request arrives");
    // The deferred label lands on the record and survives the real
    // session start; `routed_by` resolved at dispatch time, so the
    // override is corpus-only and the record keeps "lead".
    let routing = {
        let mut routing = None;
        for _ in 0..400 {
            if let Ok(worker) = team.worker("routingworker").await {
                if let Some(block) = worker.routing {
                    if block.laya_tier.is_some() {
                        routing = Some(block);
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        routing.expect("the deferred label landed")
    };
    assert_eq!(routing.laya_tier.as_deref(), Some("high"));
    assert_eq!(routing.routed_by, "lead");
    let status = team.handle_lead_host_tool("team_status", serde_json::json!({})).await;
    assert!(status.success, "{}", status.output);
    let status: serde_json::Value = serde_json::from_str(&status.output).expect("status JSON");
    let workers = status["result"].as_array().expect("workers");
    let worker = workers
        .iter()
        .find(|worker| worker["id"] == "routingworker")
        .expect("routing worker status");
    assert_eq!(worker["routing"]["laya_tier"], "high");
    assert_eq!(worker["routing"]["routed_by"], "lead");
    team.shutdown().await;

}

fn drain_notices(
    receiver: &mut broadcast::Receiver<crate::mnemo2::team::BoardSignal>,
) -> Vec<String> {
    let mut notices = Vec::new();
    while let Ok(signal) = receiver.try_recv() {
        if let crate::mnemo2::team::BoardSignal::Notice { text, .. } = signal {
            notices.push(text);
        }
    }
    notices
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
async fn shutdown_completes_with_an_open_project_and_team() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (lead, _team) = open_git_lead(
        &root,
        &config,
        &state,
        Arc::new(ScriptedLlm::new(Vec::new())),
    )
    .await;
    let mut app_state = crate::mnemo2::ipc::Mnemo2State::default();
    app_state.install_runtime_for_test(Arc::new(lead));

    tokio::time::timeout(Duration::from_secs(5), app_state.shutdown())
        .await
        .expect("shutdown with an open project and team completes");
}

/// Design §6 step 2's disabled pin: with no `[general.laya]` table
/// (the shipped default) — even with a pretend install present — the
/// engine start leaves the classifier slot empty and the runtime
/// holds no manager: a byte-identical pre-Laya startup.
#[tokio::test]
async fn laya_disabled_leaves_the_classifier_slot_empty() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    // The install alone must not attach anything — only the flag does.
    doctor_installed_laya(config.path());
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens with Laya disabled");
    let engine = runtime.engine_handle().await.expect("engine handle");
    let setup = mnemo::headless::Setup::open(Some(config.path().to_path_buf()))
        .expect("open")
        .for_engine(&engine);
    let slot = setup
        .classifier_slot()
        .expect("engine-backed: the slot handle");
    assert!(
        slot.read().expect("classifier slot lock").is_none(),
        "a disabled Laya leaves the classifier slot empty"
    );
    assert!(
        runtime.laya_manager().is_none(),
        "no manager is held while Laya is disabled"
    );
    assert!(
        matches!(
            *setup
                .laya_manager()
                .status_arc()
                .read()
                .expect("classifier status lock"),
            mnemo::memory::classifier::ClassifierStatus::Disabled
        ),
        "the shared status stays in its creation state"
    );
    runtime.shutdown().await;
}

/// Design §6 step 2's attach pin: with `[general.laya]` enabled and a
/// pretend-installed runtime, the engine start fills the engine's live
/// classifier slot and the runtime holds the manager the attach
/// spawned through (the workers share the engine's brain, so this one
/// fill covers every session). `shutdown` then stops that manager
/// BEFORE the engine drops — the observable is the runtime no longer
/// holding it (the explicit stop is the primary path; the manager's
/// Drop is only the safety net). No real sidecar ever runs: the
/// fire-and-forget start task the attach parks fails instantly on the
/// doctored (invalid-image) binary, so by assertion time the status
/// reads Starting or Failed — never its creation state.
#[tokio::test]
async fn laya_enabled_and_installed_fills_the_slot_and_shutdown_stops_it() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    enable_laya_in_config(config.path());
    doctor_installed_laya(config.path());
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens with Laya enabled + a doctored install");
    let engine = runtime.engine_handle().await.expect("engine handle");
    let setup = mnemo::headless::Setup::open(Some(config.path().to_path_buf()))
        .expect("open")
        .for_engine(&engine);
    let slot = setup
        .classifier_slot()
        .expect("engine-backed: the slot handle");
    assert!(
        slot.read().expect("classifier slot lock").is_some(),
        "the engine's live classifier slot is filled at start"
    );
    let manager = runtime
        .laya_manager()
        .expect("the runtime holds the manager the attach spawned through");
    assert_eq!(
        manager.base_dir(),
        config.path().join("laya"),
        "the held manager roots at the config dir's laya/"
    );
    assert!(
        !matches!(
            *setup
                .laya_manager()
                .status_arc()
                .read()
                .expect("classifier status lock"),
            mnemo::memory::classifier::ClassifierStatus::Disabled
        ),
        "the attach parked the sidecar start (Starting or already Failed)"
    );
    runtime.shutdown().await;
    assert!(
        runtime.laya_manager().is_none(),
        "shutdown stopped the managed runtime before the engine dropped"
    );
}

#[tokio::test]
async fn production_lead_open_wires_the_live_classifier_slot() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    enable_laya_in_config(config.path());
    doctor_installed_laya(config.path());
    crate::mnemo2::dispatch_routing::save_routing_settings(
        config.path(),
        &crate::mnemo2::dispatch_routing::RoutingSettings {
            mode: crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
            ..crate::mnemo2::dispatch_routing::RoutingSettings::default()
        },
    )
    .expect("save the lead's Mnemo2 routing settings");
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("open the lead through the production team path");
    let engine = runtime.engine_handle().await.expect("engine handle");
    let setup = mnemo::headless::Setup::open(Some(config.path().to_path_buf()))
        .expect("open setup")
        .for_engine(&engine);
    let engine_slot = setup
        .classifier_slot()
        .expect("engine-backed classifier slot");
    let team = runtime.team_handle().await.expect("production team");
    let team_slot = team
        .classifier_slot_for_test()
        .expect("the team shares the live classifier slot");
    assert!(Arc::ptr_eq(&engine_slot, &team_slot));
    assert_eq!(
        team.routing_settings_for_test().mode,
        crate::mnemo2::dispatch_routing::RoutingMode::Shadow,
        "mnemo2.toml is loaded from the lead's config directory"
    );
    runtime.shutdown().await;
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
async fn a_reopened_lead_answers_a_new_prompt() {
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
        .expect("the first prompt is accepted");
    drain_turn(&mut rx).await;
    let saved = wait_for_persisted(&state.path().join(LEAD_STATE_FILE)).await;
    let restored_len = saved.transcript.messages.len();
    first.shutdown().await;

    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["after ", "reopen"])]));
    let second = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("the lead reopens");
    let mut rx = second.subscribe();
    second
        .send_prompt("are you there?".to_string(), Vec::new())
        .await
        .expect("the reopened prompt is accepted");
    let events = drain_turn(&mut rx).await;
    let text = events
        .iter()
        .filter_map(|event| match &event.event.event {
            SerializableAgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "after reopen");
    assert_eq!(llm.calls(), 1);
    let snapshot = second.snapshot().await.expect("snapshot");
    assert!(!snapshot.busy, "the reopened lead returned to idle");
    assert_eq!(snapshot.transcript.messages.len(), restored_len + 2);
    assert_eq!(
        serde_json::to_value(&snapshot.transcript.messages[..restored_len])
            .expect("serialize the resumed prefix"),
        serde_json::to_value(&saved.transcript.messages)
            .expect("serialize the saved history"),
        "the restored history is retained"
    );
    for _ in 0..100 {
        if let Ok(bytes) = std::fs::read(state.path().join(LEAD_STATE_FILE)) {
            if let Ok(saved) = serde_json::from_slice::<PersistedLead>(&bytes) {
                if saved.transcript.messages.len() == restored_len + 2 {
                    assert_eq!(
                        serde_json::to_value(&saved.transcript.messages)
                            .expect("serialize the persisted transcript"),
                        serde_json::to_value(&snapshot.transcript.messages)
                            .expect("serialize the live transcript")
                    );
                    second.shutdown().await;
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the reopened lead did not persist its longer transcript");
}

#[tokio::test]
async fn a_new_project_holds_until_its_first_code_index_ends() {
    use crate::mnemo2::indexing::PassState;

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    std::fs::create_dir_all(root.path().join("src")).expect("src");
    std::fs::write(root.path().join("src/lib.rs"), "pub fn indexed() {}\n").expect("source");
    write_config(config.path(), &["scripted-a"]);
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens");
    assert!(runtime.indexing().snapshot().first_run);

    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let code = loop {
        let code = runtime.indexing().snapshot().code.expect("first-run code pass");
        if code.state != PassState::Running {
            break code;
        }
        assert!(std::time::Instant::now() < deadline, "the first index never ended");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(code.state, PassState::Done);
    runtime.shutdown().await;

    // The index now exists: reopening never holds the project.
    let reopened = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead reopens");
    let opened = reopened.indexing().snapshot();
    assert!(!opened.first_run);
    // Every open still shows its catch-up pass, and that pass ends.
    assert!(opened.code.is_some(), "the reopen shows its code pass");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while reopened.indexing().snapshot().code.map(|code| code.state) == Some(PassState::Running) {
        assert!(std::time::Instant::now() < deadline, "the catch-up pass never ended");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        reopened.indexing().snapshot().code.map(|code| code.state),
        Some(PassState::Done)
    );
    reopened.shutdown().await;
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
        .set_model("test/scripted-b".to_string(), None)
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
        .set_model("test/not-configured".to_string(), None)
        .await
        .is_err());
    runtime.shutdown().await;
}

/// `/new` (plan 13336839 step 1): a fresh conversation keeps the chosen
/// model and effort, empties the live transcript, advances the generation,
/// and writes the empty transcript to `lead.json`.
#[tokio::test]
async fn new_conversation_empties_the_transcript_and_keeps_the_model() {
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
    let fresh_len = runtime
        .snapshot()
        .await
        .expect("a fresh snapshot")
        .transcript
        .messages
        .len();
    let mut rx = runtime.subscribe();
    runtime
        .send_prompt("first turn".to_string(), Vec::new())
        .await
        .expect("prompt");
    drain_turn(&mut rx).await;
    // The pump persists a finished turn from a deferred task (it sleeps 10 ms
    // after the terminal event); wait for that write before the switch, so the
    // assertions at the end see this call's own save and not the first turn's
    // late one.
    let after_turn = wait_for_persisted(&state.path().join(LEAD_STATE_FILE)).await;
    assert_eq!(
        after_turn.model.as_deref(),
        Some("test/scripted-a"),
        "the first turn persisted on the model it ran on"
    );
    let switched = runtime
        .set_model("test/scripted-b".to_string(), None)
        .await
        .expect("the model switch succeeds");
    assert_eq!(switched.model.as_deref(), Some("test/scripted-b"));
    assert!(
        !switched.transcript.messages.is_empty(),
        "the transcript carries over"
    );
    let before = runtime.snapshot().await.expect("snapshot");
    let cleared = runtime
        .new_conversation()
        .await
        .expect("the new conversation starts");
    assert_eq!(
        cleared.transcript.messages.len(),
        fresh_len,
        "the new conversation starts as empty as a freshly opened lead"
    );
    assert_eq!(cleared.model.as_deref(), Some("test/scripted-b"));
    assert_eq!(cleared.model_id.as_deref(), Some("scripted-b"));
    assert!(
        cleared.generation > before.generation,
        "the generation advances so stale events cannot touch the new UI"
    );
    let saved: PersistedLead = serde_json::from_slice(
        &std::fs::read(state.path().join(LEAD_STATE_FILE)).expect("lead.json"),
    )
    .expect("lead JSON");
    assert_eq!(saved.model.as_deref(), Some("test/scripted-b"));
    assert_eq!(
        serde_json::to_value(&saved.transcript).expect("the persisted transcript"),
        serde_json::to_value(&cleared.transcript).expect("the live transcript"),
        "lead.json holds the empty transcript"
    );
    runtime.shutdown().await;
}

/// `/new` while a turn runs is refused up front: the running turn (and any
/// approval or question it waits on) must be stopped first.
#[tokio::test]
async fn new_conversation_is_refused_while_the_lead_works() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new_gated(
            vec!["scripted-a"],
            vec![(Some(gate_rx), text_turn(&["turn one"]))],
        ))),
    )
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

    let refused = runtime
        .new_conversation()
        .await
        .expect_err("a busy lead cannot start a new conversation");
    assert!(
        refused.contains("stop the current turn"),
        "the refusal asks to stop the turn first: {refused}"
    );

    gate_tx.send(()).expect("the first turn is released");
    drain_turn(&mut rx).await;
    runtime.shutdown().await;
}

/// `/new` on a lead that never chose a model writes no explicit model to
/// `lead.json`: the configured default keeps serving, and a reopen restores
/// that same default.
#[tokio::test]
async fn new_conversation_after_reopen_keeps_the_default_model() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a", "scripted-b"]);
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens");
    let cleared = runtime
        .new_conversation()
        .await
        .expect("the new conversation starts");
    assert_eq!(
        cleared.model.as_deref(),
        Some("test/scripted-a"),
        "the configured default model keeps serving"
    );
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.path().join(LEAD_STATE_FILE)).expect("lead.json"),
    )
    .expect("lead JSON");
    assert!(
        saved["model"].is_null(),
        "no explicit model choice was written: {saved}"
    );
    runtime.shutdown().await;

    let reopened = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead reopens");
    assert_eq!(
        reopened
            .snapshot()
            .await
            .expect("the reopened snapshot")
            .model
            .as_deref(),
        Some("test/scripted-a"),
        "the reopened lead still runs the configured default"
    );
    reopened.shutdown().await;
}

/// Plan 7f0b3848: the lead's model is the picker's choice (or the
/// default model) in every workflow state. With Mnemo1's
/// `[models.executing]` configured in the shared config — the shape
/// the user's real config.toml has — a Planning → Executing move must
/// NOT hand the lead's next request to that slot. The protection is
/// the TeamLead policy's stateless route (`WorkflowPolicy::route()`
/// emits `["lead"]` for TeamLead mode, pinned kernel-side by the route
/// test in workflow_policy.rs; `ModelsConfig` has no `lead` slot, so
/// the resolver's walk resolves nothing for the lead). This test pins
/// the end-to-end outcome at the runtime seam: both turns serve on the
/// lead's own scripted provider and no model switch is announced —
/// if per-state routing ever returns for the lead, this goes red.
#[tokio::test]
async fn lead_ignores_the_mnemo1_per_state_models_across_a_state_change() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a", "scripted-b"]);
    enable_per_state_models_in_config(config.path());
    // Non-vacuous guard: prove the fixture's `[models.executing]` slot
    // is live under Mnemo's own parse path — it parses and resolves
    // against the fixture endpoint. If the section ever stopped
    // parsing, the lead-turn assertions below would pass vacuously.
    let loaded =
        mnemo::config::Config::load(config.path()).expect("the fixture config parses");
    let executing = loaded
        .general
        .models
        .executing
        .as_ref()
        .expect("[models.executing] parsed");
    assert!(
        loaded.resolve_model_ref(Some(executing)).is_some(),
        "the fixture's [models.executing] resolves against the fixture endpoint"
    );
    let scripted = std::sync::Arc::new(ScriptedLlm::new(vec![
        text_turn(&["first"]),
        text_turn(&["second"]),
    ]));
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(scripted.clone()),
    )
    .await
    .expect("the lead opens");
    let mut rx = runtime.subscribe();
    runtime
        .send_prompt("first turn".to_string(), Vec::new())
        .await
        .expect("prompt");
    drain_turn(&mut rx).await;
    assert_eq!(
        scripted.calls(),
        1,
        "the Planning turn serves on the lead's own provider"
    );

    // Planning → Executing, the transition the lead's create_plan
    // drives in production.
    let session_id = {
        let live = runtime.live.lock().await;
        live.as_ref().expect("live lead").session.id()
    };
    let engine = runtime.engine_handle().await.expect("engine");
    let agent_loop = engine
        .host()
        .agent_loops
        .lock()
        .await
        .get(&session_id.agent_id())
        .cloned()
        .expect("the lead loop is registered");
    let workflow = agent_loop
        .workflow_handle()
        .as_any_arc()
        .downcast::<std::sync::Arc<tokio::sync::Mutex<mnemo::app::workflow::Workflow>>>()
        .expect("the lead's workflow is the concrete app workflow");
    workflow
        .lock()
        .await
        .create_plan("Lead plan", "goal", "ctx", vec!["step".into()])
        .expect("create_plan");

    // The Executing turn: still the lead's own provider (scripted-a) —
    // `[models.executing]` (scripted-b) must never take it over.
    runtime
        .send_prompt("second turn".to_string(), Vec::new())
        .await
        .expect("prompt");
    drain_turn(&mut rx).await;
    assert_eq!(
        scripted.calls(),
        2,
        "the Executing turn stays on the lead's own model, not [models.executing]"
    );
    let snapshot = runtime.snapshot().await.expect("snapshot");
    assert_ne!(
        snapshot.model_id.as_deref(),
        Some("scripted-b"),
        "the lead never announces [models.executing]'s model"
    );
    runtime.shutdown().await;
}

/// The model inventory names every model by its explicit spec and lists
/// the reasoning efforts it accepts, so the picker is never ambiguous.
#[tokio::test]
async fn models_carry_their_reasoning_efforts_and_default() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a", "scripted-b"]);
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens");
    let model = &runtime.models()[0];
    assert_eq!(model.spec, "test/scripted-a");
    assert_eq!(model.endpoint, "test");
    assert_eq!(
        model.reasoning_efforts,
        ["max", "high", "medium", "low", "minimal", "off"]
    );
    assert_eq!(model.default_effort.as_deref(), Some("max"));
    runtime.shutdown().await;
}

#[tokio::test]
async fn lead_reasoning_effort_is_validated_persisted_and_restored() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a", "scripted-b"]);
    let runtime = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens");
    assert_eq!(
        runtime.snapshot().await.expect("snapshot").reasoning_effort,
        None
    );
    let refused = runtime
        .set_model("test/scripted-b".to_string(), Some("turbo".to_string()))
        .await
        .expect_err("an unsupported effort is refused");
    assert!(refused.contains("choose one of: max, high"), "{refused}");
    let switched = runtime
        .set_model("test/scripted-b".to_string(), Some("low".to_string()))
        .await
        .expect("model + effort switch");
    assert_eq!(switched.model.as_deref(), Some("test/scripted-b"));
    assert_eq!(switched.reasoning_effort.as_deref(), Some("low"));
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(state.path().join(LEAD_STATE_FILE)).expect("lead.json"),
    )
    .expect("lead JSON");
    assert_eq!(saved["reasoning_effort"], "low");
    runtime.shutdown().await;

    let reopened = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead reopens");
    let restored = reopened.snapshot().await.expect("restored snapshot");
    assert_eq!(restored.model.as_deref(), Some("test/scripted-b"));
    assert_eq!(restored.reasoning_effort.as_deref(), Some("low"));
    reopened.shutdown().await;
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
    )
    .multimodal());
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
    // The provider level: the follow-up request itself must carry
    // the steered image URL, not just the injection event.
    let recorded = llm.recorded_messages();
    assert_eq!(recorded.len(), 2, "two provider requests ran");
    assert!(
        recorded[1].iter().any(|message| match &message.content {
            MessageContent::Parts(parts) => parts.iter().any(|part| {
                matches!(
                    part,
                    mnemo::provider::ContentPart::ImageUrl { image_url }
                        if image_url.url == "data:image/png;base64,AAA"
                )
            }),
            _ => false,
        }),
        "the follow-up provider request must contain the steered image URL: {:?}",
        recorded[1]
    );
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_failed_idle_prompt_reopens_the_merge_gate() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (runtime, team) = open_git_team_spawns(
        &root,
        &config,
        &state,
        Arc::new(ScriptedLlm::new(Vec::new())),
        1,
    )
    .await;
    // Install the test's TeamRuntime as the lead handle. The production
    // pump keeps its original handle; this test isolates send_prompt's
    // gate arms instead of reproducing that cross-handle interleave.
    *runtime.team.lock().await = Some(team.clone());
    assert!(
        !team.lead_is_busy(),
        "baseline: nothing has armed the merge gate"
    );

    // The finding's failure path: a session that rejects an idle
    // prompt (a parked session refuses prompts in production; a
    // closed one is the deterministic harness for the same arm —
    // both fail inside `Session::send` before any turn can run).
    // No turn ever started, so live.busy stays false.
    {
        let live = runtime.live.lock().await;
        live.as_ref()
            .expect("the lead session is live")
            .session
            .close()
            .await
            .expect("close the session");
    }
    let err = runtime
        .send_prompt("hello".to_string(), Vec::new())
        .await
        .expect_err("a closed session rejects the idle prompt");
    assert!(
        err.starts_with("could not send the prompt"),
        "{err}"
    );
    // THE regression: `begin_lead_turn` armed the team gate on the
    // way in; the failed prompt leaves no turn running, so no
    // terminal event will ever arrive to clear it — the error arm
    // must re-open the gate or every later merge stays blocked.
    assert!(
        !team.lead_is_busy(),
        "a failed idle prompt must clear the merge gate"
    );
    team.shutdown().await;
    runtime.shutdown().await;
}

#[tokio::test]
async fn a_successful_busy_steer_keeps_the_merge_gate_closed() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let llm = Arc::new(ScriptedLlm::new_gated(
        vec!["scripted-a"],
        vec![
            (Some(gate_rx), text_turn(&["turn one"])),
            (None, text_turn(&["turn two"])),
        ],
    ));
    let (runtime, team) = open_git_team_spawns(&root, &config, &state, llm, 1).await;
    // The production pump keeps its original TeamRuntime handle; this
    // replacement isolates send_prompt's arms and the test's gate writes.
    *runtime.team.lock().await = Some(team.clone());
    runtime
        .send_prompt("first".to_string(), Vec::new())
        .await
        .expect("the first prompt is accepted");
    assert!(
        runtime.snapshot().await.expect("snapshot").busy,
        "the gated turn is busy"
    );
    assert!(
        team.lead_is_busy(),
        "the first prompt armed the merge gate"
    );

    // The pump clears the team gate without the live lock, so when
    // the busy flag is stale (the turn just finished at the kernel,
    // its terminal event still queued in the pump) that clear can
    // interleave with the steer send itself. Simulate the worst
    // case by clearing the gate immediately before the busy
    // submission: send_prompt must return with the gate closed.
    team.set_lead_busy(false);
    assert!(!team.lead_is_busy());
    runtime
        .send_prompt("a steer".to_string(), Vec::new())
        .await
        .expect("the busy submission queues as a steer");
    assert!(
        team.lead_is_busy(),
        "a successful busy steer send ends with the merge gate closed"
    );

    gate_tx.send(()).expect("the first turn is released");
    wait_until_idle(&runtime).await;
    team.shutdown().await;
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
    // Pin the stop order: the active turn's Finished comes first,
    // then the flushed steer's SuggestionInjected, then the
    // follow-up turn's Finished.
    let mut order: Vec<&'static str> = Vec::new();
    loop {
        let event = next_event(&mut rx).await;
        match event.event.event {
            SerializableAgentEvent::SuggestionInjected { text, images } => {
                assert_eq!(text, "a steer");
                assert!(
                    images.is_empty(),
                    "the image-free steer injects without images: {images:?}"
                );
                order.push("injected");
            }
            SerializableAgentEvent::Finished { .. } => {
                order.push("finished");
            }
            _ => {}
        }
        let finished = order.iter().filter(|kind| **kind == "finished").count();
        if finished == 2 && order.contains(&"injected") {
            break;
        }
    }
    assert_eq!(
        order,
        vec!["finished", "injected", "finished"],
        "stop must end the active turn before the queued steer injects"
    );
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
    let pending = runtime
        .snapshot()
        .await
        .expect("snapshot")
        .pending
        .expect("a pending approval");
    assert_eq!(pending.tool_name.as_deref(), Some("shell"));
    match pending.interaction {
        PendingInteraction::Approval { request_id: id } => {
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
    let pending = runtime
        .snapshot()
        .await
        .expect("snapshot")
        .pending
        .expect("a pending question");
    // The snapshot carries the question itself, not just its id, so a
    // refreshed UI can still show what the lead is asking.
    assert_eq!(pending.question.as_deref(), Some("Which branch?"));
    assert_eq!(
        pending
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect::<Vec<_>>(),
        ["main", "wt/mnemo"]
    );
    let wire = serde_json::to_value(&pending).expect("pending JSON");
    assert_eq!(wire["kind"], "question");
    assert_eq!(wire["question"], "Which branch?");
    match pending.interaction {
        PendingInteraction::Question { question_id } => {
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

fn git_status(root: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "-uall"])
        .current_dir(root)
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&output.stdout).to_string()
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: lead.models(),
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: lead.models(),
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

    // Isolate worker accounting from automatic notification turns, which
    // would otherwise consume scripted responses on a variable schedule.
    lead.live
        .lock()
        .await
        .as_ref()
        .expect("live lead")
        .notifications
        .as_ref()
        .expect("notification pump")
        .abort();

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
    team.message(
    &worker.id,
    "second usage turn".into(),
    MessageSender::User,
)
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
    reopened
        .live
        .lock()
        .await
        .as_ref()
        .expect("reopened live lead")
        .notifications
        .as_ref()
        .expect("reopened notification pump")
        .abort();
    assert_eq!(reopened_team.snapshot().await.usage, after_lead);

    // Resuming the stopped worker recovers its saved session snapshot
    // and records only the NEW turn's usage — no double count from the
    // recovered session or the reloaded ledger.
    reopened_team.resume(&worker.id).await.expect("resume worker");
    reopened_team
        .message(&worker.id, "resume usage turn".into(), MessageSender::User)
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: lead.models(),
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
async fn dispatch_has_no_per_turn_cap_and_slots_limit_concurrency() {
    use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions, WorkerStatus};

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    write_config(config.path(), &["scripted-a"]);
    let lead = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(
            (0..5).map(|_| text_turn(&["done"])).collect(),
        ))),
    )
    .await
    .expect("the lead opens");
    let team = TeamRuntime::open(TeamRuntimeOptions {
        engine: lead.engine_handle().await.expect("the shared engine"),
        workspace_root: root.path().to_path_buf(),
        state_dir: state.path().to_path_buf(),
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(5),
        models: lead.models(),
        pricing: Vec::new(),
    })
    .await
    .expect("the team opens");

    // The per-turn delegation cap is gone: five dispatches — more
    // than the old four-per-turn budget — all land in ONE lead turn
    // (no begin_lead_turn between them) while the slots allow.
    let mut workers = Vec::new();
    for index in 0..5 {
        workers.push(
            team.dispatch(format!("task {index}"), None)
                .await
                .expect("dispatch beyond the old four-per-turn cap"),
        );
    }
    // The concurrent slot limit remains the only spawn limit.
    let full = team
        .dispatch("one task too many".to_string(), None)
        .await
        .expect_err("the slot cap refuses dispatch beyond concurrency");
    assert!(full.contains("slots are busy"), "{full}");

    let stopped = team.stop(&workers[0].id).await.expect("stop");
    assert_eq!(stopped.status, WorkerStatus::Stopped);
    let resumed = team.resume(&workers[0].id).await.expect("resume");
    assert_eq!(resumed.status, WorkerStatus::Idle);
    for worker in &workers {
        let _ = team.stop(&worker.id).await;
    }
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
    use crate::mnemo2::team::{
        InboxKind, MessageSender, TeamRuntime, TeamRuntimeOptions, WorkerStatus,
    };

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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
    assert_eq!(routed.inbox.len(), 1);
    assert_eq!(routed.inbox[0].sender, MessageSender::Mnemo);
    assert_eq!(routed.inbox[0].kind, InboxKind::Fix);
    assert!(routed.inbox[0].text.starts_with("Fix review finding F-large:"));
    assert!(routed.inbox[0].at_ms > 0);
    for _ in 0..100 {
        if llm.calls() >= 2
            && team.worker(&worker.id).await.expect("worker").status == WorkerStatus::Completed
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(llm.calls(), 2, "the tracked worker received the fix brief");
    let mut header_in_transcript = false;
    for _ in 0..100 {
        let fixed = team.worker(&worker.id).await.expect("fixed worker");
        header_in_transcript = fixed.transcript.messages.iter().any(|message| {
            message
                .content
                .as_text()
                .starts_with("[From Mnemo2]\nFix review finding F-large:")
        });
        if header_in_transcript {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(header_in_transcript, "the Mnemo2 header reaches the transcript");
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
async fn worker_inbox_persists_senders_before_a_busy_turn_ends() {
    use crate::mnemo2::team::{InboxKind, MessageSender, WorkerStatus};

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let llm = Arc::new(ScriptedLlm::new_gated(
        vec!["scripted-a"],
        vec![
            (Some(gate_rx), text_turn(&["initial"])),
            (None, text_turn(&["after all three steers"])),
        ],
    ));
    let (lead, team) = open_git_team(&root, &config, &state, llm.clone()).await;
    let worker = team
        .dispatch("wait for the sender test".into(), None)
        .await
        .expect("dispatch worker");
    wait_for_calls(&llm, 1).await;

    let built_in = team
        .handle_lead_host_tool_from(
            "team_message",
            serde_json::json!({"worker_id": worker.id, "text": "lead guidance"}),
            MessageSender::Lead,
        )
        .await;
    assert!(built_in.success, "{}", built_in.output);
    team.message(&worker.id, "user guidance".into(), MessageSender::User)
        .await
        .expect("user message");
    team.message(
        &worker.id,
        "external guidance".into(),
        MessageSender::ExternalLead,
    )
    .await
    .expect("external lead message");

    let expected = [
        (
            MessageSender::Lead,
            "lead",
            "lead guidance",
            "[From the built-in lead]",
        ),
        (
            MessageSender::User,
            "user",
            "user guidance",
            "[From the user in mission control]",
        ),
        (
            MessageSender::ExternalLead,
            "external_lead",
            "external guidance",
            "[From the lead (Claude Code)]",
        ),
    ];
    let working = team.worker(&worker.id).await.expect("working worker");
    assert_eq!(working.status, WorkerStatus::Working);
    assert_eq!(working.inbox.len(), expected.len());
    for (index, (sender, _label, text, _header)) in expected.iter().enumerate() {
        assert_eq!(working.inbox[index].sender, *sender);
        assert_eq!(working.inbox[index].kind, InboxKind::Steer);
        assert_eq!(working.inbox[index].text, *text);
        assert!(working.inbox[index].at_ms > 0);
    }

    // The first provider call is still gated, so this is the durable ledger
    // before the worker's turn ended or its transcript snapshot was copied.
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state.path().join("team.json")).expect("team ledger"),
    )
    .expect("ledger JSON");
    let saved = ledger["workers"][0]["inbox"]
        .as_array()
        .expect("persisted inbox");
    assert_eq!(saved.len(), expected.len());
    for (index, (_sender, label, text, _header)) in expected.iter().enumerate() {
        assert_eq!(saved[index]["sender"], *label);
        assert_eq!(saved[index]["kind"], "steer");
        assert_eq!(saved[index]["text"], *text);
    }
    assert_eq!(llm.calls(), 1, "the initial turn remains gated");

    gate_tx.send(()).expect("release the initial turn");
    wait_for_calls(&llm, 2).await;
    // The steered turn end is not the worker's finished turn any more, so
    // wait for the accumulated steers to reach the transcript instead of a
    // status flip.
    for _ in 0..100 {
        let record = team.worker(&worker.id).await.expect("worker transcript");
        let all_delivered = expected.iter().all(|(_sender, _label, text, header)| {
            let delivered = format!("{header}\n{text}");
            record
                .transcript
                .messages
                .iter()
                .any(|message| message.content.as_text() == delivered)
        });
        if all_delivered {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let completed = team.worker(&worker.id).await.expect("completed transcript");
    for (_sender, _label, text, header) in expected {
        let delivered = format!("{header}\n{text}");
        assert!(
            completed
                .transcript
                .messages
                .iter()
                .any(|message| message.content.as_text() == delivered),
            "the worker transcript contains {delivered:?}"
        );
    }

    team.shutdown().await;
    lead.shutdown().await;
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
    team.message(&worker.id, "continue working".into(), MessageSender::User)
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
async fn lead_merges_from_inside_its_own_turn() {
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
    team.review(
        &worker.id,
        "reviewed".into(),
        Vec::new(),
        review_evidence(),
    )
        .await
        .expect("review");
    team.record_check(&worker.id, "workspace tests".into(), "passed".into(), None)
        .await
        .expect("check");

    // The lead calling team_merge is, by definition, mid-turn.
    team.set_lead_busy(true);
    let error = team
        .merge(&worker.id)
        .await
        .expect_err("a UI merge waits for the lead's turn");
    assert!(error.contains("lead must be idle"), "{error}");
    let result = team
        .handle_lead_host_tool("team_merge", serde_json::json!({"worker_id": worker.id}))
        .await;
    assert!(result.success, "{}", result.output);
    assert_eq!(
        team.worker(&worker.id).await.expect("worker").status,
        WorkerStatus::Merged
    );
    team.set_lead_busy(false);

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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
    let worker = team
        .dispatch("implement".into(), None)
        .await
        .expect("dispatch");
    wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    commit_worker_file(&worker, "README.md", "worker\n", "worker edit");
    std::fs::write(root.path().join("README.md"), "lead\n").expect("lead edit");
    run_git(root.path(), &["add", "README.md"]);
    run_git(root.path(), &["commit", "-m", "lead edit"]);
    team.review(
        &worker.id,
        "reviewed".into(),
        Vec::new(),
        review_evidence(),
    )
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
    std::fs::write(root.path().join("lead-note.txt"), "unrelated work\n")
        .expect("unrelated work");
    mark_merge_interrupted_for_test(&state);

    let reopened = TeamRuntime::open(TeamRuntimeOptions {
        engine: lead.engine_handle().await.expect("engine"),
        workspace_root: root.path().to_path_buf(),
        state_dir: state.path().to_path_buf(),
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
        pricing: Vec::new(),
    })
    .await
    .expect("team reopens without changing the lead");
    let recovered = reopened.worker(&worker.id).await.expect("recovered worker");
    assert_eq!(recovered.status, WorkerStatus::Conflict);
    assert!(recovered
        .error
        .as_deref()
        .is_some_and(|error| error.contains("unrelated changes")));
    assert_eq!(
        std::fs::read_to_string(root.path().join("lead-note.txt")).expect("preserved file"),
        "unrelated work\n"
    );
    let merge_head =
        crate::mnemo2::phase3::git(root.path(), &["rev-parse", "-q", "--verify", "MERGE_HEAD"])
            .await
            .expect("merge-head probe");
    assert!(
        merge_head.status.success(),
        "an unsafe automatic abort must not run"
    );
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: {
            let mut models = lead.models();
            models.push(LeadModel {
                endpoint: "broken".into(),
                model: "broken".into(),
                spec: "broken".into(),
                is_default: false,
                reasoning_efforts: Vec::new(),
                default_effort: None,
            });
            models
        },
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
    let failed = team
        .worker(worker_id)
        .await
        .expect("tracked failed dispatch");
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
        &[
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}", failed.branch)
        ],
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
    let base_tip = crate::mnemo2::phase3::tip(&checkout)
        .await
        .expect("base tip");
    mnemo::app::worktrees::rollback_agent_worktree(
        root.path().to_path_buf(),
        "rollback001".into(),
        base_tip.clone(),
    )
    .await
    .expect("untouched fresh checkout rolls back");
    assert!(!checkout.exists());
    assert!(
        crate::mnemo2::phase3::git_value(root.path(), &["rev-parse", &branch])
            .await
            .is_err()
    );

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
    assert!(
        crate::mnemo2::phase3::git_value(root.path(), &["rev-parse", &branch])
            .await
            .is_ok()
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("feature.txt")).expect("worker file"),
        "useful commit\n"
    );

    let (dirty, _) = mnemo::app::worktrees::provision_agent_worktree(
        root.path().to_path_buf(),
        "dirty00002".into(),
        "wt/lead".into(),
    )
    .await
    .expect("dirty-test checkout");
    let dirty_tip = crate::mnemo2::phase3::tip(&dirty)
        .await
        .expect("dirty base");
    std::fs::write(dirty.join("untracked.txt"), "keep worker work\n").expect("dirty work");
    let preserved = mnemo::app::worktrees::rollback_agent_worktree(
        root.path().to_path_buf(),
        "dirty00002".into(),
        dirty_tip,
    )
    .await
    .expect_err("dirty work is never deleted");
    assert!(preserved.contains("uncommitted"), "{preserved}");
    assert_eq!(
        std::fs::read_to_string(dirty.join("untracked.txt")).expect("dirty file"),
        "keep worker work\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("lead-note.txt")).expect("lead file"),
        "keep lead work\n"
    );
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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
    assert!(
        !branch_exists(root.path(), &worker.branch),
        "the merge deletes the worker branch; the merge commit keeps its tip"
    );
    let main_tip = crate::mnemo2::phase3::git_value(root.path(), &["rev-parse", "main"])
        .await
        .expect("main tip");
    run_git(root.path(), &["branch", "-f", "main", &record.commit]);
    let on_main = team
        .revert_merge(&worker.id)
        .await
        .expect_err("a merge that reached main cannot revert");
    assert!(on_main.contains("already reached main"), "{on_main}");
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
    team.message(
    &worker.id,
    "second usage turn".into(),
    MessageSender::User,
)
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

    team.message(
        &worker.id,
        "continue without a budget".into(),
        MessageSender::User,
    )
        .await
        .expect("usage never blocks continuation");
    wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    team.stop_all().await.expect("stop all");
    let stopped = team.snapshot().await;
    assert_eq!(stopped.active, 0);
    assert_eq!(stopped.workers[0].status, WorkerStatus::Stopped);
    // A restart must not race the old runtime's pending ledger flush: shut it
    // down (which persists) before reopening the same state dir.
    team.shutdown().await;

    let reopened = TeamRuntime::open(TeamRuntimeOptions {
        engine: lead.engine_handle().await.expect("engine"),
        workspace_root: root.path().to_path_buf(),
        state_dir: state.path().to_path_buf(),
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
        pricing: Vec::new(),
    })
    .await
    .expect("reopen");
    let restored = reopened.worker(&worker.id).await.expect("restored worker");
    assert_eq!(restored.total_tokens, 275_000);
    assert_eq!(restored.requests, 2);
    assert_eq!(restored.cost_micros, Some(750_000));
    assert!(restored.brief_sent);

    reopened.shutdown().await;
    lead.shutdown().await;
}

/// `tier` resolves to the tier's first available model, is recorded on
/// the worker, and the dispatch schema lists each tier with its prices.
#[tokio::test]
async fn dispatch_by_tier_resolves_first_model_and_lists_tiers() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a", "scripted-b", "scripted-c"]);
    let endpoints = config.path().join("endpoints.toml");
    let mut text = std::fs::read_to_string(&endpoints).expect("endpoints.toml");
    text.push_str(
        "\n[[pricing]]\nmodel = \"scripted-a\"\ninput_per_1m = 15.0\noutput_per_1m = 75.0\ncached_per_1m = 1.5\n\n[tiers]\ndifficult = [\"test/missing\", \"test/scripted-a\"]\nsimple = [\"test/scripted-b\"]\n",
    );
    std::fs::write(&endpoints, text).expect("write tiers");
    let lead = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]))),
    )
    .await
    .expect("lead opens");
    let team = lead.team_handle().await.expect("team");

    // The schema: five ladder ids whose generated descriptions carry
    // editable definitions, models, prices and fold-down destinations.
    let dispatch = team
        .lead_host_tools()
        .into_iter()
        .find(|tool| tool.name == "team_dispatch")
        .expect("team_dispatch");
    let tier = &dispatch.parameters["properties"]["tier"];
    assert_eq!(
        tier["enum"],
        serde_json::json!(["trivial", "routine", "medium", "high", "escalate"])
    );
    let description = tier["description"].as_str().expect("tier description");
    assert!(
        description.contains("high (High) - Subtle bugs")
            && description.contains(": test/scripted-a ($15/$75)"),
        "{description}"
    );
    assert!(description.contains("trivial (Trivial)"));
    assert!(description.contains(": test/scripted-b"));
    assert!(description.contains("routine (Routine)"), "{description}");
    assert!(description.contains("no model; folds down to trivial"), "{description}");
    assert!(!description.contains("test/missing"), "{description}");
    assert!(dispatch.parameters["properties"]["model"]["description"]
        .as_str()
        .is_some_and(|d| d.contains("Prefer `tier`")));
    // With tiers configured the lead picks among the tiers' models only,
    // and a tier is required.
    assert_eq!(
        dispatch.parameters["properties"]["model"]["enum"],
        serde_json::json!(["test/scripted-b", "test/scripted-a"])
    );
    assert!(dispatch.parameters["required"]
        .as_array()
        .is_some_and(|required| required.contains(&serde_json::json!("tier"))));

    let plan = new_plan(&lead, &["**One** - edit src/lib.rs"]);
    // A model outside every tier is refused, and so is a dispatch that
    // names neither tier nor model.
    let outside = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker0003",
                "plan_id": plan,
                "step": 1,
                "brief": "tiered task",
                "model": "test/scripted-c"
            }),
        )
        .await;
    assert!(!outside.success);
    assert!(outside.output.contains("not in any model tier"), "{}", outside.output);
    let untiered = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker0003",
                "plan_id": plan,
                "step": 1,
                "brief": "tiered task"
            }),
        )
        .await;
    assert!(!untiered.success);
    assert!(untiered.output.contains("choose a `tier`"), "{}", untiered.output);
    assert!(team.worker("worker0003").await.is_err());

    // Dispatch by tier: the first AVAILABLE model of the tier.
    let result = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker0001",
                "plan_id": plan,
                "step": 1,
                "brief": "tiered task",
                "tier": "high"
            }),
        )
        .await;
    assert!(result.success, "{}", result.output);
    let worker = team.worker("worker0001").await.expect("worker");
    assert_eq!(worker.model.as_deref(), Some("test/scripted-a"));
    assert_eq!(worker.tier.as_deref(), Some("high"));

    // An empty tier folds down to the nearest tier below with a model.
    let empty = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker1002",
                "plan_id": plan,
                "step": 1,
                "brief": "tiered task",
                "tier": "routine"
            }),
        )
        .await;
    assert!(empty.success, "{}", empty.output);
    assert!(empty
        .output
        .contains("routine has no model; ran on trivial"));
    let folded = team.worker("worker1002").await.expect("folded worker");
    assert_eq!(folded.model.as_deref(), Some("test/scripted-b"));
    assert_eq!(folded.tier.as_deref(), Some("trivial"));

    // The lead moves a worker to another tier with team_resume: the
    // record takes the tier's model and comes back idle. A model outside
    // the tiers is refused there too.
    team.stop("worker0001").await.expect("stop");
    let outside = team
        .handle_lead_host_tool(
            "team_resume",
            serde_json::json!({"worker_id": "worker0001", "model": "test/scripted-c"}),
        )
        .await;
    assert!(!outside.success);
    assert!(outside.output.contains("not in any model tier"), "{}", outside.output);
    let moved = team
        .handle_lead_host_tool(
            "team_resume",
            serde_json::json!({"worker_id": "worker0001", "tier": "trivial"}),
        )
        .await;
    assert!(moved.success, "{}", moved.output);
    let worker = team.worker("worker0001").await.expect("worker");
    assert_eq!(worker.model.as_deref(), Some("test/scripted-b"));
    assert_eq!(worker.tier.as_deref(), Some("trivial"));
    assert_eq!(worker.status, crate::mnemo2::team::WorkerStatus::Idle);
    // The user's own change (Settings-free, any configured model) clears
    // the tier.
    team.stop("worker0001").await.expect("stop");
    let changed = team
        .change_model(
            "worker0001",
            Some("test/scripted-c".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("change model");
    assert_eq!(changed.model.as_deref(), Some("test/scripted-c"));
    assert_eq!(changed.tier, None);

    // A settings save refreshes tiers for new dispatches.
    team.set_catalog(
        lead.models(),
        tiers_dto! {
            average: vec!["test/scripted-b".into()],
            ..Default::default()
        },
        Vec::new(),
    );
    assert_eq!(team.tier_models("routine")[0].spec, "test/scripted-b");
    assert!(team.tier_models("high").is_empty());

    let _ = team.stop("worker0001").await;
    team.shutdown().await;
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
    let plan = new_plan(
        &lead,
        &["**One** - edit src/lib.rs", "**Two** - edit src/main.rs"],
    );
    let invalid = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": worker_id,
                "plan_id": plan,
                "step": 1,
                "brief": "invalid model task",
                "model": "test/missing"
            }),
        )
        .await;
    assert!(!invalid.success);
    assert_eq!(
        invalid
            .data
            .as_ref()
            .and_then(|data| data.get("error_kind"))
            .and_then(|value| value.as_str()),
        Some("domain")
    );
    assert!(team.snapshot().await.workers.is_empty());
    assert!(!root.path().join(".worktrees/agent-worker00").exists());
    assert!(crate::mnemo2::phase3::git_value(
        root.path(),
        &["rev-parse", "--verify", "refs/heads/wt/agent-worker00"],
    )
    .await
    .is_err());

    let args = serde_json::json!({"worker_id": worker_id, "plan_id": plan, "step": 1, "brief": "retry this task"});
    let first = team
        .handle_lead_host_tool("team_dispatch", args.clone())
        .await;
    assert!(first.success, "{}", first.output);
    let receipt: serde_json::Value = serde_json::from_str(&first.output).expect("JSON receipt");
    assert_eq!(
        receipt
            .pointer("/result/worker/id")
            .and_then(|value| value.as_str()),
        Some(worker_id)
    );
    assert!(receipt.pointer("/result/worker/status").is_some());
    assert!(
        receipt.pointer("/result/worker/brief").is_none(),
        "the dispatch answer stays compact: {receipt}"
    );
    assert_eq!(
        receipt
            .pointer("/result/reused")
            .and_then(|value| value.as_bool()),
        Some(false)
    );
    let worker = team.worker(worker_id).await.expect("tracked worker");
    assert!(worker.brief_sent);

    let retried = team.handle_lead_host_tool("team_dispatch", args).await;
    let receipt: serde_json::Value =
        serde_json::from_str(&retried.output).expect("retry receipt");
    assert_eq!(
        receipt
            .pointer("/result/reused")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
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
    assert!(conflict.contains("different brief"), "{conflict}");
    let model_conflict = team
        .dispatch_with_id(
            "retry this task".into(),
            Some("test/scripted-a".into()),
            Some(worker_id.to_string()),
            None,
        )
        .await
        .expect_err("a retry cannot silently change the worker model");
    assert!(
        model_conflict.contains("different brief, model"),
        "{model_conflict}"
    );
    let step_conflict = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({"worker_id": worker_id, "plan_id": plan, "step": 2, "brief": "retry this task"}),
        )
        .await;
    assert!(!step_conflict.success, "{}", step_conflict.output);
    assert!(
        step_conflict.output.contains("plan step"),
        "{}",
        step_conflict.output
    );
    let busy = team
        .dispatch("a genuinely different task".into(), None)
        .await
        .expect_err("an idempotent retry does not free a slot");
    assert!(busy.contains("slots are busy"), "{busy}");

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
    )
    .await;
    let plan = new_plan(&lead, &["**High-risk step** - implement src/lib.rs"]);

    // An unknown risk classification is rejected outright.
    let invalid = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "hiresk0001",
                "plan_id": plan,
                "step": 1,
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

    // High-risk dispatch requires a board plan step: a plan-free
    // high-risk dispatch is refused.
    let plan_free = team
        .dispatch_with_id(
            "high-risk task".into(),
            None,
            Some("hiresk0001".into()),
            Some("high".into()),
        )
        .await
        .expect_err("high-risk work needs a board plan");
    assert!(plan_free.contains("board plan step"), "{plan_free}");

    // The dispatched plan must declare acceptance scenarios: one without
    // them is refused before provisioning.
    let no_section = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "hiresk0001",
                "plan_id": plan,
                "step": 1,
                "brief": "high-risk task",
                "review_risk": "high"
            }),
        )
        .await;
    assert!(!no_section.success);
    assert!(
        no_section.output.contains(&format!("plan '{plan}' does not declare")),
        "{}",
        no_section.output
    );
    assert!(team.worker("hiresk0001").await.is_err());
    assert!(team.snapshot().await.workers.is_empty());

    // …while observable scenarios added through the board unblock it,
    // and the risk is recorded.
    lead.board_handle()
        .expect("the lead's plan board")
        .update(
            &plan,
            mnemo::app::plan_board::PlanEdit {
                add_acceptance: vec![
                    "A high-risk worker cannot merge without structured evidence.".into(),
                ],
                ..Default::default()
            },
        )
        .expect("the scenarios are recorded");
    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "hiresk0001",
                "plan_id": plan,
                "step": 1,
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
    let merge_audit_error = merge_audit.err().unwrap_or_default();
    assert!(
        merge_audit_error.contains("never merged"),
        "the audit merge refusal names the reviewer: {merge_audit_error}"
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
    // The answer stays compact — the review's identity, not its evidence,
    // which the record keeps for `team_status` detail and the merge gate.
    assert_eq!(
        review_receipt
            .pointer("/result/review/summary")
            .and_then(|value| value.as_str()),
        Some("reviewed")
    );
    assert!(
        review_receipt.pointer("/result/review/evidence").is_none(),
        "the review answer carries its evidence: {review_receipt}"
    );
    let reviewed_record = team.worker(&worker.id).await.expect("reviewed record");
    let evidence = reviewed_record
        .review
        .as_ref()
        .and_then(|review| review.evidence.as_ref())
        .expect("the record keeps the review evidence");
    assert!(evidence.unanchored_pass);
    assert_eq!(
        evidence.independent_reviewer_id.as_deref(),
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

/// Regression: high-risk dispatch and audits read the dispatched board
/// plan, not Mnemo1's `.coding/plans/stack.json`, which the board never
/// writes. A plan created through `plan_create` with acceptance
/// scenarios takes a high-risk dispatch and an audit with no stack file,
/// and its scenarios survive a later `plan_update`.
#[tokio::test]
async fn high_risk_dispatch_and_audit_use_the_board_plan_without_stack_json() {
    use mnemo::app::tool::workflow::board::{PlanCreateTool, PlanUpdateTool};
    use mnemo::tool::Tool;

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
        ])),
        2,
    )
    .await;
    let board = lead.board_handle().expect("the lead's plan board");
    let created = PlanCreateTool::new(board.clone())
        .execute(serde_json::json!({
            "title": "Persist workers",
            "goal": "Reload worker records after a restart",
            "context": "verified: the host owns dispatch; src/lib.rs is the anchor file for the change",
            "steps": ["**Persist** - implement src/lib.rs"],
            "acceptance_scenarios": [
                "BOARD-SCENARIO-MARKER a restart reloads every saved worker record."
            ]
        }))
        .await;
    assert!(created.success, "{}", created.output);
    let plan = created.data.expect("plan data")["id"]
        .as_str()
        .expect("plan id")
        .to_string();
    let updated = PlanUpdateTool::new(board.clone())
        .execute(serde_json::json!({
            "plan_id": plan,
            "decision": "keep the team.json format",
            "context_append": "Also: Windows paths."
        }))
        .await;
    assert!(updated.success, "{}", updated.output);
    assert!(
        !root.path().join(".coding/plans/stack.json").exists(),
        "the board never writes Mnemo1's plan stack"
    );

    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "boardrisk01",
                "plan_id": plan,
                "step": 1,
                "brief": "persist worker records",
                "review_risk": "high"
            }),
        )
        .await;
    assert!(dispatched.success, "{}", dispatched.output);
    let worker = team.worker("boardrisk01").await.expect("high-risk worker");
    assert_eq!(worker.review_risk, "high");
    team.stop(&worker.id).await.expect("stop");
    std::fs::write(worker.worktree.join("feature.rs"), "pub fn feature() {}\n")
        .expect("worker source");
    run_git(&worker.worktree, &["add", "feature.rs"]);
    run_git(&worker.worktree, &["commit", "-m", "feature"]);

    let audited = team
        .handle_lead_host_tool(
            "team_audit_dispatch",
            serde_json::json!({"worker_id": worker.id, "reviewer_id": "boardaudit01"}),
        )
        .await;
    assert!(audited.success, "{}", audited.output);
    let audit = team.worker("boardaudit01").await.expect("audit record");
    assert!(audit.brief.contains(&plan), "the audit names the worker's plan");
    assert!(
        audit.brief.contains("BOARD-SCENARIO-MARKER"),
        "the audit brief carries the board plan's acceptance scenarios"
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
    )
    .await;
    let plan = new_audited_plan(
        &lead,
        "Ship the audited change",
        "A fix commit invalidates the prior audit.",
    );

    let (worker, _) = team
        .dispatch_on_plan(
            "high-risk feature".into(),
            None,
            None,
            Some("stalesk01".into()),
            Some("high".into()),
            false,
            &plan,
            1,
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
        open_git_team_spawns(&root, &config, &state, llm.clone(), 2).await;
    let plan = new_audited_plan(
        &lead,
        "Ship PLANID03-GOAL-MARKER.",
        "The audit receives the plan.",
    );

    // One implementation worker with a distinctive intent brief.
    let (worker, _) = team
        .dispatch_on_plan(
            "INTENT-MARKER-77 implement the flux capacitor".into(),
            None,
            None,
            Some("auditsk01".into()),
            None,
            false,
            &plan,
            1,
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
        config_dir: state.path().join("config"),
        classifier: None,
        routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
        slots: Some(1),
        models: lead.models(),
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

/// Regression: lead and workers share one engine bus, and neither pump
/// filtered by session. A worker's `message_parent` reached the lead's
/// handler ("message_parent requires a 'worker_id' string" / "unknown
/// team host tool"), the lead's `team_dispatch` reached a worker's
/// ("unknown worker host tool"), worker events streamed into the lead
/// UI, and every worker folded the lead's usage and errors into its own
/// record.
#[tokio::test]
async fn worker_events_stay_out_of_the_lead_stream_and_host_tools() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    let llm = Arc::new(ScriptedLlm::new(vec![
        tool_turn("message_parent", r#"{"text":"halfway there"}"#),
        text_turn(&["worker done"]),
        text_turn(&["lead acknowledged"]),
    ]));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("lead opens");
    let mut lead_events = lead.subscribe();
    let team = lead.team_handle().await.expect("lead's actual team");
    let worker = team
        .dispatch_with_id(
            "report progress".into(),
            None,
            Some("isolate0001".into()),
            None,
        )
        .await
        .expect("worker dispatches");
    let mut reported = None;
    for _ in 0..150 {
        let record = team.worker(&worker.id).await.expect("worker record");
        let result = record
            .transcript
            .messages
            .iter()
            .find(|message| message.name.as_deref() == Some("message_parent"))
            .cloned();
        if result.is_some() && llm.calls() >= 3 && !lead.snapshot().await.expect("lead").busy {
            reported = result;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let result = reported.expect("the worker's message_parent call resolved");
    assert!(
        !result.tool_is_error,
        "the worker's own handler answers message_parent: {:?}",
        result.content.as_text()
    );
    assert_eq!(
        result.content.as_text(),
        "message sent to the lead. A reply, if any, arrives later as a new message that starts with a [From …] header; do not assume or invent a reply — continue your task or end your turn."
    );
    let mut forwarded = Vec::new();
    while let Ok(event) = lead_events.try_recv() {
        forwarded.push(event.event.event);
    }
    assert!(
        !forwarded.iter().any(|event| matches!(
            event,
            SerializableAgentEvent::HostToolCall { name, .. } if name == "message_parent"
        )),
        "a worker's host-tool call leaked onto the lead channel: {forwarded:?}"
    );
    let record = team.worker(&worker.id).await.expect("worker record");
    assert!(record.error.is_none(), "{record:?}");
    assert!(record.pending.is_none(), "{record:?}");
    assert_ne!(
        record.status,
        crate::mnemo2::team::WorkerStatus::NeedsYou,
        "an auto-answered message_parent never needs the user"
    );

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
        text_turn(&["lead saw the finish"]),
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
            .filter(|message| {
                let text = message.content.as_text();
                text.starts_with(TEAM_NOTICE_PREFIX) && text.contains("need the API key")
            })
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
        .find(|message| {
            let text = message.content.as_text();
            text.starts_with(TEAM_NOTICE_PREFIX) && text.contains("need the API key")
        })
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

/// `team_dispatch` needs a valid open plan and step, checked on the board
/// before any record, branch, or worktree exists.
#[tokio::test]
async fn dispatch_without_a_valid_plan_step_is_refused_before_provisioning() {
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
    let plan = new_plan(&lead, &["**One** - edit src/lib.rs"]);
    let cases = [
        (
            "no plan or step",
            serde_json::json!({"worker_id": "refused001", "brief": "x"}),
        ),
        (
            "no step",
            serde_json::json!({"worker_id": "refused001", "brief": "x", "plan_id": plan}),
        ),
        (
            "step zero",
            serde_json::json!({"worker_id": "refused001", "brief": "x", "plan_id": plan, "step": 0}),
        ),
        (
            "unknown plan",
            serde_json::json!({"worker_id": "refused001", "brief": "x", "plan_id": "deadbeef", "step": 1}),
        ),
        (
            "missing step",
            serde_json::json!({"worker_id": "refused001", "brief": "x", "plan_id": plan, "step": 9}),
        ),
    ];
    for (label, args) in cases {
        let result = team.handle_lead_host_tool("team_dispatch", args).await;
        assert!(!result.success, "{label}: {}", result.output);
    }
    let unknown = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({"worker_id": "refused001", "brief": "x", "plan_id": "deadbeef", "step": 1}),
        )
        .await;
    assert_eq!(
        unknown
            .data
            .as_ref()
            .and_then(|data| data.get("error_kind"))
            .and_then(|v| v.as_str()),
        Some("domain"),
        "a board refusal is a domain error"
    );
    assert!(team.snapshot().await.workers.is_empty());
    assert!(!root.path().join(".worktrees/agent-refused0").exists());
    assert!(crate::mnemo2::phase3::git_value(
        root.path(),
        &["rev-parse", "--verify", "refs/heads/wt/agent-refused0"],
    )
    .await
    .is_err());

    team.shutdown().await;
    lead.shutdown().await;
}

/// A dispatch composes the board brief header ahead of the lead's brief,
/// stamps the worker from the arguments, and re-dispatch stays idempotent
/// on the lead's own brief.
#[tokio::test]
async fn dispatch_composes_the_plan_brief_and_stamps_the_worker() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (lead, team) = open_git_team(
        &root,
        &config,
        &state,
        Arc::new(ScriptedLlm::new(vec![text_turn(&["worker done"])])),
    )
    .await;
    let plan = new_plan(
        &lead,
        &["**One** - edit src/lib.rs", "**Two** - edit src/main.rs"],
    );
    let args = serde_json::json!({
        "worker_id": "stamped001", "plan_id": plan, "step": 2, "brief": "do the second thing"
    });
    let first = team
        .handle_lead_host_tool("team_dispatch", args.clone())
        .await;
    assert!(first.success, "{}", first.output);
    let worker = team.worker("stamped001").await.expect("worker");
    assert_eq!(worker.plan_id.as_deref(), Some(plan.as_str()));
    assert_eq!(
        worker.plan_step,
        Some(1),
        "stamped 0-based from the 1-based step"
    );
    assert_eq!(
        worker.brief, "do the second thing",
        "the record keeps the lead's brief"
    );
    let opening = worker
        .opening_prompt
        .clone()
        .expect("a composed opening prompt");
    assert!(opening.starts_with("# Plan: Board plan"), "{opening}");
    assert!(opening.contains("## Your step (2)"), "{opening}");
    assert!(opening.contains("edit src/main.rs"), "{opening}");
    assert!(
        opening.trim_end().ends_with("do the second thing"),
        "{opening}"
    );
    wait_for_worker_status(
        &team,
        "stamped001",
        crate::mnemo2::team::WorkerStatus::Completed,
    )
    .await;
    let done = team
        .stop("stamped001")
        .await
        .expect("stop snapshots the transcript");
    assert!(
        done.transcript
            .messages
            .iter()
            .any(|message| message.content.as_text() == KICKOFF),
        "the worker's first prompt is the kickoff; the task rides the pinned system head"
    );

    let again = team.handle_lead_host_tool("team_dispatch", args).await;
    let receipt: serde_json::Value = serde_json::from_str(&again.output).expect("receipt");
    assert_eq!(
        receipt.pointer("/result/reused").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(team.snapshot().await.workers.len(), 1);

    team.shutdown().await;
    lead.shutdown().await;
}

/// The Control Room's IPC dispatch is the same plan-checked path: no valid
/// plan or step is refused before provisioning, a valid one stamps the
/// worker, composes the header, and signals the board.
#[tokio::test]
async fn manual_dispatch_requires_a_plan_step_and_stamps_the_worker() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (lead, team) = open_git_team(
        &root,
        &config,
        &state,
        Arc::new(ScriptedLlm::new(vec![text_turn(&["worker done"])])),
    )
    .await;
    let plan = new_plan(
        &lead,
        &["**One** - edit src/lib.rs", "**Two** - edit src/main.rs"],
    );
    for (label, plan_id, step) in [
        ("blank plan", "", 1),
        ("step zero", plan.as_str(), 0),
        ("unknown plan", "deadbeef", 1),
        ("missing step", plan.as_str(), 9),
    ] {
        let refused = team
            .dispatch_on_plan(
                "x".into(),
                None,
                None,
                None,
                None,
                false,
                plan_id,
                step,
            )
            .await;
        assert!(refused.is_err(), "{label}");
    }
    assert!(team.snapshot().await.workers.is_empty());

    let mut signals = team.subscribe_board();
    let (worker, reused) = team
        .dispatch_on_plan(
            "do the second thing".into(),
            None,
            None,
            None,
            None,
            false,
            &plan,
            2,
        )
        .await
        .expect("dispatch");
    assert!(!reused);
    assert_eq!(worker.plan_id.as_deref(), Some(plan.as_str()));
    assert_eq!(worker.plan_step, Some(1));
    assert_eq!(worker.brief, "do the second thing");
    let opening = worker
        .opening_prompt
        .clone()
        .expect("composed opening prompt");
    assert!(opening.contains("## Your step (2)"), "{opening}");
    assert!(
        opening.trim_end().ends_with("do the second thing"),
        "{opening}"
    );
    assert!(signals.try_recv().is_ok(), "board signalled");

    team.shutdown().await;
    lead.shutdown().await;
}

/// Parallel steps that share a file need a recorded contract before the
/// second dispatch; a parallel dispatch appends one Integration check step;
/// team_status filters by plan.
#[tokio::test]
async fn parallel_dispatch_needs_contracts_and_adds_an_integration_step() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (lead, team) = open_git_team_slots(
        &root,
        &config,
        &state,
        Arc::new(ScriptedLlm::new(vec![
            text_turn(&["worker done"]),
            text_turn(&["worker done"]),
        ])),
        Vec::new(),
        2,
    )
    .await;
    let board = lead.board_handle().expect("board");
    let plan = new_plan(
        &lead,
        &[
            "**Logic** - implement src/game.js",
            "**UI** - render src/ui.js using src/game.js",
        ],
    );
    let other = new_plan(&lead, &["**Solo** - edit src/solo.rs"]);
    team.begin_lead_turn();
    team.set_lead_busy(false);
    let (first, _) = team
        .dispatch_on_plan(
            "logic".into(),
            None,
            None,
            Some("logic0001".into()),
            None,
            false,
            &plan,
            1,
        )
        .await
        .expect("first dispatch");
    assert_eq!(
        board.get(&plan).unwrap().steps.len(),
        2,
        "no parallelism yet"
    );

    let refused = team
        .dispatch_on_plan(
            "ui".into(),
            None,
            None,
            Some("ui000001".into()),
            None,
            false,
            &plan,
            2,
        )
        .await
        .expect_err("shared path without a contract");
    assert!(
        refused.contains("`src/game.js`") && refused.contains("step 1"),
        "{refused}"
    );
    assert!(refused.contains("plan_update"), "{refused}");
    assert_eq!(
        team.snapshot().await.workers.len(),
        1,
        "nothing provisioned"
    );

    board
        .update(
            &plan,
            mnemo::app::plan_board::PlanEdit {
                interface: Some("`src/game.js` - exports `Game` as a named export".into()),
                ..Default::default()
            },
        )
        .unwrap();
    team.begin_lead_turn();
    let (second, _) = team
        .dispatch_on_plan(
            "ui".into(),
            None,
            None,
            Some("ui000001".into()),
            None,
            false,
            &plan,
            2,
        )
        .await
        .expect("second dispatch with a contract");
    let steps = board.get(&plan).unwrap().steps;
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[2].header.as_deref(), Some("Integration check"));

    // Re-dispatching the same step is idempotent and adds nothing.
    team.dispatch_on_plan(
        "ui".into(),
        None,
        None,
        Some("ui000001".into()),
        None,
        false,
        &plan,
        2,
    )
        .await
        .expect("re-dispatch");
    assert_eq!(board.get(&plan).unwrap().steps.len(), 3);

    let status = |args: serde_json::Value| {
        let team = &team;
        async move {
            let result = team.handle_lead_host_tool("team_status", args).await;
            assert!(result.success, "{}", result.output);
            result
                .data
                .and_then(|d| d.as_array().map(|a| a.len()))
                .unwrap_or(0)
        }
    };
    assert_eq!(status(serde_json::json!({})).await, 2);
    assert_eq!(status(serde_json::json!({"plan_id": plan})).await, 2);
    assert_eq!(status(serde_json::json!({"plan_id": other})).await, 0);
    assert_eq!(
        status(serde_json::json!({"plan_id": plan, "worker_id": first.id})).await,
        1
    );
    assert_eq!(
        status(serde_json::json!({"plan_id": other, "worker_id": second.id})).await,
        0
    );

    team.shutdown().await;
    lead.shutdown().await;
}

/// A merge ticks the step, closes the plan with a written report, and the
/// lead is told; a revert reopens the step; the snapshot shows the workers
/// per step and a project-relative report path.
#[tokio::test]
async fn merge_ticks_and_closes_the_plan_then_a_revert_reopens_it() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["worker done"])]));
    let (lead, team) = open_git_team(&root, &config, &state, llm).await;
    let board = lead.board_handle().expect("board");
    let plan = new_plan(&lead, &["**One** - edit src/lib.rs"]);
    let mut signals = team.subscribe_board();
    let worker = mergeable_worker(&team, "ticker0001", &plan, 1, "feature.rs").await;

    let open = team.board_snapshot().await.expect("snapshot");
    let view = open.plans.iter().find(|p| p.id == plan).expect("plan");
    assert_eq!(view.steps[0].workers.len(), 1);
    assert_eq!(view.steps[0].workers[0].id, worker.id);
    assert!(view.report_path.is_none());
    let json = serde_json::to_value(&open).expect("snapshot json");
    assert_eq!(
        json.pointer("/plans/0/steps/0/number")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    assert_eq!(
        json.pointer("/plans/0/steps/0/workers/0/id")
            .and_then(|v| v.as_str()),
        Some("ticker0001")
    );

    let merged = team.merge(&worker.id).await.expect("merge");
    let commit = merged.merge.as_ref().expect("merge record").commit.clone();
    let plan_now = board.get(&plan).expect("plan");
    assert!(plan_now.steps[0].done, "the merge ticked the step");
    assert_eq!(plan_now.status.as_str(), "closed");
    let report = plan_now.report_path.clone().expect("closing report");
    assert!(
        std::path::Path::new(&report).is_file(),
        "the report was written"
    );
    let notices = drain_notices(&mut signals);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("step 1 is done"), "{}", notices[0]);
    assert!(
        notices[0].contains(&worker.id) && notices[0].contains(&commit[..8]),
        "{}",
        notices[0]
    );
    assert!(
        notices[0].contains("is closed") && notices[0].contains(".coding/reviews/"),
        "{}",
        notices[0]
    );
    let markdown = board.read_markdown(&plan).expect("plan file");
    assert!(
        markdown.contains("step 1 merged") && markdown.contains("reviewed and fine"),
        "{markdown}"
    );

    let closed = team.board_snapshot().await.expect("snapshot");
    let relative = closed
        .plans
        .iter()
        .find(|p| p.id == plan)
        .and_then(|p| p.report_path.clone())
        .expect("relative report");
    assert!(
        relative.starts_with(".coding/reviews/team-") && relative.ends_with(".md"),
        "{relative}"
    );
    assert!(root.path().join(&relative).is_file(), "{relative}");

    team.revert_merge(&worker.id).await.expect("revert");
    let reopened = board.get(&plan).expect("plan");
    assert!(!reopened.steps[0].done, "the revert unticked the step");
    assert_eq!(reopened.status.as_str(), "open");
    let notices = drain_notices(&mut signals);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("step 1 is reopened"), "{}", notices[0]);

    team.shutdown().await;
    lead.shutdown().await;
}

/// A step with two tasks ticks only after both are closed.
#[tokio::test]
async fn a_step_with_two_tasks_ticks_only_after_both_close() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let llm = Arc::new(ScriptedLlm::new(vec![
        text_turn(&["one done"]),
        text_turn(&["two done"]),
    ]));
    let (lead, team) = open_git_team(&root, &config, &state, llm).await;
    let board = lead.board_handle().expect("board");
    let plan = new_plan(
        &lead,
        &["**One** - edit src/lib.rs", "**Two** - edit src/main.rs"],
    );
    let first = mergeable_worker(&team, "taskaaaa1", &plan, 1, "a.rs").await;
    let second = mergeable_worker(&team, "taskbbbb1", &plan, 1, "b.rs").await;
    let mut signals = team.subscribe_board();

    team.merge(&first.id).await.expect("merge the first");
    assert!(
        !board.get(&plan).unwrap().steps[0].done,
        "the second task is still open"
    );
    assert!(drain_notices(&mut signals).is_empty());
    team.merge(&second.id).await.expect("merge the second");
    let now = board.get(&plan).unwrap();
    assert!(now.steps[0].done, "both closed, so the step ticked");
    assert_eq!(now.status.as_str(), "open", "step 2 is still to do");
    let notices = drain_notices(&mut signals);
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("step 1 is done") && !notices[0].contains("is closed"),
        "{}",
        notices[0]
    );

    // Reverting one of two merged tasks keeps the step done.
    team.revert_merge(&second.id)
        .await
        .expect("revert the second");
    assert!(board.get(&plan).unwrap().steps[0].done);
    assert!(drain_notices(&mut signals).is_empty());

    team.shutdown().await;
    lead.shutdown().await;
}

/// The lead's actual pump delivers the board notice as a TeamManager
/// message, once.
#[tokio::test]
async fn the_board_notice_reaches_the_actual_lead() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    let llm = Arc::new(ScriptedLlm::new(vec![
        text_turn(&["worker done"]),
        text_turn(&["lead ack"]),
        text_turn(&["lead ack"]),
        text_turn(&["lead ack"]),
    ]));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("lead opens");
    let team = lead.team_handle().await.expect("lead's actual team");
    let plan = new_plan(&lead, &["**One** - edit src/lib.rs"]);
    let worker = mergeable_worker(&team, "livenote01", &plan, 1, "feature.rs").await;
    // Let any worker-finished notice turn settle before merging.
    let mut stable = 0;
    let mut last = usize::MAX;
    for _ in 0..200 {
        let busy = lead.snapshot().await.expect("snapshot").busy;
        if !busy && llm.calls() == last {
            stable += 1;
            if stable >= 8 {
                break;
            }
        } else {
            stable = 0;
        }
        last = llm.calls();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    team.set_lead_busy(false);
    team.merge(&worker.id).await.expect("merge");
    let mut seen = 0;
    for _ in 0..150 {
        let snapshot = lead.snapshot().await.expect("snapshot");
        seen = snapshot
            .transcript
            .messages
            .iter()
            .filter(|m| {
                let text = m.content.as_text();
                text.starts_with(TEAM_NOTICE_PREFIX) && text.contains("step 1 is done")
            })
            .count();
        if seen > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(seen, 1, "the lead received exactly one board notice");

    lead.shutdown().await;
}

/// The engine opens the project's own `.coding/` stores (the ones Mnemo1
/// uses) and hands them to the workflow policy factory.
#[tokio::test]
async fn the_lead_engine_shares_the_project_stores() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    let lead = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("lead opens");
    let engine = lead.engine_handle().await.expect("engine");
    assert!(engine.memory_store_handle().is_some(), "memory is enabled");
    assert!(
        engine.knowledge_store_handle().is_some(),
        "knowledge is wired"
    );
    assert!(
        engine.codegraph_handle().is_some(),
        "the code graph is indexed"
    );
    assert_eq!(
        engine.reviews_dir(),
        root.path().join(".coding").join("reviews")
    );
    let coding = root.path().join(".coding");
    assert!(
        coding.join("memory.db").is_file(),
        "the shared memory store"
    );
    assert!(
        coding.join("codegraph.db").is_file(),
        "the shared code graph"
    );
    let ignored = git_status(root.path());
    assert!(
        !ignored.contains("memory.db") && !ignored.contains("codegraph.db"),
        "{ignored}"
    );

    lead.shutdown().await;
}

#[tokio::test]
async fn external_lead_queue_attaches_pushes_drains_and_detaches() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (lead, team) =
        open_git_lead(&root, &config, &state, Arc::new(ScriptedLlm::new(Vec::new()))).await;

    assert!(!team.external_lead_attached());
    assert_eq!(team.external_lead_since_ms(), None);
    assert!(
        !team.push_external_notice("nobody leads".into()),
        "with no external lead nothing is queued, so the caller uses the in-app lead"
    );
    assert!(team.peek_external_notices().is_empty());
    assert!(team.attach_external_lead(), "the first attach is new");
    assert!(!team.attach_external_lead(), "a second attach changes nothing");
    assert!(team.external_lead_attached());
    assert!(team.external_lead_since_ms().is_some());
    assert!(team.peek_external_notices().is_empty(), "the refused notice was not kept");

    assert!(team.push_external_notice("first".into()));
    assert!(team.push_external_notice("second".into()));
    assert_eq!(team.peek_external_notices(), vec!["first", "second"]);
    assert_eq!(team.drain_external_notices(), vec!["first", "second"]);
    assert!(team.drain_external_notices().is_empty(), "drained once");

    // A detach does not drop what the lead never read: it is parked for
    // the in-app lead (see `release_external_lead`), and a push after the
    // detach is refused.
    assert!(team.push_external_notice("unread".into()));
    assert!(team.detach_external_lead());
    assert!(!team.detach_external_lead(), "already detached");
    assert!(!team.external_lead_attached());
    assert!(team.drain_external_notices().is_empty());
    assert!(!team.push_external_notice("after the detach".into()));
    assert_eq!(team.take_undelivered_notices(), vec!["unread"]);
    assert!(team.take_undelivered_notices().is_empty(), "taken once");
    assert!(team.attach_external_lead());
    assert!(team.drain_external_notices().is_empty(), "a new lead starts clean");

    // What nobody took is handed to the next external lead, in order.
    assert!(team.push_external_notice("older".into()));
    assert!(team.detach_external_lead());
    team.restore_undelivered_notices(vec!["oldest".into()]);
    assert!(team.attach_external_lead());
    assert_eq!(team.drain_external_notices(), vec!["oldest", "older"]);

    lead.shutdown().await;
}

#[tokio::test]
async fn requeued_external_notices_go_to_the_front_in_order() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (lead, team) =
        open_git_lead(&root, &config, &state, Arc::new(ScriptedLlm::new(Vec::new()))).await;
    team.attach_external_lead();

    assert!(team.push_external_notice("one".into()));
    assert!(team.push_external_notice("two".into()));
    let drained = team.drain_external_notices();
    assert!(team.push_external_notice("three".into()), "a newer notice arrives meanwhile");
    team.requeue_external_notices(drained);
    assert_eq!(team.peek_external_notices(), vec!["one", "two", "three"]);
    team.requeue_external_notices(Vec::new());
    assert_eq!(team.peek_external_notices().len(), 3);

    // If the lead detached before the put-back, they are not lost.
    let drained = team.drain_external_notices();
    team.detach_external_lead();
    team.requeue_external_notices(drained);
    assert!(team.drain_external_notices().is_empty());
    assert_eq!(team.take_undelivered_notices(), vec!["one", "two", "three"]);

    lead.shutdown().await;
}

#[tokio::test]
async fn the_external_wait_says_how_it_ended() {
    use crate::mnemo2::team::ExternalWaitStatus;

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (lead, team) =
        open_git_lead(&root, &config, &state, Arc::new(ScriptedLlm::new(Vec::new()))).await;

    // Not attached: nothing to wait for.
    let outcome = team.wait_external(Duration::from_secs(5)).await;
    assert_eq!(outcome.status, ExternalWaitStatus::Detached);
    assert!(outcome.notices.is_empty());

    team.attach_external_lead();
    let outcome = team.wait_external(Duration::from_millis(100)).await;
    assert_eq!(outcome.status, ExternalWaitStatus::Timeout);
    assert_eq!(outcome.status.as_str(), "timeout");

    team.push_external_notice("ready".into());
    let outcome = team.wait_external(Duration::from_secs(5)).await;
    assert_eq!(outcome.status.as_str(), "notices");
    assert_eq!(outcome.notices, vec!["ready"]);

    let waiting = {
        let team = team.clone();
        tokio::spawn(async move { team.wait_external(Duration::from_secs(30)).await })
    };
    for _ in 0..100 {
        if team.external_waiter_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    team.detach_external_lead();
    let outcome = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the detach frees the waiter")
        .expect("waiter task");
    assert_eq!(outcome.status.as_str(), "detached");
    assert!(outcome.notices.is_empty());

    lead.shutdown().await;
}

/// Wait until the lead's transcript has a message holding every needle.
async fn lead_transcript_has(lead: &LeadRuntime, needles: &[&str]) -> bool {
    for _ in 0..250 {
        let snapshot = lead.snapshot().await.expect("lead snapshot");
        let seen = snapshot.transcript.messages.iter().any(|message| {
            let text = message.content.as_text();
            needles.iter().all(|needle| text.contains(needle))
        });
        if seen {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn releasing_the_external_lead_gives_its_unread_notices_to_the_in_app_lead() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["lead will review"])]));
    let (lead, team) = open_git_lead(&root, &config, &state, llm.clone()).await;
    team.attach_external_lead();
    assert!(team.push_external_notice(format!("{TEAM_NOTICE_PREFIX} worker_id=aaaa0001\nfirst-unread")));
    assert!(team.push_external_notice(format!("{TEAM_NOTICE_PREFIX} worker_id=bbbb0002\nsecond-unread")));

    assert!(lead.release_external_lead().await, "an external lead was attached");
    assert!(!team.external_lead_attached());
    assert!(
        lead_transcript_has(&lead, &["first-unread", "second-unread"]).await,
        "one prompt carries both"
    );
    assert_eq!(llm.calls(), 1, "a single idle prompt, not one per notice");
    assert!(team.take_undelivered_notices().is_empty());
    assert!(
        !lead.release_external_lead().await,
        "releasing again changes nothing"
    );

    lead.shutdown().await;
}

#[tokio::test]
async fn notices_the_in_app_lead_cannot_take_stay_queued_for_later() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["noted"])]));
    let (lead, team) = open_git_lead(&root, &config, &state, llm.clone()).await;
    team.attach_external_lead();
    assert!(team.push_external_notice(format!("{TEAM_NOTICE_PREFIX} worker_id=cccc0003\nparked-notice")));

    // The in-app session is unavailable for a moment: nowhere to hand the
    // notice.
    let live = lead.live.lock().await.take().expect("live lead");
    let session_id = live.session.id();
    assert!(lead.release_external_lead().await);
    assert_eq!(
        team.peek_external_notices().len(),
        0,
        "not in the (detached) external queue"
    );
    let parked = team.take_undelivered_notices();
    assert_eq!(parked.len(), 1, "kept, not silently lost: {parked:?}");
    assert!(parked[0].contains("parked-notice"));
    team.restore_undelivered_notices(parked);

    // The session comes back; the next in-app notice carries them along.
    *lead.live.lock().await = Some(live);
    let delivered = deliver_lead_notice(
        &lead.engine,
        session_id,
        &team,
        format!("{TEAM_NOTICE_PREFIX} worker_id=dddd0004\nfresh-notice"),
    )
    .await;
    assert!(delivered);
    assert!(
        lead_transcript_has(&lead, &["parked-notice", "fresh-notice"]).await,
        "parked notices go first, in the same message"
    );
    assert!(team.take_undelivered_notices().is_empty());

    lead.shutdown().await;
}

/// Regression: the attach check and the push were two steps, so a detach
/// in between stranded the notice in a queue nobody read.
#[tokio::test]
async fn a_notice_arriving_after_a_detach_reaches_the_in_app_lead() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["on it"])]));
    let (lead, team) = open_git_lead(&root, &config, &state, llm).await;
    team.attach_external_lead();
    team.detach_external_lead();
    let session_id = lead.live.lock().await.as_ref().expect("live").session.id();
    assert!(
        deliver_lead_notice(
            &lead.engine,
            session_id,
            &team,
            format!("{TEAM_NOTICE_PREFIX} worker_id=eeee0005\nlate-notice"),
        )
        .await
    );
    assert!(team.peek_external_notices().is_empty(), "never queued for a lead that left");
    assert!(lead_transcript_has(&lead, &["late-notice"]).await);

    lead.shutdown().await;
}

#[tokio::test]
async fn waiting_for_external_notices_wakes_on_a_push_and_times_out_empty() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (lead, team) =
        open_git_lead(&root, &config, &state, Arc::new(ScriptedLlm::new(Vec::new()))).await;
    team.attach_external_lead();

    // Nothing queued: the wait ends empty after its timeout.
    let started = std::time::Instant::now();
    assert!(team
        .wait_external_notices(Duration::from_millis(150))
        .await
        .is_empty());
    assert!(started.elapsed() >= Duration::from_millis(140));
    assert!(!team.external_waiter_alive(), "a finished wait is not alive");

    // A queued notice returns at once.
    team.push_external_notice("ready".into());
    assert_eq!(
        team.wait_external_notices(Duration::from_secs(30)).await,
        vec!["ready"]
    );

    // A push while blocked wakes the waiter, which is visibly alive.
    let waiting = {
        let team = team.clone();
        tokio::spawn(async move { team.wait_external_notices(Duration::from_secs(30)).await })
    };
    for _ in 0..100 {
        if team.external_waiter_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(team.external_waiter_alive());
    team.push_external_notice("woken".into());
    let woken = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the push wakes the waiter")
        .expect("waiter task");
    assert_eq!(woken, vec!["woken"]);

    // A detach also frees a blocked waiter, with nothing.
    let waiting = {
        let team = team.clone();
        tokio::spawn(async move { team.wait_external_notices(Duration::from_secs(30)).await })
    };
    for _ in 0..100 {
        if team.external_waiter_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    team.detach_external_lead();
    let freed = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the detach frees the waiter")
        .expect("waiter task");
    assert!(freed.is_empty());

    lead.shutdown().await;
}

#[tokio::test]
async fn an_external_lead_idles_out_unless_a_waiter_is_blocked() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a"]);
    let (lead, team) =
        open_git_lead(&root, &config, &state, Arc::new(ScriptedLlm::new(Vec::new()))).await;
    let minutes = |n: u64| n * 60 * 1000;
    let now = crate::mnemo2::run_file::now_ms();
    let idle = crate::mnemo2::team::EXTERNAL_LEAD_IDLE_MS;

    assert!(
        !team.external_idle_expired(now + minutes(60), idle),
        "a detached lead has nothing to idle out"
    );
    team.attach_external_lead();
    assert!(!team.external_idle_expired(now + minutes(9), idle));
    assert!(team.external_idle_expired(now + minutes(11), idle));
    team.touch_external_lead();
    assert!(
        !team.external_idle_expired(crate::mnemo2::run_file::now_ms() + minutes(9), idle),
        "a call restarts the clock"
    );

    // A blocked waiter is presence: it never idles out.
    let waiting = {
        let team = team.clone();
        tokio::spawn(async move { team.wait_external_notices(Duration::from_secs(30)).await })
    };
    for _ in 0..100 {
        if team.external_waiter_alive() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!team.external_idle_expired(now + minutes(120), idle));
    team.detach_external_lead();
    let _ = waiting.await;

    lead.shutdown().await;
}

#[tokio::test]
async fn an_attached_external_lead_gets_worker_notices_instead_of_the_in_app_lead() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    // Worker one finishes; worker two finishes; then the in-app lead
    // answers the one notice it is sent after the external lead detaches.
    let llm = Arc::new(ScriptedLlm::new(vec![
        text_turn(&["worker one done"]),
        text_turn(&["worker two done"]),
        text_turn(&["lead will review"]),
    ]));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("lead opens");
    let team = lead.team_handle().await.expect("lead's actual team");
    team.attach_external_lead();

    let first = team
        .dispatch_with_id("finish quietly".into(), None, Some("firstwrk01".into()), None)
        .await
        .expect("worker dispatches");
    let notices = team.wait_external_notices(Duration::from_secs(30)).await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    // It is the exact text the in-app lead would have been sent.
    assert!(notices[0].starts_with(TEAM_NOTICE_PREFIX), "{}", notices[0]);
    assert!(notices[0].contains("finished its turn"), "{}", notices[0]);
    assert!(
        notices[0].contains(&format!("worker_id={}", first.id)),
        "{}",
        notices[0]
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let snapshot = lead.snapshot().await.expect("lead snapshot");
    assert!(!snapshot.busy);
    assert_eq!(llm.calls(), 1, "only the worker ran; the in-app lead was not prompted");
    assert!(
        !snapshot
            .transcript
            .messages
            .iter()
            .any(|message| message.content.as_text().starts_with(TEAM_NOTICE_PREFIX)),
        "the in-app lead saw no notice"
    );

    // Detached, the next notice goes to the in-app lead again.
    team.detach_external_lead();
    let second = team
        .dispatch_with_id("finish quietly".into(), None, Some("secondwk02".into()), None)
        .await
        .expect("second worker dispatches");
    let mut delivered = false;
    for _ in 0..200 {
        let snapshot = lead.snapshot().await.expect("lead snapshot");
        let seen = snapshot.transcript.messages.iter().any(|message| {
            let text = message.content.as_text();
            text.starts_with(TEAM_NOTICE_PREFIX)
                && text.contains(&format!("worker_id={}", second.id))
        });
        if seen && !snapshot.busy && llm.calls() >= 3 {
            delivered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(delivered, "after the detach the in-app lead gets the notice");
    assert!(team.drain_external_notices().is_empty());

    lead.shutdown().await;
}

/// The failure notice names the worker's actual last tool errors, not just
/// the terminal error, so the lead can see what really went wrong.
#[tokio::test]
async fn the_failure_notice_names_the_tool_errors() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    // Five consecutive failing batches trip the session's doom guard
    // (agent::MAX_RETRIES, raised 3 → 5); keep this count in sync.
    let llm = Arc::new(ScriptedLlm::new(
        (1..=5)
            .map(|attempt| {
                tool_turn("not_a_real_tool", &format!(r#"{{"attempt":{attempt}}}"#))
            })
            .collect(),
    ));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("lead opens");
    let team = lead.team_handle().await.expect("lead's actual team");
    team.attach_external_lead();
    let worker = team
        .dispatch_with_id(
            "fail on a bad tool".into(),
            None,
            Some("failedwrk1".into()),
            None,
        )
        .await
        .expect("worker dispatches");
    let notices = team.wait_external_notices(Duration::from_secs(30)).await;
    let failure = notices
        .iter()
        .find(|notice| notice.contains("failed:"))
        .unwrap_or_else(|| panic!("the failure notice among {notices:?}"));
    assert!(failure.contains(&worker.id), "{failure}");
    assert!(failure.contains("not_a_real_tool"), "{failure}");
    assert!(
        failure.contains("unknown tool 'not_a_real_tool'"),
        "the tool's own error: {failure}"
    );
    assert!(failure.contains("team_message"), "{failure}");
    lead.shutdown().await;
}

/// A steer ends its turn at the next break and drives a follow-up turn, so
/// that turn end is not a finished turn: the lead hears "finished its turn"
/// exactly once, after the follow-up really ends.
#[tokio::test]
async fn a_steered_turn_is_not_a_finished_turn() {
    use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
    use crate::mnemo2::team::{MessageSender, WorkerStatus};

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let (follow_up_tx, follow_up_rx) = tokio::sync::oneshot::channel();
    let llm = Arc::new(ScriptedLlm::new_gated(
        vec!["scripted-a"],
        vec![
            (Some(gate_rx), text_turn(&["first turn done"])),
            (Some(follow_up_rx), text_turn(&["follow-up done"])),
        ],
    ));
    let (lead, _) = open_git_team(&root, &config, &state, llm.clone()).await;
    let team = lead.team_handle().await.expect("lead's actual team");
    team.attach_external_lead();
    let mut events = team.subscribe();
    let worker = team
        .dispatch_with_id("steer me".into(), None, Some("steerwrk01".into()), None)
        .await
        .expect("worker dispatches");
    wait_for_calls(&llm, 1).await;
    // Working means the pump has processed the turn's Started: the steer
    // flag set below cannot be cleared by that first start any more.
    wait_for_worker_status(&team, &worker.id, WorkerStatus::Working).await;

    team.message(
        &worker.id,
        "also check the tests".into(),
        MessageSender::Lead,
    )
    .await
    .expect("the busy worker takes the steer");
    gate_tx.send(()).expect("release the first turn");

    // The steered turn end reads Working on the bus; only the follow-up's
    // Finished reads Completed.
    let mut finished_statuses: Vec<WorkerStatus> = Vec::new();
    let mut follow_up_started = false;
    let mut saw_finished = false;
    let mut follow_up_gate = Some(follow_up_tx);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(received) = tokio::time::timeout(remaining, events.recv()).await else {
            break;
        };
        let Ok(event) = received else { continue };
        if event.worker_id != worker.id {
            continue;
        }
        match &event.event.event {
            SerializableAgentEvent::Started if saw_finished => follow_up_started = true,
            SerializableAgentEvent::Finished { .. } => {
                let status = team.worker(&worker.id).await.expect("worker record").status;
                finished_statuses.push(status);
                if saw_finished {
                    break;
                }
                saw_finished = true;
                // The follow-up turn is gated: release it only once the
                // steered turn end has been observed.
                if let Some(gate) = follow_up_gate.take() {
                    gate.send(()).expect("release the follow-up turn");
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        finished_statuses,
        vec![WorkerStatus::Working, WorkerStatus::Completed],
        "the steered turn end is not Completed"
    );
    assert!(follow_up_started, "the follow-up turn's Started is on the bus");

    let record = team.worker(&worker.id).await.expect("worker record");
    assert_eq!(record.status, WorkerStatus::Completed);
    assert_eq!(record.turns, 1, "the steered turn end never counts as a turn");

    let notices = team.wait_external_notices(Duration::from_secs(30)).await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("finished its turn"), "{}", notices[0]);
    let tag = TurnTag::parse(&notices[0]).expect("the notice carries a turn tag");
    assert_eq!(tag.state, NoticeState::Finished);
    assert_eq!(tag.turns, 1);
    assert_eq!(
        notices[0].lines().next().expect("a first line"),
        format!(
            "{TEAM_NOTICE_PREFIX} worker_id={}{}",
            worker.id,
            tag.suffix()
        ),
        "the tag rides the first line right after the worker id"
    );
    team.shutdown().await;
    lead.shutdown().await;
}

/// A steer whose follow-up turn never starts still settles the worker as
/// Completed within the grace period: it can never hang in Working.
#[tokio::test]
async fn a_lost_follow_up_still_finishes() {
    use crate::mnemo2::team::WorkerStatus;

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let llm = Arc::new(ScriptedLlm::new_gated(
        vec!["scripted-a"],
        vec![(Some(gate_rx), text_turn(&["only turn"]))],
    ));
    let (lead, _) = open_git_team(&root, &config, &state, llm.clone()).await;
    let team = lead.team_handle().await.expect("lead's actual team");
    team.attach_external_lead();
    let worker = team
        .dispatch_with_id("finish alone".into(), None, Some("loststeer1".into()), None)
        .await
        .expect("worker dispatches");
    // The turn is held open; mark the steer wait while the worker runs and
    // then let the turn end with no steer and no follow-up.
    wait_for_worker_status(&team, &worker.id, WorkerStatus::Working).await;
    // The pump clears the flag on the turn's `Started`, which may still be
    // queued: settle first, then set the flag the held turn end will see.
    team.set_steer_pending_for_test(&worker.id, true).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    team.set_steer_pending_for_test(&worker.id, true).await;
    gate_tx.send(()).expect("release the only turn");

    let completed = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    assert_eq!(completed.turns, 1);
    assert_eq!(llm.calls(), 1, "no follow-up turn ever ran");
    let notices = team.wait_external_notices(Duration::from_secs(5)).await;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(notices[0].contains("finished its turn"), "{}", notices[0]);
    team.shutdown().await;
    lead.shutdown().await;
}

/// A finished notice that is queued but not yet delivered is dropped when
/// the worker starts another turn: it no longer describes that worker.
#[tokio::test]
async fn a_message_to_a_completed_worker_retracts_its_queued_notice() {
    use crate::mnemo2::team::{MessageSender, WorkerStatus};

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (gate_tx, gate_rx) = tokio::sync::oneshot::channel();
    let llm = Arc::new(ScriptedLlm::new_gated(
        vec!["scripted-a"],
        vec![
            (None, text_turn(&["first turn done"])),
            (Some(gate_rx), text_turn(&["second turn held"])),
        ],
    ));
    let (lead, _) = open_git_team(&root, &config, &state, llm.clone()).await;
    let team = lead.team_handle().await.expect("lead's actual team");
    team.attach_external_lead();
    let worker = team
        .dispatch_with_id("finish quietly".into(), None, Some("retract001".into()), None)
        .await
        .expect("worker dispatches");
    wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    let mut queued = false;
    for _ in 0..250 {
        if !team.peek_external_notices().is_empty() {
            queued = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(queued, "the finished notice is queued and undrained");

    team.message(&worker.id, "another look".into(), MessageSender::Lead)
        .await
        .expect("a new turn starts");
    wait_for_calls(&llm, 2).await;

    let outcome = team.wait_external(Duration::from_millis(150)).await;
    assert_eq!(
        outcome.status,
        crate::mnemo2::team::ExternalWaitStatus::Timeout,
        "{outcome:?}"
    );
    assert!(outcome.notices.is_empty(), "{:?}", outcome.notices);

    gate_tx.send(()).expect("release the held turn");
    let _ = team.stop(&worker.id).await;
    team.shutdown().await;
    lead.shutdown().await;
}

#[tokio::test]
async fn completed_worker_turn_sends_the_lead_one_review_notice() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    let llm = Arc::new(ScriptedLlm::new(vec![
        text_turn(&["worker done"]),
        text_turn(&["lead will review"]),
    ]));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("lead opens");
    let team = lead.team_handle().await.expect("lead's actual team");
    let worker = team
        .dispatch_with_id(
            "finish quietly".into(),
            None,
            Some("finish0001".into()),
            None,
        )
        .await
        .expect("worker dispatches");
    let mut notice = None;
    for _ in 0..100 {
        let snapshot = lead.snapshot().await.expect("lead snapshot");
        let notices: Vec<String> = snapshot
            .transcript
            .messages
            .iter()
            .map(|message| message.content.as_text())
            .filter(|text| {
                text.starts_with(TEAM_NOTICE_PREFIX) && text.contains("finished its turn")
            })
            .collect();
        if !notices.is_empty() && !snapshot.busy && llm.calls() >= 2 {
            notice = Some(notices);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let notices = notice.expect("the completed turn wakes the lead");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains(&format!("worker_id={}", worker.id)),
        "{}",
        notices[0]
    );
    assert!(notices[0].contains("team_status"), "{}", notices[0]);
    assert!(notices[0].contains("team_review"), "{}", notices[0]);

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
    let llm = Arc::new(ScriptedLlm::new(vec![
        text_turn(&["worker done"]),
        text_turn(&["audit complete"]),
        text_turn(&["lead acknowledged"]),
        text_turn(&["lead acknowledged the audit"]),
        text_turn(&["lead idle"]),
    ]));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm.clone()))
        .await
        .expect("lead opens");
    let team = lead.team_handle().await.expect("lead's actual team");
    let plan = new_audited_plan(&lead, "Audited change.", "The lead adjudicates every audit.");
    let (worker, _) = team
        .dispatch_on_plan(
            "implement the audited change".into(),
            None,
            None,
            Some("notice0002".into()),
            None,
            false,
            &plan,
            1,
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
            .filter(|message| {
                let text = message.content.as_text();
                text.starts_with(TEAM_NOTICE_PREFIX)
                    && text.contains("worker_id=auditnot01")
            })
            .count();
        // The implementer's own finish also wakes the lead, so the
        // audit's notice is looked up by its worker, not counted alone.
        if notices == 1 && !snapshot.busy && llm.calls() >= 4 {
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
        .find(|message| {
            let text = message.content.as_text();
            text.starts_with(TEAM_NOTICE_PREFIX) && text.contains("worker_id=auditnot01")
        })
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
    assert_eq!(
        stopped.brief, "retain the running brief",
        "the record keeps the lead's brief"
    );
    assert!(
        stopped
            .transcript
            .messages
            .iter()
            .any(|message| message.content.as_text() == KICKOFF),
        "the fallback transcript records the kickoff the session was sent"
    );

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
    let worker = team
        .dispatch("retain this work".into(), None)
        .await
        .expect("dispatch");
    let mut worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    for _ in 0..100 {
        worker = team.worker(&worker.id).await.expect("worker");
        if worker.transcript.messages.len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        worker.transcript.messages.len() >= 2,
        "the completed transcript is saved before stopping"
    );
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
    assert!(
        observed
            .iter()
            .any(|event| event.starts_with("CompactStarted")),
        "{observed:?}"
    );
    assert!(
        observed
            .iter()
            .any(|event| event.starts_with("Compacted { before:")),
        "{observed:?}"
    );
    assert!(compacted.transcript.messages.len() >= 2);
    assert!(compacted.used_tokens > 0);

    team.shutdown().await;
    lead.shutdown().await;
}

#[tokio::test]
async fn lead_compact_summarizes_history_and_reports_the_outcome() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    // The summarizer only runs once the conversation is longer than
    // keep_recent (6) + 1 messages, so the lead drives four turns first,
    // and the first prompt is large enough that the summary is a real
    // reduction rather than a rewrite of the same history.
    let big = "keep this lead history around ".repeat(2_000);
    let llm = Arc::new(ScriptedLlm::new(vec![
        text_turn(&["first turn"]),
        text_turn(&["second turn"]),
        text_turn(&["third turn"]),
        text_turn(&["fourth turn"]),
        text_turn(&["compact summary"]),
    ]));
    let lead = LeadRuntime::open_inner(options(&root, &config, &state), Some(llm))
        .await
        .expect("the lead opens");
    let mut events = lead.subscribe();
    for text in [big.as_str(), "second", "third", "fourth"] {
        lead.send_prompt(text.to_string(), Vec::new())
            .await
            .expect("prompt");
        drain_turn(&mut events).await;
    }
    wait_until_idle(&lead).await;
    let before = lead.snapshot().await.expect("snapshot before compaction");
    assert!(
        before.used_tokens > 1_000,
        "the history must be worth compacting: {before:?}"
    );
    lead.compact().await.expect("compaction accepted");
    // The summarization runs between turns: poll the bus for the paired
    // outcome, recording the position of both events to prove the order.
    let mut sequence = 0usize;
    let mut started_at = usize::MAX;
    let mut compacted_at = usize::MAX;
    let mut outcome = None;
    for _ in 0..500 {
        while let Ok(event) = events.try_recv() {
            sequence += 1;
            match &event.event.event {
                SerializableAgentEvent::CompactStarted => started_at = sequence,
                SerializableAgentEvent::Compacted {
                    before: event_before,
                    after: event_after,
                } => {
                    compacted_at = sequence;
                    outcome = Some((*event_before, *event_after));
                }
                _ => {}
            }
        }
        if outcome.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (event_before, event_after) = outcome.expect("compaction reports its outcome");
    assert!(
        started_at < compacted_at,
        "CompactStarted must precede Compacted"
    );
    assert_eq!(
        event_before, before.used_tokens,
        "the event reports the pre-compaction token count"
    );
    assert!(
        event_after < event_before,
        "compaction must reduce the used tokens: {event_before} -> {event_after}"
    );
    let after = lead.snapshot().await.expect("snapshot after compaction");
    assert!(
        after.used_tokens < before.used_tokens,
        "the compacted snapshot re-reads the smaller conversation: {before:?} -> {after:?}"
    );
    lead.shutdown().await;
    // Once the session is gone the same call is rejected, so the IPC
    // surface reports the error instead of compacting nothing.
    let error = lead
        .compact()
        .await
        .expect_err("compaction needs an open session");
    assert_eq!(error, "the lead session is not open");
}

#[test]
fn lead_state_rejects_a_different_project_identity() {
    let first = tempfile::tempdir().expect("first project");
    let second = tempfile::tempdir().expect("second project");
    let state = tempfile::tempdir().expect("state");
    LeadPersistence::new(state.path(), first.path())
        .save(Some("test/model".to_string()), None, Transcript::default())
        .expect("save lead");
    assert!(LeadPersistence::new(state.path(), second.path())
        .load()
        .is_err());
}

#[tokio::test]
async fn slots_restore_from_the_ledger_without_an_explicit_override() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (lead, team) =
        open_git_team(&root, &config, &state, Arc::new(ScriptedLlm::new(vec![]))).await;
    team.set_slots(5).await.expect("set slots");
    assert_eq!(team.snapshot().await.slots, 5);
    team.shutdown().await;
    let restored =
        crate::mnemo2::team::TeamRuntime::open(crate::mnemo2::team::TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            config_dir: state.path().join("config"),
            classifier: None,
            routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
            slots: None,
            models: lead.models(),
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
    let worker = team
        .dispatch("small retained task".to_string(), None)
        .await
        .expect("dispatch");
    commit_worker_file(
        &worker,
        "retained.txt",
        "retained work\n",
        "retained change",
    );
    let worker_tip = crate::mnemo2::phase3::tip(&worker.worktree)
        .await
        .expect("worker tip");
    team.stop(&worker.id).await.expect("stop worker");
    let report = team.cleanup().await.expect("clean checkout");
    assert!(
        !worker.worktree.exists(),
        "cleanup did not remove the checkout: {report:?}"
    );
    let reviewed = team
        .review(
            &worker.id,
            "reviewed retained branch".to_string(),
            Vec::new(),
            review_evidence(),
        )
        .await
        .expect("review retained branch");
    assert_eq!(
        reviewed.status,
        crate::mnemo2::team::WorkerStatus::Cancelled
    );
    assert_eq!(reviewed.review.as_ref().expect("review").tip, worker_tip);
    let checked = team
        .record_check(
            &worker.id,
            "workspace tests".to_string(),
            "passed".to_string(),
            None,
        )
        .await
        .expect("check retained branch");
    assert_eq!(checked.status, crate::mnemo2::team::WorkerStatus::Cancelled);
    assert_eq!(checked.checks[0].tip, worker_tip);
    let lead_tip = crate::mnemo2::phase3::tip(root.path())
        .await
        .expect("lead tip");
    assert!(team.merge(&worker.id).await.is_err());
    assert_eq!(
        crate::mnemo2::phase3::tip(root.path())
            .await
            .expect("unchanged lead"),
        lead_tip
    );
    team.shutdown().await;
    lead.shutdown().await;
}

fn branch_exists(root: &std::path::Path, branch: &str) -> bool {
    std::process::Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ])
        .current_dir(root)
        .output()
        .expect("git runs")
        .status
        .success()
}

#[tokio::test]
async fn deleting_a_worker_stops_it_and_removes_its_record_checkout_and_branch() {
    use crate::mnemo2::team::WorkerStatus;

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]));
    let (lead, team) = open_git_team(&root, &config, &state, llm).await;
    let worker = team
        .dispatch_with_id(
            "say done".into(),
            None,
            Some("deleteme0001".into()),
            None,
        )
        .await
        .expect("dispatch");
    let worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    let ids = vec![worker.id.clone()];
    // A finished worker's session stays live (idle) until stopped;
    // deleting it stops the session first.
    let preview = team.deletion_preview(&ids).await.expect("preview");
    assert!(preview[0].live, "{preview:?}");
    assert!(preview[0].checkout, "{preview:?}");
    assert!(preview[0].blocked.is_none(), "{preview:?}");
    assert!(
        !preview[0].discards_work(),
        "nothing beyond the lead branch: {preview:?}"
    );
    let outcome = team.delete_workers(&ids, false).await.expect("delete");
    assert!(outcome.snapshot.workers.iter().all(|record| record.id != worker.id));
    assert!(team.worker(&worker.id).await.is_err());
    assert!(!worker.worktree.exists(), "the checkout is removed");
    assert!(
        !branch_exists(root.path(), &worker.branch),
        "the branch is deleted"
    );
    let ledger = std::fs::read_to_string(state.path().join("team.json")).expect("ledger");
    assert!(
        !ledger.contains("deleteme0001"),
        "the history is gone from team.json"
    );

    team.shutdown().await;
    lead.shutdown().await;
}

#[tokio::test]
async fn deleting_unmerged_worker_work_requires_confirmation() {
    use crate::mnemo2::team::WorkerStatus;

    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let llm = Arc::new(ScriptedLlm::new(vec![text_turn(&["done"])]));
    let (lead, team) = open_git_team(&root, &config, &state, llm).await;
    let worker = team
        .dispatch_with_id(
            "build it".into(),
            None,
            Some("unmerged0001".into()),
            None,
        )
        .await
        .expect("dispatch");
    let worker = wait_for_worker_status(&team, &worker.id, WorkerStatus::Completed).await;
    team.stop(&worker.id).await.expect("stop");
    commit_worker_file(&worker, "feature.txt", "work\n", "worker work");
    std::fs::write(worker.worktree.join("scratch.txt"), "draft\n").expect("dirty file");

    let ids = vec![worker.id.clone()];
    let preview = team.deletion_preview(&ids).await.expect("preview");
    assert_eq!(preview[0].unmerged_commits, 1, "{preview:?}");
    assert!(preview[0].uncommitted, "{preview:?}");
    // Without confirmation the worker is not deleted — its per-worker
    // result reports why, and the outcome still reports it listed.
    let outcome = team
        .delete_workers(&ids, false)
        .await
        .expect("the batch reports the refused worker instead of failing");
    let refused = outcome.results[0]
        .error
        .as_ref()
        .expect("the unconfirmed worker is refused");
    assert!(refused.contains("confirm discarding"), "{refused}");
    assert!(team.worker(&worker.id).await.is_ok(), "nothing was deleted");
    assert!(worker.worktree.exists());
    assert!(branch_exists(root.path(), &worker.branch));

    let confirmed = team
        .delete_workers(&ids, true)
        .await
        .expect("confirmed delete");
    assert!(
        confirmed.results[0].error.is_none(),
        "{:?}",
        confirmed.results
    );
    assert!(team.worker(&worker.id).await.is_err());
    assert!(!worker.worktree.exists());
    assert!(!branch_exists(root.path(), &worker.branch));

    team.shutdown().await;
    lead.shutdown().await;
}

/// The deletion preview names the merged worker that landed a stopped
/// record's plan step. Git cannot prove that the record's commit and a
/// replay worker's merged commit carry the same work, but the ledger
/// knows another implementation worker on the same plan step was merged.
/// The commit count stays honest — the commit really is missing from the
/// lead branch — and a record on a different step is never superseded.
#[tokio::test]
async fn deletion_preview_names_the_worker_that_landed_the_step() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    // One branch per stopped record, each with one commit the lead
    // branch does not have — a step another worker re-applied by hand.
    let branch_with_commit = |id: &str, file: &str| {
        let branch = mnemo::app::worktrees::agent_branch(id);
        run_git(root.path(), &["checkout", "-b", &branch]);
        std::fs::write(root.path().join(file), "worker work\n").expect("worker file");
        run_git(root.path(), &["add", "--", file]);
        run_git(root.path(), &["commit", "-m", "worker work"]);
        run_git(root.path(), &["checkout", "wt/lead"]);
        branch
    };
    let superseded_branch = branch_with_commit("ledgerab0001", "ledger.txt");
    let other_branch = branch_with_commit("summaryab001", "summary.txt");
    // The merged replay worker's retained branch sits at the lead tip:
    // its step landed through the merge, so nothing of its own is missing.
    let replay_branch = mnemo::app::worktrees::agent_branch("replayab0001");
    run_git(root.path(), &["branch", &replay_branch]);
    let record = |id: &str, name: &str, status: &str, step: usize, branch: &str| {
        serde_json::json!({
            "id": id,
            "name": name,
            "brief": "step task",
            "model": null,
            "model_id": null,
            "branch": branch,
            "worktree": root
                .path()
                .join(".worktrees")
                .join(format!("agent-{}", mnemo::app::worktrees::agent_short_id(id))),
            "status": status,
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 0,
            "checks": [],
            "error": null,
            "kind": "implementation",
            "plan_id": "01762f92",
            "plan_step": step,
        })
    };
    let mut landed = record("replayab0001", "senderReplay36", "merged", 0, &replay_branch);
    landed["merge"] = serde_json::json!({
        "commit": "replaymerge000",
        "worker_tip": "replaytip00000",
        "branch": replay_branch,
        "target": "wt/lead",
        "checks": [],
        "review_summary": "the replay landed the step",
        "merged_at_ms": 1790000000000u64,
    });
    std::fs::write(
        state.path().join("team.json"),
        serde_json::to_vec(&serde_json::json!({
            "workers": [
                landed,
                record("ledgerab0001", "senderLedger34", "stopped", 0, &superseded_branch),
                record("summaryab001", "teamSummary35", "stopped", 1, &other_branch),
            ],
            "slots": 1,
        }))
        .expect("ledger JSON"),
    )
    .expect("ledger");
    let lead = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("lead opens");
    let team = lead.team_handle().await.expect("team");

    let ids = vec![
        "ledgerab0001".to_string(),
        "summaryab001".to_string(),
        "replayab0001".to_string(),
    ];
    let preview = team.deletion_preview(&ids).await.expect("preview");
    // The stopped record names the worker that landed its step...
    assert_eq!(
        preview[0].superseded_by.as_deref(),
        Some("senderReplay36"),
        "{preview:?}"
    );
    // ...while the count stays honest: the commit really is missing.
    assert_eq!(preview[0].unmerged_commits, 1, "{preview:?}");
    assert!(preview[0].blocked.is_none(), "{preview:?}");
    // A different step is never superseded, even with its own missing work.
    assert_eq!(preview[1].superseded_by, None, "{preview:?}");
    assert_eq!(preview[1].unmerged_commits, 1, "{preview:?}");
    // The merged worker itself is not superseded; it landed the step.
    assert_eq!(preview[2].superseded_by, None, "{preview:?}");
    // The frontend reads the name and the null exactly as serialized.
    let value = serde_json::to_value(&preview).expect("serialized preview");
    assert_eq!(value[0]["superseded_by"], serde_json::json!("senderReplay36"));
    assert_eq!(value[1]["superseded_by"], serde_json::Value::Null);

    lead.shutdown().await;
}

/// Legacy ledgers hold records whose ids share an 8-character prefix and
/// therefore one branch and checkout (`wt/agent-00000003`). Deleting one
/// such record must never destroy the other's work.
#[tokio::test]
async fn deleting_a_worker_keeps_a_branch_another_record_still_uses() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_config(config.path(), &["scripted-a"]);
    let (checkout, branch) = mnemo::app::worktrees::provision_agent_worktree(
        root.path().to_path_buf(),
        "sharedab0001".into(),
        "wt/lead".into(),
    )
    .await
    .expect("provision the shared checkout");
    let record = |id: &str| {
        serde_json::json!({
            "id": id,
            "name": format!("Worker {id}"),
            "brief": "legacy task",
            "model": null,
            "model_id": null,
            "branch": branch,
            "worktree": checkout,
            "status": "stopped",
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 0,
            "checks": [],
            "error": null,
        })
    };
    std::fs::write(
        state.path().join("team.json"),
        serde_json::to_vec(&serde_json::json!({
            "workers": [record("sharedab0001"), record("sharedab0002")],
            "slots": 1,
        }))
        .expect("ledger JSON"),
    )
    .expect("legacy ledger");
    let lead = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("lead opens");
    let team = lead.team_handle().await.expect("team");

    let first = vec!["sharedab0001".to_string()];
    let preview = team.deletion_preview(&first).await.expect("preview");
    assert!(preview[0].shared_branch, "{preview:?}");
    assert!(!preview[0].checkout, "{preview:?}");
    team.delete_workers(&first, false)
        .await
        .expect("record-only delete");
    assert!(checkout.exists(), "the other record's checkout is kept");
    assert!(
        branch_exists(root.path(), &branch),
        "the other record's branch is kept"
    );
    assert!(team.worker("sharedab0002").await.is_ok());

    team.delete_workers(&["sharedab0002".to_string()], false)
        .await
        .expect("last owner deletes the shared state");
    assert!(!checkout.exists());
    assert!(!branch_exists(root.path(), &branch));

    lead.shutdown().await;
}

/// The lead picks each worker's model: `team_dispatch` advertises the
/// exact configured specs (a bare model id like 'deepseek-flash' was a
/// recurring spawn failure) and describes workers as full executors.
#[tokio::test]
async fn team_dispatch_offers_the_configured_models_for_full_executors() {
    let root = tempfile::tempdir().expect("workspace");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    write_config(config.path(), &["scripted-a", "scripted-b"]);
    let lead = LeadRuntime::open_inner(
        options(&root, &config, &state),
        Some(Arc::new(ScriptedLlm::new(Vec::new()))),
    )
    .await
    .expect("the lead opens");
    let team = lead.team_handle().await.expect("team");
    let dispatch = team
        .lead_host_tools()
        .into_iter()
        .find(|tool| tool.name == "team_dispatch")
        .expect("team_dispatch");
    assert_eq!(
        dispatch.parameters["properties"]["model"]["enum"],
        serde_json::json!(["test/scripted-a", "test/scripted-b"])
    );
    assert!(dispatch
        .description
        .contains("file_edit, file_write, shell, git"));
    assert!(dispatch.description.contains("never read-only"));
    assert_eq!(
        dispatch.parameters["properties"]["reasoning_effort"]["enum"],
        serde_json::json!(["max", "high", "medium", "low", "minimal", "off"])
    );
    lead.shutdown().await;
}

/// A worker's reasoning effort is explicit, validated against its model,
/// and kept on the record so every later session of the worker uses it.
#[tokio::test]
async fn worker_reasoning_effort_is_validated_and_recorded() {
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
    let without_model = team
        .dispatch_task(
            "task".into(),
            None,
            Some("high".into()),
            Some("effort0001".into()),
        )
        .await
        .expect_err("an effort needs an explicit model");
    assert!(
        without_model.contains("needs an explicit model"),
        "{without_model}"
    );
    let unsupported = team
        .dispatch_task(
            "task".into(),
            Some("test/scripted-a".into()),
            Some("turbo".into()),
            Some("effort0001".into()),
        )
        .await
        .expect_err("an unsupported effort is refused");
    assert!(
        unsupported.contains("choose one of: max, high"),
        "{unsupported}"
    );
    assert!(
        team.snapshot().await.workers.is_empty(),
        "nothing was provisioned"
    );

    let worker = team
        .dispatch_task(
            "task".into(),
            Some("test/scripted-a".into()),
            Some("minimal".into()),
            Some("effort0001".into()),
        )
        .await
        .expect("dispatch with an explicit effort");
    assert_eq!(worker.reasoning_effort.as_deref(), Some("minimal"));
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state.path().join("team.json")).expect("ledger"),
    )
    .expect("the ledger JSON");
    let persisted = ledger["workers"]
        .as_array()
        .and_then(|workers| workers.iter().find(|worker| worker["id"] == "effort0001"))
        .expect("the worker record");
    assert_eq!(
        persisted["reasoning_effort"], "minimal",
        "the ledger persists the effort"
    );
    let changed = team
        .dispatch_task(
            "task".into(),
            Some("test/scripted-a".into()),
            Some("high".into()),
            Some("effort0001".into()),
        )
        .await
        .expect_err("the same worker id cannot silently change effort");
    assert!(changed.contains("reasoning effort"), "{changed}");

    let _ = team.stop(&worker.id).await;
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
        if worker
            .transcript
            .messages
            .iter()
            .any(|message| message.name.as_deref() == Some("git"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        worker = team.worker(&worker.id).await.expect("worker");
    }
    let advertised = llm.advertised_tools();
    let first = advertised.first().expect("worker request");
    for name in ["file_edit", "file_write", "shell", "git", "git_read"] {
        assert!(
            first.iter().any(|tool| tool == name),
            "{name} is not advertised to the worker: {first:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(worker.worktree.join("executor-proof.txt"))
            .expect("worker-written file"),
        "worker wrote this\n"
    );
    assert!(
        worker.transcript.messages.iter().any(|message| {
            message.name.as_deref() == Some("shell")
                && message.content.as_text().contains("worker-shell")
                && !message.tool_is_error
        }),
        "shell did not execute: {:?}",
        worker.transcript
    );
    for name in ["file_write", "git"] {
        assert!(
            worker.transcript.messages.iter().any(|message| {
                message.name.as_deref() == Some(name) && !message.tool_is_error
            }),
            "{name} did not execute successfully: {:?}",
            worker.transcript
        );
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
    let (lead, team) =
        open_git_team(&root, &config, &state, Arc::new(ScriptedLlm::new(vec![]))).await;
    let session = team
        .spawn_worker_session(WorkerSessionSpec {
            model: None,
            reasoning_effort: None,
            worktree: root.path().to_path_buf(),
            resume: None,
            kind: "implementation".to_string(),
            trace_identity: super::trace_tests::worker_identity("Executor test"),
            pinned_task: None,
        })
        .await
        .expect("spawn executor");
    let tools = session.tool_names().await.expect("registered tool names");
    for name in ["file_edit", "file_write", "shell", "git", "git_read"] {
        assert!(
            tools.iter().any(|tool| tool == name),
            "missing coding tool {name}"
        );
    }
    for name in [
        "create_plan",
        "update_plan",
        "finish",
        "spawn_agent",
        "ask_user",
    ] {
        assert!(
            !tools.iter().any(|tool| tool == name),
            "unexpected workflow tool {name}"
        );
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
        .spawn_worker_session(WorkerSessionSpec {
            model: None,
            reasoning_effort: None,
            worktree: root.path().to_path_buf(),
            resume: None,
            kind: "audit".to_string(),
            trace_identity: super::trace_tests::worker_identity("Audit test"),
            pinned_task: None,
        })
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

// ---------------------------------------------------------------------
// change_model's live switch on a working worker (plan 1586c6c7): these
// fixtures run WITHOUT an injected transport — a real config-resolved
// provider against a local SSE stub server — so the tests observe which
// model actually serves each worker request.
// ---------------------------------------------------------------------

/// One scripted SSE response, optionally held behind a gate so the test
/// can switch the model while the worker's request is in flight.
struct StubTurn {
    body: String,
    gate: Option<tokio::sync::oneshot::Receiver<()>>,
}

/// A one-request-per-connection SSE server: the i-th request gets the
/// i-th scripted turn and every request body's `model` field is
/// recorded. The repo has no mock-HTTP crate; like the provider tests
/// this speaks just enough HTTP/1.1.
struct StubServer {
    addr: std::net::SocketAddr,
    bodies: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    arrivals: tokio::sync::mpsc::UnboundedReceiver<()>,
}

impl StubServer {
    /// Start the server. The test must make exactly as many requests as
    /// scripted turns (the accept task ends after the last one).
    async fn start(mut turns: Vec<StubTurn>) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the stub listener");
        let addr = listener.local_addr().expect("the stub address");
        let (arrivals_tx, arrivals_rx) = tokio::sync::mpsc::unbounded_channel();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&bodies);
        tokio::spawn(async move {
            for turn in turns.drain(..) {
                let (mut socket, _) = listener.accept().await.expect("accept a request");
                let mut buffer: Vec<u8> = Vec::new();
                let head_end = loop {
                    let mut chunk = [0u8; 4096];
                    let read = socket.read(&mut chunk).await.expect("read the head");
                    buffer.extend_from_slice(&chunk[..read]);
                    if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                        break pos + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
                let content_length: usize = head
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.trim().parse().ok())
                    })
                    .expect("the request carries content-length");
                let mut body = buffer[head_end..].to_vec();
                while body.len() < content_length {
                    let mut chunk = [0u8; 4096];
                    let read = socket.read(&mut chunk).await.expect("read the body");
                    body.extend_from_slice(&chunk[..read]);
                }
                body.truncate(content_length);
                let body: serde_json::Value =
                    serde_json::from_slice(&body).expect("a JSON request body");
                recorded.lock().expect("the request log").push(body);
                arrivals_tx.send(()).expect("the arrival channel");
                if let Some(gate) = turn.gate {
                    let _ = gate.await;
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                    turn.body.len(),
                    turn.body
                );
                socket
                    .write_all(reply.as_bytes())
                    .await
                    .expect("write the scripted reply");
                let _ = socket.shutdown().await;
            }
        });
        Self {
            addr,
            bodies,
            arrivals: arrivals_rx,
        }
    }

    /// Wait for the next request to arrive (still in flight while its
    /// response is gated).
    async fn next_arrival(&mut self) {
        self.arrivals.recv().await.expect("a request arrives");
    }

    /// The `model` field of every request so far, in order.
    fn models(&self) -> Vec<String> {
        self.bodies
            .lock()
            .expect("the request log")
            .iter()
            .map(|body| {
                body["model"]
                    .as_str()
                    .expect("the request names its model")
                    .to_string()
            })
            .collect()
    }

    /// One recorded request body, cloned out so no lock guard outlives
    /// the call.
    fn body(&self, index: usize) -> serde_json::Value {
        self.bodies
            .lock()
            .expect("the request log")
            .get(index)
            .cloned()
            .expect("a recorded request")
    }

    /// How many requests the stub has recorded.
    fn bodies_len(&self) -> usize {
        self.bodies.lock().expect("the request log").len()
    }
}

/// Concatenate SSE `data:` chunks plus the `[DONE]` terminator.
fn stub_sse(chunks: &[serde_json::Value]) -> String {
    chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .chain(std::iter::once("data: [DONE]\n\n".to_string()))
        .collect()
}

/// An SSE turn answering with `text` and ending the turn.
fn stub_text_turn(text: &str) -> String {
    stub_sse(&[
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","usage":{"prompt_tokens":10,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":0},"prompt_tokens_details":{"cached_tokens":0}},"choices":[]}),
    ])
}

/// An SSE turn calling a tool the worker does not have: the loop
/// records the "unknown tool" result and issues the next request — the
/// second provider call in the SAME turn the live switch pins across.
fn stub_unknown_tool_turn() -> String {
    stub_sse(&[
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"definitely_not_a_tool","arguments":"{}"}}]},"finish_reason":null}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","usage":{"prompt_tokens":10,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":0},"prompt_tokens_details":{"cached_tokens":0}},"choices":[]}),
    ])
}

/// An SSE turn calling the worker's `message_parent` host tool — a
/// SUCCESSFUL tool interaction (the team answers it), so each iteration
/// grows the conversation by a call + a result without tripping the
/// consecutive-tool-error cap that ends a turn after three failures.
fn stub_message_parent_turn(index: usize) -> String {
    let arguments = format!("{{\"text\":\"progress {index}\"}}");
    stub_sse(&[
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":format!("call_{index}"),"type":"function","function":{"name":"message_parent","arguments":arguments}}]},"finish_reason":null}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        serde_json::json!({"id":"1","object":"chat.completion.chunk","model":"stub","usage":{"prompt_tokens":10,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":0},"prompt_tokens_details":{"cached_tokens":0}},"choices":[]}),
    ])
}

/// The two-model fixture config over a stub server: `model-a` (the
/// worker's first model) and `model-b` (the switch target).
fn write_stub_config(dir: &Path, addr: std::net::SocketAddr) {
    std::fs::create_dir_all(dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"model-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        dir.join("endpoints.toml"),
        format!(
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://{addr}/v1/\"\nmodels = [\"model-a\", \"model-b\"]\n"
        ),
    )
    .expect("endpoints.toml");
}

/// [`write_stub_config`] with EXPLICIT per-model windows — the
/// compact-then-switch regression needs a switch target (`model-b`) whose
/// window genuinely cannot hold the worker's conversation, so the deferred
/// summarize-then-swap path runs for real.
fn write_stub_config_windows(
    dir: &Path,
    addr: std::net::SocketAddr,
    model_a_max_context: usize,
    model_b_max_context: usize,
) {
    std::fs::create_dir_all(dir).expect("config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"model-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        dir.join("endpoints.toml"),
        format!(
            "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://{addr}/v1/\"\n\n[[endpoint.models]]\nid = \"model-a\"\nmax_context = {model_a_max_context}\n\n[[endpoint.models]]\nid = \"model-b\"\nmax_context = {model_b_max_context}\n"
        ),
    )
    .expect("endpoints.toml");
}

/// open_git_team's shape WITHOUT the injected transport: the engine
/// resolves providers from the fixture config (kind local pointing at
/// the stub server), so the live model-switch tests observe which model
/// actually serves each worker request.
async fn open_stub_team(
    root: &TempDir,
    config: &TempDir,
    state: &TempDir,
    addr: std::net::SocketAddr,
    slots: usize,
) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_stub_config(config.path(), addr);
    let lead = LeadRuntime::open_inner(options(root, config, state), None)
        .await
        .expect("lead opens");
    let team =
        crate::mnemo2::team::TeamRuntime::open(crate::mnemo2::team::TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            config_dir: state.path().join("config"),
            classifier: None,
            routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
            slots: Some(slots),
            models: lead.models(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
    let team = Arc::new(team);
    team.attach_board(lead.board_handle().expect("the lead's plan board"));
    (lead, team)
}

/// [`open_stub_team`] over [`write_stub_config_windows`]: the worker's
/// first model gets a large window, the switch target a small one.
async fn open_stub_team_with_windows(
    root: &TempDir,
    config: &TempDir,
    state: &TempDir,
    addr: std::net::SocketAddr,
    slots: usize,
    model_a_max_context: usize,
    model_b_max_context: usize,
) -> (LeadRuntime, Arc<crate::mnemo2::team::TeamRuntime>) {
    init_git(root.path());
    run_git(root.path(), &["checkout", "-b", "wt/lead"]);
    write_stub_config_windows(
        config.path(),
        addr,
        model_a_max_context,
        model_b_max_context,
    );
    let lead = LeadRuntime::open_inner(options(root, config, state), None)
        .await
        .expect("lead opens");
    let team =
        crate::mnemo2::team::TeamRuntime::open(crate::mnemo2::team::TeamRuntimeOptions {
            engine: lead.engine_handle().await.expect("engine"),
            workspace_root: root.path().to_path_buf(),
            state_dir: state.path().to_path_buf(),
            config_dir: state.path().join("config"),
            classifier: None,
            routing_log_path: Some(state.path().join("dispatch_routing.jsonl")),
            slots: Some(slots),
            models: lead.models(),
            pricing: Vec::new(),
        })
        .await
        .expect("team opens");
    let team = Arc::new(team);
    team.attach_board(lead.board_handle().expect("the lead's plan board"));
    (lead, team)
}

/// A working worker switches models LIVE: the provider request in
/// flight finishes on the old model, the worker's next request serves
/// on the new one, and no stop/resume happens — the record is updated
/// and persisted, the status stays Working, the session generation does
/// not advance, and the kernel's model_changed re-points the observed
/// model id once the new model serves.
#[tokio::test]
async fn change_model_switches_a_working_worker_live() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (release, gate) = tokio::sync::oneshot::channel();
    let mut stub = StubServer::start(vec![
        StubTurn {
            body: stub_unknown_tool_turn(),
            gate: Some(gate),
        },
        StubTurn {
            body: stub_text_turn("done on the new model"),
            gate: None,
        },
    ])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    let worker = team
        .dispatch_with_id(
            "implement the counting feature".into(),
            Some("test/model-a".into()),
            Some("worker0001".into()),
            None,
        )
        .await
        .expect("dispatch the worker");
    assert_eq!(worker.model, Some("test/model-a".into()));
    assert_eq!(worker.generation, 1);
    let generation_before = worker.generation;

    // Request 1 is in flight on the old model — hold it there.
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the first worker request arrives");
    assert_eq!(stub.models(), ["model-a"]);

    // The switch applies while the worker is working: it returns at once
    // with the record updated and the status STILL Working — no stop,
    // no restart.
    let switched = team
        .change_model(
            "worker0001",
            Some("test/model-b".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("the live switch is applied");
    assert_eq!(switched.model, Some("test/model-b".into()));
    assert_eq!(switched.generation, generation_before);
    assert!(
        matches!(switched.status, crate::mnemo2::team::WorkerStatus::Working),
        "the worker keeps working through the switch: {switched:?}"
    );
    release.send(()).expect("release the held response");

    // The follow-up request serves on the new model, the turn ends, and
    // the kernel's model_changed re-points the observed model id.
    wait_for_worker_model(&team, "worker0001", "model-b").await;
    let settled =
        wait_for_worker_status(&team, "worker0001", crate::mnemo2::team::WorkerStatus::Completed)
            .await;
    assert_eq!(
        settled.generation,
        generation_before,
        "no stop/resume: the session generation did not advance: {settled:?}"
    );
    assert_eq!(
        stub.models(),
        ["model-a", "model-b"],
        "the in-flight request finished on the old model, the next one served on the new one"
    );
    // The record change persisted to the team ledger.
    let ledger = std::fs::read_to_string(state.path().join("team.json")).expect("ledger");
    assert!(
        ledger.contains("test/model-b"),
        "the switch persisted: {ledger}"
    );
    team.shutdown().await;
    lead.shutdown().await;
}

/// Regression (bug 2026-11-02, plan 1238e0de): switching a WORKING worker
/// to a model whose window cannot hold the conversation summarizes on the
/// OLD model and then completes the swap. That completion is INFORMATION,
/// never an error: the worker keeps working, records NO error, and its next
/// request serves on the new model. Before the fix the kernel emitted
/// `Error { retrying: false }` for the completed swap and the pump failed
/// the worker for a switch that had actually succeeded.
#[tokio::test]
async fn change_model_to_a_smaller_context_summarizes_without_failing_the_worker() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    // model-a 200K (trigger 100K) holds the conversation; model-b 32K
    // (trigger 16K) cannot, so the switch defers and the OLD model
    // summarizes first.
    let (release1, gate1) = tokio::sync::oneshot::channel();
    let (release4, gate4) = tokio::sync::oneshot::channel();
    let (release_summary, gate_summary) = tokio::sync::oneshot::channel();
    let (release_last, gate_last) = tokio::sync::oneshot::channel();
    let mut turns = vec![StubTurn {
        body: stub_message_parent_turn(1),
        gate: Some(gate1),
    }];
    // Three more SUCCESSFUL tool turns grow the conversation past
    // keep_recent, so the pre-swap summary actually rewrites it (the
    // oversized brief is summarized away instead of surviving into the new
    // window). A successful call never trips the consecutive-error cap.
    for index in 2..=3 {
        turns.push(StubTurn {
            body: stub_message_parent_turn(index),
            gate: None,
        });
    }
    turns.push(StubTurn {
        body: stub_message_parent_turn(4),
        gate: Some(gate4),
    });
    // The deferred swap's summarization call, served by the OLD model.
    // GATED: when the kernel decides the conversation needs a summary,
    // the test releases it explicitly; when it skips summarization, the
    // FIRST model-b request takes this turn instead — so whichever
    // request reaches it is held either way.
    turns.push(StubTurn {
        body: stub_text_turn("Summary of the earlier work."),
        gate: Some(gate_summary),
    });
    // The post-swap request on the summarized path, held so the record
    // is inspected while the worker is genuinely working on the new
    // model.
    turns.push(StubTurn {
        body: stub_text_turn("done on the new model"),
        gate: Some(gate_last),
    });
    let mut stub = StubServer::start(turns).await;
    let (lead, team) =
        open_stub_team_with_windows(&root, &config, &state, stub.addr, 1, 200_000, 32_000)
            .await;

    // ~120K chars ≈ 30K tokens of brief: over model-b's 16K trigger.
    let brief = format!(
        "implement the counting feature. Context: {}",
        "context detail ".repeat(8_000)
    );
    let worker = team
        .dispatch_with_id(
            brief,
            Some("test/model-a".into()),
            Some("worker0001".into()),
            None,
        )
        .await
        .expect("dispatch the worker");
    assert_eq!(worker.generation, 1);

    // Request 1 arrives on model-a; release it so the tool loop runs on
    // through request 4, which stays in flight (the worker is busy).
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("request 1 arrives");
    release1.send(()).expect("release request 1");
    for request in 2..=4 {
        tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
            .await
            .unwrap_or_else(|_| panic!("request {request} arrives"));
    }
    assert_eq!(
        stub.models(),
        vec!["model-a".to_string(); 4],
        "the tool loop ran on the old model"
    );

    // The switch lands while the worker is working: the conversation
    // exceeds model-b's trigger, so it DEFERS — no restart, still Working.
    let switched = team
        .change_model(
            "worker0001",
            Some("test/model-b".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("the deferred switch is accepted");
    assert_eq!(switched.generation, 1, "no stop/resume");
    assert!(
        matches!(switched.status, crate::mnemo2::team::WorkerStatus::Working),
        "the worker keeps working through the deferred switch: {switched:?}"
    );

    // Release request 4 and observe which path the deferred swap took —
    // the kernel decides either way. Summary sent: the summarization
    // request arrives on the OLD model, the test releases it, and the
    // next request then serves the NEW model, held by the test's last
    // stub turn. Summary skipped: the swap completes silently and the
    // FIRST model-b request takes the gated summary turn instead, so
    // it is held there. Either way the record is inspected while the
    // worker is genuinely working on the new model.
    release4.send(()).expect("release request 4");
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("a request arrives after the switch");
    let summarized = stub.models().last().map(String::as_str) == Some("model-a");
    // Whichever stub turn ends up holding the first model-b request: the
    // last turn when the summary ran first, the summary turn itself
    // when the swap skipped summarization (that request took it).
    let release_model_b = if summarized {
        release_summary
            .send(())
            .expect("release the summarization response");
        tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
            .await
            .expect("the model-b request arrives");
        release_last
    } else {
        release_summary
    };
    assert_eq!(
        stub.models().last().map(String::as_str),
        Some("model-b"),
        "the next request serves on the new model (models={:?})",
        stub.models()
    );

    // The regression: the completed swap is informational. Poll until the
    // pump folded it (the badge the CompactStarted raised is cleared), then
    // assert the record never went Failed and carries no error.
    let mut observed = team.worker("worker0001").await.expect("worker record");
    for _ in 0..250 {
        if !observed.compacting {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        observed = team.worker("worker0001").await.expect("worker record");
    }
    assert!(
        !observed.compacting,
        "the compacting badge ended with the swap: status={:?} error={:?} compacting={} models={:?}",
        observed.status,
        observed.error,
        observed.compacting,
        stub.models()
    );
    assert_eq!(
        observed.error, None,
        "a summarized switch is not an error: status={:?}",
        observed.status
    );
    // The worker is still WORKING the turn on the new model — the
    // informational event never fails it and never idles it (a mid-turn
    // compaction's Compacted must not flip a live turn to Idle either).
    assert!(
        matches!(
            observed.status,
            crate::mnemo2::team::WorkerStatus::Working
        ),
        "the worker keeps working after the summarized switch: status={:?} error={:?} compacting={} models={:?}",
        observed.status,
        observed.error,
        observed.compacting,
        stub.models()
    );

    // Release whichever stub turn holds the model-b request: the last
    // turn on the summarized path, the summary turn the first model-b
    // request took when the swap skipped summarization.
    release_model_b.send(()).expect("release the model-b response");
    let settled = wait_for_worker_status(
        &team,
        "worker0001",
        crate::mnemo2::team::WorkerStatus::Completed,
    )
    .await;
    assert_eq!(
        settled.error, None,
        "the turn completed cleanly: status={:?}",
        settled.status
    );
    assert_eq!(
        settled.generation, 1,
        "the live switch never restarted the session"
    );
    team.shutdown().await;
    lead.shutdown().await;
}

/// Idle and stopped workers keep the restart path: the record is
/// updated the same way, but the session is restarted (the generation
/// ADVANCES) — the live switch must not hijack a non-working worker.
#[tokio::test]
async fn change_model_still_restarts_idle_and_stopped_workers() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let stub = StubServer::start(vec![StubTurn {
        body: stub_text_turn("done"),
        gate: None,
    }])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    let worker = team
        .dispatch_with_id(
            "implement the counting feature".into(),
            Some("test/model-a".into()),
            Some("worker0001".into()),
            None,
        )
        .await
        .expect("dispatch the worker");
    assert_eq!(worker.generation, 1);

    // The turn finishes on its own: the worker is live but not busy.
    let finished =
        wait_for_worker_status(&team, "worker0001", crate::mnemo2::team::WorkerStatus::Completed)
            .await;
    assert_eq!(finished.model_id.as_deref(), Some("model-a"));

    // Today's idle path: restart on the new model — the generation
    // advances (a NEW session), the worker comes back idle.
    let restarted = team
        .change_model(
            "worker0001",
            Some("test/model-b".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("the idle path restarts on the new model");
    assert_eq!(restarted.model, Some("test/model-b".into()));
    assert_eq!(restarted.generation, 2, "the idle path restarts the session");
    assert!(
        matches!(restarted.status, crate::mnemo2::team::WorkerStatus::Idle),
        "the restarted worker idles: {restarted:?}"
    );
    assert_eq!(
        stub.models(),
        ["model-a"],
        "the restarted session idles without a request"
    );

    // Stopped path: stop, then change — resumed idle on the new model,
    // again through a new session.
    team.stop("worker0001").await.expect("stop the worker");
    wait_for_worker_status(&team, "worker0001", crate::mnemo2::team::WorkerStatus::Stopped)
        .await;
    let resumed = team
        .change_model(
            "worker0001",
            Some("test/model-a".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("the stopped path resumes on the new model");
    assert_eq!(resumed.model, Some("test/model-a".into()));
    assert_eq!(resumed.generation, 3, "the stopped path resumes in a new session");
    assert!(
        matches!(resumed.status, crate::mnemo2::team::WorkerStatus::Idle),
        "the resumed worker idles: {resumed:?}"
    );
    team.shutdown().await;
    lead.shutdown().await;
}

/// Plan 2ba2168a step 1: Mnemo2 worker and lead sessions opt out of
/// the pre-prompt routing gate at spawn — Laya routes a worker once at
/// dispatch instead, so no turn is ever classified per prompt and no
/// routing.jsonl rows are written for the team's sessions.
#[tokio::test]
async fn mnemo2_worker_sessions_skip_turn_routing() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let stub = StubServer::start(vec![StubTurn {
        body: stub_text_turn("done"),
        gate: None,
    }])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    team.dispatch_with_id(
        "implement the counting feature".into(),
        Some("test/model-a".into()),
        Some("worker0001".into()),
        None,
    )
    .await
    .expect("dispatch the worker");

    // The dispatch spawns the worker's session on the lead's engine;
    // both loops are registered under the identities stamped at
    // spawn, so the skip is observable on each before the worker's
    // turn even finishes.
    let engine = lead.engine_handle().await.expect("the engine");
    let loops = engine.host().agent_loops.lock().await;
    let worker_loop = loops
        .values()
        .find(|loopy| {
            loopy
                .trace_identity()
                .map(|identity| identity.worker_id.as_deref() == Some("worker0001"))
                .unwrap_or(false)
        })
        .expect("the worker loop is registered");
    assert!(
        worker_loop.skip_turn_routing(),
        "a Mnemo2 worker session skips the per-turn gate"
    );
    let lead_loop = loops
        .values()
        .find(|loopy| {
            loopy
                .trace_identity()
                .map(|identity| {
                    matches!(
                        identity.actor_kind,
                        mnemo::provider::trace::TraceActorKind::Lead
                    )
                })
                .unwrap_or(false)
        })
        .expect("the lead loop is registered");
    assert!(
        lead_loop.skip_turn_routing(),
        "the Mnemo2 lead session skips the per-turn gate"
    );
    drop(loops);

    wait_for_worker_status(
        &team,
        "worker0001",
        crate::mnemo2::team::WorkerStatus::Completed,
    )
    .await;
    team.shutdown().await;
    lead.shutdown().await;
}

/// The live switch must never erase an already-observed model id that
/// already matches the new model: the new model's events can be pumped
/// before change_model's record update (the pin landed mid-turn, or the
/// target was already serving) — an unconditional clear would leave
/// the record without its correct id, with no later event to repair it.
#[tokio::test]
async fn change_model_keeps_the_already_observed_model_id() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (release, gate) = tokio::sync::oneshot::channel();
    let mut stub = StubServer::start(vec![StubTurn {
        body: stub_text_turn("done on the switch target"),
        gate: Some(gate),
    }])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    // The worker runs ON the switch target from the start: its first
    // request is held in flight, so the new model's model_changed event
    // is pumped BEFORE the switch runs.
    let worker = team
        .dispatch_with_id(
            "implement the counting feature".into(),
            Some("test/model-b".into()),
            Some("worker0001".into()),
            None,
        )
        .await
        .expect("dispatch the worker");
    assert_eq!(worker.model, Some("test/model-b".into()));
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the worker's first request arrives");
    wait_for_worker_model(&team, "worker0001", "model-b").await;
    let generation = worker.generation;

    // Switch to the model already serving: the observed id must survive
    // the record update — the returned record still carries it.
    let switched = team
        .change_model(
            "worker0001",
            Some("test/model-b".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("the live switch is applied");
    assert_eq!(
        switched.model_id.as_deref(),
        Some("model-b"),
        "the already-observed id for the new model is kept: {switched:?}"
    );
    assert_eq!(switched.generation, generation);
    release.send(()).expect("release the held response");

    // The turn ends on the target model with the id intact — no further
    // event was needed to repair it.
    let settled =
        wait_for_worker_status(&team, "worker0001", crate::mnemo2::team::WorkerStatus::Completed)
            .await;
    assert_eq!(settled.model_id.as_deref(), Some("model-b"));
    assert_eq!(settled.generation, generation);
    team.shutdown().await;
    lead.shutdown().await;
}

/// An unreported audit switches live while its request is in flight: the
/// in-flight request finishes on the old model, the next request serves on
/// the new one, and the session is not stopped or restarted.
#[tokio::test]
async fn change_model_switches_a_busy_unreported_audit() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let (release, gate) = tokio::sync::oneshot::channel();
    let mut stub = StubServer::start(vec![
        StubTurn {
            body: stub_text_turn("implementation done"),
            gate: None,
        },
        StubTurn {
            body: stub_text_turn("audit done"),
            gate: Some(gate),
        },
    ])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 2).await;
    let plan = new_audited_plan(
        &lead,
        "Ship the counting feature",
        "A step's implementation can be audited while the audit runs.",
    );
    let (worker, _) = team
        .dispatch_on_plan(
            "implement the counting feature".into(),
            Some("test/model-a".into()),
            None,
            Some("worker0001".into()),
            None,
            false,
            &plan,
            1,
        )
        .await
        .expect("dispatch the worker");
    assert_eq!(worker.generation, 1);
    // The implementation turn finishes so the audit can be dispatched;
    // stop the worker and move its tip so there is something to audit.
    wait_for_worker_status(&team, "worker0001", crate::mnemo2::team::WorkerStatus::Completed)
        .await;
    // The implementation turn's request is the first arrival: consume it so
    // the wait after the audit dispatch is genuinely about the audit request.
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the implementation request arrives");
    team.stop("worker0001").await.expect("stop the worker");
    std::fs::write(worker.worktree.join("feature.rs"), "pub fn feature() {}\n")
        .expect("worker source");
    run_git(&worker.worktree, &["add", "feature.rs"]);
    run_git(&worker.worktree, &["commit", "-m", "feature"]);

    // The audit worker runs on model-a with its request held in flight —
    // busy.
    team.audit_dispatch(
        "worker0001",
        "auditor1",
        Some("test/model-a".into()),
        None,
        None,
    )
    .await
    .expect("dispatch the audit worker");
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the audit worker's first request arrives");
    // The pump has folded TurnStarted by now, so the switch takes the live
    // busy path instead of the stop-and-restart one.
    wait_for_worker_status(&team, "auditor1", crate::mnemo2::team::WorkerStatus::Working).await;
    let audit = team.worker("auditor1").await.expect("the audit record");
    assert_eq!(audit.kind, "audit");
    let generation = audit.generation;

    // An unreported audit switches live: its in-flight request finishes
    // on the old model, its next request uses the new one, and the session
    // is not stopped or restarted.
    let switched = team
        .change_model(
            "auditor1",
            Some("test/model-b".into()),
            None,
            crate::mnemo2::team::ChangeSource::User,
        )
        .await
        .expect("switch the unreported audit");
    assert_eq!(switched.model, Some("test/model-b".into()));
    assert_eq!(switched.generation, generation);
    assert!(!switched.report_delivered);
    release.send(()).expect("release the held response");

    let settled =
        wait_for_worker_status(&team, "auditor1", crate::mnemo2::team::WorkerStatus::Completed)
            .await;
    assert_eq!(settled.model, Some("test/model-b".into()));
    assert_eq!(settled.generation, generation);
    // The switch only affects the NEXT request: the request already in
    // flight finished on the old model and this script's audit turn ends
    // there (the next-request coverage lives in
    // `change_model_switches_a_working_worker_live`).
    assert_eq!(stub.models(), ["model-a", "model-a"]);
    team.shutdown().await;
    lead.shutdown().await;
}

/// Plan 40eef714 step 2: dispatch pins the whole task — the plan header,
/// the step's spec and the lead's brief — into the stable system head,
/// and the first user message is the short kickoff instead of the task.
#[tokio::test]
async fn dispatch_pins_the_task_and_sends_the_kickoff() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let mut stub = StubServer::start(vec![StubTurn {
        body: stub_text_turn("done"),
        gate: None,
    }])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    let plan = new_plan(&lead, &["**Change** - implement src/lib.rs"]);
    team.begin_lead_turn();
    team.set_lead_busy(false);
    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker0001",
                "plan_id": plan,
                "step": 1,
                "brief": "BRIEF-91x implement the counting feature",
            }),
        )
        .await;
    assert!(dispatched.success, "{}", dispatched.output);

    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the worker's first request arrives");
    let body = stub.body(0);
    let messages = body["messages"]
        .as_array()
        .expect("the request messages");
    assert_eq!(messages[0]["role"], "system", "{messages:?}");
    let system = messages[0]["content"].as_str().expect("the system text");
    assert!(
        system.contains("# Plan: Board plan"),
        "the plan header is pinned: {system}"
    );
    assert!(
        system.contains("BRIEF-91x"),
        "the lead's brief is pinned: {system}"
    );
    assert!(
        system.contains("## Your step (1)") && system.contains("**Change** - implement src/lib.rs"),
        "the step's full spec is pinned: {system}"
    );
    assert!(
        system.contains("## Your task (pinned)"),
        "the pinned heading renders: {system}"
    );
    let first_user = messages
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .expect("the first user message");
    assert_eq!(first_user, KICKOFF);
    team.shutdown().await;
    lead.shutdown().await;
}

/// A read-back dispatch sends the read-back kickoff and remembers the
/// choice on the durable record.
#[tokio::test]
async fn read_back_dispatch_sends_the_read_back_kickoff() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let mut stub = StubServer::start(vec![StubTurn {
        body: stub_text_turn("No questions."),
        gate: None,
    }])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    let plan = new_plan(&lead, &["**Change** - implement src/lib.rs"]);
    team.begin_lead_turn();
    team.set_lead_busy(false);
    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker0001",
                "plan_id": plan,
                "step": 1,
                "brief": "BRIEF-91x implement the counting feature",
                "read_back": true,
            }),
        )
        .await;
    assert!(dispatched.success, "{}", dispatched.output);

    let worker = team.worker("worker0001").await.expect("the worker record");
    assert!(
        worker.read_back,
        "the record remembers the read-back first turn: {worker:?}"
    );

    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the worker's first request arrives");
    let body = stub.body(0);
    let first_user = body["messages"]
        .as_array()
        .expect("the request messages")
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| message["content"].as_str())
        .expect("the first user message");
    assert_eq!(first_user, READ_BACK_KICKOFF);

    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state.path().join("team.json")).expect("the ledger"),
    )
    .expect("the ledger JSON");
    let persisted = ledger["workers"]
        .as_array()
        .and_then(|workers| workers.iter().find(|worker| worker["id"] == "worker0001"))
        .expect("the worker record");
    assert_eq!(
        persisted["read_back"], true,
        "the read-back flag persisted: {ledger}"
    );
    team.shutdown().await;
    lead.shutdown().await;
}

/// A worker stopped and resumed on its transcript keeps the task pinned in
/// its new session's system head.
#[tokio::test]
async fn resumed_worker_keeps_its_pinned_task() {
    let root = tempfile::tempdir().expect("root");
    let config = tempfile::tempdir().expect("config");
    let state = tempfile::tempdir().expect("state");
    let mut stub = StubServer::start(vec![
        StubTurn {
            body: stub_text_turn("done"),
            gate: None,
        },
        StubTurn {
            body: stub_text_turn("resumed"),
            gate: None,
        },
    ])
    .await;
    let (lead, team) = open_stub_team(&root, &config, &state, stub.addr, 1).await;
    let plan = new_plan(&lead, &["**Change** - implement src/lib.rs"]);
    team.begin_lead_turn();
    team.set_lead_busy(false);
    let dispatched = team
        .handle_lead_host_tool(
            "team_dispatch",
            serde_json::json!({
                "worker_id": "worker0001",
                "plan_id": plan,
                "step": 1,
                "brief": "BRIEF-91x implement the counting feature",
            }),
        )
        .await;
    assert!(dispatched.success, "{}", dispatched.output);
    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the worker's first request arrives");
    wait_for_worker_status(&team, "worker0001", crate::mnemo2::team::WorkerStatus::Completed)
        .await;

    team.stop("worker0001").await.expect("stop the worker");
    team.resume("worker0001")
        .await
        .expect("resume the worker on its transcript");
    team.message(
        "worker0001",
        "carry on".to_string(),
        crate::mnemo2::team::MessageSender::Lead,
    )
    .await
    .expect("message the resumed worker");

    tokio::time::timeout(Duration::from_secs(30), stub.next_arrival())
        .await
        .expect("the resumed session's first request arrives");
    assert_eq!(stub.bodies_len(), 2);
    let body = stub.body(1);
    let system = body["messages"][0]["content"]
        .as_str()
        .expect("the resumed system text");
    assert!(
        system.contains("BRIEF-91x"),
        "the resumed session keeps its pinned task: {system}"
    );
    assert!(
        system.contains("## Your task (pinned)"),
        "the resumed session keeps the pinned heading: {system}"
    );
    team.shutdown().await;
    lead.shutdown().await;
}
