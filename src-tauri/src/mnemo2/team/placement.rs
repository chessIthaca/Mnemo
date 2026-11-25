// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Placement: which host a dispatched worker runs on (plan 9a2f4b67, phase 3
//! step 5).
//!
//! Placement runs after the tier and model are settled and before anything is
//! provisioned. It answers one question — this computer or one paired worker
//! node — and never touches a worker's routing. A node wins automatically only
//! when it is connected, has a free slot, can run this project's checks on its
//! operating system, and already passed those checks on it. Otherwise the
//! worker runs locally exactly as it did before remote workers existed.
//!
//! A lead may pin a host: `"local"` keeps the worker here, a name (or node id)
//! sends it there and fails loudly when that host cannot take it. A Claude
//! Code agent worker always stays local in this phase.

use serde_json::Value;

use mnemo::remote::pairing::NodeRecord;

use super::remote::{checks_hash, requires_windows};
use super::remote::Qualification;
use super::spawn;
use super::TeamRuntime;

/// The git version a node needs for the bundles and worktrees of the worker
/// path.
const MIN_GIT: (u64, u64) = (2, 30);

/// Where a dispatched worker will run.
#[derive(Debug)]
pub(super) enum Placement {
    /// This computer, exactly as before remote workers existed.
    Local,
    /// One checkout on one paired worker node.
    Node(NodeChoice),
}

/// The node a worker was placed on.
#[derive(Debug)]
pub(super) struct NodeChoice {
    /// The node's 32-hex id, the stable handle the record stores.
    pub(super) node_id: String,
    /// The node's operator-visible name.
    pub(super) name: String,
    /// `windows` or `macos`, as the node reported it.
    pub(super) os: String,
    /// True when the lead named this host: a pinned host never falls back to
    /// the local computer.
    pub(super) pinned: bool,
}

/// What placement needs to know about one dispatch.
pub(super) struct PlacementRequest<'a> {
    /// The lead's `host` argument, when it passed one.
    pub(super) pinned: Option<&'a str>,
    /// A Claude Code agent worker always runs on this computer (phase 3).
    pub(super) claude_agent: bool,
}

impl TeamRuntime {
    /// Choose the host for one new dispatch.
    ///
    /// Order: a Claude Code agent stays local; an explicit host must be usable
    /// or the dispatch fails; with no pin the qualified idle node with the most
    /// free slots wins, ties by name, and this computer is the fallback.
    pub(super) async fn place_worker(
        &self,
        request: PlacementRequest<'_>,
    ) -> Result<Placement, String> {
        if request.claude_agent {
            return match request.pinned {
                Some(host) if host != "local" => Err(
                    "a Claude Code agent worker runs on this computer; omit host or pass host \"local\""
                        .to_string(),
                ),
                _ => Ok(Placement::Local),
            };
        }
        let commands = spawn::load_check_commands(&self.workspace_root);
        let wants_windows = requires_windows(commands.as_slice());
        let checks_hash = checks_hash(commands.as_slice());
        let Some(server) = self.remote.server() else {
            return match request.pinned {
                None | Some("local") => Ok(Placement::Local),
                Some(host) => Err(not_paired(host)),
            };
        };
        if let Some(host) = request.pinned {
            if host == "local" {
                return Ok(Placement::Local);
            }
            let record = self.remote.host(host).map_err(|_| not_paired(host))?;
            if !server.is_connected(&record.node_id) {
                return Err(format!("host '{host}' is not connected"));
            }
            self.pinned_node_usable(&record, host, wants_windows).await?;
            return Ok(Placement::Node(choice(&record, true)));
        }
        let mut best: Option<(usize, NodeRecord)> = None;
        // Without a pin a broken node registry is never a dispatch failure:
        // the worker runs here, as before remote workers existed.
        let nodes = match server.nodes().list() {
            Ok(nodes) => nodes,
            Err(error) => {
                eprintln!("team: could not read the paired hosts: {error}; placing locally");
                return Ok(Placement::Local);
            }
        };
        for record in nodes {
            if !server.is_connected(&record.node_id) {
                continue;
            }
            if !os_can_run(&record.os, wants_windows) {
                continue;
            }
            let slots = self.remote.slots_of(&record).await;
            let busy = self.remote.busy_on(&record.node_id).await;
            if busy >= slots {
                continue;
            }
            match self.remote.qualification(&record.node_id, &checks_hash) {
                Qualification::Passed => {}
                Qualification::Pending => {
                    self.remote.qualify(&record.node_id);
                    continue;
                }
                Qualification::Failed => continue,
            }
            if !self.node_git_current(&record).await {
                continue;
            }
            let free = slots - busy;
            let better = match &best {
                None => true,
                Some((current, chosen)) => {
                    free > *current || (free == *current && record.name < chosen.name)
                }
            };
            if better {
                best = Some((free, record));
            }
        }
        Ok(match best {
            Some((_, record)) => Placement::Node(choice(&record, false)),
            None => Placement::Local,
        })
    }

