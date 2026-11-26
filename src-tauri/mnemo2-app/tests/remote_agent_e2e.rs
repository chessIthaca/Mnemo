// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Two-process end-to-end test of a remote Claude Code agent worker (plan
//! 5e64ccd5, phase 4 step 7): the hub is a real `TeamRuntime` in the test
//! process — its engine, its memory store, a real `HubServer` behind
//! `HostDirectory` — and the worker node is a real `mnemo2-node` child process
//! paired over loopback. The desktop app's SSH session is stood in for: the
//! test writes the Claude Code transcript the session would write under its
//! `CLAUDE_CONFIG_DIR` and registers the session with `team_agent_started`.
//! Mnemo never launches `claude`: a fake `claude` first on the node's PATH
//! records whether anything tried to run it, and the node's log is checked too.
//!
//! Everything lives under the checkout's `target/` (the repository's
//! `.cargo/config.toml` points `TMP` there), so no test leaves anything in the
//! system temp folder.

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
use mnemo::headless::{Engine, EngineOptions, MemoryBackend};
use mnemo::memory::MemoryStoreTrait;
use mnemo::provider::trace::LlmRequestContext;
use mnemo::provider::{
    Capabilities, LlmClient, LlmEvent, Message, ProviderKind, ToolChoice, ToolSchema,
};
use mnemo::remote::catalog::ModelCatalog;
use mnemo::remote::hub::{HubOptions, HubServer, HubServices};
use mnemo::remote::pairing::{NodeIdentity, NodeRecord};
use mnemo::remote::repo::{CheckoutList, CheckoutListAnswer, CHECKOUT_LIST};
use mnemo::workspace::Workspace;
use mnemo_app::mnemo2::dispatch_routing::{save_routing_settings, RoutingMode, RoutingSettings};
use mnemo_app::mnemo2::runtime::LeadModel;
use mnemo_app::mnemo2::team::{
    HostDirectory, MessageSender, PairOffer, TeamRuntime, TeamRuntimeOptions, WorkerRecord,
    WorkerStatus,
};

/// The worker this test dispatches and follows to the merge.
const WORKER_ID: &str = "remoteagent01";
/// The Claude Code session id the stand-in session registers (its transcript's
/// file stem, which is what `claude.watch` reports back).
const SESSION_ID: &str = "local_test1";
/// The saved SSH connection the host is linked to: naming it is the user's
/// standing approval to start Claude Code workers on that connection.
const SSH: &str = "test-ssh";
/// The node's name, as pairing registers it (a linkable host name: letters,
/// digits, '.', '_' and '-').
const NODE_NAME: &str = "remote-agent-e2e-node";
/// The hub endpoint's sentinel API key: it must never reach the node.
const SENTINEL: &str = "sk-SENTINEL-e2e";

/// The overall budget of one e2e test.
const TEST_BUDGET: Duration = Duration::from_secs(360);

/// Run `body` under the overall budget of one e2e test.
async fn within<F: Future>(body: F) -> F::Output {
    tokio::time::timeout(TEST_BUDGET, body)
        .await
        .expect("the e2e test exceeded its 360 s budget")
}

// ── the hub's scripted model ───────────────────────────────────────────────

/// The hub's model endpoint: records the model id each request arrived with.
/// A Claude Code agent worker never calls it, so every request is a test bug.
struct ScriptedModel {
    requests: Mutex<Vec<String>>,
    conversations: Mutex<Vec<Vec<Message>>>,
}

impl ScriptedModel {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            conversations: Mutex::new(Vec::new()),
        })
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
        Err(mnemo::error::Error::Provider(
            "a Claude Code agent worker never calls the hub's model".into(),
        ))
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

// ── the hub under test ─────────────────────────────────────────────────────

