// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The app's one worker hub (plan 9a2f4b67, phase 3 step 7).
//!
//! A paired worker node reaches this machine through one [`HubServer`] per app
//! instance. The server serves the *open* project: its model clients, its
//! memory store and its model catalog — never its API keys, because the
//! catalog is the sanitised shape nodes mirror for model routing.
//!
//! Remoting stays off until the operator has paired a first node: a machine
//! that never pairs one binds no port and writes no lock file, so a test or a
//! fresh install never opens a listener.
//!
//! Hub state lives in `<config_dir>/hub` behind an exclusive `hub.lock`, one
//! hub per profile: a second Mnemo2 window reports which process already
//! serves the paired nodes instead of binding a second listener
//! ([`AppHub::ensure_started`]).

use std::collections::HashMap;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, RwLock as StdRwLock, Weak};

use mnemo::config::{Config, ModelRef};
use mnemo::headless::Engine;
use mnemo::memory::MemoryStoreTrait;
use mnemo::provider::LlmClient;
use mnemo::remote::catalog::ModelCatalog;
use mnemo::remote::hub::{HubOptions, HubServer, HubServices};
use mnemo::remote::pairing::{NodeRecord, NodeRegistry};

use crate::mnemo2::team::remote::HostDirectory;
use crate::mnemo2::team::{PairOffer, TeamRuntime};

/// The port a hub tries first; when another program holds it the hub falls
/// back to an ephemeral port and persists the one it actually bound.
pub const DEFAULT_HUB_PORT: u16 = 47310;

/// The file under `<config_dir>/hub` naming the bound port.
const HUB_FILE: &str = "hub.json";

/// The exclusive claim on this profile's hub, next to [`HUB_FILE`]. It holds
/// the serving process's pid; a pid that is no longer alive is replaced.
const LOCK_FILE: &str = "hub.lock";

/// The hub of one config dir. The map is process-wide so every project and
/// every window of this process reaches the same listener; the key is the
/// canonical config dir, which keeps tests isolated in their own temp dirs.
pub fn app_hub(config_dir: &Path) -> Arc<AppHub> {
    static HUBS: OnceLock<StdMutex<HashMap<PathBuf, Arc<AppHub>>>> = OnceLock::new();
    let hubs = HUBS.get_or_init(|| StdMutex::new(HashMap::new()));
    let key = std::fs::canonicalize(config_dir).unwrap_or_else(|_| config_dir.to_path_buf());
    let mut guard = hubs.lock().unwrap_or_else(|e| e.into_inner());
    Arc::clone(guard.entry(key).or_insert_with_key(|key| Arc::new(AppHub::new(key))))
}

/// The wire shape of `<state_dir>/hub.json`.
#[derive(serde::Serialize, serde::Deserialize)]
struct HubFile {
    /// The port the listener bound, reused on the next start.
    port: u16,
}

/// The app's one hub listener and the services it serves.
pub struct AppHub {
    /// `<config_dir>/hub`: the paired-node registry, the bound port and the
    /// lock.
    state_dir: PathBuf,
    /// The running listener; `None` until the first node is paired (or the
    /// lead asks to pair one).
    server: StdMutex<Option<Arc<HubServer>>>,
    /// What the hub answers for the open project.
    services: Arc<AppHubServices>,
    /// The held `hub.lock` file; dropping the handle frees the claim.
    lock: StdMutex<Option<std::fs::File>>,
}

impl AppHub {
    /// A hub over one config dir. Prefer [`app_hub`], which shares the instance
    /// across the process; this constructor is for a caller that must own a
    /// distinct instance (the lock tests).
    fn new(config_dir: &Path) -> Self {
        Self {
            state_dir: config_dir.join("hub"),
            server: StdMutex::new(None),
            services: Arc::new(AppHubServices::new()),
            lock: StdMutex::new(None),
        }
    }

