// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Two-process end-to-end test of a remote Mnemo worker (plan 9a2f4b67,
//! phase 3 step 8): the hub is a real `TeamRuntime` in the test process — its
//! engine, its memory store, a real `HubServer` behind `HostDirectory` — and
//! the worker node is a real `mnemo2-node` child process paired through its
//! command line.
//!
//! The test walks the whole lead workflow over the link: dispatch places the
//! Mnemo worker on the node, the worker edits, writes a typed knowledge file
//! and commits in its node checkout, `team_run_checks` runs the project's
//! checks on the node, `team_review` reviews the shadow checkout and
//! `team_merge` lands the branch on the lead branch — from the lead's side
//! exactly like a local worker.
//!
//! Every directory comes from `tempfile::tempdir()`, which lives under
//! `target/` through the repository's `.cargo/config.toml` `TMP` setting, so no
//! test leaves anything in the system temp folder.

use std::collections::VecDeque;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use mnemo::agent::policy::PermissionPolicy;
use mnemo::app::plan_board::{PlanBoard, PlanDraft};
use mnemo::config::{Config, ModelRef};
use mnemo::headless::{Engine, EngineOptions, MemoryBackend, SpawnOptions};
use mnemo::memory::plan_format::PlanKind;
use mnemo::memory::indexer::MACHINE_SOURCE;
use mnemo::memory::{Memory, MemoryClass, MemoryFilter, MemoryStoreTrait, MemoryTier};
use mnemo::provider::trace::LlmRequestContext;
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, Role, ToolChoice,
    ToolSchema,
};
use mnemo::remote::catalog::ModelCatalog;
use mnemo::remote::hub::{HubOptions, HubServer, HubServices, NodeHandle, RemoteSpawn};
use mnemo::remote::pairing::{NodeIdentity, NodeRecord};
use mnemo::remote::CODE_BAD_PARAMS;
use mnemo::workspace::Workspace;
use mnemo_app::mnemo2::dispatch_routing::{
    save_routing_settings, RoutingMode, RoutingSettings,
};
use mnemo_app::mnemo2::runtime::LeadModel;
use mnemo_app::mnemo2::team::{
    HostDirectory, PairOffer, TeamRuntime, TeamRuntimeOptions, WorkerStatus,
};

/// The hub's API key: it must never reach the node.
const SENTINEL: &str = "sk-SENTINEL-e2e";

/// A hub-store row marked as machine knowledge: a node session must never see it.
const MACHINE_TITLE: &str = "remote e2e machine only";

/// The worker the test dispatches.
const WORKER_ID: &str = "remotee2e0001";

/// The overall budget of one e2e test.
const TEST_BUDGET: Duration = Duration::from_secs(240);

/// Run `body` under the overall budget of one e2e test.
async fn within<F: Future>(body: F) -> F::Output {
    tokio::time::timeout(TEST_BUDGET, body)
        .await
        .expect("the e2e test exceeded its 240 s budget")
}

// ── the hub's scripted model ───────────────────────────────────────────────

/// What one model request answers.
enum Script {
    /// These stream items, in order.
    Items(Vec<LlmEvent>),
}

/// The hub's model endpoint: pops one script per request and records the model
/// id each request arrived with.
struct ScriptedModel {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<String>>,
    /// The message list of every request, in order: what the node's worker
    /// sent the hub, tool results included.
    conversations: Mutex<Vec<Vec<Message>>>,
}

impl ScriptedModel {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
            conversations: Mutex::new(Vec::new()),
        })
    }

    /// The `model()` of every request the hub served, in order.
    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