/// The hub's services: the scripted model for every model request, the team
/// engine's own memory store, and a catalog mirroring the hub's config.
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
    let code = server.pairing().issue_code(NODE_NAME);
    let hub = hub.to_string();
    let output = cli(
        &[
            "pair",
            "--hub",
            &hub,
            "--code",
            &code.code,
            "--name",
            NODE_NAME,
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
    NodeIdentity::load(&config_dir.join("node"))
        .expect("node.json reads")
        .expect("pairing wrote node.json")
}

/// Write a fake `claude` executable into `dir`: the node's PATH starts with
/// `dir`, so anything that runs `claude` runs this instead and drops a marker
/// file. `.cmd` on Windows, a shell script elsewhere (the lead's instruction).
fn write_claude_shim(dir: &Path, marker_env: &str) {
    std::fs::create_dir_all(dir).expect("shim dir");
    if std::env::consts::OS == "windows" {
        std::fs::write(
            dir.join("claude.cmd"),
            format!("@echo off\r\necho ran>\"%{marker_env}%\"\r\n"),
        )
        .expect("claude.cmd");
        return;
    }
    let path = dir.join("claude");
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho ran > \"${marker_env}\"\n"),
    )
    .expect("claude shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make the claude shim executable");
    }
}

/// A `mnemo2-node run` child process, with its collected output.
struct NodeProcess {
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
    /// The fake `claude` writes here when something runs it.
    claude_marker: PathBuf,
}

impl NodeProcess {
    /// Start `mnemo2-node run` over `workspace` with one pool slot, its Claude
    /// config dir in `claude_home`, and the fake `claude` first on its PATH.
    fn start(
        config_dir: &Path,
        workspace: &Path,
        claude_home: &Path,
        shim: Option<&Path>,
        claude_marker: &Path,
    ) -> Self {
        let mut path = std::ffi::OsString::new();
        if let Some(shim) = shim {
            path.push(shim);
            path.push(if std::env::consts::OS == "windows" {
                ";"
            } else {
                ":"
            });
        }
        path.push(std::env::var_os("PATH").unwrap_or_default());
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
            // The watch reads the session's transcript from this config dir,
            // exactly as the real Claude Code session on the host writes it.
            .env("CLAUDE_CONFIG_DIR", claude_home)
            .env("MNEMO_CLAUDE_SHIM_MARKER", claude_marker)
            .env("PATH", path)
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
        Self {
            child,
            logs,
            claude_marker: claude_marker.to_path_buf(),
        }
    }

    /// What the node printed so far, for failure messages.
    fn logs(&self) -> String {
        self.logs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .join("\n")
    }

    /// Whether anything on the node ran the fake `claude`.
    fn claude_ran(&self) -> bool {
        self.claude_marker.exists()
    }