    /// The running listener, or `None` while remoting is off.
    pub fn server(&self) -> Option<Arc<HubServer>> {
        self.server
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// The `host:port` a node dials in its `mnemo2-node pair` command.
    ///
    /// The address is this machine's LAN IPv4 — found by letting the OS pick
    /// the route to an unroutable documentation address, which sends no
    /// packet — and the port the listener bound.
    pub fn address(&self) -> Option<String> {
        let port = self.server()?.local_addr().port();
        let ip = lan_ipv4().unwrap_or(Ipv4Addr::LOCALHOST);
        Some(format!("{ip}:{port}"))
    }

    /// Start the listener if it is not running yet and return it.
    ///
    /// Takes the profile's exclusive `hub.lock` first: a live holder means
    /// another Mnemo2 window serves this profile's nodes and the error names
    /// its process. The port comes from `hub.json` (default
    /// [`DEFAULT_HUB_PORT`]); when another program holds it the hub binds an
    /// ephemeral port and persists that.
    ///
    /// Needs a Tokio runtime — call it from async code, not from a blocking
    /// thread.
    pub fn ensure_started(&self) -> Result<Arc<HubServer>, String> {
        let mut slot = self.server.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(running) = slot.as_ref() {
            return Ok(Arc::clone(running));
        }
        let lock = self.acquire_lock()?;
        let started = match self.start_listener() {
            Ok(started) => started,
            Err(error) => {
                // A hub that never started must not leave its claim behind:
                // the pid in the file names this live process, which would
                // lock out every later attempt.
                drop(lock);
                let _ = std::fs::remove_file(self.state_dir.join(LOCK_FILE));
                return Err(error);
            }
        };
        *self.lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(lock);
        let started = Arc::new(started);
        *slot = Some(Arc::clone(&started));
        Ok(started)
    }

    /// Start the listener only when this profile has at least one paired node.
    ///
    /// This is what keeps remoting off on an unpaired machine: no listener, no
    /// `hub` directory and no lock file before the first pairing.
    pub fn start_if_paired(&self) -> Result<(), String> {
        if NodeRegistry::new(self.state_dir.clone())
            .list()?
            .is_empty()
        {
            return Ok(());
        }
        self.ensure_started().map(|_| ())
    }

    /// Point the hub's services at `engine` and `team`, and let the team reach
    /// the listener through this hub.
    ///
    /// The binding is set even while no listener runs: [`TeamRuntime`] picks
    /// the server up once pairing starts it.
    pub fn bind_project(
        self: &Arc<Self>,
        engine: &Arc<Engine>,
        team: &Arc<TeamRuntime>,
        config_dir: &Path,
    ) {
        self.services
            .bind(Arc::downgrade(engine), config_dir.to_path_buf());
        if let Err(error) = self.start_if_paired() {
            eprintln!("mnemo2: the worker hub did not start: {error}");
        }
        let hosts: Arc<dyn HostDirectory> = self.clone();
        team.attach_hosts(hosts);
    }

    /// Stop answering for the project behind `engine`. The listener stays up:
    /// it belongs to the app instance, not to the project that happened to
    /// open first.
    ///
    /// Only the runtime whose engine holds the binding may clear it: an older
    /// runtime finishing its shutdown must not unbind the project that
    /// replaced it.
    pub fn unbind_project(&self, engine: &Arc<Engine>) {
        self.services.unbind(engine);
    }

    /// Take the profile's exclusive hub claim.
    fn acquire_lock(&self) -> Result<std::fs::File, String> {
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|error| format!("could not create {}: {error}", self.state_dir.display()))?;
        let path = self.state_dir.join(LOCK_FILE);
        match create_lock_file(&path) {
            Ok(file) => Ok(file),
            Err(LockError::Held(pid)) => Err(match pid {
                Some(pid) => format!(
                    "worker hosts are served by another Mnemo2 window (process {pid}); pair and manage hosts there"
                ),
                None => "worker hosts are served by another Mnemo2 window; pair and manage hosts there"
                    .to_string(),
            }),
            Err(LockError::Failed(error)) => {
                Err(format!("could not claim {}: {error}", path.display()))
            }
        }
    }