    /// Whether a pinned host can take a worker: its OS can run the checks, its
    /// git is new enough, and it has a free slot.
    async fn pinned_node_usable(
        &self,
        record: &NodeRecord,
        host: &str,
        wants_windows: bool,
    ) -> Result<(), String> {
        if !os_can_run(&record.os, wants_windows) {
            return Err(format!(
                "host '{host}' runs {}; this project's checks need a Windows host",
                record.os
            ));
        }
        if !self.node_git_current(record).await {
            return Err(format!(
                "host '{host}' has git {}; Mnemo needs git 2.30 or newer",
                self.node_git_text(record).await
            ));
        }
        let slots = self.remote.slots_of(record).await;
        if self.remote.busy_on(&record.node_id).await >= slots {
            return Err(format!("all {slots} slots on host '{host}' are busy"));
        }
        Ok(())
    }

    /// The node's `git --version` line, or the wording for a node that did not
    /// say.
    async fn node_git_text(&self, record: &NodeRecord) -> String {
        let Some(server) = self.remote.server() else {
            return "an unknown version".to_string();
        };
        match server.node(&record.node_id).status().await {
            Ok(status) => status
                .get("git")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "an unknown version".to_string()),
            Err(_) => "an unknown version".to_string(),
        }
    }

    /// Whether the node's git is [`MIN_GIT`] or newer.
    async fn node_git_current(&self, record: &NodeRecord) -> bool {
        git_at_least(&self.node_git_text(record).await, MIN_GIT)
    }
}

/// The refusal a pinned host gets when nothing is paired under that name.
fn not_paired(host: &str) -> String {
    format!("host '{host}' is not paired; see team_hosts")
}

/// The placement choice for one node record.
fn choice(record: &NodeRecord, pinned: bool) -> NodeChoice {
    NodeChoice {
        node_id: record.node_id.clone(),
        name: record.name.clone(),
        os: record.os.clone(),
        pinned,
    }
}

/// Whether a host on `os` can run this project's checks.
///
/// Only a project whose checks need a Windows shell constrains the host; every
/// other project's checks run on either supported OS.
fn os_can_run(os: &str, wants_windows: bool) -> bool {
    !wants_windows || os == "windows"
}