    /// Kill the process and wait until it is gone.
    async fn kill(&mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

// ── the scratch project ────────────────────────────────────────────────────

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
fn init_repo(root: &Path) {
    std::fs::create_dir_all(root).expect("repo dir");
    run_git(root, &["init", "-q", "-b", "wt/mnemo"]);
    run_git(root, &["config", "user.name", "Mnemo2 Test"]);
    run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
    std::fs::write(root.join("README.md"), "remote agent e2e\n").expect("readme");
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

/// Commit `file` with `body` in `dir`, with an identity the test provides (the
/// hub's project config does not travel to a node clone).
fn commit_file(dir: &Path, file: &str, body: &str) -> String {
    std::fs::write(dir.join(file), body).expect("write the committed file");
    run_git(dir, &["add", file]);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Mnemo2 Remote Worker",
            "-c",
            "user.email=remote-worker@example.invalid",
            "commit",
            "-q",
            "-m",
            &format!("add {file} on the node"),
        ])
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    head_sha(dir)
}

/// The full sha `dir` is at.
fn head_sha(dir: &Path) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git rev-parse failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// A file's content, or `None` when it cannot be read.
fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Every file under `dir`, relative to it, sorted.
fn files_under(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path);
            } else if let Ok(rest) = path.strip_prefix(dir) {
                found.push(rest.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    found.sort();
    found
}

// ── polling ────────────────────────────────────────────────────────────────

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

/// Poll `probe` until it answers `true`; `false` once `timeout` has passed.
async fn wait_until<F, Fut>(timeout: Duration, mut probe: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let start = std::time::Instant::now();
    loop {
        if probe().await {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ── the hub config ─────────────────────────────────────────────────────────

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

// ── the rig: one hub, one scratch project, one plan ────────────────────────

/// Everything a test drives: the real hub over a scratch project, its
/// `TeamRuntime` with the one-step plan the dispatch belongs to, and the
/// external lead attached.
struct Rig {
    base: PathBuf,
    root: PathBuf,
    team: Arc<TeamRuntime>,
    server: Arc<HubServer>,
    plan_id: String,
    _tmp: tempfile::TempDir,
}

impl Rig {
    async fn start() -> Self {
        let tmp = tempfile::tempdir().expect("temp dir");
        let base = tmp.path().to_path_buf();
        let hub_config = base.join("hub-config");
        write_hub_config(&hub_config);
        // Routing off: the dispatch pins its tier, so no classifier runs.
        let routing = RoutingSettings::default();
        assert_eq!(routing.mode, RoutingMode::Off, "dispatch routing starts off");
        save_routing_settings(&hub_config, &routing).expect("save the routing settings");
        let root = base.join("project");
        init_repo(&root);

        // The engine over the project: its store is the hub store the worker's
        // routed writes land in. The database lives outside the checkout so the
        // lead tree stays clean for the merge.
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

        // The real hub behind the team's host directory.
        let scripted = ScriptedModel::new();
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
                    model: scripted,
                    memory: Some(store),
                    catalog,
                }),
            )
            .expect("start the hub"),
        );

        // The team over the same engine and project, with its plan board.
        let state = base.join("team-state");
        std::fs::create_dir_all(&state).expect("team state");
        let team = Arc::new(
            TeamRuntime::open(TeamRuntimeOptions {
                engine: Arc::new(engine),
                workspace_root: root.clone(),
                state_dir: state,
                config_dir: hub_config,
                classifier: None,
                routing_log_path: Some(base.join("routing.jsonl")),
                slots: Some(2),
                models: vec![LeadModel {
                    endpoint: "hubep".to_string(),
                    model: "scripted".to_string(),
                    spec: "hubep/scripted".to_string(),
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
            base.join("plans"),
            base.join("reviews"),
        ));
        team.attach_board(board.clone());
        let plan = board
            .create(PlanDraft {
                title: "Remote Claude agent worker end to end".to_string(),
                goal: "A remote Claude Code agent worker is placed on a linked host, \
                       followed through its session, checked on the node, reviewed and merged."
                    .to_string(),
                context: "The desktop app's SSH session is stood in for by a transcript.".to_string(),
                steps: vec![
                    "Add the worker's artifact at the project root (remote.txt, next to \
                     .coding/checks.toml) and commit it from remote.txt's directory"
                        .to_string(),
                ],
                ..PlanDraft::default()
            })
            .expect("create the plan");

        // One tier runs as a Claude Code agent on the host: `high` = "sonnet".
        let mut dto = mnemo::headless::TiersDto::default();
        for tier in dto.tiers.iter_mut() {
            tier.models = vec!["hubep/scripted".to_string()];
        }
        dto.tiers[3].claude_agent = Some("sonnet".to_string());
        team.set_catalog(
            vec![LeadModel {
                endpoint: "hubep".to_string(),
                model: "scripted".to_string(),
                spec: "hubep/scripted".to_string(),
                is_default: true,
                reasoning_efforts: Vec::new(),
                default_effort: None,
            }],
            dto,
            Vec::new(),
        );
        assert!(team.attach_external_lead(), "the external lead attaches");

        team.attach_hosts(Arc::new(TestHosts {
            server: server.clone(),
            address: server.local_addr().to_string(),
        }));

        Self {
            base,
            root,
            team,
            server,
            plan_id: plan.id.clone(),
            _tmp: tmp,
        }
    }

    /// `team_hosts list`: every host entry.
    async fn hosts(&self) -> Vec<Value> {
        let answer = self
            .team
            .handle_lead_host_tool("team_hosts", json!({"action": "list"}))
            .await;
        assert!(answer.success, "team_hosts list failed: {}", answer.output);
        let parsed: Value =
            serde_json::from_str(&answer.output).expect("team_hosts list is JSON");
        parsed["hosts"].as_array().cloned().unwrap_or_default()
    }

    /// One host entry by name.
    async fn host(&self, name: &str) -> Value {
        self.hosts()
            .await
            .into_iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("no worker host named '{name}'"))
    }

    /// `team_status` for one worker; `detail` asks for the full record.
    async fn status(&self, worker_id: &str, detail: bool) -> Value {
        let answer = self
            .team
            .handle_lead_host_tool(
                "team_status",
                json!({"worker_id": worker_id, "detail": detail}),
            )
            .await;
        assert!(answer.success, "team_status failed: {}", answer.output);
        let parsed: Value = serde_json::from_str(&answer.output).expect("team_status is JSON");
        parsed["result"][0].clone()
    }

    /// Pair the real node child, start it, and wait until the hub monitor
    /// qualified it with the project's checks. Returns it and the hub's name for
    /// it once its checks passed.
    async fn start_node(
        &self,
        claude_home: &Path,
        shim: Option<&Path>,
        claude_marker: &Path,
    ) -> (NodeProcess, NodeIdentity, String) {
        let node_config = self.base.join("node-config");
        let node_workspace = self.base.join("node-workspace");
        std::fs::create_dir_all(&node_workspace).expect("node workspace");
        let identity = pair_node(&self.server, self.server.local_addr(), &node_config).await;
        let node = NodeProcess::start(
            &node_config,
            &node_workspace,
            claude_home,
            shim,
            claude_marker,
        );
        assert!(
            self.server
                .wait_connected(&identity.node_id, Duration::from_secs(60))
                .await,
            "the node should connect; its output:\n{}",
            node.logs()
        );
        let team = self.team.clone();
        let host = wait_for(
            "the node to pass the project's checks",
            Duration::from_secs(120),
            move || {
                let team = team.clone();
                async move {
                    let answer = team
                        .handle_lead_host_tool("team_hosts", json!({"action": "list"}))
                        .await;
                    if !answer.success {
                        return None;
                    }
                    let parsed: Value = serde_json::from_str(&answer.output).ok()?;
                    parsed["hosts"].as_array()?.iter().find_map(|entry| {
                        (entry["checks"].as_str() == Some("passed"))
                            .then(|| entry["name"].as_str().unwrap_or_default().to_string())
                    })
                }
            },
        )
        .await;
        (node, identity, host)
    }
}

// ── the stand-in Claude Code session ──────────────────────────────────────

/// Claude Code's projects folder for `checkout`: every non-alphanumeric
/// character of the path becomes `-` (the node's own `slug`).
fn claude_slug(checkout: &Path) -> String {
    checkout
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The stand-in session's transcript: `projects/<slug(cwd)>/<session>.jsonl`
/// under the node's `CLAUDE_CONFIG_DIR`.
fn transcript_path(claude_home: &Path, checkout: &Path, session: &str) -> PathBuf {
    claude_home
        .join("projects")
        .join(claude_slug(checkout))
        .join(format!("{session}.jsonl"))
}

/// The transcript's path relative to the Claude config folder, for the
/// "nothing else was written there" assertion.
fn transcript_relative(checkout: &Path, session: &str) -> String {
    format!("projects/{}/{session}.jsonl", claude_slug(checkout))
}

/// Write a transcript from these records.
fn write_records(path: &Path, records: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("transcript folder");
    }
    let mut body = String::new();
    for record in records {
        body.push_str(&record.to_string());
        body.push('\n');
    }
    std::fs::write(path, body).expect("write the transcript");
}

/// Append one record to a transcript, as the session would.
fn append_record(path: &Path, record: &Value) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("open the transcript");
    writeln!(file, "{record}").expect("append to the transcript");
}

/// The first user record: it carries the checkout as its `cwd` and the launch
/// prompt (with the worker marker on its first line).
fn prompt_record(checkout: &Path, prompt: &str) -> Value {
    json!({
        "type": "user",
        "cwd": checkout.to_string_lossy(),
        "message": {"role": "user", "content": prompt},
    })
}

/// An assistant record that used one tool.
fn tool_record(checkout: &Path) -> Value {
    json!({
        "type": "assistant",
        "cwd": checkout.to_string_lossy(),
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "Bash",
                "input": {"command": "git status --short"},
            }],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 120, "output_tokens": 8},
        },
    })
}