    /// Bind on the persisted port (or an ephemeral one) and persist the port
    /// actually bound.
    fn start_listener(&self) -> Result<HubServer, String> {
        let port = self.read_port();
        let started = match self.start_on(port) {
            Ok(started) => started,
            Err(error) if port != 0 => self.start_on(0).map_err(|fallback| {
                format!(
                    "could not start the worker hub on port {port} ({error}) or an ephemeral port ({fallback})"
                )
            })?,
            Err(error) => return Err(format!("could not start the worker hub: {error}")),
        };
        self.persist_port(started.local_addr().port())?;
        Ok(started)
    }

    /// One bind attempt on `port` (`0` lets the OS choose).
    fn start_on(&self, port: u16) -> std::io::Result<HubServer> {
        HubServer::start(
            HubOptions {
                // All interfaces: a node on the tailnet connects to this
                // machine's LAN address. Windows may raise its firewall prompt
                // for the first bind; the hub's state stays in `state_dir`.
                bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
                state_dir: self.state_dir.clone(),
            },
            self.services.clone(),
        )
    }

    /// The persisted port, or [`DEFAULT_HUB_PORT`] on a fresh profile.
    fn read_port(&self) -> u16 {
        std::fs::read_to_string(self.state_dir.join(HUB_FILE))
            .ok()
            .and_then(|text| serde_json::from_str::<HubFile>(&text).ok())
            .map(|file| file.port)
            .unwrap_or(DEFAULT_HUB_PORT)
    }

    /// Persist the port the listener actually bound, so a restart keeps it.
    fn persist_port(&self, port: u16) -> Result<(), String> {
        std::fs::create_dir_all(&self.state_dir)
            .map_err(|error| format!("could not create {}: {error}", self.state_dir.display()))?;
        let text = serde_json::to_string_pretty(&HubFile { port })
            .map_err(|error| format!("could not encode the hub port: {error}"))?;
        mnemo::config::write_atomic(&self.state_dir.join(HUB_FILE), &text)
            .map_err(|error| format!("could not save the hub port: {error}"))
    }
}

/// Why a hub claim could not be taken.
enum LockError {
    /// Another process holds it; the pid when the file names one.
    Held(Option<u32>),
    /// The claim could not be created or replaced.
    Failed(std::io::Error),
}

/// Create `path` exclusively and write this process's pid into it.
///
/// A file left by a process that is no longer alive is stale: it is removed
/// and the claim retried.
fn create_lock_file(path: &Path) -> Result<std::fs::File, LockError> {
    let create = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    };
    match create() {
        Ok(mut file) => {
            let _ = write!(file, "{}", std::process::id());
            Ok(file)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let pid = std::fs::read_to_string(path)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok());
            if pid.is_some_and(crate::instance_marker::pid_alive) {
                return Err(LockError::Held(pid));
            }
            // A dead (or unreadable) holder: replace the stale claim.
            let _ = std::fs::remove_file(path);
            match create() {
                Ok(mut file) => {
                    let _ = write!(file, "{}", std::process::id());
                    Ok(file)
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    Err(LockError::Held(pid))
                }
                Err(error) => Err(LockError::Failed(error)),
            }
        }
        Err(error) => Err(LockError::Failed(error)),
    }
}

/// This machine's LAN IPv4 address, without sending a packet: connecting a UDP
/// socket only makes the OS pick the route, and the documentation range
/// `192.0.2.0/24` is never actually reachable.
fn lan_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) => Some(ip),
        IpAddr::V6(_) => None,
    }
}

/// What the hub answers for the open project: the engine's model clients and
/// memory store, and the project's sanitised model catalog.
pub struct AppHubServices {
    /// The open project's engine (weak: closing the project must release it)
    /// and its config dir, the source of the catalog.
    binding: StdRwLock<Option<(Weak<Engine>, PathBuf)>>,
    /// The last catalog built, answered while the config cannot be read.
    last_catalog: StdMutex<Option<ModelCatalog>>,
}

impl AppHubServices {
    /// Services with no project bound: they answer no models and no memory.
    fn new() -> Self {
        Self {
            binding: StdRwLock::new(None),
            last_catalog: StdMutex::new(None),
        }
    }

