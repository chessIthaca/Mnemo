// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The machine-wide shared models host (plan d3336a8a step 2): one process
//! per machine holds the embedding models, every Mnemo instance embedding
//! over loopback HTTP.
//!
//! The host is a small detached console binary (`mnemo-models`, built next
//! to `mnemo2-mcp`) that serves `POST /v1/embed` and `GET /health` on
//! `127.0.0.1` behind a bearer token. Instances lease it through
//! [`crate::host::shared_service`] under `<config>/run/models/`, exactly
//! like the shared `laya-serve`: the first instance spawns it under the
//! spawn lock and publishes the descriptor, the others attach after a
//! health check. The idle exit is built into the host itself — no watchdog
//! process: the serve loop counts [`shared_service::live_leases`] quanta
//! and returns once no live lease has existed for the idle budget, so a
//! crash of every instance never leaves it running forever.
//!
//! Every attach falls back to the in-process path (today's behaviour) when
//! the host is unreachable:
//! [`crate::memory::embedder::RemoteEmbedder`] embeds through a lazily
//! created in-process `BundledEmbedder` while the host is down, and retries
//! the host at most once per cooldown window — re-reading the descriptor on
//! every retry, so a replaced host is reached at its new port. A shared
//! service is an optimisation, never a single point of failure.
//!
//! The client side here is synchronous by design (the spawn path is a
//! handful of syscalls and one short loopback probe — callers hop onto a
//! blocking task, the `shared_service` discipline); the host itself serves
//! on `std::net` with one thread per connection and loads each served
//! model lazily on its first request.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::host::shared_service::{self, LeaseGuard, ServiceInfo};
use crate::memory::embedder::{dim_for_model, model_for_id};
use serde::{Deserialize, Serialize};

/// The lease-directory name: `~/.mnemo/run/models/`.
pub const SERVICE_NAME: &str = "models";

/// The health-probe budget of the startup attach-only leg (the plan's
/// "health check within at most 1 s" — startup never spawns or waits for a
/// spawn).
pub const STARTUP_PROBE: Duration = Duration::from_secs(1);

/// The health-probe budget of a background attach (a just-spawned host binds
/// within milliseconds; 3 s covers a slow cold start).
pub const ATTACH_PROBE: Duration = Duration::from_secs(3);

/// How long a spawner waits for a freshly spawned host to answer `/health`
/// before giving up on it (it then self-exits by its own idle clock — the
/// lease is never taken).
pub const SPAWN_HEALTH_WAIT: Duration = Duration::from_secs(10);

/// How long one `/v1/embed` request may take in total (the host loads a
/// served model lazily on its first request, which takes seconds; a
/// genuinely hung host falls back after this).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// How long the embedder retries the remote after a failure (the re-point
/// window: each retry re-reads the descriptor).
pub const HOST_DOWN_COOLDOWN: Duration = Duration::from_secs(60);

/// The host's default idle exit — no live lease for this long ends the
/// process (the binary's `--idle-ms` default).
pub const HOST_IDLE: Duration = Duration::from_secs(60);

/// The env var the spawning instance hands the bearer token in (args are
/// visible in Unix process listings; the token also lands in the
/// user-restricted `service.json`).
pub const TOKEN_ENV: &str = "MNEMO_MODELS_TOKEN";

/// The env var overriding the host-binary lookup (a portable install, a
/// test harness).
pub const HOST_BINARY_ENV: &str = "MNEMO_MODELS_EXE";

/// Poll between health probes inside one probe budget.
const PROBE_POLL: Duration = Duration::from_millis(250);

/// Connect/read timeout of one health probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The serve loop's accept poll (the latency bound before a connection is
/// picked up).
const ACCEPT_POLL: Duration = Duration::from_millis(25);

/// The serve loop's idle-clock quantum.
const IDLE_POLL: Duration = Duration::from_secs(1);

/// Request head/body caps (a step-9/10 batch of texts is well under this).
const HEAD_CAP: usize = 64 * 1024;
const BODY_CAP: usize = 16 * 1024 * 1024;

/// Read/write timeouts of one served connection (the lazy model load in
/// between body and answer is not bounded — that work is the request's).
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The `POST /v1/embed` body: the model id plus the texts to embed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EmbedRequest {
    /// The model id (must be in the host's serve list).
    pub model: String,
    /// The texts to embed; the answer carries one vector per text.
    pub texts: Vec<String>,
}

/// The `POST /v1/embed` answer: one vector per text, plus the model's dim
/// (the caller validates it against its own in-process dim — stored vectors
/// must stay compatible).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EmbedResponse {
    /// One vector per requested text, in order.
    pub vectors: Vec<Vec<f32>>,
    /// The dim of every returned vector.
    pub dim: usize,
}

/// The `GET /health` answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct HealthResponse {
    /// The model ids this host serves.
    pub models: Vec<String>,
    /// The version of the lib build serving (diagnostics).
    pub version: String,
}

/// The body of every host error response.
#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

/// A validated models-host endpoint: the loopback port, the bearer token,
/// the served models and the host's identity (pid + creation id, carried
/// for diagnostics and pid-reuse checks).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostEndpoint {
    /// The loopback port the host answers on.
    pub port: u16,
    /// The bearer token `/v1/embed` and `/health` require.
    pub token: String,
    /// The model ids the host serves (the descriptor's list).
    pub models: Vec<String>,
    /// The host process id.
    pub pid: u32,
    /// The host's creation identity (the pid-reuse guard).
    pub created: String,
}

/// What the spawn-lock closure produced: an existing host to attach to, or
/// a child this instance spawned (and whose descriptor it published).
enum Acquired {
    /// A live host another instance published while we waited for the lock
    /// (or that appeared before we looked) — attach to it, never a second
    /// one.
    Attach(ServiceInfo),
    /// Our own freshly spawned child, its descriptor published under the
    /// lock.
    Spawned(ServiceInfo),
}

