// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Two-process end-to-end tests of the remote worker node (plan a040713f,
//! phase 2 step 6): the hub runs in the test process, the node is a real
//! `mnemo2-node` child process paired through its command line.
//!
//! Every directory comes from `tempfile::tempdir()`, which lives under the
//! checkout's `target/` (`.cargo/config.toml` `[env]`), never the system temp
//! folder. Every port is picked by the OS.

use std::collections::{BTreeSet, VecDeque};
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
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use mnemo::agent::policy::PermissionPolicy;
use mnemo::config::{Config, ModelRef};
use mnemo::headless::{
    HostToolSpec, SessionCommand, SessionId, SpawnOptions,
};
use mnemo::memory::{HashEmbedder, MemoryStore, MemoryStoreTrait, MemoryTier};
use mnemo::provider::trace::LlmRequestContext;
use mnemo::provider::{
    Capabilities, FinishReason, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice,
    ToolSchema,
};
use mnemo::remote::catalog::ModelCatalog;
use mnemo::remote::hub::{
    EngineEventItem, HubOptions, HubServer, HubServices, NodeHandle, RemoteSpawn,
};
use mnemo::remote::pairing::{NodeIdentity, NodeRegistry};
use mnemo::runtime::channels::SerializableAgentEvent;
use mnemo::tool::{SafetyLevel, ToolResult};

/// The API key the hub holds and no node file may ever contain.
const SENTINEL: &str = "sk-SENTINEL-e2e";
/// The memory the scripted worker writes.
const MEMORY_TITLE: &str = "remote e2e fact";
/// The overall budget of one e2e test.
const TEST_BUDGET: Duration = Duration::from_secs(120);

/// Run `body` under the overall budget of one e2e test.
async fn within<F: Future>(body: F) -> F::Output {
    tokio::time::timeout(TEST_BUDGET, body)
        .await
        .expect("the e2e test exceeded its 120 s budget")
}

// ── the hub's scripted model ───────────────────────────────────────────────

/// What one model request answers.
enum Script {
    /// These stream items, in order.
    Items(Vec<LlmEvent>),
    /// Accept the request and never yield an item.
    Stall,
}

/// The hub's model endpoint: pops one script per request and records the model
/// id each request arrived with.
struct ScriptedModel {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<String>>,
}

impl ScriptedModel {
    fn new(scripts: Vec<Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
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
        _messages: &[Message],
        _tools: &[ToolSchema],
        _tool_choice: Option<ToolChoice>,
    ) -> mnemo::error::Result<BoxStream<'_, LlmEvent>> {
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
        Ok(match script {
            Script::Items(items) => Box::pin(futures::stream::iter(items)),
            Script::Stall => Box::pin(futures::stream::pending()),
        })
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

/// The scripted worker turn: write a memory, message the parent, finish.
fn full_script() -> Vec<Script> {
    vec![
        memory_write_call("call-1"),
        tool_call("call-2", "message_parent", json!({ "text": "hi" })),
        final_text("done"),
    ]
}

// The title is deliberately untyped. A typed record (DECISION:/SPEC:/HOW:) is
// a knowledge file in the worker's own checkout plus a node-local derived row,
// exactly as for a local worker: it reaches the lead through the worker's
// branch merge (on a node, phase 3's git-bundle sync) and is indexed on the
// hub after the merge. An untyped write is a shared row and goes to the hub.
fn memory_write_call(id: &str) -> Script {
    tool_call(
        id,
        "memory_write",
        json!({
            "tier": "semantic",
            "title": MEMORY_TITLE,
            "content": "written from the node",
        }),
    )
}

// ── the hub under test ─────────────────────────────────────────────────────

struct E2eServices {
    model: Arc<ScriptedModel>,
    store: Arc<MemoryStore>,
    catalog: ModelCatalog,
}

impl HubServices for E2eServices {
    fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
        Some(Arc::new(ScriptedClient {
            shared: self.model.clone(),
            model: model.model.clone(),
            caps: Capabilities::openai(),
        }))
    }

    fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
        Some(self.store.clone())
    }

    fn catalog(&self) -> ModelCatalog {
        self.catalog.clone()
    }
}

struct Hub {
    server: HubServer,
    model: Arc<ScriptedModel>,
    store: Arc<MemoryStore>,
    state_dir: PathBuf,
    catalog_revision: String,
}