/// The tool's result, as the session records it.
fn tool_result_record(checkout: &Path) -> Value {
    json!({
        "type": "user",
        "cwd": checkout.to_string_lossy(),
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_01",
                "content": "nothing to commit",
            }],
        },
    })
}

/// A plain user record: what the lead's `team_message` adds to the session.
fn user_record(checkout: &Path, text: &str) -> Value {
    json!({
        "type": "user",
        "cwd": checkout.to_string_lossy(),
        "message": {"role": "user", "content": text},
    })
}

/// The assistant record that ends a turn: text, no tool use, `end_turn`.
fn end_record(checkout: &Path, text: &str) -> Value {
    json!({
        "type": "assistant",
        "cwd": checkout.to_string_lossy(),
        "message": {
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 40, "output_tokens": 6},
        },
    })
}

// ── the tests ──────────────────────────────────────────────────────────────

/// One placement on the linked host, as the dispatch answered it.
struct Placement {
    checkout: PathBuf,
    transcript: PathBuf,
    sha: String,
}

/// Plan 5e64ccd5, phase 4 step 7: with a qualified host linked to an SSH
/// connection, an agent-tier dispatch places the worker there, answers with the
/// `claude_session` block the lead starts, follows the stand-in session through
/// its two turns, runs the project's checks on the node, reviews and merges.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_remote_agent_worker_session_check_review_merge() {
    within(Box::pin(remote_agent_scenario())).await;
}