/// The config dir a models cache dir derives from — the share is derived
/// from `cache_dir` ONLY while its last component is `models` (any other
/// cache directory has no share and keeps today's in-process behaviour).
pub fn shared_config_dir(cache_dir: &Path) -> Option<PathBuf> {
    if cache_dir.file_name()?.to_str()? != "models" {
        return None;
    }
    let config_dir = cache_dir.parent()?;
    (!config_dir.as_os_str().is_empty()).then(|| config_dir.to_path_buf())
}

/// A fresh bearer token for a spawning instance (std-only entropy, the same
/// discipline as the shared-service lease tokens: nanoseconds, the pid and
/// a process-local sequence).
fn service_token() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{nanos:x}-{}-{sequence:x}", std::process::id())
}

/// A free loopback port (bind `:0`, read it, drop the socket). The standard
/// TOCTOU window is harmless here: a taken port fails the host's bind and
/// the spawner's health wait, never a wedged instance.
fn alloc_free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0))?;
    Ok(listener.local_addr()?.port())
}

/// One loopback `GET /health` with the bearer token, parsing the status line
/// and the JSON body. A plain socket call like
/// [`shared_service::http_post_ok`] (the spawn path is synchronous by
/// design). `Some` carries the answered model list; `None` is every
/// failure (dead port, wrong token, non-200, unreadable body).
fn http_get_health(port: u16, token: &str, timeout: Duration) -> Option<Vec<String>> {
    let addr = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port);
    let mut stream = TcpStream::connect_timeout(&addr, timeout).ok()?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).ok()?;
    let text = String::from_utf8_lossy(&response).into_owned();
    let header_end = text.find("\r\n\r\n")?;
    let (head, rest) = text.split_at(header_end);
    let ok = head.starts_with("HTTP/1.") && head.split_whitespace().nth(1) == Some("200");
    if !ok {
        return None;
    }
    let health: HealthResponse = serde_json::from_str(&rest[4..]).ok()?;
    Some(health.models)
}

/// The health-probe seam: one probe of a host at (port, token) — did it
/// answer?
type ProbeFn = Box<dyn Fn(u16, &str) -> bool + Send + Sync>;

/// One instance's connection to the shared models host of one config dir:
/// the lease, the last validated endpoint, and the (test) seams.
///
/// Clients are created through [`shared_client`] so the startup attach, the
/// deferred load, rewire and the code embedder share one lease and one
/// endpoint cache per config dir.
pub struct ModelsHostClient {
    /// `<config>/run/models` — this service's lease directory.
    dir: PathBuf,
    /// The config dir (the models cache is `<config>/models`).
    config_dir: PathBuf,
    /// The pid leases are taken for (this instance; tests inject a stub
    /// instance pid).
    instance_pid: u32,
    /// This instance's lease on the host service (replaced wholesale on
    /// re-attach — the lease guard's token check makes the swap safe).
    lease: Mutex<Option<LeaseGuard>>,
    /// The last validated endpoint (re-read after every failure — the
    /// re-point rule).
    endpoint: Mutex<Option<HostEndpoint>>,
    /// Test seam: the host binary to spawn (a stub script); `None` uses the
    /// production lookup.
    binary_override: Option<PathBuf>,
    /// Test seam: replaces the health probe (a stub host that never speaks
    /// HTTP); `None` probes the real `/health`.
    probe_override: Option<ProbeFn>,
    /// The spawn-lock wait (production: [`shared_service::SPAWN_LOCK_WAIT`];
    /// tests shrink it).
    lock_wait: Duration,
}

impl ModelsHostClient {
    /// A client for `config_dir` acting as `instance_pid`.
    fn with_dir(config_dir: &Path, instance_pid: u32) -> Self {
        Self {
            dir: shared_service::service_dir(config_dir, SERVICE_NAME),
            config_dir: config_dir.to_path_buf(),
            instance_pid,
            lease: Mutex::new(None),
            endpoint: Mutex::new(None),
            binary_override: None,
            probe_override: None,
            lock_wait: shared_service::SPAWN_LOCK_WAIT,
        }
    }

    /// The config dir this client derives everything from (the models cache
    /// is `<config>/models`).
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The cached endpoint (the last one attach/acquire validated).
    pub fn endpoint(&self) -> Option<HostEndpoint> {
        self.endpoint
            .lock()
            .expect("models-host endpoint lock poisoned")
            .clone()
    }

    /// Attach to an already-published, healthy host serving `model`.
    ///
    /// Never spawns and never publishes — the startup path calls this with
    /// [`STARTUP_PROBE`] knowing nothing here starts a process. `None` when
    /// no descriptor exists, the host is dead or mismatched, or it does not
    /// answer its health probe within `probe_budget` (a live-but-binding
    /// host gets the probe retried inside the budget instead of a second
    /// spawn — the step-1 lesson).
    pub fn attach(&self, model: &str, probe_budget: Duration) -> Option<HostEndpoint> {
        let info = shared_service::read_service(&self.dir)?;
        let endpoint = self.validate(&info, model)?;
        let deadline = Instant::now() + probe_budget;
        loop {
            if self.probe(endpoint.port, &endpoint.token, model) {
                eprintln!(
                    "info: attached to the shared models host on port {}",
                    endpoint.port
                );
                return self.lease(endpoint);
            }
            if Instant::now() + PROBE_POLL > deadline {
                return None;
            }
            thread::sleep(PROBE_POLL);
        }
    }