/// Write the hub's config dir: `hubep` is the default endpoint, `scripted` its
/// one model, and the endpoint holds the sentinel API key.
fn write_hub_config(dir: &Path) {
    std::fs::create_dir_all(dir).expect("hub config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_provider = \"hubep\"\ndefault_model = \"scripted\"\n",
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

/// Start a hub over `<root>/hub` (memory store and registry), answering model
/// calls from `model`. A restarted hub passes the same `root`.
fn start_hub(root: &Path, config_dir: &Path, model: Arc<ScriptedModel>) -> Hub {
    let config = Config::load(config_dir).expect("hub config loads");
    assert_eq!(
        config.keys.get("hubep"),
        Some(SENTINEL),
        "the hub itself holds the key"
    );
    let hub_dir = root.join("hub");
    std::fs::create_dir_all(&hub_dir).expect("hub dir");
    let store = Arc::new(
        MemoryStore::open(&hub_dir.join("memory.db"), Arc::new(HashEmbedder::new()))
            .expect("hub memory store"),
    );
    let catalog = ModelCatalog::from_config(&config, String::new());
    let catalog_revision = catalog.revision.clone();
    let state_dir = hub_dir.join("state");
    let server = HubServer::start(
        HubOptions {
            bind: "127.0.0.1:0".parse().expect("loopback"),
            state_dir: state_dir.clone(),
        },
        Arc::new(E2eServices {
            model: model.clone(),
            store: store.clone(),
            catalog,
        }),
    )
    .expect("hub starts");
    Hub {
        server,
        model,
        store,
        state_dir,
        catalog_revision,
    }
}

// ── a byte-forwarding proxy the tests can cut ──────────────────────────────

/// A TCP forwarder in front of the hub: the node dials it, so a test can cut
/// every link and point it at a restarted hub.
struct LinkCutter {
    addr: SocketAddr,
    target: Arc<Mutex<SocketAddr>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    accept: JoinHandle<()>,
}

impl LinkCutter {
    async fn start(target: SocketAddr) -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("cutter bind");
        let addr = listener.local_addr().expect("cutter addr");
        let target = Arc::new(Mutex::new(target));
        let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let current = target.clone();
        let live = connections.clone();
        let accept = tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    continue;
                };
                let hub = *current.lock().unwrap_or_else(|poison| poison.into_inner());
                let forward = tokio::spawn(async move {
                    let Ok(mut server) = tokio::net::TcpStream::connect(hub).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                });
                let mut live = live.lock().unwrap_or_else(|poison| poison.into_inner());
                live.retain(|task| !task.is_finished());
                live.push(forward);
            }
        });
        Self {
            addr,
            target,
            connections,
            accept,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Forward new connections to `hub`.
    fn set_target(&self, hub: SocketAddr) {
        *self.target.lock().unwrap_or_else(|poison| poison.into_inner()) = hub;
    }

    /// Drop every forwarded connection (both of its sockets).
    fn cut_all(&self) {
        for task in self
            .connections
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }

    /// How many forwarded connections are still open.
    fn open_connections(&self) -> usize {
        self.connections
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .filter(|task| !task.is_finished())
            .count()
    }
}

impl Drop for LinkCutter {
    fn drop(&mut self) {
        self.accept.abort();
        self.cut_all();
    }
}

// ── the node process ───────────────────────────────────────────────────────

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

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Pair the node in `config_dir` with the hub at `hub`, through the CLI.
async fn pair_node(server: &HubServer, hub: SocketAddr, config_dir: &Path) -> NodeIdentity {
    let code = server.pairing().issue_code("e2e node");
    let hub = hub.to_string();
    let output = cli(
        &[
            "pair",
            "--hub",
            &hub,
            "--code",
            &code.code,
            "--name",
            "e2e-node",
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
        text_of(&output.stderr)
    );
    NodeIdentity::load(&config_dir.join("node"))
        .expect("node.json reads")
        .expect("pairing wrote node.json")
}

/// A running `mnemo2-node run` child; killed when dropped.
struct NodeProcess {
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
}

impl NodeProcess {
    fn start(config_dir: &Path, workspace: &Path) -> Self {
        let mut child = Command::new(node_binary())
            .args([
                "run",
                "--workspace",
                &workspace.to_string_lossy(),
                "--config-dir",
                &config_dir.to_string_lossy(),
            ])
            .env("MNEMO_HOME", config_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("mnemo2-node run starts");
        let logs = Arc::new(Mutex::new(Vec::new()));
        if let Some(stdout) = child.stdout.take() {
            drain(stdout, "out", logs.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            drain(stderr, "err", logs.clone());
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
    }
}

/// Collect a child pipe's lines so the child never blocks on a full pipe.
fn drain<R>(pipe: R, tag: &'static str, logs: Arc<Mutex<Vec<String>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            logs.lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(format!("[{tag}] {line}"));
        }
    });
}

// ── driving a session ──────────────────────────────────────────────────────

/// The message_parent host tool the lead would serve.
fn message_parent_tool() -> HostToolSpec {
    HostToolSpec {
        name: "message_parent".into(),
        description: "Send a message to the lead".into(),
        parameters: json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        }),
        safety: SafetyLevel::AutoRun,
        timeout_ms: Some(30_000),
    }
}