/// Whether a `git --version` line names [`MIN_GIT`] or newer.
fn git_at_least(text: &str, wanted: (u64, u64)) -> bool {
    let mut numbers = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse::<u64>().ok());
    let (Some(major), Some(minor)) = (numbers.next(), numbers.next()) else {
        return false;
    };
    (major, minor) >= wanted
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use mnemo::remote::catalog::ModelCatalog;
    use mnemo::remote::hub::{HubOptions, HubServer, HubServices};
    use mnemo::remote::node::{self, NodeConfig};
    use mnemo::remote::pairing::NodeRecord;

    use crate::mnemo2::team::{HostDirectory, PairOffer, TeamRuntime, TeamRuntimeOptions};

    use super::{git_at_least, os_can_run, Placement, PlacementRequest};

    /// A hub with no model clients and no memory: placement never calls either.
    struct StubServices {
        catalog: ModelCatalog,
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

    /// The team's view of one real hub.
    struct TestHosts {
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

    fn run_git(root: &Path, args: &[&str]) {
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

    fn init_repo(root: &Path) {
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
    fn test_catalog(dir: &Path) -> ModelCatalog {
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

    /// A team runtime over one scratch repo.
    async fn open_test_team(root: &Path, state: &Path, config: &Path) -> TeamRuntime {
        let engine = mnemo::headless::Engine::start(mnemo::headless::EngineOptions {
            workspace: mnemo::workspace::Workspace::new(root),
            config_dir: Some(config.to_path_buf()),
            permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
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

    /// A hub, a paired in-process node, and the team's placement seam over one
    /// scratch project.
    struct Rig {
        root: tempfile::TempDir,
        _state: tempfile::TempDir,
        _hub_state: tempfile::TempDir,
        _config: tempfile::TempDir,
        _node_home: tempfile::TempDir,
        hub: Arc<HubServer>,
        address: String,
        node_id: String,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    }

    impl Rig {
        async fn start() -> Self {
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
                extension: None,
            };
            tokio::spawn(node::run(cfg, async move {
                let _ = shutdown_rx.await;
            }));
            assert!(
                hub.wait_connected(&identity.node_id, Duration::from_secs(30))
                    .await,
                "the paired node connects"
            );
            Self {
                root,
                _state: state,
                _hub_state: hub_state,
                _config: config,
                _node_home: node_home,
                hub,
                address,
                node_id: identity.node_id,
                shutdown: Some(shutdown),
            }
        }

        fn hosts(&self) -> Arc<dyn HostDirectory> {
            Arc::new(TestHosts {
                server: self.hub.clone(),
                address: self.address.clone(),
            })
        }

        /// A team runtime attached to the rig's hub with the host already
        /// measured as passing this project's checks.
        async fn team_measured(&self) -> Arc<TeamRuntime> {
            let team = Arc::new(
                open_test_team(
                    self.root.path(),
                    self._state.path(),
                    self._config.path(),
                )
                .await,
            );
            let commands = crate::mnemo2::team::spawn::load_check_commands(self.root.path());
            team.remote.note_check(
                &self.node_id,
                &crate::mnemo2::team::remote::checks_hash(commands.as_slice()),
                true,
                None,
            );
            team.attach_hosts(self.hosts());
            team
        }

        /// A team runtime attached to the rig's hub with the host NOT measured:
        /// the first placement starts a qualification.
        async fn team_unmeasured(&self) -> Arc<TeamRuntime> {
            // A project with no check commands has nothing to measure.
            std::fs::create_dir_all(self.root.path().join(".coding")).expect("coding dir");
            std::fs::write(
                self.root.path().join(".coding/checks.toml"),
                "commands = [\"git status\"]\n",
            )
            .expect("checks.toml");
            let team = Arc::new(
                open_test_team(
                    self.root.path(),
                    self._state.path(),
                    self._config.path(),
                )
                .await,
            );
            team.attach_hosts(self.hosts());
            team
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn placement_without_hosts_is_local() {
        let root = tempfile::tempdir().expect("root");
        init_repo(root.path());
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        let _ = test_catalog(config.path());
        let team = open_test_team(root.path(), state.path(), config.path()).await;

        let placement = team
            .place_worker(PlacementRequest {
                pinned: None,
                claude_agent: false,
            })
            .await
            .expect("placement");
        assert!(
            matches!(placement, Placement::Local),
            "no pinned host and no node: the worker stays here"
        );
        let pinned = team
            .place_worker(PlacementRequest {
                pinned: Some("node-a"),
                claude_agent: false,
            })
            .await
            .expect_err("an unpaired host is refused");
        assert_eq!(pinned, "host 'node-a' is not paired; see team_hosts");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn placement_pinned_local_stays_local() {
        let rig = Rig::start().await;
        let team = rig.team_measured().await;
        let placement = team
            .place_worker(PlacementRequest {
                pinned: Some("local"),
                claude_agent: false,
            })
            .await
            .expect("placement");
        assert!(
            matches!(placement, Placement::Local),
            "host \"local\" wins over an idle qualified node"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn placement_pinned_claude_agent_is_refused() {
        let root = tempfile::tempdir().expect("root");
        init_repo(root.path());
        let state = tempfile::tempdir().expect("state");
        let config = tempfile::tempdir().expect("config");
        let _ = test_catalog(config.path());
        let team = open_test_team(root.path(), state.path(), config.path()).await;

        let refused = team
            .place_worker(PlacementRequest {
                pinned: Some("node-a"),
                claude_agent: true,
            })
            .await
            .expect_err("a Claude agent worker cannot be pinned to a node");
        assert_eq!(
            refused,
            "a Claude Code agent worker runs on this computer; omit host or pass host \"local\""
        );
        for pinned in [None, Some("local")] {
            assert!(matches!(
                team.place_worker(PlacementRequest {
                    pinned,
                    claude_agent: true,
                })
                .await
                .expect("placement"),
                Placement::Local
            ));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn placement_prefers_a_qualified_idle_node() {
        let rig = Rig::start().await;
        let team = rig.team_measured().await;
        match team
            .place_worker(PlacementRequest {
                pinned: None,
                claude_agent: false,
            })
            .await
            .expect("placement")
        {
            Placement::Node(choice) => {
                assert_eq!(choice.name, "node-a");
                assert_eq!(choice.node_id, rig.node_id);
                assert!(!choice.pinned);
            }
            Placement::Local => panic!("the qualified idle node should take the worker"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn placement_skips_pending_and_starts_qualification() {
        let rig = Rig::start().await;
        let team = rig.team_unmeasured().await;
        assert!(
            matches!(
                team.place_worker(PlacementRequest {
                    pinned: None,
                    claude_agent: false,
                })
                .await
                .expect("placement"),
                Placement::Local
            ),
            "an unmeasured node takes no worker"
        );
        // The qualification clones and checks on the node; the whole suite
        // runs in parallel with it, so the wait is generous.
        let deadline = Instant::now() + Duration::from_secs(120);
        while team.remote.host_states().is_empty() {
            assert!(
                Instant::now() < deadline,
                "placement started a qualification on the unmeasured node"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn placement_pinned_node_takes_the_worker() {
        let rig = Rig::start().await;
        let team = rig.team_unmeasured().await;
        match team
            .place_worker(PlacementRequest {
                pinned: Some("node-a"),
                claude_agent: false,
            })
            .await
            .expect("a pinned host needs no qualification")
        {
            Placement::Node(choice) => {
                assert!(choice.pinned);
                assert_eq!(choice.node_id, rig.node_id);
            }
            Placement::Local => panic!("a pinned node takes the worker"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn team_hosts_pairs_lists_and_revokes() {
        let rig = Rig::start().await;
        let team = rig.team_measured().await;

        let listed = team
            .handle_lead_host_tool("team_hosts", serde_json::json!({"action": "list"}))
            .await;
        assert!(listed.success, "{}", listed.output);
        let listed: serde_json::Value = serde_json::from_str(&listed.output).expect("list JSON");
        assert_eq!(listed["action"], "team_hosts");
        assert_eq!(listed["hub"]["listening"], true);
        let hosts = listed["hosts"].as_array().expect("hosts");
        assert_eq!(hosts.len(), 1, "{listed}");
        assert_eq!(hosts[0]["name"], "node-a");
        assert_eq!(hosts[0]["node_id"], rig.node_id.as_str());
        assert_eq!(hosts[0]["connected"], true);
        assert_eq!(hosts[0]["checks"], "passed");
        assert_eq!(hosts[0]["slots"], 2);
        assert_eq!(hosts[0]["busy"], 0);

        let paired = team
            .handle_lead_host_tool(
                "team_hosts",
                serde_json::json!({"action": "pair", "name": "studio"}),
            )
            .await;
        assert!(paired.success, "{}", paired.output);
        let paired: serde_json::Value = serde_json::from_str(&paired.output).expect("pair JSON");
        let code = paired["code"].as_str().expect("a pairing code");
        assert_eq!(
            paired["commands"][0],
            format!(
                "mnemo2-node pair --hub {} --code {code} --name studio",
                rig.address
            )
        );
        assert_eq!(
            paired["commands"][1],
            "mnemo2-node run --workspace <an empty folder for Mnemo's work>"
        );
        let expires = paired["expires_in_s"].as_u64().expect("expiry");
        assert!((590..=600).contains(&expires), "{paired}");

        // A host name travels into a command line the user pastes: nothing a
        // shell treats specially gets through, for pair or revoke.
        let too_long = "a".repeat(65);
        for bad in ["bad name", "-rf", "x;rm -rf", "$(whoami)", "a\"b", too_long.as_str()] {
            for args in [
                serde_json::json!({"action": "pair", "name": bad}),
                serde_json::json!({"action": "revoke", "name": bad}),
            ] {
                let refused = team.handle_lead_host_tool("team_hosts", args).await;
                assert!(!refused.success, "{bad}: {}", refused.output);
                assert_eq!(
                    refused.output,
                    "a host name may use letters, digits, '.', '_' and '-' (at most 64)"
                );
            }
        }

        let missing = team
            .handle_lead_host_tool(
                "team_hosts",
                serde_json::json!({"action": "revoke", "name": "nobody"}),
            )
            .await;
        assert!(!missing.success);
        let nameless = team
            .handle_lead_host_tool("team_hosts", serde_json::json!({"action": "revoke"}))
            .await;
        assert!(!nameless.success);

        let revoked = team
            .handle_lead_host_tool(
                "team_hosts",
                serde_json::json!({"action": "revoke", "name": "node-a"}),
            )
            .await;
        assert!(revoked.success, "{}", revoked.output);
        let revoked: serde_json::Value =
            serde_json::from_str(&revoked.output).expect("revoke JSON");
        assert_eq!(revoked["revoked"]["name"], "node-a");
        assert_eq!(revoked["failed_workers"], serde_json::json!([]));
        assert!(rig.hub.nodes().list().expect("nodes").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reserved_checkout_is_visible_until_released() {
        let rig = Rig::start().await;
        let team = rig.team_measured().await;
        assert!(!team.remote.is_reserved(&rig.node_id, "wt/agent-aaaaaaaa"));
        team.remote.reserve(&rig.node_id, "wt/agent-aaaaaaaa");
        assert!(team.remote.is_reserved(&rig.node_id, "wt/agent-aaaaaaaa"));
        assert!(!team.remote.is_reserved(&rig.node_id, "wt/agent-bbbbbbbb"));
        team.remote.release_reservation(&rig.node_id, "wt/agent-aaaaaaaa");
        assert!(!team.remote.is_reserved(&rig.node_id, "wt/agent-aaaaaaaa"));
    }

    #[test]
    fn placement_windows_checks_skip_mac() {
        assert!(super::requires_windows(&[
            "powershell -File scripts/check.ps1".to_string()
        ]));
        assert!(!super::requires_windows(&["cargo test".to_string()]));
        assert!(os_can_run("windows", true));
        assert!(!os_can_run("macos", true));
        assert!(os_can_run("macos", false));
        assert!(os_can_run("windows", false));
    }

    #[test]
    fn placement_git_version_floor() {
        assert!(git_at_least("git version 2.43.0.windows.1", (2, 30)));
        assert!(git_at_least("git version 2.30.0", (2, 30)));
        assert!(!git_at_least("git version 2.29.9", (2, 30)));
        assert!(!git_at_least("an unknown version", (2, 30)));
    }
}