    /// Attach-or-spawn — the background paths only (the deferred load,
    /// rewire, reembed, the post-download swap, the code embedder): startup
    /// must stay attach-only.
    ///
    /// `serve_models` is the host's full serve list when THIS instance
    /// spawns it (the requested model plus the code-search model — the
    /// exact-set rule: an instance attaches only when its requested model is
    /// in the descriptor's list). The spawn publishes the descriptor only
    /// while holding the spawn lock, and the health wait happens after the
    /// lock is released, so a slow start never serialises other instances
    /// behind it.
    pub fn acquire(&self, model: &str, serve_models: &[&str]) -> Option<HostEndpoint> {
        if let Some(endpoint) = self.attach(model, ATTACH_PROBE) {
            return Some(endpoint);
        }
        // No usable host published → spawn one under the lock.
        let outcome = shared_service::with_spawn_lock_wait(&self.dir, self.lock_wait, |locked| {
            if !locked {
                // Publish-only-when-locked: without the lock there is no
                // guarantee exactly one host owns the directory — fall back.
                return Ok(None);
            }
            // Re-read: a winner may have published while we waited for the
            // lock (the step-1 lesson — attach to it, never a second host).
            if let Some(info) = shared_service::read_service(&self.dir) {
                if crate::process_tree::pid_alive(info.pid) {
                    return Ok(info
                        .models
                        .iter()
                        .any(|served| served == model)
                        .then_some(Acquired::Attach(info)));
                }
                // A dead pid owns the descriptor — the spawn below replaces it.
            }
            let Some(binary) = self.host_binary() else {
                return Ok(None);
            };
            for serve in serve_models {
                // A bogus config must never leave a host process running.
                if model_for_id(serve).is_err() {
                    return Ok(None);
                }
            }
            let port = alloc_free_port()?;
            let token = service_token();
            let pid = spawn_host(&binary, &self.dir, &self.config_dir, port, &token, serve_models)?;
            let created = crate::process_tree::process_creation_id(pid).unwrap_or_default();
            let info = ServiceInfo {
                pid,
                port,
                token: token.clone(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                started_ms: shared_service::now_ms(),
                created,
                models: serve_models.iter().map(|serve| serve.to_string()).collect(),
            };
            shared_service::write_service(&self.dir, &info)?;
            Ok(Some(Acquired::Spawned(info)))
        })
        .map_err(|err| {
            eprintln!("warning: the models-host spawn path failed: {err}");
            err
        })
        .ok()?;
        let (info, we_spawned) = match outcome? {
            Acquired::Attach(info) => (info, false),
            Acquired::Spawned(info) => (info, true),
        };
        let endpoint = self.validate(&info, model)?;
        // The health wait — outside the lock, so a wedged lock never
        // serialises health waits behind it.
        let deadline = Instant::now() + SPAWN_HEALTH_WAIT;
        loop {
            if self.probe(endpoint.port, &endpoint.token, model) {
                eprintln!(
                    "info: the shared models host is serving on port {}",
                    endpoint.port
                );
                return self.lease(endpoint);
            }
            if Instant::now() + PROBE_POLL > deadline {
                break;
            }
            thread::sleep(PROBE_POLL);
        }
        // A host we spawned that never answered is cleaned up: the
        // descriptor goes (it still names our child) and the lease is never
        // taken, so the child exits by its own idle clock. A foreign
        // descriptor is left alone — its owner manages it.
        if we_spawned {
            shared_service::remove_service_if_pid(&self.dir, info.pid);
        }
        None
    }

    /// Re-read and re-validate the descriptor — the re-point rule: after a
    /// host failure the next retry must never keep the cached port or
    /// token, so a host another instance replaced is reached at its new
    /// port. No probe and no lease: the lease this instance holds is on
    /// the SERVICE directory (it survives a host swap), and the embed
    /// request itself is the health check.
    pub fn refresh_endpoint(&self, model: &str) -> Option<HostEndpoint> {
        let info = shared_service::read_service(&self.dir)?;
        let endpoint = self.validate(&info, model)?;
        *self
            .endpoint
            .lock()
            .expect("models-host endpoint lock poisoned") = Some(endpoint.clone());
        Some(endpoint)
    }

    /// The descriptor validated for `model`: the pid must be live, its
    /// creation identity must match (a reused pid is never trusted), and
    /// the host must serve the model (the exact-set rule).
    fn validate(&self, info: &ServiceInfo, model: &str) -> Option<HostEndpoint> {
        if !info.models.iter().any(|served| served == model) {
            return None;
        }
        if !crate::process_tree::pid_alive(info.pid) {
            return None;
        }
        if crate::process_tree::process_creation_id(info.pid).as_deref()
            != Some(info.created.as_str())
        {
            return None;
        }
        Some(HostEndpoint {
            port: info.port,
            token: info.token.clone(),
            models: info.models.clone(),
            pid: info.pid,
            created: info.created.clone(),
        })
    }

    /// The health check: `GET /health` with the descriptor's token, and the
    /// requested model must be in the answered list (a stray service that
    /// happens to sit on the port fails this). Tests override the whole
    /// check (a stub host never speaks HTTP).
    fn probe(&self, port: u16, token: &str, model: &str) -> bool {
        if let Some(probe) = &self.probe_override {
            return probe(port, token);
        }
        http_get_health(port, token, PROBE_TIMEOUT)
            .is_some_and(|models| models.iter().any(|served| served == model))
    }

    /// Take (or refresh) this instance's lease and cache the endpoint.
    /// A lease failure means the host could idle-exit while we embed —
    /// safer to not use it (`None`, the caller falls back in-process).
    fn lease(&self, endpoint: HostEndpoint) -> Option<HostEndpoint> {
        let guard = shared_service::take_lease(&self.dir, self.instance_pid)
            .map_err(|err| {
                eprintln!("warning: could not lease the shared models host: {err}");
                err
            })
            .ok()?;
        *self.lease.lock().expect("models-host lease lock poisoned") = Some(guard);
        *self
            .endpoint
            .lock()
            .expect("models-host endpoint lock poisoned") = Some(endpoint.clone());
        Some(endpoint)
    }

    /// The `mnemo-models` binary: the test override, then the production
    /// lookup for this config dir.
    fn host_binary(&self) -> Option<PathBuf> {
        if let Some(binary) = &self.binary_override {
            return binary.is_file().then(|| binary.clone());
        }
        host_binary_lookup(&self.config_dir)
    }

    /// Tests only: an endpoint as-if-attached (no probe, no lease) — the
    /// embedder's failure paths start from a known port.
    #[cfg(test)]
    pub(crate) fn override_endpoint(&self, port: u16, token: &str, models: &[&str]) {
        *self
            .endpoint
            .lock()
            .expect("models-host endpoint lock poisoned") = Some(HostEndpoint {
            port,
            token: token.to_string(),
            models: models.iter().map(|model| model.to_string()).collect(),
            pid: std::process::id(),
            created: String::new(),
        });
    }
}

/// The shared client for `config_dir` — one per config dir per process, so
/// the startup attach, the deferred load, rewire and the code embedder share
/// one lease and one endpoint cache. The registry holds `Weak`s: the client
/// (and with it the lease) lives exactly as long as the last embedder using
/// it, so the host idles out once this process really stops embedding.
pub fn shared_client(config_dir: &Path) -> Arc<ModelsHostClient> {
    let mut registry = registry().lock().expect("models-host registry lock poisoned");
    let key = config_dir.to_path_buf();
    if let Some(client) = registry.get(&key).and_then(Weak::upgrade) {
        return client;
    }
    let client = Arc::new(ModelsHostClient::with_dir(config_dir, std::process::id()));
    registry.insert(key, Arc::downgrade(&client));
    client
}

/// The process-global per-config-dir client registry.
fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<ModelsHostClient>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<ModelsHostClient>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The `mnemo-models` binary: the [`HOST_BINARY_ENV`] override (a portable
/// install, a test harness), a sibling of the current executable (dev
/// builds; the bundlers ship it next to the app binary), then
/// `<config>/bin` (the `rebuild:mnemo2` install target). Mirrors the
/// `mnemo2-mcp` lookup the shared-service watchdog uses.
pub fn host_binary_lookup(config_dir: &Path) -> Option<PathBuf> {
    if let Some(value) = std::env::var_os(HOST_BINARY_ENV) {
        let path = PathBuf::from(value);
        return path.is_file().then_some(path);
    }
    let name = crate::host::laya::exe_name(std::env::consts::OS, "mnemo-models");
    let current = std::env::current_exe().ok()?;
    let sibling = current.parent()?.join(&name);
    if sibling.is_file() {
        return Some(sibling);
    }
    let installed = config_dir.join("bin").join(name);
    installed.is_file().then_some(installed)
}

/// Spawn the host binary detached: a hidden console and its own process
/// group ([`shared_service::detach`] — never `DETACHED_PROCESS`, never
/// inside a kill-on-close job; surviving the spawner is the point), the
/// token via environment, stderr into `<dir>/host.log`, and a reaper thread
/// so the child never becomes a zombie. Returns the child pid.
fn spawn_host(
    binary: &Path,
    dir: &Path,
    config_dir: &Path,
    port: u16,
    token: &str,
    serve_models: &[&str],
) -> io::Result<u32> {
    fs::create_dir_all(dir)?;
    let log = File::create(dir.join("host.log"))?;
    let mut command = Command::new(binary);
    command
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--port")
        .arg(port.to_string())
        .arg("--models")
        .arg(serve_models.join(","))
        .arg("--idle-ms")
        .arg(HOST_IDLE.as_millis().to_string())
        .env(TOKEN_ENV, token)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    shared_service::detach(&mut command);
    let mut child = command.spawn().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("spawning {} failed: {err}", binary.display()),
        )
    })?;
    let pid = child.id();
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