async fn remote_agent_scenario() {
    // (1) the hub and its one-step plan.
    let rig = Rig::start().await;
    let claude_home = rig.base.join("claude-home");
    let shim = rig.base.join("claude-shim");
    let claude_marker = rig.base.join("claude-ran.marker");
    write_claude_shim(&shim, "MNEMO_CLAUDE_SHIM_MARKER");

    // (2) the real node: pair, run, wait for the project's checks, then link it
    // to the saved SSH connection (the user's standing approval).
    let (mut node, identity, host) = rig
        .start_node(&claude_home, Some(&shim), &claude_marker)
        .await;
    link_host(&rig, &host).await;

    // (3) the dispatch answers with the SSH session; (4) the stand-in session
    // writes the transcript and commits in the node checkout.
    let placement = dispatch_remote(&rig).await;

    // (5) the session ends its turn: the worker finishes with its last answer.
    first_turn(&rig, &placement).await;

    // (6) team_message continues the session; its next ended turn finishes the
    // worker again, counted once.
    continue_turn(&rig, &placement).await;

    // (7) checks on the node at the tip, then the review and the merge.
    let worker = check_and_merge(&rig, &host).await;

    // (8) the node checkout is released; (9) Mnemo never ran claude and wrote
    // nothing into the Claude config folder besides the test's transcript.
    assert_released(&rig, &identity, &worker).await;
    assert_mnemo_never_ran_claude(&node, &claude_home, &placement);
    node.kill().await;
}

/// Link the qualified host to the saved SSH connection and check the list.
async fn link_host(rig: &Rig, host: &str) {
    assert_eq!(host, NODE_NAME);
    let answer = rig
        .team
        .handle_lead_host_tool(
            "team_hosts",
            json!({"action": "link", "name": host, "ssh": SSH}),
        )
        .await;
    assert!(answer.success, "linking the host failed: {}", answer.output);
    let entry = rig.host(NODE_NAME).await;
    assert_eq!(entry["ssh"], SSH, "the host names its SSH connection: {entry}");
    assert_eq!(
        entry["claude_home"], false,
        "nothing has written a Claude config folder yet: {entry}"
    );
}

