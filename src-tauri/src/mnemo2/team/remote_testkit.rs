// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Test support for the remote-worker steps: one scratch project, a real hub
//! and a paired in-process node, shared by the check-run and merge tests.
//!
//! Everything lives under the checkout's `target/` (the repository's
//! `.cargo/config.toml` points `TMP` there), so no test leaves anything in the
//! system temp folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mnemo::remote::catalog::ModelCatalog;
use mnemo::remote::hub::{HubOptions, HubServer, HubServices};
use mnemo::remote::node::{self, NodeConfig, NodeExtension};
use mnemo::remote::pairing::NodeRecord;
use serde_json::Value;

use tokio::sync::Mutex;

use crate::mnemo2::team::record::{RemoteCheckout, WorkerRecord};
use crate::mnemo2::team::remote::{HostDirectory, PairOffer, RemoteWorkers};
use crate::mnemo2::team::{TeamRuntime, TeamRuntimeOptions};

/// Run Git in `root`; panic on failure.
pub(super) fn run_git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A committed scratch project on the lead branch.
pub(super) fn init_repo(root: &Path) {
    std::fs::create_dir_all(root).expect("repo dir");
    run_git(root, &["init", "-q", "-b", "wt/mnemo"]);
    run_git(root, &["config", "user.name", "Mnemo2 Test"]);
    run_git(root, &["config", "user.email", "mnemo2@example.invalid"]);
    std::fs::write(root.join("README.md"), "seed\n").expect("seed file");
    std::fs::write(root.join(".gitignore"), ".worktrees/\n.coding/\n").expect("ignore file");
    run_git(root, &["add", "README.md", ".gitignore"]);
    run_git(root, &["commit", "-q", "-m", "seed"]);
}

/// The catalog a real hub would offer: the team tests' scripted endpoint.
pub(super) fn test_catalog(dir: &Path) -> ModelCatalog {
    std::fs::write(
        dir.join("config.toml"),
        "[general]\ndefault_provider = \"test\"\ndefault_model = \"scripted-a\"\n",
    )
    .expect("config.toml");
    std::fs::write(
        dir.join("endpoints.toml"),
        "[[endpoint]]\nname = \"test\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"scripted-a\"]\n",
    )
    .expect("endpoints.toml");
    let config = mnemo::config::Config::load(dir).expect("catalog config");
    ModelCatalog::from_config(&config, String::new())
}

/// A hub with no model clients and no memory: the repo, checkout and check
/// paths under test never need either.
pub(super) struct StubServices {
    pub(super) catalog: ModelCatalog,
}

impl HubServices for StubServices {
    fn model_client(
        &self,
        _model: &mnemo::config::ModelRef,
    ) -> Option<Arc<dyn mnemo::provider::LlmClient>> {
        None
    }

    fn memory(&self) -> Option<Arc<dyn mnemo::memory::MemoryStoreTrait>> {
        None
    }

    fn catalog(&self) -> ModelCatalog {
        self.catalog.clone()
    }
}

/// The team's view of one real hub, for tests.
pub(super) struct TestHosts {
    server: Arc<HubServer>,
    address: String,
}

#[async_trait::async_trait]
impl HostDirectory for TestHosts {
    fn server(&self) -> Option<Arc<HubServer>> {
        Some(self.server.clone())
    }

    fn address(&self) -> Option<String> {
        Some(self.address.clone())
    }

    async fn pair(&self, name: Option<&str>) -> Result<PairOffer, String> {
        let code = self.server.pairing().issue_code(name.unwrap_or("node"));
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
        self.server
            .revoke(&record.node_id)
            .map_err(|error| error.to_string())?;
        Ok(record)
    }
}