/// The lazily loaded models of a serving host.
struct HostState {
    /// `<config>/models` — the machine-global model cache (the same dir the
    /// in-process `BundledEmbedder` uses, so a host's download is shared).
    cache_dir: PathBuf,
    /// The model ids this host serves (validated against the catalog).
    serving: Vec<String>,
    /// Loaded models: id → the ONNX session plus its dim. One loader at a
    /// time; embeds serialize on the session, not this table.
    loaded: Mutex<HashMap<String, LoadedModel>>,
}

/// One loaded model: its ONNX session and its dim (recorded at load time).
struct LoadedModel {
    session: Arc<Mutex<fastembed::TextEmbedding>>,
    dim: usize,
}

impl HostState {
    /// Embed `texts` with `model_id`, loading the model lazily on its first
    /// request (a first use may download into the shared cache — the request
    /// blocks for it, by design). Errors carry the one-line reason the HTTP
    /// answer shows.
    fn embed(&self, model_id: &str, texts: &[String]) -> Result<(Vec<Vec<f32>>, usize), String> {
        if !self.serving.iter().any(|served| served == model_id) {
            return Err(format!("model '{model_id}' is not served by this host"));
        }
        let loaded_model = {
            let mut table = self
                .loaded
                .lock()
                .map_err(|_| String::from("the model table is poisoned"))?;
            if let Some(existing) = table.get(model_id) {
                LoadedModel {
                    session: Arc::clone(&existing.session),
                    dim: existing.dim,
                }
            } else {
                let model = model_for_id(model_id)?;
                let dim = dim_for_model(&model);
                let init =
                    fastembed::TextInitOptions::new(model).with_cache_dir(self.cache_dir.clone());
                let session = fastembed::TextEmbedding::try_new(init).map_err(|err| {
                    format!("failed to load embedding model '{model_id}': {err}")
                })?;
                let entry = LoadedModel {
                    session: Arc::new(std::sync::Mutex::new(session)),
                    dim,
                };
                table.insert(
                    model_id.to_string(),
                    LoadedModel {
                        session: Arc::clone(&entry.session),
                        dim: entry.dim,
                    },
                );
                entry
            }
        };
        let mut session = loaded_model
            .session
            .lock()
            .map_err(|_| String::from("the model session is poisoned"))?;
        let vectors = session
            .embed(texts, None)
            .map_err(|err| format!("inference for '{model_id}' failed: {err}"))?;
        Ok((vectors, loaded_model.dim))
    }
}