/// A session of the scripted model with the message_parent host tool.
fn spawn_request() -> RemoteSpawn {
    RemoteSpawn {
        options: SpawnOptions {
            name: Some("e2e worker".into()),
            model: Some("hubep/scripted".into()),
            permissions: Some(PermissionPolicy::OverridePermissions),
            host_tools: vec![message_parent_tool()],
            skip_turn_routing: true,
            ..SpawnOptions::default()
        },
        checkout: None,
    }
}

async fn prompt(handle: &NodeHandle, session: SessionId, text: &str) {
    handle
        .send(
            session,
            SessionCommand::Prompt {
                text: text.into(),
                images: Vec::new(),
            },
        )
        .await
        .expect("the prompt reaches the node");
}

/// Follow `events` until `session` finishes a turn with `Stop`, answering its
/// message_parent calls. Fails on a non-retrying error. Answers every event
/// of the session seen on the way.
async fn drive_to_stop(
    handle: &NodeHandle,
    events: &mut mpsc::Receiver<EngineEventItem>,
    session: SessionId,
    limit: Duration,
) -> Vec<SerializableAgentEvent> {
    let deadline = tokio::time::Instant::now() + limit;
    let mut seen = Vec::new();
    loop {
        let item = tokio::time::timeout_at(deadline, events.recv())
            .await
            .unwrap_or_else(|_| panic!("the turn did not finish; saw {seen:?}"))
            .expect("the event stream ended");
        let EngineEventItem::Event { event, .. } = item else {
            continue;
        };
        if !event.is_from(session) {
            continue;
        }
        seen.push(event.event.clone());
        match event.event {
            SerializableAgentEvent::HostToolCall { call_id, name, .. }
                if name == "message_parent" =>
            {
                handle
                    .send(
                        session,
                        SessionCommand::HostToolResult {
                            call_id,
                            result: Some(ToolResult::success("delivered")),
                            error: None,
                        },
                    )
                    .await
                    .expect("the host tool result reaches the node");
            }
            SerializableAgentEvent::Error {
                error,
                retrying: false,
            } => panic!("the turn failed: {error}; saw {seen:?}"),
            SerializableAgentEvent::Finished {
                reason: FinishReason::Stop,
            } => return seen,
            _ => {}
        }
    }
}

/// Poll `check` until it holds or `limit` passes.
async fn eventually<F, Fut>(limit: Duration, mut check: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if check().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// How many memories titled `title` the store holds, over every tier.
async fn rows_titled(store: &dyn MemoryStoreTrait, title: &str) -> usize {
    let mut found = 0;
    for tier in [
        MemoryTier::Working,
        MemoryTier::Episodic,
        MemoryTier::Semantic,
        MemoryTier::Procedural,
    ] {
        found += store
            .list_by_tier(tier)
            .await
            .expect("memory tier lists")
            .iter()
            .filter(|memory| memory.title == title)
            .count();
    }
    found
}

/// A repository for the node's workspace.
fn make_repo(root: &Path) -> PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    std::fs::write(repo.join("README.md"), "# e2e repo\n").expect("README");
    repo
}