    /// Serve `engine` and the config dir its catalog is read from.
    fn bind(&self, engine: Weak<Engine>, config_dir: PathBuf) {
        *self.binding.write().unwrap_or_else(|e| e.into_inner()) = Some((engine, config_dir));
    }

    /// Answer nothing until a project is bound again — but only when the
    /// binding still belongs to `engine`.
    fn unbind(&self, engine: &Arc<Engine>) {
        let mut binding = self.binding.write().unwrap_or_else(|e| e.into_inner());
        let ours = binding
            .as_ref()
            .is_some_and(|(bound, _)| Weak::ptr_eq(bound, &Arc::downgrade(engine)));
        if ours {
            *binding = None;
        }
    }

    /// The bound project's engine, if it is still alive.
    fn engine(&self) -> Option<Arc<Engine>> {
        self.binding
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|(engine, _)| engine.upgrade())
    }

    /// The bound project's config dir.
    fn config_dir(&self) -> Option<PathBuf> {
        self.binding
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(_, config_dir)| config_dir.clone())
    }
}

impl HubServices for AppHubServices {
    fn model_client(&self, model: &ModelRef) -> Option<Arc<dyn LlmClient>> {
        self.engine()?.model_client(model)
    }

    fn memory(&self) -> Option<Arc<dyn MemoryStoreTrait>> {
        self.engine()?.memory_store_handle()
    }

    fn catalog(&self) -> ModelCatalog {
        if let Some(config_dir) = self.config_dir() {
            if let Ok(config) = Config::load(&config_dir) {
                let agent_md = std::fs::read_to_string(config_dir.join("agent.md")).unwrap_or_default();
                let catalog = ModelCatalog::from_config(&config, agent_md);
                *self.last_catalog.lock().unwrap_or_else(|e| e.into_inner()) = Some(catalog.clone());
                return catalog;
            }
            // An unreadable config keeps the last good catalog: a node that
            // asks mid-switch must not see a made-up model inventory.
            eprintln!(
                "mnemo2: could not read the worker-hub catalog from {}; answering the last one",
                config_dir.display()
            );
        }
        self.last_catalog
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| ModelCatalog::from_config(&Config::default(), String::new()))
    }
}

#[async_trait::async_trait]
impl HostDirectory for AppHub {
    fn server(&self) -> Option<Arc<HubServer>> {
        AppHub::server(self)
    }

    fn address(&self) -> Option<String> {
        AppHub::address(self)
    }

    async fn pair(&self, name: Option<&str>) -> Result<PairOffer, String> {
        let server = self.ensure_started()?;
        let code = server.pairing().issue_code(name.unwrap_or(""));
        let hub = self
            .address()
            .ok_or_else(|| "the worker hub has no address on this machine".to_string())?;
        Ok(PairOffer {
            code: code.code,
            expires_ms: code.expires_ms,
            hub,
        })
    }