/// Serve the models host on `127.0.0.1:port` until no live instance lease
/// has existed for `idle` — the idle exit is built into the host itself.
///
/// This is the loop the `mnemo-models` binary runs: bind, then answer one
/// thread per connection (`GET /health`, `POST /v1/embed`), both behind the
/// bearer token, loading each served model lazily on its first request.
/// Returning `Ok` is the idle exit (main ends, the process goes); an `Err`
/// is a startup failure (an unknown model id, a port that cannot bind).
///
/// Tests run the same loop in-process against temp config dirs.
pub fn serve(
    config_dir: &Path,
    port: u16,
    token: &str,
    models: Vec<String>,
    idle: Duration,
) -> io::Result<()> {
    let dir = shared_service::service_dir(config_dir, SERVICE_NAME);
    let mut serving = Vec::new();
    for model in &models {
        if model_for_id(model).is_err() {
            return Err(io::Error::other(format!(
                "model '{model}' is not in the bundled catalog"
            )));
        }
        if !serving.iter().any(|serve| serve == model) {
            serving.push(model.clone());
        }
    }
    let state = Arc::new(HostState {
        cache_dir: crate::config::embedder_cache_dir(config_dir),
        serving,
        loaded: Mutex::new(HashMap::new()),
    });
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port))?;
    listener.set_nonblocking(true)?;
    eprintln!(
        "info: mnemo-models serving {:?} on 127.0.0.1:{port}",
        state.serving
    );
    // The idle clock: count live leases once per IDLE_POLL quantum; a
    // connection resets the empty streak too (an attaching instance probes
    // before it leases — the spawner's health wait must not race the exit).
    let mut empty_for = Duration::ZERO;
    let mut last_idle_check = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                empty_for = Duration::ZERO;
                let state = Arc::clone(&state);
                let token = token.to_string();
                thread::spawn(move || handle_connection(stream, &token, state));
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(err),
        }
        thread::sleep(ACCEPT_POLL);
        if last_idle_check.elapsed() >= IDLE_POLL {
            last_idle_check = Instant::now();
            if shared_service::live_leases(&dir).is_empty() {
                empty_for += IDLE_POLL;
                if empty_for >= idle {
                    eprintln!("info: mnemo-models exiting — no live instance lease for {idle:?}");
                    return Ok(());
                }
            } else {
                empty_for = Duration::ZERO;
            }
        }
    }
}

/// Answer one connection: parse the request, check the bearer token, then
/// dispatch. One request per connection (`Connection: close`).
fn handle_connection(mut stream: TcpStream, token: &str, state: Arc<HostState>) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Some(incoming) = read_request(&mut stream) else {
        let _ = respond(&mut stream, 400, "Bad Request", "{\"error\":\"malformed request\"}");
        return;
    };
    let ParsedRequest {
        method,
        path,
        headers,
        body,
    } = incoming;
    // Bearer check first — nothing runs without it.
    let expected = format!("Bearer {token}");
    let authorized = headers
        .iter()
        .any(|(name, value)| name.eq_ignore_ascii_case("authorization") && value == &expected);
    if !authorized {
        let _ = respond(&mut stream, 401, "Unauthorized", &error_body("missing or invalid bearer token"));
        return;
    }
    if method == "GET" && path == "/health" {
        let answer = HealthResponse {
            models: state.serving.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let body = serde_json::to_string(&answer).unwrap_or_default();
        let _ = respond(&mut stream, 200, "OK", &body);
        return;
    }
    if method == "POST" && path == "/v1/embed" {
        match serde_json::from_slice::<EmbedRequest>(&body) {
            Ok(request) => match state.embed(&request.model, &request.texts) {
                Ok((vectors, dim)) => {
                    let answer = EmbedResponse { vectors, dim };
                    let body = serde_json::to_string(&answer).unwrap_or_default();
                    let _ = respond(&mut stream, 200, "OK", &body);
                }
                Err(message) => {
                    let _ = respond(&mut stream, 503, "Service Unavailable", &error_body(&message));
                }
            },
            Err(err) => {
                let _ = respond(&mut stream, 400, "Bad Request", &error_body(&err.to_string()));
            }
        }
        return;
    }
    let code = if method == "GET" || method == "POST" { 404 } else { 405 };
    let _ = respond(&mut stream, code, "Not Found", &error_body("unknown method or path"));
}

/// The JSON error body of `message`.
fn error_body(message: &str) -> String {
    serde_json::to_string(&ErrorBody {
        error: message.to_string(),
    })
    .unwrap_or_default()
}

/// One parsed HTTP request.
struct ParsedRequest {
    /// The request method (`GET` or `POST`).
    method: String,
    /// The request path (`/health` or `/v1/embed`).
    path: String,
    /// The headers, (name, value) pairs, names matched case-insensitively.
    headers: Vec<(String, String)>,
    /// The `Content-Length` body (empty when the request carries none).
    body: Vec<u8>,
}