impl ScriptedModel {
    /// The messages of the last request the hub served: the worker's whole
    /// conversation up to its final turn.
    fn last_conversation(&self) -> Vec<Message> {
        self.conversations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

/// The client `HubServices::model_client` hands out.
struct ScriptedClient {
    shared: Arc<ScriptedModel>,
    model: String,
    caps: Capabilities,
}

#[async_trait]
impl LlmClient for ScriptedClient {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::OpenAI
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn provider_name(&self) -> &str {
        "scripted"
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
        self.shared
            .conversations
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(messages.to_vec());
        self.shared
            .requests
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(self.model.clone());
        let script = self
            .shared
            .scripts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop_front()
            .ok_or_else(|| mnemo::error::Error::Provider("no script left".into()))?;
        let Script::Items(items) = script;
        Ok(Box::pin(futures::stream::iter(items)))
    }

    async fn complete_with_context(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
        _context: &LlmRequestContext,
    ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
        self.complete(messages, tools, tool_choice).await
    }
}

/// One tool-call request: the model calls `name` with `args`.
fn tool_call(id: &str, name: &str, args: Value) -> Script {
    Script::Items(vec![
        LlmEvent::ToolCallStart {
            index: 0,
            id: id.into(),
            name: name.into(),
        },
        LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: args.to_string(),
        },
        LlmEvent::Finish {
            reason: FinishReason::ToolCalls,
        },
    ])
}

/// The closing request: the answer text, a stop and the usage.
fn final_text(text: &str) -> Script {
    Script::Items(vec![
        LlmEvent::TextDelta { text: text.into() },
        LlmEvent::Finish {
            reason: FinishReason::Stop,
        },
        LlmEvent::Usage {
            prompt_tokens: 10,
            completion_tokens: 2,
            reasoning_tokens: 0,
            cached_tokens: 0,
            ttft_ms: None,
            generation_ms: None,
        },
    ])
}

/// The scripted worker turn, in order: write a file, write a typed HOW record,
/// commit, look for the hub-only machine row, report to the lead, finish.
fn worker_script() -> Vec<Script> {
    vec![
        tool_call(
            "call-1",
            "file_write",
            json!({"path": "remote.txt", "content": "written on the node\n"}),
        ),
        tool_call(
            "call-2",
            "memory_write",
            json!({
                "tier": "procedural",
                "title": "HOW: remote e2e how",
                "content": "Run the remote worker e2e from the checkout root.",
            }),
        ),
        tool_call(
            "call-3",
            "shell",
            json!({"command": "git add -A", "purpose": "stage the remote work"}),
        ),
        tool_call(
            "call-4",
            "shell",
            json!({"command": "git commit -m remote-e2e", "purpose": "commit the remote work"}),
        ),
        tool_call(
            "call-5",
            "memory_search",
            json!({"query": MACHINE_TITLE}),
        ),
        tool_call(
            "call-6",
            "message_parent",
            json!({"text": "committed remote-e2e on the node"}),
        ),
        final_text("done"),
    ]
}

// ── the hub under test ─────────────────────────────────────────────────────
/// The hub's services: the scripted model for every model request, the team
/// engine's own memory store (so a worker's routed writes land where the test
/// looks), and a catalog mirroring the hub's config.
struct TestServices {
    model: Arc<ScriptedModel>,
    memory: Option<Arc<dyn MemoryStoreTrait>>,
    catalog: ModelCatalog,
}

impl HubServices for TestServices {
    fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
        Some(Arc::new(ScriptedClient {
            shared: self.model.clone(),
            model: model.model.clone(),
            caps: Capabilities::openai(),
        }))
    }

    fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
        self.memory.clone()
    }

    fn catalog(&self) -> ModelCatalog {
        self.catalog.clone()
    }
}

/// The team's host directory over the test's real [`HubServer`].
struct TestHosts {
    server: Arc<HubServer>,
    address: String,
}

#[async_trait]
impl HostDirectory for TestHosts {
    fn server(&self) -> Option<Arc<HubServer>> {
        Some(self.server.clone())
    }

    fn address(&self) -> Option<String> {
        Some(self.address.clone())
    }

    async fn pair(&self, name: Option<&str>) -> Result<PairOffer, String> {
        let code = self.server.pairing().issue_code(name.unwrap_or("worker node"));
        Ok(PairOffer {
            code: code.code,
            expires_ms: code.expires_ms,
            hub: self.address.clone(),
        })
    }

    async fn revoke(&self, host: &str) -> Result<NodeRecord, String> {
        let record = self
            .server
            .nodes()
            .list()?
            .into_iter()
            .find(|record| record.node_id == host || record.name == host)
            .ok_or_else(|| format!("no worker host named '{host}'"))?;
        self.server.revoke(&record.node_id)?;
        Ok(record)
    }
}