/// Dispatch the agent-tier worker without a host and stand its session in: the
/// transcript the SSH session would write, and the commit it makes.
async fn dispatch_remote(rig: &Rig) -> Placement {
    let answer = rig
        .team
        .handle_lead_host_tool_from(
            "team_dispatch",
            json!({
                "worker_id": WORKER_ID,
                "plan_id": rig.plan_id,
                "step": 1,
                "brief": "Add the worker's artifact at the project root and commit it",
                "tier": "high",
            }),
            MessageSender::ExternalLead,
        )
        .await;
    assert!(answer.success, "dispatch failed: {}", answer.output);
    let answer_json: Value = serde_json::from_str(
        answer
            .output
            .lines()
            .next()
            .expect("the dispatch answer's JSON line"),
    )
    .expect("the dispatch answer is JSON");
    let result = &answer_json["result"];
    let block = &result["claude_session"];
    assert!(
        !block.is_null(),
        "a remote agent dispatch carries claude_session: {answer_json}"
    );
    assert!(
        result["claude_agent"].is_null(),
        "a remote agent dispatch carries no local launch block: {answer_json}"
    );
    assert_eq!(block["target"], "ssh");
    assert_eq!(block["host"], SSH);
    assert_eq!(block["use_worktree"], false);
    let checkout = PathBuf::from(block["cwd"].as_str().expect("the checkout path"));
    assert!(
        checkout.is_dir(),
        "the session's checkout exists on the node: {}",
        checkout.display()
    );
    let prompt = block["prompt"].as_str().expect("the full prompt").to_string();
    let first_line = prompt.lines().next().unwrap_or_default();
    assert!(
        first_line.starts_with(&format!("Mnemo worker {WORKER_ID} on branch ")),
        "the prompt's first line carries the worker marker: {first_line}"
    );

    let worker = rig.team.worker(WORKER_ID).await.expect("the worker record");
    assert!(
        worker.launch_token.is_none(),
        "a remote session carries no launch token"
    );
    assert!(
        worker.claude_session.is_none(),
        "no session id until the lead reports it"
    );
    assert_eq!(worker.claude_agent.as_deref(), Some("sonnet"));
    let remote = worker.remote.as_ref().expect("the worker runs on the host");
    assert_eq!(remote.ssh.as_deref(), Some(SSH), "the record names the SSH connection");
    let status = rig.status(WORKER_ID, false).await;
    assert_eq!(status["host"], NODE_NAME, "team_status names the host: {status}");
    assert_eq!(
        status["agent_phase"], "awaiting_launch",
        "the worker waits for its session: {status}"
    );

    let transcript = transcript_path(&rig_claude_home(rig), &checkout, SESSION_ID);
    write_records(
        &transcript,
        &[
            prompt_record(&checkout, &prompt),
            tool_record(&checkout),
            tool_result_record(&checkout),
        ],
    );
    let sha = commit_file(&checkout, "remote.txt", "written on the node\n");
    let answer = rig
        .team
        .handle_lead_host_tool_from(
            "team_agent_started",
            json!({"worker_id": WORKER_ID, "agent_id": SESSION_ID}),
            MessageSender::ExternalLead,
        )
        .await;
    assert!(
        answer.success,
        "registering the session failed: {}",
        answer.output
    );
    rig.team.watch_agents_now().await;
    let status = rig.status(WORKER_ID, false).await;
    assert_eq!(
        status["agent_phase"], "running",
        "the watch follows the session: {status}"
    );
    assert!(
        status["agent"]["tool_calls"].as_u64().unwrap_or(0) >= 1,
        "the transcript's tool use is counted: {status}"
    );
    assert!(
        status["agent"]["tokens"].as_u64().unwrap_or(0) > 0,
        "the transcript's usage is counted: {status}"
    );

    Placement {
        checkout,
        transcript,
        sha,
    }
}

/// The Clade config folder of a rig: the one the node child was started with.
fn rig_claude_home(rig: &Rig) -> PathBuf {
    rig.base.join("claude-home")
}

/// End the first turn: Mnemo finishes the worker with its last answer and the
/// external lead gets exactly one finished notice quoting it.
async fn first_turn(rig: &Rig, placement: &Placement) {
    let _ = rig.team.drain_external_notices();
    append_record(
        &placement.transcript,
        &end_record(&placement.checkout, &format!("Done: {}", placement.sha)),
    );
    rig.team.watch_agents_now().await;
    let worker = rig.team.worker(WORKER_ID).await.expect("the worker record");
    assert_eq!(worker.status, WorkerStatus::Completed);
    assert_eq!(
        worker.report.as_deref(),
        Some(format!("Done: {}", placement.sha).as_str()),
        "the report is the session's last answer"
    );
    let notices = rig.team.drain_external_notices();
    let finished: Vec<&String> = notices
        .iter()
        .filter(|notice| notice.contains("finished"))
        .collect();
    assert_eq!(
        finished.len(),
        1,
        "one finished notice for the ended turn: {notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains(&format!("Done: {}", placement.sha))),
        "the notice quotes the session's answer: {notices:?}"
    );
}