/// Read one HTTP/1.1 request: the request line, the headers and the
/// Content-Length body (chunked bodies are refused — the client is ours and
/// always sends a length).
fn read_request(stream: &mut TcpStream) -> Option<ParsedRequest> {
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = find_header_end(&buffer) {
            break at;
        }
        if buffer.len() > HEAD_CAP {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    let content_length: usize = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0);
    if content_length > BODY_CAP {
        return None;
    }
    let mut body = buffer[header_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some(ParsedRequest {
        method,
        path,
        headers,
        body,
    })
}

/// The offset of the `\r\n\r\n` header/body separator, if fully buffered.
fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Write one response: status line, JSON headers, body — then the
/// connection closes (`Connection: close`).
fn respond(stream: &mut TcpStream, code: u16, reason: &str, body: &str) -> io::Result<()> {
    let head = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    Ok(())
}

/// Start a REAL host for `config_dir` and publish its descriptor for THIS
/// process — the full-fidelity attach target for tests (the descriptor pid
/// is alive and carries this process's real creation identity, and the
/// served models answer `/health` before this returns). Tests only.
#[cfg(test)]
pub fn spawn_test_host(config_dir: &Path, models: &[&str], idle: Duration) -> (u16, String) {
    let port = alloc_free_port().expect("a free loopback port");
    let token = service_token();
    let info = ServiceInfo {
        pid: std::process::id(),
        port,
        token: token.clone(),
        version: "test".into(),
        started_ms: shared_service::now_ms(),
        created: crate::process_tree::process_creation_id(std::process::id()).unwrap_or_default(),
        models: models.iter().map(|model| model.to_string()).collect(),
    };
    shared_service::write_service(&shared_service::service_dir(config_dir, SERVICE_NAME), &info)
        .expect("publish the test descriptor");
    let serve_config = config_dir.to_path_buf();
    let serve_token = token.clone();
    let serve_models: Vec<String> = models.iter().map(|model| model.to_string()).collect();
    thread::spawn(move || {
        let _ = serve(&serve_config, port, &serve_token, serve_models, idle);
    });
    // Bind happens in milliseconds, but the harness must never race it:
    // wait for the first healthy answer before the test attaches.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if http_get_health(port, &token, PROBE_TIMEOUT).is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the test host never answered its health probe"
        );
        thread::sleep(PROBE_POLL);
    }
    (port, token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// A live process that plays another instance (its pid gets a lease).
    struct StubProcess(std::process::Child);

    impl StubProcess {
        fn start() -> Self {
            let mut command = if std::env::consts::OS == "windows" {
                let mut command = Command::new(
                    std::env::var_os("COMSPEC")
                        .expect("COMSPEC names the Windows command processor"),
                );
                command.args(["/C", "ping -n 120 127.0.0.1 >NUL"]);
                command
            } else {
                let mut command = Command::new("/bin/sh");
                command.args(["-c", "sleep 120"]);
                command
            };
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // No console flash for stubs either — the same discipline as the
            // real spawns (see shared_service::detach).
            crate::host::laya::hide_console_window(&mut command);
            Self(command.spawn().expect("start the stub instance"))
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for StubProcess {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A stand-in host binary: a script that stays alive for the spawn-path
    /// tests' child (the REAL host loop is exercised in-process by the
    /// `spawn_test_host`-based tests). Picked per OS at runtime so this file
    /// carries no Windows-only conditional compilation (the policy ratchet).
    fn write_stub_host_script(dir: &Path) -> PathBuf {
        if std::env::consts::OS == "windows" {
            let path = dir.join("mnemo-models-stub.cmd");
            fs::write(&path, "@echo off\r\nping -n 120 127.0.0.1 >NUL\r\n")
                .expect("write the stub script");
            path
        } else {
            let path = dir.join("mnemo-models-stub.sh");
            fs::write(&path, "#!/bin/sh\nsleep 120\n").expect("write the stub script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mut permissions =
                    fs::metadata(&path).expect("stat the stub script").permissions();
                permissions.set_mode(0o755);
                fs::set_permissions(&path, permissions).expect("chmod the stub script");
            }
            path
        }
    }

    /// End whatever host a test spawned behind a descriptor and remove the
    /// service dir (a client never kills a shared host — the test has to).
    /// Only for tests that SPAWNED a child: `spawn_test_host`-based tests
    /// describe THIS process's pid in their descriptors.
    struct HostCleanup {
        dir: PathBuf,
    }

    impl Drop for HostCleanup {
        fn drop(&mut self) {
            if let Some(info) = shared_service::read_service(&self.dir) {
                // Identity-checked kill first; a stub child whose identity
                // could not be recorded is still ended (the test owns it).
                if !shared_service::kill_service_tree(info.pid, &info.created) {
                    let _ = shared_service::kill_detached_tree(info.pid);
                }
            }
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    /// A client acting as one instance (the lease is taken for
    /// `instance_pid`; tests inject a stub pid so two clients lease as two
    /// instances).
    fn test_client(config_dir: &Path, instance_pid: u32) -> ModelsHostClient {
        ModelsHostClient::with_dir(config_dir, instance_pid)
    }

    /// A well-formed descriptor for `pid`: the creation identity is the REAL
    /// one (a pid of 4 billion names no live process, so it stays default).
    fn test_descriptor(pid: u32, port: u16, token: &str, models: &[&str]) -> ServiceInfo {
        ServiceInfo {
            pid,
            port,
            token: token.to_string(),
            version: "test".into(),
            started_ms: shared_service::now_ms(),
            created: crate::process_tree::process_creation_id(pid).unwrap_or_default(),
            models: models.iter().map(|model| model.to_string()).collect(),
        }
    }

    /// One hand-written HTTP request against the host; returns the status
    /// code and the full response text.
    fn raw_http(port: u16, request: &str, body: &str, authorization: &str) -> (u16, String) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).expect("connect to the test host");
        let head = format!(
            "{request}\r\nHost: 127.0.0.1:{port}\r\nAuthorization: {authorization}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).expect("send the request");
        stream.write_all(body.as_bytes()).expect("send the body");
        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read the answer");
        let text = String::from_utf8_lossy(&response).into_owned();
        let code = text
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        (code, text)
    }

    /// Start a real serve thread on a free port and observe its return —
    /// the idle tests need the loop's own exit, not just a healthy answer.
    fn serve_thread(
        config_dir: &Path,
        models: &[&str],
        idle: Duration,
    ) -> (u16, String, mpsc::Receiver<io::Result<()>>) {
        let port = alloc_free_port().expect("a free loopback port");
        let token = service_token();
        let (tx, exited) = mpsc::channel();
        let config_dir = config_dir.to_path_buf();
        let models: Vec<String> = models.iter().map(|model| model.to_string()).collect();
        let serve_token = token.clone();
        thread::spawn(move || {
            let _ = tx.send(serve(&config_dir, port, &serve_token, models, idle));
        });
        (port, token, exited)
    }

    #[test]
    fn shared_config_dir_requires_models_suffix() {
        assert_eq!(
            shared_config_dir(Path::new("/x/config/models")),
            Some(PathBuf::from("/x/config"))
        );
        assert_eq!(shared_config_dir(Path::new("/x/config/models-cache")), None);
        assert_eq!(shared_config_dir(Path::new("models")), None);
    }

    #[test]
    fn host_rejects_a_bad_token() {
        let home = tempfile::tempdir().unwrap();
        let (port, token) = spawn_test_host(home.path(), &["all-MiniLM-L6-v2"], HOST_IDLE);
        let (code, _response) = raw_http(port, "GET /health HTTP/1.1", "", "Bearer wrong-token");
        assert_eq!(code, 401, "health requires the bearer token");
        let body = serde_json::to_string(&EmbedRequest {
            model: "all-MiniLM-L6-v2".into(),
            texts: vec!["hello".into()],
        })
        .expect("encode the request");
        let (code, _response) = raw_http(
            port,
            "POST /v1/embed HTTP/1.1",
            &body,
            "Bearer wrong-token",
        );
        assert_eq!(code, 401, "embedding requires the bearer token");
        // The right token answers health with the served list.
        let models = http_get_health(port, &token, PROBE_TIMEOUT)
            .expect("health answers with the right token");
        assert!(
            models.iter().any(|model| model == "all-MiniLM-L6-v2"),
            "health lists the served models: {models:?}"
        );
    }

    #[test]
    fn host_rejects_a_model_it_does_not_serve() {
        let home = tempfile::tempdir().unwrap();
        let (port, token) = spawn_test_host(home.path(), &["all-MiniLM-L6-v2"], HOST_IDLE);
        let body = serde_json::to_string(&EmbedRequest {
            model: "nomic-embed-text-v1.5".into(),
            texts: vec!["hello".into()],
        })
        .expect("encode the request");
        let (code, response) = raw_http(
            port,
            "POST /v1/embed HTTP/1.1",
            &body,
            &format!("Bearer {token}"),
        );
        assert_eq!(code, 503);
        assert!(
            response.contains("not served"),
            "the answer says why: {response}"
        );
    }

    #[test]
    fn serve_refuses_an_unknown_model_list() {
        let home = tempfile::tempdir().unwrap();
        let error = serve(
            home.path(),
            41234,
            "token",
            vec!["not-a-real-model".to_string()],
            HOST_IDLE,
        )
        .expect_err("an unknown model id must refuse to serve");
        assert!(
            error.to_string().contains("not-a-real-model"),
            "the error names the model: {error}"
        );
    }

    #[test]
    fn host_exits_after_idle() {
        let home = tempfile::tempdir().unwrap();
        let (_port, _token, exited) =
            serve_thread(home.path(), &["all-MiniLM-L6-v2"], Duration::from_millis(300));
        let result = rx_wait(exited, Duration::from_secs(10))
            .expect("the host exits on its own once no lease exists");
        assert!(result.is_ok(), "the idle exit is a clean Ok");
    }

    #[test]
    fn host_stays_up_while_an_instance_holds_a_live_lease() {
        let home = tempfile::tempdir().unwrap();
        let dir = shared_service::service_dir(home.path(), SERVICE_NAME);
        let (_port, _token, exited) =
            serve_thread(home.path(), &["all-MiniLM-L6-v2"], Duration::from_millis(300));
        let lease = shared_service::take_lease(&dir, std::process::id())
            .expect("take this instance's lease");
        // Well past the idle budget: the live lease keeps the host serving.
        thread::sleep(Duration::from_millis(1800));
        assert!(
            exited.try_recv().is_err(),
            "a live lease must keep the host up"
        );
        drop(lease);
        let result = rx_wait(exited, Duration::from_secs(10))
            .expect("the host exits once the lease is gone");
        assert!(result.is_ok());
    }

    /// Wait for a serve thread's return value (the mpsc channel's
    /// recv_timeout error text is unreadable when unwrapped).
    fn rx_wait(
        exited: mpsc::Receiver<io::Result<()>>,
        within: Duration,
    ) -> Option<io::Result<()>> {
        let deadline = Instant::now() + within;
        loop {
            match exited.try_recv() {
                Ok(result) => return Some(result),
                Err(mpsc::TryRecvError::Disconnected) => return None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if Instant::now() > deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn attach_leases_a_healthy_host_and_honors_the_model_set() {
        let home = tempfile::tempdir().unwrap();
        let (port, token) = spawn_test_host(
            home.path(),
            &["all-MiniLM-L6-v2", "jina-embeddings-v2-base-code"],
            HOST_IDLE,
        );
        let client = test_client(home.path(), std::process::id());
        let endpoint = client
            .attach("all-MiniLM-L6-v2", STARTUP_PROBE)
            .expect("attach to the healthy host");
        assert_eq!(endpoint.port, port);
        assert_eq!(endpoint.token, token);
        assert!(
            shared_service::live_leases(&shared_service::service_dir(home.path(), SERVICE_NAME))
                .contains(&std::process::id()),
            "attaching takes this instance's lease"
        );
        assert_eq!(client.endpoint().map(|endpoint| endpoint.port), Some(port));
        // A model not in the descriptor's list never attaches (the
        // exact-set rule).
        assert!(client.attach("nomic-embed-text-v1.5", STARTUP_PROBE).is_none());
    }

    #[test]
    fn attach_skips_a_dead_or_reused_pid() {
        let home = tempfile::tempdir().unwrap();
        let dir = shared_service::service_dir(home.path(), SERVICE_NAME);
        // A descriptor naming a dead pid (a recycled u32 no live process has).
        shared_service::write_service(
            &dir,
            &test_descriptor(4_000_000_000, 41235, "token", &["all-MiniLM-L6-v2"]),
        )
        .expect("publish the stale descriptor");
        let client = test_client(home.path(), std::process::id());
        assert!(client.attach("all-MiniLM-L6-v2", STARTUP_PROBE).is_none());
        // A descriptor whose creation identity does not match (pid reuse).
        let reused = ServiceInfo {
            created: "not-the-real-identity".into(),
            ..test_descriptor(std::process::id(), 41236, "token", &["all-MiniLM-L6-v2"])
        };
        shared_service::write_service(&dir, &reused).expect("publish the reused descriptor");
        assert!(client.attach("all-MiniLM-L6-v2", STARTUP_PROBE).is_none());
    }

    #[test]
    fn refresh_endpoint_follows_a_replaced_host() {
        let home = tempfile::tempdir().unwrap();
        let (port_a, _token_a) =
            spawn_test_host(home.path(), &["all-MiniLM-L6-v2"], HOST_IDLE);
        let client = test_client(home.path(), std::process::id());
        let endpoint = client
            .attach("all-MiniLM-L6-v2", STARTUP_PROBE)
            .expect("attach to host A");
        assert_eq!(endpoint.port, port_a);
        // Host A dies; another instance starts a replacement (its own port
        // and token) and publishes the descriptor over A's.
        let (port_b, token_b) =
            spawn_test_host(home.path(), &["all-MiniLM-L6-v2"], HOST_IDLE);
        assert_ne!(port_b, port_a);
        let refreshed = client
            .refresh_endpoint("all-MiniLM-L6-v2")
            .expect("the re-read finds the replacement");
        assert_eq!(
            refreshed.port, port_b,
            "the re-point rule: never the cached port after a failure"
        );
        assert_eq!(refreshed.token, token_b, "never the cached token either");
    }

    #[test]
    fn acquire_spawns_publishes_and_leases_then_others_attach() {
        let home = tempfile::tempdir().unwrap();
        let dir = shared_service::service_dir(home.path(), SERVICE_NAME);
        let _cleanup = HostCleanup { dir: dir.clone() };
        let instance = StubProcess::start();
        let mut client = test_client(home.path(), instance.pid());
        client.binary_override = Some(write_stub_host_script(home.path()));
        client.probe_override = Some(Box::new(|_, _| true)); // the stub never speaks HTTP
        let endpoint = client
            .acquire(
                "all-MiniLM-L6-v2",
                &["all-MiniLM-L6-v2", "jina-embeddings-v2-base-code"],
            )
            .expect("the first instance spawns the host");
        let info = shared_service::read_service(&dir).expect("the descriptor is published");
        assert_eq!(info.pid, endpoint.pid, "the descriptor names the child");
        assert_eq!(
            info.models,
            vec![
                "all-MiniLM-L6-v2".to_string(),
                "jina-embeddings-v2-base-code".to_string(),
            ],
            "the host serves the requested model plus the code model"
        );
        assert!(
            shared_service::live_leases(&dir).contains(&instance.pid()),
            "spawning takes this instance's lease"
        );
        // A second instance attaches — never a second host.
        let second_instance = StubProcess::start();
        let mut second = test_client(home.path(), second_instance.pid());
        second.probe_override = Some(Box::new(|_, _| true));
        let attached = second
            .acquire(
                "all-MiniLM-L6-v2",
                &["all-MiniLM-L6-v2", "jina-embeddings-v2-base-code"],
            )
            .expect("the second instance attaches");
        assert_eq!(attached.port, endpoint.port, "the same host serves both");
        assert_eq!(
            shared_service::read_service(&dir)
                .expect("descriptor")
                .pid,
            info.pid,
            "attaching spawned nothing"
        );
        let mut leases = vec![instance.pid(), second_instance.pid()];
        leases.sort_unstable();
        assert_eq!(shared_service::live_leases(&dir), leases);
    }

    #[test]
    fn acquire_attaches_instead_of_spawning_a_second_host() {
        let home = tempfile::tempdir().unwrap();
        let (port, _token) = spawn_test_host(home.path(), &["all-MiniLM-L6-v2"], HOST_IDLE);
        let mut client = test_client(home.path(), std::process::id());
        // A host binary exists — it must never be used while a live host is
        // already published.
        client.binary_override = Some(write_stub_host_script(home.path()));
        let endpoint = client
            .acquire(
                "all-MiniLM-L6-v2",
                &["all-MiniLM-L6-v2", "jina-embeddings-v2-base-code"],
            )
            .expect("attach to the live host");
        assert_eq!(endpoint.port, port);
        let info = shared_service::read_service(&shared_service::service_dir(home.path(), SERVICE_NAME))
            .expect("descriptor");
        assert_eq!(
            info.pid,
            std::process::id(),
            "the descriptor is untouched — no second host"
        );
    }

    #[test]
    fn acquire_never_publishes_without_the_spawn_lock() {
        let home = tempfile::tempdir().unwrap();
        let dir = shared_service::service_dir(home.path(), SERVICE_NAME);
        fs::create_dir_all(&dir).expect("create the service dir");
        // A fresh, wedged spawn lock: the age check keeps it untakeable, so
        // the wait expires and the caller proceeds UNLOCKED.
        fs::write(dir.join("spawn.lock"), "wedged-by-a-crashed-spawner")
            .expect("wedge the lock");
        let mut client = test_client(home.path(), std::process::id());
        client.binary_override = Some(write_stub_host_script(home.path()));
        client.probe_override = Some(Box::new(|_, _| true));
        client.lock_wait = Duration::from_millis(100);
        assert!(client.acquire("all-MiniLM-L6-v2", &["all-MiniLM-L6-v2"]).is_none());
        assert!(
            shared_service::read_service(&dir).is_none(),
            "an unlocked caller never publishes a shared host"
        );
    }

    #[test]
    fn acquire_without_a_host_binary_returns_none() {
        let home = tempfile::tempdir().unwrap();
        let dir = shared_service::service_dir(home.path(), SERVICE_NAME);
        let mut client = test_client(home.path(), std::process::id());
        client.binary_override = Some(home.path().join("mnemo-models-missing"));
        assert!(client.acquire("all-MiniLM-L6-v2", &["all-MiniLM-L6-v2"]).is_none());
        assert!(
            shared_service::read_service(&dir).is_none(),
            "no descriptor without a binary"
        );
    }

    #[test]
    fn acquire_skips_spawning_for_an_unknown_model() {
        let home = tempfile::tempdir().unwrap();
        let dir = shared_service::service_dir(home.path(), SERVICE_NAME);
        let _cleanup = HostCleanup { dir: dir.clone() };
        let mut client = test_client(home.path(), std::process::id());
        client.binary_override = Some(write_stub_host_script(home.path()));
        assert!(client.acquire("not-a-real-model", &["not-a-real-model"]).is_none());
        assert!(
            shared_service::read_service(&dir).is_none(),
            "a bogus config must never leave a host process running"
        );
    }
}