    async fn revoke(&self, host: &str) -> Result<NodeRecord, String> {
        let wanted = host.trim();
        if wanted.is_empty() {
            return Err("name the host to revoke (its name or id)".to_string());
        }
        let running = self.server();
        let nodes = match running.as_ref() {
            Some(server) => server.nodes(),
            None => Arc::new(NodeRegistry::new(self.state_dir.clone())),
        };
        let record = nodes
            .list()?
            .into_iter()
            .find(|record| record.node_id == wanted || record.name.eq_ignore_ascii_case(wanted))
            .ok_or_else(|| format!("no paired worker host named {wanted:?}"))?;
        match running.as_ref() {
            Some(server) => server.revoke(&record.node_id)?,
            None => nodes.revoke(&record.node_id)?,
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key value that must never reach a node; it names its own test.
    const SENTINEL_KEY: &str = "sk-catalog-must-not-carry-this-0123456789";

    /// A scratch config dir under the checkout's `target/` (the repository sets
    /// `TMP`/`TEMP` there), so a test never reads or writes the user's profile.
    fn config_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp config dir")
    }

    /// A model ref no unbound or unconfigured hub can serve.
    fn unserved_model() -> ModelRef {
        ModelRef {
            endpoint: "local".to_string(),
            model: "test-model".to_string(),
            reasoning_effort: None,
        }
    }

    /// A started engine over `dir`: enough to be the hub's bound project in the
    /// binding tests (no git, no memory, no code graph). The config names one
    /// keyless `local` endpoint `endpoint_name` serving the default model, so
    /// the engine's readiness gate passes and the catalog carries that name.
    async fn test_engine(dir: &Path, endpoint_name: &str) -> Arc<Engine> {
        std::fs::create_dir_all(dir).expect("engine dir");
        std::fs::write(
            dir.join("config.toml"),
            "[general]\ndefault_model = \"m\"\n",
        )
        .expect("test config");
        std::fs::write(
            dir.join("endpoints.toml"),
            format!(
                "[[endpoint]]\nname = \"{endpoint_name}\"\nkind = \"local\"\nbase_url = \"http://127.0.0.1:9/v1/\"\nmodels = [\"m\"]\n"
            ),
        )
        .expect("test endpoints");
        Arc::new(
            Engine::start(mnemo::headless::EngineOptions {
                workspace: mnemo::workspace::Workspace::new(dir),
                config_dir: Some(dir.to_path_buf()),
                permissions: mnemo::agent::policy::PermissionPolicy::OverridePermissions,
                memory: mnemo::headless::MemoryBackend::Disabled,
                index_codegraph: false,
                require_git: false,
                auto_install_embedder: false,
            })
            .await
            .expect("start the test engine"),
        )
    }

    #[test]
    fn no_listener_before_pairing() {
        let config = config_dir();
        let hub = Arc::new(AppHub::new(config.path()));
        hub.start_if_paired()
            .expect("an unpaired profile starts nothing");
        assert!(
            hub.server().is_none(),
            "an unpaired profile must not listen"
        );
        assert!(hub.address().is_none(), "no address without a listener");
        assert!(
            !config.path().join("hub").exists(),
            "no hub directory, no lock file and no port before the first pairing"
        );
        assert!(
            hub.services.model_client(&unserved_model()).is_none(),
            "an unbound hub answers no model client"
        );
    }

    #[test]
    fn app_hub_is_shared_per_config_dir() {
        let config = config_dir();
        let first = app_hub(config.path());
        let second = app_hub(config.path());
        assert!(
            Arc::ptr_eq(&first, &second),
            "one hub serves every caller of a config dir"
        );
    }

    #[tokio::test]
    async fn pair_starts_the_listener_on_the_persisted_port() {
        let config = config_dir();
        // Hand the hub a free port as its persisted choice: it must bind that
        // exact port instead of drifting to an ephemeral one.
        let probe = std::net::TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("probe a free port");
        let port = probe.local_addr().expect("probe address").port();
        drop(probe);
        std::fs::create_dir_all(config.path().join("hub")).expect("hub dir");
        std::fs::write(
            config.path().join("hub").join(HUB_FILE),
            format!("{{\"port\":{port}}}"),
        )
        .expect("persisted port");

        let hub = Arc::new(AppHub::new(config.path()));
        let offer = hub
            .pair(Some("node-a"))
            .await
            .expect("pairing starts the hub");

        let server = hub.server().expect("the listener is running");
        assert_eq!(
            server.local_addr().port(),
            port,
            "the listener binds the persisted port"
        );
        let persisted: HubFile = serde_json::from_str(
            &std::fs::read_to_string(config.path().join("hub").join(HUB_FILE)).expect("hub.json"),
        )
        .expect("hub.json decodes");
        assert_eq!(persisted.port, port, "hub.json keeps the bound port");
        assert_eq!(offer.code.len(), 9, "a pairing code reads XXXX-XXXX");
        assert!(offer.expires_ms > 0, "the code carries its expiry");
        assert!(
            offer.hub.ends_with(&format!(":{port}")),
            "the pairing offer names the listener: {}",
            offer.hub
        );
        assert!(
            config.path().join("hub").join(LOCK_FILE).exists(),
            "the claim stays on disk while this process serves the hub"
        );
    }

    #[tokio::test]
    async fn second_instance_sees_the_hub_lock() {
        let config = config_dir();
        let first = Arc::new(AppHub::new(config.path()));
        first.pair(None).await.expect("the first hub starts");

        let second = Arc::new(AppHub::new(config.path()));
        let error = second
            .pair(Some("node-b"))
            .await
            .expect_err("the second instance must refuse");
        assert!(error.contains("another Mnemo2 window"), "{error}");
        assert!(
            error.contains(&std::process::id().to_string()),
            "the refusal names the serving process: {error}"
        );
        assert!(second.server().is_none(), "the second instance binds nothing");
    }

    #[tokio::test]
    async fn catalog_has_no_api_key() {
        let config = config_dir();
        // An endpoint and its key: the mirrored catalog carries the endpoint
        // shape a node needs for model routing, never the secret.
        let mut cfg = Config::default();
        cfg.endpoints.push(mnemo::config::Endpoint {
            name: "local".to_string(),
            base_url: "http://127.0.0.1:8080/v1/".to_string(),
            ..mnemo::config::Endpoint::default()
        });
        cfg.keys
            .insert("local".to_string(), SENTINEL_KEY.to_string());
        cfg.save_all(config.path()).expect("save the config");
        assert!(
            std::fs::read_to_string(config.path().join("keys.toml"))
                .expect("keys.toml")
                .contains(SENTINEL_KEY),
            "the sentinel really is in the key store"
        );

        let services = AppHubServices::new();
        // A catalog read needs the config dir, not a live engine.
        services.bind(Weak::new(), config.path().to_path_buf());
        let catalog = services.catalog();
        let json = serde_json::to_string(&catalog).expect("serialize the catalog");
        assert!(
            !json.contains(SENTINEL_KEY),
            "a node must never receive an API key: {json}"
        );
        assert!(
            json.contains("\"local\""),
            "the endpoint shape still travels: {json}"
        );
    }

    #[test]
    fn services_unbound_answer_none() {
        let services = AppHubServices::new();
        assert!(services.model_client(&unserved_model()).is_none());
        assert!(services.memory().is_none());
        // The catalog still answers while no project is bound, so a node that
        // asks during a project switch gets a well-formed, empty inventory.
        let catalog = services.catalog();
        assert!(!catalog.revision.is_empty());
        assert!(catalog.endpoints.is_empty());
        assert!(catalog.pricing.is_empty());

        // A bound config dir is what a catalog read uses.
        let config = config_dir();
        services.bind(Weak::new(), config.path().to_path_buf());
        assert_eq!(services.config_dir(), Some(config.path().to_path_buf()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unbinding_an_older_project_keeps_the_newer_binding() {
        let config = config_dir();
        let hub = Arc::new(AppHub::new(config.path()));
        let older_dir = config.path().join("older");
        let newer_dir = config.path().join("newer");
        let older = test_engine(&older_dir, "older-project").await;
        let newer = test_engine(&newer_dir, "newer-project").await;

        // The newer project takes the hub's services while the older one is
        // still shutting down.
        hub.services.bind(Arc::downgrade(&older), older_dir.clone());
        hub.services.bind(Arc::downgrade(&newer), newer_dir.clone());

        hub.unbind_project(&older);

        assert_eq!(
            hub.services.config_dir(),
            Some(newer_dir.clone()),
            "closing the older project keeps the newer binding"
        );
        assert!(
            hub.services
                .catalog()
                .endpoints
                .iter()
                .any(|endpoint| endpoint.name == "newer-project"),
            "the hub still answers with the newer project's catalog"
        );

        // The project that owns the binding still clears it.
        hub.unbind_project(&newer);
        assert!(
            hub.services.config_dir().is_none(),
            "closing the bound project unbinds the hub"
        );
        assert!(hub.services.model_client(&unserved_model()).is_none());
    }
}