/// Continue the finished worker in its session and end the second turn.
async fn continue_turn(rig: &Rig, placement: &Placement) {
    let answer = rig
        .team
        .handle_lead_host_tool_from(
            "team_message",
            json!({"worker_id": WORKER_ID, "text": "Also cover the edge case."}),
            MessageSender::ExternalLead,
        )
        .await;
    assert!(
        answer.success,
        "continuing the session failed: {}",
        answer.output
    );
    let data = answer.data.as_ref().expect("the message answer data");
    assert_eq!(
        data["continue_session"]["session"], SESSION_ID,
        "the lead continues the session on the host: {data}"
    );
    let worker = rig.team.worker(WORKER_ID).await.expect("the worker record");
    assert_eq!(worker.status, WorkerStatus::Working);
    assert_eq!(
        worker.turns, 1,
        "team_message opens the continuation; the count moves when a run ends"
    );

    let _ = rig.team.drain_external_notices();
    append_record(
        &placement.transcript,
        &user_record(&placement.checkout, "Also cover the edge case."),
    );
    append_record(
        &placement.transcript,
        &end_record(&placement.checkout, "Done: second turn"),
    );
    rig.team.watch_agents_now().await;
    let worker = rig.team.worker(WORKER_ID).await.expect("the worker record");
    assert_eq!(worker.status, WorkerStatus::Completed);
    assert_eq!(worker.report.as_deref(), Some("Done: second turn"));
    assert_eq!(
        worker.turns, 2,
        "the continuation run is counted once, not once per watcher pass"
    );
    let notices = rig.team.drain_external_notices();
    let finished: Vec<&String> = notices
        .iter()
        .filter(|notice| notice.contains("finished"))
        .collect();
    assert_eq!(
        finished.len(),
        1,
        "the second turn is counted once: {notices:?}"
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("Done: second turn")),
        "the notice quotes the second answer: {notices:?}"
    );
}

/// Run the project's checks on the node at the worker tip, review and merge.
async fn check_and_merge(rig: &Rig, host: &str) -> WorkerRecord {
    let answer = rig
        .team
        .handle_lead_host_tool("team_run_checks", json!({"worker_id": WORKER_ID}))
        .await;
    assert!(answer.success, "team_run_checks failed: {}", answer.output);
    wait_for(
        "git --version to pass at the worker tip on the node",
        Duration::from_secs(120),
        || async move {
            let status = rig.status(WORKER_ID, true).await;
            let checks = status["checks"].as_array()?;
            checks
                .iter()
                .any(|check| check["name"] == "git --version" && check["status"] == "passed")
                .then_some(())
        },
    )
    .await;
    let answer = rig
        .team
        .handle_lead_host_tool(
            "team_review",
            json!({
                "worker_id": WORKER_ID,
                "summary": "the stand-in remote session commits remote.txt on the node",
                "findings": [],
                "evidence": {
                    "unanchored_pass": true,
                    "intent": "the remote Claude agent worker placed on the linked host adds remote.txt in its node checkout and commits it",
                    "execution": "the hub prepares the node checkout, hands the lead the claude_session block, follows the stand-in transcript through claude.watch and finishes each run at the turn end",
                    "integration": "the same dispatch answer, check run, review and merge paths a local agent worker uses, with the node checkout and the hub shadow sync",
                    "test_quality": "the transcript is written by the test and never by Mnemo; the finished report, the one-notice-per-turn count and the second turn are asserted, and a fake claude first on the node's PATH must stay unrun",
                    "residual_risk": "a real SSH session is stood in for by the transcript and team_agent_started; the release of the node checkout is asserted through checkout.list",
                },
            }),
        )
        .await;
    assert!(answer.success, "team_review failed: {}", answer.output);
    let answer = rig
        .team
        .handle_lead_host_tool("team_merge", json!({"worker_id": WORKER_ID}))
        .await;
    assert!(answer.success, "team_merge failed: {}", answer.output);

    let worker = rig.team.worker(WORKER_ID).await.expect("the worker record");
    assert_eq!(worker.status, WorkerStatus::Merged);
    assert_eq!(
        read(&rig.root.join("remote.txt"))
            .map(|body| body.trim_end().to_string())
            .as_deref(),
        Some("written on the node"),
        "the lead branch holds the node's commit"
    );
    let entry = rig.host(host).await;
    assert_eq!(entry["busy"], 0, "no busy slot after the merge: {entry}");
    worker
}