// ── the tests ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_node_session_uses_hub_model_and_hub_memory() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let hub_config = root.path().join("hub-cfg");
        let node_config = root.path().join("node-cfg");
        write_hub_config(&hub_config);
        let hub = start_hub(root.path(), &hub_config, ScriptedModel::new(full_script()));
        let repo = make_repo(root.path());
        let identity = pair_node(&hub.server, hub.server.local_addr(), &node_config).await;
        let mut node = NodeProcess::start(&node_config, &repo);
        assert!(
            hub.server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node never attached; its output:\n{}",
            node.logs()
        );
        let handle = hub.server.node(&identity.node_id);
        let mut events = handle.events();
        let session = handle.spawn_session(spawn_request()).await.expect("spawn");
        prompt(&handle, session, "write the memory, tell the lead, finish").await;
        let seen = drive_to_stop(&handle, &mut events, session, Duration::from_secs(90)).await;
        assert!(
            seen.iter().any(|event| matches!(
                event,
                SerializableAgentEvent::HostToolCall { name, .. } if name == "message_parent"
            )),
            "the message_parent host tool call should reach the hub"
        );

        let requests = hub.model.requests();
        assert_eq!(
            requests,
            vec!["scripted".to_string(); 3],
            "the hub should serve exactly the three scripted requests"
        );
        assert_eq!(
            rows_titled(hub.store.as_ref(), MEMORY_TITLE).await,
            1,
            "memory_write should land once in the hub's store; session events: {seen:?}"
        );

        handle.close_and_wait(session).await.expect("close");
        node.kill().await;
        let log = std::fs::read_to_string(node_config.join("node").join("node.log"))
            .expect("the node writes <home>/node.log");
        assert!(
            log.contains(&format!("node {} serving hub", identity.node_id)),
            "node.log should hold the startup line; it holds:\n{log}"
        );
        let node_store = node_config.join("node").join("state").join("memory.db");
        assert!(node_store.is_file(), "the node keeps a local store file");
        let local = MemoryStore::open(&node_store, Arc::new(HashEmbedder::new()))
            .expect("the node's store opens");
        assert_eq!(
            rows_titled(&local, MEMORY_TITLE).await,
            0,
            "the node's own store must not hold the shared memory"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_unpair_removes_identity() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let hub_config = root.path().join("hub-cfg");
        let node_config = root.path().join("node-cfg");
        write_hub_config(&hub_config);
        let hub = start_hub(root.path(), &hub_config, ScriptedModel::new(Vec::new()));
        pair_node(&hub.server, hub.server.local_addr(), &node_config).await;
        let identity_file = NodeIdentity::path(&node_config.join("node"));
        assert!(identity_file.is_file(), "pairing writes node.json");
        let config = node_config.to_string_lossy().into_owned();
        let unpair = cli(&["unpair", "--config-dir", &config], &node_config).await;
        assert_eq!(
            unpair.status.code(),
            Some(0),
            "unpair: {}",
            text_of(&unpair.stderr)
        );
        assert!(!identity_file.exists(), "unpair deletes node.json");
        let status = cli(&["status", "--config-dir", &config], &node_config).await;
        assert_eq!(status.status.code(), Some(3), "status after unpair");
        let again = cli(&["unpair", "--config-dir", &config], &node_config).await;
        assert_eq!(again.status.code(), Some(0), "unpair when not paired exits 0");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_hub_restart_mid_turn_resumes() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let hub_config = root.path().join("hub-cfg");
        let node_config = root.path().join("node-cfg");
        write_hub_config(&hub_config);
        // Hub 1 accepts the first model request and then never answers it.
        let first = start_hub(
            root.path(),
            &hub_config,
            ScriptedModel::new(vec![Script::Stall]),
        );
        let cutter = LinkCutter::start(first.server.local_addr()).await;
        let repo = make_repo(root.path());
        let identity = pair_node(&first.server, cutter.addr(), &node_config).await;
        let mut node = NodeProcess::start(&node_config, &repo);
        assert!(
            first
                .server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node never attached; its output:\n{}",
            node.logs()
        );
        let handle = first.server.node(&identity.node_id);
        let session = handle.spawn_session(spawn_request()).await.expect("spawn");
        prompt(&handle, session, "survive the restart").await;
        let model = first.model.clone();
        assert!(
            eventually(Duration::from_secs(30), || {
                let model = model.clone();
                async move { !model.requests().is_empty() }
            })
            .await,
            "hub 1 should receive the first model request; node output:\n{}",
            node.logs()
        );

        // Kill hub 1 mid-turn: shut it down, cut every link, drop its store.
        let Hub { server, store, .. } = first;
        server.shutdown().await;
        cutter.cut_all();
        drop(store);
        drop(handle);

        // Hub 2: same store file and registry, a fresh model, a new address.
        let second = start_hub(
            root.path(),
            &hub_config,
            ScriptedModel::new(vec![memory_write_call("call-1"), final_text("done")]),
        );
        cutter.set_target(second.server.local_addr());
        assert!(
            second
                .server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node should redial the restarted hub; its output:\n{}",
            node.logs()
        );
        let model = second.model.clone();
        assert!(
            eventually(Duration::from_secs(60), || {
                let model = model.clone();
                async move { !model.requests().is_empty() }
            })
            .await,
            "request 1 should be re-issued to hub 2; node output:\n{}",
            node.logs()
        );
        let handle = second.server.node(&identity.node_id);
        let mut events = handle.events();
        let seen = drive_to_stop(&handle, &mut events, session, Duration::from_secs(90)).await;
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                SerializableAgentEvent::Error { retrying: false, .. }
            )),
            "no non-retrying error may reach the session"
        );
        assert_eq!(
            second.model.requests(),
            vec!["scripted".to_string(); 2],
            "hub 2 serves the re-issued request and the closing one"
        );
        assert_eq!(
            rows_titled(second.store.as_ref(), MEMORY_TITLE).await,
            1,
            "the memory row exists exactly once after the restart"
        );
        handle.close_and_wait(session).await.expect("close");
        node.kill().await;
        let node_store = node_config.join("node").join("state").join("memory.db");
        assert!(node_store.is_file(), "the node keeps a local store file");
        let local = MemoryStore::open(&node_store, Arc::new(HashEmbedder::new()))
            .expect("the node's store opens");
        assert_eq!(
            rows_titled(&local, MEMORY_TITLE).await,
            0,
            "the node's own store must not hold the shared memory"
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_node_config_holds_no_api_key() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let hub_config = root.path().join("hub-cfg");
        let node_config = root.path().join("node-cfg");
        write_hub_config(&hub_config);
        let hub = start_hub(
            root.path(),
            &hub_config,
            ScriptedModel::new(vec![final_text("done")]),
        );
        let repo = make_repo(root.path());
        let identity = pair_node(&hub.server, hub.server.local_addr(), &node_config).await;
        let mut node = NodeProcess::start(&node_config, &repo);
        assert!(
            hub.server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node never attached; its output:\n{}",
            node.logs()
        );
        let handle = hub.server.node(&identity.node_id);
        let mut events = handle.events();
        let session = handle.spawn_session(spawn_request()).await.expect("spawn");
        prompt(&handle, session, "just answer").await;
        drive_to_stop(&handle, &mut events, session, Duration::from_secs(90)).await;
        let status = handle.status().await.expect("node.status");
        assert_eq!(
            status.get("catalog_revision").and_then(Value::as_str),
            Some(hub.catalog_revision.as_str()),
            "the node mirrors the hub's catalog"
        );
        handle.close_and_wait(session).await.expect("close");
        node.kill().await;

        // `status` shows the pairing and never the token.
        let shown = cli(
            &["status", "--config-dir", &node_config.to_string_lossy()],
            &node_config,
        )
        .await;
        assert_eq!(shown.status.code(), Some(0));
        let shown = text_of(&shown.stdout);
        assert!(shown.contains(&identity.node_id), "{shown}");
        assert!(shown.contains(&identity.hub_id), "{shown}");
        assert!(!shown.contains(&identity.token), "status must not print the token");

        let mut files = Vec::new();
        collect_files(&node_config, &mut files);
        assert!(
            files.iter().any(|file| file.ends_with("config.toml")),
            "the node mirrored the hub config: {files:?}"
        );
        for file in &files {
            assert_ne!(
                file.file_name().and_then(|name| name.to_str()),
                Some("keys.toml"),
                "a node never holds a keys.toml: {}",
                file.display()
            );
            let bytes = std::fs::read(file).unwrap_or_default();
            assert!(
                !contains(&bytes, SENTINEL.as_bytes()),
                "{} holds the hub's API key",
                file.display()
            );
        }
        let identity_file = std::fs::read_to_string(NodeIdentity::path(&node_config.join("node")))
            .expect("node.json");
        let keys: BTreeSet<String> = serde_json::from_str::<Value>(&identity_file)
            .expect("node.json is JSON")
            .as_object()
            .expect("node.json is an object")
            .keys()
            .cloned()
            .collect();
        let expected: BTreeSet<String> = ["hub", "node_id", "token", "hub_id", "paired_ms"]
            .iter()
            .map(|key| key.to_string())
            .collect();
        assert_eq!(keys, expected, "node.json holds only the pairing fields");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_unpaired_node_exits_3() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let node_config = root.path().join("node-cfg");
        let repo = make_repo(root.path());
        let config = node_config.to_string_lossy().into_owned();
        let run = cli(
            &[
                "run",
                "--workspace",
                &repo.to_string_lossy(),
                "--config-dir",
                &config,
            ],
            &node_config,
        )
        .await;
        assert_eq!(
            run.status.code(),
            Some(3),
            "run without pairing: {}",
            text_of(&run.stderr)
        );
        let status = cli(&["status", "--config-dir", &config], &node_config).await;
        assert_eq!(status.status.code(), Some(3), "status without pairing");
        let usage = cli(&["run", "--config-dir", &config], &node_config).await;
        assert_eq!(usage.status.code(), Some(2), "a missing --workspace is usage");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_revoked_node_exits_4() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let hub_config = root.path().join("hub-cfg");
        let node_config = root.path().join("node-cfg");
        write_hub_config(&hub_config);
        let hub = start_hub(root.path(), &hub_config, ScriptedModel::new(Vec::new()));
        let repo = make_repo(root.path());
        let identity = pair_node(&hub.server, hub.server.local_addr(), &node_config).await;
        NodeRegistry::new(hub.state_dir.clone())
            .revoke(&identity.node_id)
            .expect("revoke");
        let run = cli(
            &[
                "run",
                "--workspace",
                &repo.to_string_lossy(),
                "--config-dir",
                &node_config.to_string_lossy(),
            ],
            &node_config,
        )
        .await;
        assert_eq!(
            run.status.code(),
            Some(4),
            "a revoked node is refused for good: {}",
            text_of(&run.stderr)
        );
        // A burnt or unknown code is refused too.
        let again = cli(
            &[
                "pair",
                "--hub",
                &hub.server.local_addr().to_string(),
                "--code",
                "NOPE-NOPE",
                "--config-dir",
                &root.path().join("other-cfg").to_string_lossy(),
            ],
            &node_config,
        )
        .await;
        assert_eq!(again.status.code(), Some(4), "{}", text_of(&again.stderr));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_hub_shutdown_closes_node_link() {
    within(async {
        let root = tempfile::tempdir().expect("temp dir");
        let hub_config = root.path().join("hub-cfg");
        let node_config = root.path().join("node-cfg");
        write_hub_config(&hub_config);
        let hub = start_hub(root.path(), &hub_config, ScriptedModel::new(Vec::new()));
        let hub_addr = hub.server.local_addr();
        let cutter = LinkCutter::start(hub_addr).await;
        let repo = make_repo(root.path());
        let identity = pair_node(&hub.server, cutter.addr(), &node_config).await;
        let mut node = NodeProcess::start(&node_config, &repo);
        assert!(
            hub.server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node never attached; its output:\n{}",
            node.logs()
        );
        let handle = hub.server.node(&identity.node_id);
        let mut events = handle.events();
        assert!(
            eventually(Duration::from_secs(10), || async {
                cutter.open_connections() > 0
            })
            .await,
            "the node's link runs through the proxy"
        );

        hub.server.shutdown().await;
        // The hub's side of every link is closed by the hub itself: nothing
        // is left open behind the proxy even though the node keeps redialing.
        assert!(
            eventually(Duration::from_secs(15), || async {
                cutter.open_connections() == 0
            })
            .await,
            "a shut-down hub must close the node's link; node output:\n{}",
            node.logs()
        );
        let error = handle
            .status()
            .await
            .expect_err("a shut-down hub has no node link");
        assert_eq!(error.code, mnemo::remote::CODE_LINK_DOWN);
        // The follower task is aborted with the hub: its stream ends.
        let ended = tokio::time::timeout(Duration::from_secs(10), async {
            while events.recv().await.is_some() {}
        })
        .await;
        assert!(ended.is_ok(), "the event follower must end with the hub");
        assert!(
            tokio::net::TcpStream::connect(hub_addr).await.is_err(),
            "the hub's listener must be closed"
        );
        node.kill().await;
    })
    .await;
}

// ── file helpers ───────────────────────────────────────────────────────────

/// Every regular file under `root`.
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

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}