/// Write the hub's config dir: `hubep` is the default endpoint, `scripted` its
/// one model, and the endpoint holds the sentinel API key.
fn write_hub_config(dir: &Path) {
    std::fs::create_dir_all(dir).expect("hub config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[general]\n\
         default_provider = \"hubep\"\n\
         default_model = \"scripted\"\n\
         bundled_embedding_model = \"hash\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        dir.join("endpoints.toml"),
        "[[endpoint]]\nname = \"hubep\"\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted\"]\nmax_context = 32000\n",
    )
    .expect("endpoints.toml");
    std::fs::write(
        dir.join("keys.toml"),
        format!("[hubep]\napi_key = \"{SENTINEL}\"\n"),
    )
    .expect("keys.toml");
}

// ── the node under test ────────────────────────────────────────────────────

/// The freshly built node binary Cargo puts beside the test executable.
fn node_binary() -> &'static str {
    env!("CARGO_BIN_EXE_mnemo2-node")
}

/// Run one `mnemo2-node` subcommand to completion.
async fn cli(args: &[&str], config_dir: &Path) -> std::process::Output {
    let command = Command::new(node_binary())
        .args(args)
        .env("MNEMO_HOME", config_dir)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(Duration::from_secs(60), command)
        .await
        .expect("the mnemo2-node command finished in time")
        .expect("mnemo2-node starts")
}

/// Pair the node in `config_dir` with the hub at `hub`, through the CLI.
async fn pair_node(server: &HubServer, hub: SocketAddr, config_dir: &Path) -> NodeIdentity {
    let code = server.pairing().issue_code("remote e2e node");
    let hub = hub.to_string();
    let output = cli(
        &[
            "pair",
            "--hub",
            &hub,
            "--code",
            &code.code,
            "--name",
            "remote e2e node",
            "--config-dir",
            &config_dir.to_string_lossy(),
        ],
        config_dir,
    )
    .await;
    assert_eq!(
        output.status.code(),
        Some(0),
        "pairing failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let identity = NodeIdentity::load(&config_dir.join("node"))
        .expect("node.json reads")
        .expect("pairing wrote node.json");
    assert_eq!(
        identity.node_id,
        server.nodes().list().expect("node registry reads")[0].node_id,
        "pairing registers the node"
    );
    identity
}

/// A `mnemo2-node run` child process, with its collected output.
struct NodeProcess {
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
}

impl NodeProcess {
    /// Start `mnemo2-node run` over `workspace` with one pool slot.
    fn start(config_dir: &Path, workspace: &Path) -> Self {
        let mut child = Command::new(node_binary())
            .args([
                "run",
                "--workspace",
                &workspace.to_string_lossy(),
                "--slots",
                "1",
                "--config-dir",
                &config_dir.to_string_lossy(),
            ])
            .env("MNEMO_HOME", config_dir)
            // The worker commits in its node checkout; give git an identity.
            .env("GIT_AUTHOR_NAME", "Mnemo2 Remote Worker")
            .env("GIT_AUTHOR_EMAIL", "remote-worker@example.invalid")
            .env("GIT_COMMITTER_NAME", "Mnemo2 Remote Worker")
            .env("GIT_COMMITTER_EMAIL", "remote-worker@example.invalid")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("mnemo2-node run starts");
        let logs = Arc::new(Mutex::new(Vec::new()));
        if let Some(stdout) = child.stdout.take() {
            let logs = logs.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    logs.lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .push(line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let logs = logs.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    logs.lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .push(line);
                }
            });
        }
        Self { child, logs }
    }

    /// What the node printed so far, for failure messages.
    fn logs(&self) -> String {
        self.logs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .join("\n")
    }

    /// Kill the process and wait until it is gone.
    async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}
// ── the scratch project ────────────────────────────────────────────────────

/// The current Unix time in seconds.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Run git in `dir`, panicking on failure.
fn run_git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A committed scratch project on the lead branch. `.worktrees/` and the
/// engine's rebuildable caches are ignored (as the real project does), so the
/// hub's shadow checkout and the node's engine never dirty the trees.
/// `.coding/checks.toml` and `.coding/knowledge/` stay tracked: the worker's
/// typed knowledge file must travel with its branch and land with the merge.
fn init_repo(root: &Path) {
    std::fs::create_dir_all(root).expect("repo dir");
    run_git(root, &["init", "-q", "-b", "wt/mnemo"]);
    run_git(root, &["config", "user.name", "Mnemo2 Test"]);
    run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
    std::fs::write(root.join("README.md"), "remote team e2e\n").expect("readme");
    std::fs::write(
        root.join(".gitignore"),
        ".worktrees/\n\
         .coding/memory.db\n\
         .coding/memory.db-*\n\
         .coding/codegraph.db\n\
         .coding/codegraph.db-*\n\
         .coding/tmp/\n\
         .coding/knowledge/HEALTH.md\n\
         .coding/knowledge/.health-state.json\n\
         .coding/knowledge/INDEX.md*\n",
    )
    .expect("gitignore");
    std::fs::create_dir_all(root.join(".coding")).expect("coding dir");
    std::fs::write(
        root.join(".coding").join("checks.toml"),
        "commands = [\"git --version\"]\n",
    )
    .expect("checks.toml");
    run_git(root, &["add", "-A"]);
    run_git(root, &["commit", "-q", "-m", "seed"]);
}