/// The node checkout must be released: the hub monitor's reconcile pass
/// detaches it (no wt/agent-* branch) once nothing references it. Asserted, not
/// worked around: a released checkout is what the merge promises the next
/// dispatch.
async fn assert_released(rig: &Rig, identity: &NodeIdentity, worker: &WorkerRecord) {
    let remote = worker
        .remote
        .clone()
        .expect("the remote record survives the merge");
    let released = wait_until(Duration::from_secs(120), || {
        let server = rig.server.clone();
        let node_id = identity.node_id.clone();
        let project = remote.project.clone();
        let checkout_name = remote.checkout.clone();
        async move {
            let Ok(answer) = server
                .node(&node_id)
                .call(
                    CHECKOUT_LIST,
                    json!(CheckoutList { project }),
                    Duration::from_secs(30),
                )
                .await
            else {
                return false;
            };
            let Ok(listed) = serde_json::from_value::<CheckoutListAnswer>(answer) else {
                return false;
            };
            listed
                .checkouts
                .into_iter()
                .find(|info| info.checkout == checkout_name)
                .map(|info| info.branch.is_none())
                .unwrap_or(false)
        }
    })
    .await;
    assert!(
        released,
        "the node checkout should be released after the merge (detached, no wt/agent-* branch); \
         worktree={:?}, remote={:?}",
        worker.worktree, worker.remote
    );
}

/// Mnemo never ran claude (the fake first on the node's PATH stayed unrun) and
/// wrote nothing into the Claude config folder besides the test's transcript.
fn assert_mnemo_never_ran_claude(node: &NodeProcess, claude_home: &Path, placement: &Placement) {
    assert!(
        !node.claude_ran(),
        "something on the node ran the fake claude; node output:\n{}",
        node.logs()
    );
    assert!(
        !node.logs().to_lowercase().contains("claude"),
        "the node log must not show a claude launch; node output:\n{}",
        node.logs()
    );
    let files = files_under(claude_home);
    assert_eq!(
        files,
        vec![transcript_relative(&placement.checkout, SESSION_ID)],
        "Mnemo wrote nothing else into the Claude config folder"
    );
}

/// Plan 5e64ccd5, phase 4 step 7: with a qualified host but no linked SSH
/// connection (the user's current setup), an agent-tier dispatch stays local
/// with the existing `claude_agent` block and a local worktree.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e2e_unlinked_host_keeps_agent_local() {
    within(Box::pin(unlinked_scenario())).await;
}

async fn unlinked_scenario() {
    let rig = Rig::start().await;
    let claude_home = rig.base.join("claude-home");
    let claude_marker = rig.base.join("claude-ran.marker");
    let (mut node, _identity, host) = rig
        .start_node(&claude_home, None, &claude_marker)
        .await;
    assert_eq!(host, NODE_NAME, "the host is qualified but not linked");

    let answer = rig
        .team
        .handle_lead_host_tool_from(
            "team_dispatch",
            json!({
                "worker_id": WORKER_ID,
                "plan_id": rig.plan_id,
                "step": 1,
                "brief": "Add the worker's artifact at the project root and commit it",
                "tier": "high",
            }),
            MessageSender::ExternalLead,
        )
        .await;
    assert!(answer.success, "dispatch failed: {}", answer.output);
    let answer_json: Value = serde_json::from_str(
        answer
            .output
            .lines()
            .next()
            .expect("the dispatch answer's JSON line"),
    )
    .expect("the dispatch answer is JSON");
    let result = &answer_json["result"];
    let block = &result["claude_agent"];
    assert!(
        !block.is_null(),
        "without a linked SSH connection the worker stays local: {answer_json}"
    );
    assert!(
        result["claude_session"].is_null(),
        "no remote session block without a link: {answer_json}"
    );
    assert_eq!(block["isolation"], "worktree");
    assert_eq!(block["model"], "sonnet");
    let worktree = PathBuf::from(block["worktree"].as_str().expect("the worktree"));
    assert!(
        worktree.starts_with(rig.root.join(".worktrees")),
        "the local worktree sits under the project: {}",
        worktree.display()
    );
    assert!(worktree.is_dir(), "the worktree exists");
    let worker = rig.team.worker(WORKER_ID).await.expect("the worker record");
    assert!(
        worker.remote.is_none(),
        "a worker with no linked SSH connection never goes to a host"
    );
    assert!(worker.claude_session.is_none());
    node.kill().await;
}