/// A hub, a paired in-process node, and the team's remote seam over one scratch
/// project.
pub(crate) struct Rig {
    pub(super) root: tempfile::TempDir,
    pub(super) state: tempfile::TempDir,
    /// The hub's state dir; held so the temp folder outlives the hub.
    pub(super) _hub_state: tempfile::TempDir,
    pub(super) config: tempfile::TempDir,
    /// The node's home; held so the temp folder outlives the node.
    pub(super) _node_home: tempfile::TempDir,
    pub(super) hub: Arc<HubServer>,
    pub(super) workers: Arc<RemoteWorkers>,
    pub(super) node_id: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Rig {
    pub(crate) async fn start() -> Self {
        Self::start_with(None).await
    }

    /// A rig whose node also serves `extension`: the Claude session watch the
    /// node binary carries, or none, exactly as [`Rig::start`].
    pub(crate) async fn start_with_extension(extension: Arc<dyn NodeExtension>) -> Self {
        Self::start_with(Some(extension)).await
    }

    async fn start_with(extension: Option<Arc<dyn NodeExtension>>) -> Self {
        let root = tempfile::tempdir().expect("project root");
        init_repo(root.path());
        let state = tempfile::tempdir().expect("team state");
        let hub_state = tempfile::tempdir().expect("hub state");
        let config = tempfile::tempdir().expect("hub config");
        let node_home = tempfile::tempdir().expect("node home");
        let catalog = test_catalog(config.path());
        let hub = Arc::new(
            HubServer::start(
                HubOptions {
                    bind: "127.0.0.1:0".parse().expect("loopback"),
                    state_dir: hub_state.path().to_path_buf(),
                },
                Arc::new(StubServices { catalog }),
            )
            .expect("start the hub"),
        );
        let address = hub.local_addr().to_string();
        let code = hub.pairing().issue_code("node-a");
        let identity = node::pair(node_home.path(), &address, &code.code, "node-a")
            .await
            .expect("pair the node");
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let workspace = node_home.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("node workspace");
        let cfg = NodeConfig {
            home: node_home.path().to_path_buf(),
            workspace,
            outage_budget: mnemo::remote::HUB_OUTAGE_BUDGET,
            index_codegraph: false,
            slots: 2,
            extension,
        };
        tokio::spawn(node::run(cfg, async move {
            let _ = shutdown_rx.await;
        }));
        assert!(
            hub.wait_connected(&identity.node_id, Duration::from_secs(30))
                .await,
            "the paired node connects"
        );
        let records = Arc::new(Mutex::new(Vec::new()));
        let live = Arc::new(Mutex::new(HashMap::new()));
        let workers = Arc::new(RemoteWorkers::new(
            root.path().to_path_buf(),
            state.path().to_path_buf(),
            records.clone(),
            live,
        ));
        workers.attach(Arc::new(TestHosts {
            server: hub.clone(),
            address,
        }));
        Self {
            root,
            state,
            _hub_state: hub_state,
            config,
            _node_home: node_home,
            hub,
            workers,
            node_id: identity.node_id,
            shutdown: Some(shutdown),
        }
    }

    /// One call into the rig's node, for a test of an extension the rig
    /// started with.
    pub(crate) async fn call_node(
        &self,
        remote: &RemoteCheckout,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.workers.call_node(remote, method, params, timeout).await
    }

    pub(super) async fn base(&self) -> String {
        crate::mnemo2::team::remote::git_value(self.root.path(), &["rev-parse", "HEAD"])
            .await
            .expect("base commit")
    }

    pub(super) fn hosts(&self) -> Arc<dyn HostDirectory> {
        Arc::new(TestHosts {
            server: self.hub.clone(),
            address: self.hub.local_addr().to_string(),
        })
    }

    /// A team runtime over the rig's project, attached to its hub. The host is
    /// pre-measured so no qualification run starts in a test.
    pub(crate) async fn team(&self) -> Arc<TeamRuntime> {
        let team = Arc::new(
            open_test_team(self.root.path(), self.state.path(), self.config.path()).await,
        );
        let commands = crate::mnemo2::team::spawn::load_check_commands(self.root.path());
        team.remote.note_check(
            &self.node_id,
            &crate::mnemo2::team::remote::checks_hash(&commands),
            true,
            None,
        );
        team.attach_hosts(self.hosts());
        team
    }

    /// Put one worker on the node: its branch and checkout there, a commit when
    /// `commit` is set, and the hub shadow worktree that stands in for it
    /// locally. Returns the record (already pointing at the node and the
    /// shadow), the node checkout and the node tip.
    pub(crate) async fn worker_on_node(
        &self,
        id: &str,
        commit: bool,
    ) -> (WorkerRecord, RemoteCheckout, String) {
        let base = self.base().await;
        let branch = format!("wt/agent-{id}");
        let remote = self
            .workers
            .prepare_checkout(&self.node_id, &branch, &base, false)
            .await
            .expect("prepare the node checkout");
        let node_checkout = PathBuf::from(&remote.path);
        let tip = if commit {
            std::fs::write(node_checkout.join("work.txt"), "remote work\n").expect("write");
            run_git(&node_checkout, &["config", "user.name", "Mnemo2 Test"]);
            run_git(&node_checkout, &["config", "user.email", "mnemo2@example.invalid"]);
            run_git(&node_checkout, &["add", "-A"]);
            run_git(&node_checkout, &["commit", "-q", "-m", "remote work"]);
            crate::mnemo2::team::remote::git_value(&node_checkout, &["rev-parse", "HEAD"])
                .await
                .expect("the node tip")
        } else {
            base.clone()
        };
        let shadow = self.root.path().join(".worktrees").join(format!("agent-{id}"));
        run_git(
            self.root.path(),
            &[
                "worktree",
                "add",
                "-q",
                shadow.to_str().expect("utf-8 path"),
                "-b",
                &branch,
                &base,
            ],
        );
        let mut record = crate::mnemo2::team::tests::worker(self.root.path(), id);
        record.branch = branch;
        record.worktree = shadow;
        record.base_commit = Some(base);
        record.remote = Some(remote.clone());
        (record, remote, tip)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// A team runtime over one scratch repo, for the remote check-run and merge
/// tests.
pub(super) async fn open_test_team(root: &Path, state: &Path, config: &Path) -> TeamRuntime {
    let engine = mnemo::headless::Engine::start(mnemo::headless::EngineOptions {
        workspace: mnemo::workspace::Workspace::new(root),
        config_dir: Some(config.to_path_buf()),
        permissions: mnemo::PermissionPolicy::OverridePermissions,
        memory: mnemo::headless::MemoryBackend::Disabled,
        index_codegraph: false,
        require_git: true,
        auto_install_embedder: false,
    })
    .await
    .expect("start the test engine");
    TeamRuntime::open(TeamRuntimeOptions {
        engine: Arc::new(engine),
        workspace_root: root.to_path_buf(),
        state_dir: state.to_path_buf(),
        config_dir: config.to_path_buf(),
        classifier: None,
        routing_log_path: Some(state.join("dispatch_routing.jsonl")),
        slots: Some(2),
        models: Vec::new(),
        pricing: Vec::new(),
    })
    .await
    .expect("open the test team")
}