/// A file's content, or `None` when it cannot be read.
fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// ── polling ────────────────��───────────────────────────────────────────────

/// Poll `probe` until it answers `Some`, panicking once `timeout` has passed.
async fn wait_for<T, F, Fut>(what: &str, timeout: Duration, mut probe: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let start = std::time::Instant::now();
    loop {
        if let Some(value) = probe().await {
            return value;
        }
        assert!(
            start.elapsed() <= timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

// ── the tests ───────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_remote_worker_dispatch_check_review_merge() {
    within(async {
        // (1) the hub's config dir and the project repo on the lead branch.
        let tmp = tempfile::tempdir().expect("temp dir");
        let base = tmp.path().to_path_buf();
        let hub_config = base.join("hub-config");
        write_hub_config(&hub_config);
        // Routing off: the dispatch pins its model, so no classifier runs.
        let routing = RoutingSettings::default();
        assert_eq!(routing.mode, RoutingMode::Off, "dispatch routing starts off");
        save_routing_settings(&hub_config, &routing).expect("save the routing settings");
        let root = base.join("project");
        init_repo(&root);

        // (2) the hub's engine over the project: its store is the hub store the
        // worker's routed writes land in. The database lives outside the
        // checkout so the lead tree stays clean for the merge.
        let engine = Engine::start(EngineOptions {
            workspace: Workspace::new(&root),
            config_dir: Some(hub_config.clone()),
            permissions: PermissionPolicy::OverridePermissions,
            memory: MemoryBackend::Sqlite {
                path: Some(base.join("hub-memory.db")),
            },
            index_codegraph: false,
            require_git: true,
            auto_install_embedder: false,
        })
        .await
        .expect("start the hub engine");
        let store = engine
            .memory_store_handle()
            .expect("the hub engine opens a memory store");

        // A machine-corpus row of the hub: a node session must never see it.
        let mut machine = Memory::new(
            MemoryTier::Semantic,
            MACHINE_TITLE,
            "hub-only machine knowledge that stays with this computer",
            now_secs(),
        );
        machine.record_class = MemoryClass::Derived;
        machine.data = json!({"source": MACHINE_SOURCE});
        store.write(machine).await.expect("write the machine row");
        // Positive control: the hub's own store DOES return the row, so its
        // absence from the node's search below comes from the hub's filter.
        let hub_hits = store
            .recall_peek(MACHINE_TITLE, &MemoryFilter::default())
            .await
            .expect("recall on the hub store");
        assert!(
            hub_hits.iter().any(|hit| hit.memory.title == MACHINE_TITLE),
            "the hub store must find its own machine row; hits: {:?}",
            hub_hits.iter().map(|hit| hit.memory.title.clone()).collect::<Vec<_>>()
        );

        // (3) the team over the same engine and project, with the plan its
        // dispatch belongs to.
        let state = base.join("team-state");
        std::fs::create_dir_all(&state).expect("team state");
        let team = Arc::new(
            TeamRuntime::open(TeamRuntimeOptions {
                engine: Arc::new(engine),
                workspace_root: root.clone(),
                state_dir: state.clone(),
                config_dir: hub_config.clone(),
                classifier: None,
                routing_log_path: Some(state.join("dispatch_routing.jsonl")),
                slots: Some(2),
                models: vec![LeadModel {
                    endpoint: "hubep".into(),
                    model: "scripted".into(),
                    spec: "hubep/scripted".into(),
                    is_default: true,
                    reasoning_efforts: Vec::new(),
                    default_effort: None,
                }],
                pricing: Vec::new(),
            })
            .await
            .expect("open the team"),
        );
        let board = Arc::new(PlanBoard::new(
            root.join(".coding").join("plans"),
            root.join(".coding").join("reviews"),
        ));
        team.attach_board(board.clone());
        let plan = board
            .create(PlanDraft {
                title: "Remote worker end to end".into(),
                goal: "Prove a Mnemo worker runs on a paired node and lands its work."
                    .into(),
                context: "Phase 3 step 8: the worker edits remote.txt and records a HOW \
                          knowledge file under .coding/knowledge/how/ on its node; the lead \
                          then checks, reviews and merges its branch from the shadow checkout."
                    .into(),
                kind: PlanKind::Implementation,
                bug: None,
                steps: vec![
                    "Write remote.txt at the project root and record the HOW knowledge file \
                     under .coding/knowledge/how/, then commit both in the node checkout."
                        .into(),
                ],
                interfaces: Vec::new(),
                acceptance: vec![
                    "The remote worker's commit lands on the lead branch with its knowledge file."
                        .into(),
                ],
                spec_deltas: Vec::new(),
                no_spec_impact: Some("the e2e only exercises the existing remote path".into()),
            })
            .expect("create the plan");

        // (4) the hub, with the scripted model and the engine's own store.
        let scripted = ScriptedModel::new(worker_script());
        let catalog = ModelCatalog::from_config(
            &Config::load(&hub_config).expect("hub config loads"),
            String::new(),
        );
        let server = Arc::new(
            HubServer::start(
                HubOptions {
                    bind: "127.0.0.1:0".parse().expect("loopback"),
                    state_dir: base.join("hub-state"),
                },
                Arc::new(TestServices {
                    model: scripted.clone(),
                    memory: Some(store.clone()),
                    catalog,
                }),
            )
            .expect("start the hub"),
        );
        team.attach_hosts(Arc::new(TestHosts {
            server: server.clone(),
            address: server.local_addr().to_string(),
        }));

        // (5) the real node: pair through the CLI, then run it.
        let node_config = base.join("node-config");
        let node_workspace = base.join("node-workspace");
        std::fs::create_dir_all(&node_workspace).expect("node workspace");
        let identity = pair_node(&server, server.local_addr(), &node_config).await;
        let mut node = NodeProcess::start(&node_config, &node_workspace);
        assert!(
            server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node should connect; its output:\n{}",
            node.logs()
        );

        // (6) the host monitor qualifies the node by running the project's
        // checks there; wait until team_hosts reports them passed.
        let host = wait_for(
            "the node to pass the project's checks",
            Duration::from_secs(120),
            || async {
                let answer = team
                    .handle_lead_host_tool("team_hosts", json!({"action": "list"}))
                    .await;
                let parsed: Value = serde_json::from_str(&answer.output).ok()?;
                let hosts = parsed.get("hosts")?.as_array()?;
                hosts.iter().find_map(|host| {
                    let name = host.get("name")?.as_str()?.to_string();
                    (host.get("checks").and_then(Value::as_str) == Some("passed"))
                        .then_some(name)
                })
            },
        )
        .await;
        assert_eq!(host, "remote e2e node", "team_hosts names the paired node");

        // (7) dispatch with no host: placement must choose the node.
        let answer = team
            .handle_lead_host_tool(
                "team_dispatch",
                json!({
                    "worker_id": WORKER_ID,
                    "plan_id": plan.id,
                    "step": 1,
                    "brief": "Write remote.txt at the project root with the line \
                              'written on the node', record a HOW memory titled \
                              'remote e2e how', commit both with git, then report to the \
                              lead with message_parent and finish.",
                    "model": "hubep/scripted",
                }),
            )
            .await;
        assert!(answer.success, "dispatch failed: {}", answer.output);

        // (8) the scripted turn runs on the node; wait until it is finished.
        let worker = wait_for(
            "the remote worker's turn",
            Duration::from_secs(180),
            || async {
                let worker = team.worker(WORKER_ID).await.ok()?;
                matches!(worker.status, WorkerStatus::Idle | WorkerStatus::Completed)
                    .then_some(worker)
            },
        )
        .await;
        assert_eq!(
            worker.host_label(),
            host,
            "team_status must name the node as the worker's host; worker: {}",
            serde_json::to_string(&worker).unwrap_or_default()
        );
        let requests = scripted.requests();
        assert!(
            !requests.is_empty(),
            "the worker's model calls should be served by the hub"
        );
        assert!(
            requests.iter().all(|model| model == "scripted"),
            "every model request must arrive as the catalog model, got {requests:?}"
        );
        // The remote worker's conversation lives on the node, so read it from
        // the requests it sent the hub. The final request carries every tool
        // result of the turn. The memory_search call (call-5) must have run
        // and returned, or its result would hide nothing.
        let conversation = scripted.last_conversation();
        let search_result = conversation
            .iter()
            .find(|message| message.tool_call_id.as_deref() == Some("call-5"))
            .unwrap_or_else(|| {
                panic!(
                    "the worker conversation has no memory_search result; tool results: {:?}",
                    conversation
                        .iter()
                        .filter_map(|message| message.tool_call_id.clone())
                        .collect::<Vec<_>>()
                )
            });
        let search_text = search_result.content.as_text();
        assert!(
            !search_result.tool_is_error && !search_text.contains("[tool error]"),
            "memory_search must succeed on the node, got: {search_text}"
        );
        assert!(
            search_text.contains("memories matched") || search_text.contains("no memories matched"),
            "memory_search must return a search answer, got: {search_text}"
        );
        assert!(
            !search_text.contains(MACHINE_TITLE),
            "a node session must never see the hub's machine knowledge: {search_text}"
        );

        // (8b) the hub record carries the conversation the node ran: the pump
        // refreshes it from the node's post-turn snapshot, so a later resume —
        // host "local" or on the node — starts from the real conversation.
        let transcript = wait_for(
            "the remote worker's transcript to reach the hub record",
            Duration::from_secs(60),
            || async {
                let worker = team.worker(WORKER_ID).await.ok()?;
                (!worker.transcript.messages.is_empty()).then(|| worker.transcript.clone())
            },
        )
        .await;
        // The opening system head carries the pinned task and the lead's brief
        // verbatim: a resume rebuilds the head from the record, but the hub
        // transcript must already hold the conversation it started from.
        let opening = transcript
            .messages
            .iter()
            .find(|message| message.role == Role::System)
            .map(|message| message.content.as_text())
            .expect("the worker's opening prompt reaches the hub record");
        assert!(
            opening.contains("Write remote.txt at the project root with the line"),
            "the hub transcript keeps the lead's opening brief: {opening}"
        );
        let replies = transcript
            .messages
            .iter()
            .filter(|message| message.role == Role::Assistant)
            .collect::<Vec<_>>();
        assert!(
            !replies.is_empty(),
            "the hub transcript keeps the worker's assistant turns: {:?}",
            transcript
                .messages
                .iter()
                .map(|message| message.role)
                .collect::<Vec<_>>()
        );
        assert!(
            replies
                .last()
                .is_some_and(|message| message.content.as_text().contains("done")),
            "the worker's closing answer reaches the hub record: {:?}",
            replies
                .last()
                .map(|message| message.content.as_text())
        );

        // (9) the project's checks run on the node, at the worker tip.
        let answer = team
            .handle_lead_host_tool("team_run_checks", json!({"worker_id": WORKER_ID}))
            .await;
        assert!(answer.success, "team_run_checks failed: {}", answer.output);
        wait_for(
            "git --version to pass at the worker tip",
            Duration::from_secs(120),
            || async {
                let answer = team
                    .handle_lead_host_tool(
                        "team_status",
                        json!({"worker_id": WORKER_ID, "detail": true}),
                    )
                    .await;
                let parsed: Value = serde_json::from_str(&answer.output).ok()?;
                let worker = parsed.get("result")?.as_array()?.first()?;
                let checks = worker.get("checks")?.as_array()?;
                checks
                    .iter()
                    .any(|check| {
                        check.get("name").and_then(Value::as_str) == Some("git --version")
                            && check.get("status").and_then(Value::as_str) == Some("passed")
                    })
                    .then_some(())
            },
        )
        .await;

        // (10) review the shadow checkout and merge the branch.
        let answer = team
            .handle_lead_host_tool(
                "team_review",
                json!({
                    "worker_id": WORKER_ID,
                    "summary": "the remote worker adds remote.txt and a HOW record on the node",
                    "findings": [],
                    "evidence": {
                        "unanchored_pass": true,
                        "intent": "the node-side worker writes remote.txt and a typed HOW record, then commits both",
                        "execution": "the hub places the worker on the node, sends the base bundle, and the worker's shell commits in its node checkout",
                        "integration": "the hub pulls the branch into the shadow checkout; the checks ran on the node at the worker tip",
                        "test_quality": "the e2e asserts the merged tree, the knowledge file and the two-parent merge commit",
                        "residual_risk": "none beyond the e2e's temporary checkout",
                    },
                }),
            )
            .await;
        assert!(answer.success, "review failed: {}", answer.output);
        let answer = team
            .handle_lead_host_tool("team_merge", json!({"worker_id": WORKER_ID}))
            .await;
        assert!(answer.success, "merge failed: {}", answer.output);

        // (11) the lead branch carries the worker's work.
        let remote = read(&root.join("remote.txt"))
            .expect("the worker's file is on the lead branch after the merge");
        assert_eq!(
            remote.trim_end(),
            "written on the node",
            "the worker's file carries the node's line ending"
        );
        let knowledge = root.join(".coding").join("knowledge");
        let mut how = Vec::new();
        collect_files(&knowledge.join("how"), &mut how);
        assert_eq!(
            how.len(),
            1,
            "exactly one HOW record travels with the branch: {how:?}"
        );
        let rel = how[0]
            .strip_prefix(&knowledge)
            .expect("the record is under .coding/knowledge")
            .to_string_lossy()
            .replace('\\', "/");
        assert!(rel.starts_with("how/"), "the record is a HOW record: {rel}");
        let parents = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["log", "-1", "--format=%P"])
            .output()
            .expect("git log runs");
        let parents = String::from_utf8_lossy(&parents.stdout);
        assert_eq!(
            parents.split_whitespace().count(),
            2,
            "the lead branch holds a --no-ff merge commit: {parents}"
        );
        let indexed = mnemo::memory::indexer::knowledge_id(&rel);
        wait_for(
            "the hub to index the merged knowledge file",
            Duration::from_secs(30),
            || {
                let store = store.clone();
                let indexed = indexed.clone();
                async move {
                    store
                        .get_memory(&indexed)
                        .await
                        .ok()
                        .flatten()
                        .map(|_| ())
                }
            },
        )
        .await;

        // (12) no API key ever reached the node's home.
        let mut files = Vec::new();
        collect_files(&node_config, &mut files);
        assert!(!files.is_empty(), "the node home holds files");
        for file in &files {
            if let Ok(bytes) = std::fs::read(file) {
                assert!(
                    !contains(&bytes, SENTINEL.as_bytes()),
                    "the node must never receive the hub's API key: {}",
                    file.display()
                );
            }
        }
        node.kill().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_pinned_unknown_model_is_refused() {
    within(async {
        let tmp = tempfile::tempdir().expect("temp dir");
        let base = tmp.path().to_path_buf();
        let hub_config = base.join("hub-config");
        write_hub_config(&hub_config);
        let catalog = ModelCatalog::from_config(
            &Config::load(&hub_config).expect("hub config loads"),
            String::new(),
        );
        let server = Arc::new(
            HubServer::start(
                HubOptions {
                    bind: "127.0.0.1:0".parse().expect("loopback"),
                    state_dir: base.join("hub-state"),
                },
                Arc::new(TestServices {
                    model: ScriptedModel::new(Vec::new()),
                    memory: None,
                    catalog,
                }),
            )
            .expect("start the hub"),
        );
        let node_config = base.join("node-config");
        let node_workspace = base.join("node-workspace");
        std::fs::create_dir_all(&node_workspace).expect("node workspace");
        let identity = pair_node(&server, server.local_addr(), &node_config).await;
        let mut node = NodeProcess::start(&node_config, &node_workspace);
        assert!(
            server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node should connect; its output:\n{}",
            node.logs()
        );
        let handle: NodeHandle = server.node(&identity.node_id);
        let error = handle
            .spawn_session(RemoteSpawn {
                options: SpawnOptions {
                    name: Some("pinned unknown model".into()),
                    model: Some("hubep/nope".into()),
                    ..SpawnOptions::default()
                },
                checkout: None,
            })
            .await
            .expect_err("a model the node's catalog does not know must be refused");
        assert_eq!(
            error.code, CODE_BAD_PARAMS,
            "the refusal is bad_params, never a silent run on the default model: {error:?}"
        );
        node.kill().await;
    })
    .await;
}

// ── helpers ────────────────────────────────────────────────────────────────

/// Every file under `root`, recursively.
fn collect_files(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|window| window == needle)
}
