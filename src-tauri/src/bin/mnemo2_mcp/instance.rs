// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Finding (and, for `serve`, starting) the Mnemo2 instance that owns a project.
//!
//! Every running Mnemo2 writes `<config>/run/mnemo2-<pid>.json`. The helper
//! lists those files, pings the candidates whose open project contains the
//! caller's directory, and drops the dead ones. Nothing here touches the
//! network unless a run file already names a matching project.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::http::{self, Endpoint};

/// How long a `/health` ping may take before the instance counts as busy.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
// Hooks get 10 s each from hooks.json. The lookup (one 1 s probe per candidate,
// 3 s in all), one call (1 s connect + 4 s read) and the process start fit in it.
/// `/health` read timeout on the hook path.
pub const HOOK_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
/// Total time a hook may spend probing run files.
pub const HOOK_LOOKUP_BUDGET: Duration = Duration::from_secs(3);
/// Read timeout for the short hook-side calls.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(4);
/// Connect timeout of a call to an instance that answered `/health` a moment
/// ago: loopback connects to a live listener are instant.
const CALL_CONNECT_TIMEOUT: Duration = Duration::from_secs(1);
/// A spawn lock older than this belongs to a crashed starter. It must outlast
/// `Env::spawn_timeout`, or a slow but live starter would lose its lock.
const LOCK_STALE: Duration = Duration::from_secs(120);
/// A takeover guard older than this belongs to a crashed helper.
const GUARD_STALE: Duration = Duration::from_secs(10);
/// An open failure reported more recently than this stands: the user has had
/// no time to fix its cause. An older one is retried instead of refused.
const OPEN_ERROR_FRESH: Duration = Duration::from_secs(30);

/// Launches Mnemo2 for a project root. `Ok(None)` means "launched, no handle".
pub type Spawner = Arc<dyn Fn(&Path) -> Result<Option<Child>, String> + Send + Sync>;

/// Everything the helper reads from its surroundings, injectable for tests.
#[derive(Clone)]
pub struct Env {
    /// `<config>/run`, where instances publish their run files.
    pub run_dir: PathBuf,
    /// `<config>`, which holds `projects.toml`.
    pub config_dir: PathBuf,
    /// This executable; sent to Mnemo2 so it can name the waiter command.
    pub helper: PathBuf,
    /// The process working directory.
    pub process_cwd: PathBuf,
    /// `CLAUDE_PROJECT_DIR`, when Claude Code set it.
    pub project_dir: Option<PathBuf>,
    /// Starts Mnemo2 hidden on a project.
    pub spawner: Spawner,
    /// How long `serve` waits for a freshly started Mnemo2 to open the project.
    pub spawn_timeout: Duration,
    /// Pause between run-file polls while waiting.
    pub poll_interval: Duration,
    /// How long a `/health` ping may take before the instance counts as busy.
    pub health_timeout: Duration,
    /// How long `wait` keeps looking for an instance that is gone or silent
    /// before it gives up (and tells the lead not to restart it).
    pub wait_reach_timeout: Duration,
    /// Pause between those `wait` lookups.
    pub wait_poll: Duration,
}

impl Env {
    /// The real environment of this process.
    pub fn from_process() -> Env {
        let config_dir = mnemo::config::resolve_config_dir(None);
        let helper = std::env::current_exe()
            .map(|path| dunce::simplified(&path).to_path_buf())
            .unwrap_or_else(|_| PathBuf::from("mnemo2-mcp"));
        let search = AppSearch {
            env_exe: std::env::var_os("MNEMO2_EXE").map(PathBuf::from),
            helper_dir: helper.parent().map(Path::to_path_buf).unwrap_or_default(),
            path_var: std::env::var_os("PATH"),
        };
        let cwd_for_app = config_dir.clone();
        let spawner: Spawner = Arc::new(move |root: &Path| {
            let app = resolve_app(&search)?;
            spawn_app(&app, root, &cwd_for_app)
                .map(Some)
                .map_err(|err| format!("could not start {}: {err}", app.display()))
        });
        Env {
            run_dir: config_dir.join("run"),
            config_dir,
            helper,
            process_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            project_dir: std::env::var_os("CLAUDE_PROJECT_DIR")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            spawner,
            // A cold start (window, webview, project open) can be slow, and
            // `serve` answers `initialize` before it is over.
            spawn_timeout: Duration::from_secs(90),
            poll_interval: Duration::from_millis(250),
            health_timeout: HEALTH_TIMEOUT,
            wait_reach_timeout: Duration::from_secs(60),
            wait_poll: Duration::from_secs(2),
        }
    }
}

/// A live Mnemo2 that has the caller's project open.
#[derive(Clone)]
pub struct Instance {
    pub pid: u32,
    pub endpoint: Endpoint,
    /// The instance's canonical project root; the key of every request.
    pub root: String,
}

impl Instance {
    /// True when `other` is the same running instance.
    pub fn same_as(&self, other: &Instance) -> bool {
        self.pid == other.pid && self.endpoint.port == other.endpoint.port
    }
}

/// POST a JSON body to a short-lived route and return the parsed 200 reply.
/// Any failure (connection, status, JSON) is None: hooks never break a session.
pub fn call(instance: &Instance, path: &str, body: &Value, read_timeout: Duration) -> Option<Value> {
    let response = http::post_with(
        &instance.endpoint,
        path,
        &[],
        &body.to_string(),
        CALL_CONNECT_TIMEOUT,
        Some(read_timeout),
    )
    .ok()?;
    if response.status != 200 {
        return None;
    }
    serde_json::from_str(&response.body).ok()
}

/// `{"root": ...}`, the body most lead routes take.
pub fn root_body(instance: &Instance) -> Value {
    json!({ "root": instance.root })
}

// ---- paths -----------------------------------------------------------------

/// Canonicalise `path` without the `\\?\` prefix; unreadable paths stay as given.
pub fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).unwrap_or_else(|_| dunce::simplified(path).to_path_buf())
}

/// True when both paths name the same file (compared canonically, and
/// case-insensitively on Windows).
pub fn same_path(a: &Path, b: &Path) -> bool {
    norm_key(&canonical(a)) == norm_key(&canonical(b))
}

/// A comparison key: case-folded on Windows, one separator style, no trailing
/// separator.
fn norm_key(path: &Path) -> PathBuf {
    let mut text = dunce::simplified(path).to_string_lossy().into_owned();
    if cfg!(windows) {
        text = text.replace('/', "\\").to_lowercase();
    }
    while text.len() > 1 && (text.ends_with('\\') || text.ends_with('/')) && !text.ends_with(":\\") {
        text.pop();
    }
    PathBuf::from(text)
}

/// True when `project` equals `dir` or is one of its ancestors.
pub fn contains(project: &Path, dir: &Path) -> bool {
    norm_key(dir).starts_with(norm_key(project))
}

fn depth(path: &Path) -> usize {
    norm_key(path).components().count()
}

// ---- run files -------------------------------------------------------------

#[derive(Deserialize)]
struct RunFile {
    pid: u32,
    port: u16,
    token: String,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    started_ms: u64,
}

fn read_run_files(run_dir: &Path) -> Vec<(PathBuf, RunFile)> {
    let Ok(entries) = fs::read_dir(run_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("mnemo2-") && name.ends_with(".json")) {
                return None;
            }
            let text = fs::read_to_string(entry.path()).ok()?;
            let run: RunFile = serde_json::from_str(&text).ok()?;
            Some((entry.path(), run))
        })
        .collect()
}

enum Probe {
    /// Answered `/health`; carries the project it reports, when it reports one.
    Alive(Option<Option<String>>),
    /// The OS refused the connection, or another process answered from that
    /// port: the run file is stale. The only verdict that deletes a run file.
    Dead,
    /// Timed out: busy (often still starting up). Skip it, keep the run file.
    Busy,
    /// Rejected the token, or answered something unusable: skip it, keep the
    /// run file.
    Unknown,
}

fn classify_error(err: &http::HttpError) -> Probe {
    if err.is_refused() {
        Probe::Dead
    } else if err.is_timeout() {
        Probe::Busy
    } else {
        Probe::Unknown
    }
}

fn probe(run: &RunFile, health_timeout: Duration) -> Probe {
    let endpoint = Endpoint {
        port: run.port,
        token: run.token.clone(),
    };
    let response = match http::post(&endpoint, "/health", &[], "{}", Some(health_timeout)) {
        Ok(response) => response,
        Err(err) => return classify_error(&err),
    };
    if response.status != 200 {
        return Probe::Unknown;
    }
    let Ok(health) = serde_json::from_str::<Value>(&response.body) else {
        return Probe::Unknown;
    };
    if health
        .get("pid")
        .and_then(Value::as_u64)
        .is_some_and(|pid| pid != u64::from(run.pid))
    {
        return Probe::Dead;
    }
    Probe::Alive(
        health
            .get("project")
            .map(|project| project.as_str().map(str::to_string)),
    )
}

/// How patient a run-file lookup is.
#[derive(Clone, Copy)]
pub struct Limits {
    /// `/health` read timeout per candidate.
    pub health_timeout: Duration,
    /// Stop probing further candidates after this long.
    pub budget: Option<Duration>,
}

impl Limits {
    /// The normal lookup (serve, wait).
    pub fn normal(env: &Env) -> Limits {
        Limits {
            health_timeout: env.health_timeout,
            budget: None,
        }
    }

    /// The hook lookup: it must fit the hook's own 10 s.
    pub fn hook(env: &Env) -> Limits {
        Limits {
            health_timeout: env.health_timeout.min(HOOK_PROBE_TIMEOUT),
            budget: Some(HOOK_LOOKUP_BUDGET),
        }
    }
}

/// What a run-file lookup found.
pub struct Lookup {
    pub instance: Option<Instance>,
    /// A matching instance timed out: it exists but is busy (typically still
    /// starting). Starting a second Mnemo2 for the project would be wrong.
    pub busy: bool,
}

/// The instance (by run file) whose open project equals or contains `cwd`.
/// Run files are removed only for instances that refuse the connection or
/// belong to another pid. Never starts anything, and makes no network call
/// unless a run file names a matching project.
pub fn lookup(env: &Env, cwd: &Path, limits: Limits) -> Lookup {
    let mut found = Lookup {
        instance: None,
        busy: false,
    };
    let mut candidates: Vec<(PathBuf, RunFile)> = read_run_files(&env.run_dir)
        .into_iter()
        .filter(|(_, run)| run.project.is_some())
        .collect();
    if candidates.is_empty() {
        return found;
    }
    let cwd = canonical(cwd);
    candidates.retain(|(_, run)| {
        run.project
            .as_deref()
            .is_some_and(|project| contains(Path::new(project), &cwd))
    });
    // The most specific project wins; a newer instance beats an older one.
    candidates.sort_by(|a, b| {
        let a_depth = depth(Path::new(a.1.project.as_deref().unwrap_or("")));
        let b_depth = depth(Path::new(b.1.project.as_deref().unwrap_or("")));
        b_depth.cmp(&a_depth).then(b.1.started_ms.cmp(&a.1.started_ms))
    });
    let started = Instant::now();
    for (path, run) in candidates {
        if limits.budget.is_some_and(|budget| started.elapsed() >= budget) {
            break;
        }
        match probe(&run, limits.health_timeout) {
            Probe::Dead => {
                let _ = fs::remove_file(&path);
            }
            Probe::Busy => found.busy = true,
            Probe::Unknown => {}
            Probe::Alive(reported) => {
                // `/health` is the live truth; the run file may lag a project switch.
                let project = match reported {
                    Some(project) => project,
                    None => run.project.clone(),
                };
                let Some(project) = project else { continue };
                if !contains(Path::new(&project), &cwd) {
                    continue;
                }
                found.instance = Some(Instance {
                    pid: run.pid,
                    endpoint: Endpoint {
                        port: run.port,
                        token: run.token,
                    },
                    root: project,
                });
                return found;
            }
        }
    }
    found
}

/// [`lookup`] with the normal limits, keeping only the instance.
pub fn find_instance(env: &Env, cwd: &Path) -> Option<Instance> {
    lookup(env, cwd, Limits::normal(env)).instance
}

/// [`lookup`] with the hook limits, keeping only the instance.
pub fn find_instance_for_hook(env: &Env, cwd: &Path) -> Option<Instance> {
    lookup(env, cwd, Limits::hook(env)).instance
}

// ---- registered projects and starting Mnemo2 ------------------------------

/// The registered project that contains `cwd` (the deepest one), as its
/// canonical path. `projects.toml` is read through Mnemo2's own `project::list`.
pub fn registered_root(env: &Env, cwd: &Path) -> Result<Option<String>, String> {
    let cwd = canonical(cwd);
    let records = mnemo_app::mnemo2::project::list(Some(env.config_dir.clone()))?;
    Ok(records
        .iter()
        .map(|record| canonical(Path::new(&record.path)))
        .filter(|path| contains(path, &cwd))
        .max_by_key(|path| depth(path))
        .map(|path| path.to_string_lossy().into_owned()))
}

/// What `serve` ends up talking to.
pub enum Resolution {
    Live(Instance),
    /// The directory is not in a registered project: serve the inert stub.
    Unregistered,
    /// Mnemo2 should be there but could not be reached or started.
    Failed(String),
}

/// Find the instance for `cwd`, starting Mnemo2 hidden when `cwd` is inside a
/// registered project and nothing is running. A matching instance that is only
/// busy (a timed-out probe) is waited for, never duplicated.
pub fn resolve(env: &Env, cwd: &Path) -> Resolution {
    let first = lookup(env, cwd, Limits::normal(env));
    if let Some(instance) = first.instance {
        return Resolution::Live(instance);
    }
    match registered_root(env, cwd) {
        Ok(Some(root)) => match start_instance(env, &root, first.busy) {
            Ok(instance) => Resolution::Live(instance),
            Err(err) => Resolution::Failed(err),
        },
        Ok(None) => Resolution::Unregistered,
        Err(err) => {
            eprintln!("mnemo2-mcp: could not read the project registry: {err}");
            Resolution::Unregistered
        }
    }
}

fn start_instance(env: &Env, root: &str, already_busy: bool) -> Result<Instance, String> {
    let mut busy = already_busy;
    // Only a failure reported after this moment belongs to the wait below.
    let began = Instant::now();
    // The guard lives until this function returns, so the lock outlasts the wait.
    let (_lock, mut child, retried) = match open_error(env, root) {
        // An instance that failed to open this project keeps running (in the
        // tray, so the user can fix it); starting another would fail the same
        // way and pile up hidden instances.
        Some(failure) if failure.age < OPEN_ERROR_FRESH => {
            return Err(open_refused(root, &failure.reason));
        }
        // The user may have fixed the cause since: ask that instance to try
        // again and wait for it, rather than starting another.
        Some(failure) => {
            if !ask_to_open(env, &failure.endpoint, root) {
                return Err(open_refused(root, &failure.reason));
            }
            (None, None, true)
        }
        None if busy => (None, None, false),
        None => {
            let lock = SpawnLock::acquire(&env.run_dir, root);
            let mut child = None;
            if !matches!(lock, SpawnLock::Busy) {
                eprintln!("mnemo2-mcp: starting Mnemo2 for {root}");
                child = (env.spawner)(Path::new(root))?;
            }
            (Some(lock), child, false)
        }
    };
    let deadline = Instant::now() + env.spawn_timeout;
    loop {
        let found = lookup(env, Path::new(root), Limits::normal(env));
        if let Some(instance) = found.instance {
            return Ok(instance);
        }
        busy |= found.busy;
        if let Some(failure) = open_error(env, root) {
            // A failure from before this wait is history (the one a retry
            // was asked to redo); a newer one is the answer.
            if failure.age < began.elapsed() {
                return Err(open_refused(root, &failure.reason));
            }
        }
        if let Some(child) = child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!(
                    "Mnemo2 exited during startup ({status}). Open Mnemo2 manually and check the project {root}."
                ));
            }
        }
        if Instant::now() >= deadline {
            let what = if retried {
                "Mnemo2 was asked to try again but did not open the project"
            } else if busy {
                "Mnemo2 is running but did not answer"
            } else {
                "Mnemo2 was started but did not open the project"
            };
            return Err(format!(
                "{what} {root} within {} s. Open Mnemo2 from the tray and check the project.",
                env.spawn_timeout.as_secs()
            ));
        }
        std::thread::sleep(env.poll_interval);
    }
}

/// An open failure a running instance reported on `/health`.
struct OpenError {
    /// The instance that reported it.
    endpoint: Endpoint,
    reason: String,
    /// How long ago it was reported. An instance that does not say counts as
    /// just now, so the failure is never retried blindly.
    age: Duration,
}

/// The failure a running instance reported for not opening `root`, if any
/// (its UI hit an error, e.g. a failed lead open, and said so on `/health`).
fn open_error(env: &Env, root: &str) -> Option<OpenError> {
    let body = serde_json::json!({ "root": root }).to_string();
    read_run_files(&env.run_dir).into_iter().find_map(|(_, run)| {
        let endpoint = Endpoint {
            port: run.port,
            token: run.token.clone(),
        };
        let response =
            http::post(&endpoint, "/health", &[], &body, Some(env.health_timeout)).ok()?;
        let health: Value = serde_json::from_str(&response.body).ok()?;
        let reason = health.get("open_error").and_then(Value::as_str)?.to_string();
        let age_ms = health
            .get("open_error_age_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Some(OpenError {
            endpoint,
            reason,
            age: Duration::from_millis(age_ms),
        })
    })
}

/// The refusal for a project an instance could not open: only the user, from
/// that instance's window, can fix what the reason names.
fn open_refused(root: &str, reason: &str) -> String {
    format!(
        "Mnemo2 could not open the project {root}: {}. Open Mnemo2 from the tray to fix it, then retry.",
        reason.trim_end_matches('.')
    )
}

/// Ask the instance behind `endpoint` to try opening `root` again; true when
/// it took the request (one without the route, or one that declines, is not
/// waited for).
fn ask_to_open(env: &Env, endpoint: &Endpoint, root: &str) -> bool {
    let body = serde_json::json!({ "root": root }).to_string();
    http::post(endpoint, "/lead/open", &[], &body, Some(env.health_timeout))
        .ok()
        .filter(|response| response.status == 200)
        .and_then(|response| serde_json::from_str::<Value>(&response.body).ok())
        .and_then(|reply| reply.get("requested").and_then(Value::as_bool))
        .unwrap_or(false)
}

/// Keeps two helpers (two Claude sessions) from both launching Mnemo2 for the
/// same project: the loser just waits for the winner's run file.
///
/// The lock is a file created with `create_new`. A lock older than
/// [`LOCK_STALE`] belongs to a crashed starter; clearing it is serialised by a
/// second, short-lived guard file and re-checked under that guard, so two
/// helpers that both saw the same stale lock cannot both take it (and one
/// cannot delete the other's fresh lock).
enum SpawnLock {
    Owned {
        path: PathBuf,
        /// Written into the file, so only its owner removes it.
        token: String,
    },
    /// The lock file could not be created; start anyway.
    Unlocked,
    /// Another helper is already starting Mnemo2 for this project.
    Busy,
}

fn lock_path(run_dir: &Path, root: &str) -> PathBuf {
    let key = norm_key(Path::new(root)).to_string_lossy().into_owned();
    let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    run_dir.join(format!("spawn-{hash:016x}.lock"))
}

fn guard_path(lock: &Path) -> PathBuf {
    let mut name = lock.as_os_str().to_os_string();
    name.push(".takeover");
    PathBuf::from(name)
}

/// A token unique across processes and across threads of one process.
fn lock_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|age| age.as_nanos())
        .unwrap_or_default();
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn create_lock_file(path: &Path, token: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(token.as_bytes())
}

/// True when a lock last written at `modified` is old enough to belong to a
/// crashed owner. A file that is gone or has an unreadable or future
/// timestamp counts as stale: there is nothing live to protect.
fn is_stale_at(modified: Option<SystemTime>, now: SystemTime, stale_after: Duration) -> bool {
    modified
        .and_then(|time| now.duration_since(time).ok())
        .is_none_or(|age| age > stale_after)
}

fn lock_is_stale(path: &Path, stale_after: Duration) -> bool {
    let modified = fs::metadata(path).and_then(|meta| meta.modified()).ok();
    is_stale_at(modified, SystemTime::now(), stale_after)
}

impl SpawnLock {
    fn acquire(run_dir: &Path, root: &str) -> SpawnLock {
        let _ = fs::create_dir_all(run_dir);
        SpawnLock::acquire_at(&lock_path(run_dir, root), LOCK_STALE)
    }

    fn acquire_at(path: &Path, stale_after: Duration) -> SpawnLock {
        let token = lock_token();
        match create_lock_file(path, &token) {
            Ok(()) => {
                return SpawnLock::Owned {
                    path: path.to_path_buf(),
                    token,
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return SpawnLock::Unlocked,
        }
        if !lock_is_stale(path, stale_after) {
            return SpawnLock::Busy;
        }
        SpawnLock::take_over(path, stale_after, token)
    }

    /// Replace a lock that looked stale. Only the holder of the guard file may
    /// clear it, and it checks again under the guard: a stale lock someone
    /// else already replaced is fresh now and is left alone.
    fn take_over(path: &Path, stale_after: Duration, token: String) -> SpawnLock {
        let guard = guard_path(path);
        match create_lock_file(&guard, &token) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                // Someone else is taking over. A dead one leaves its guard behind.
                if lock_is_stale(&guard, GUARD_STALE) {
                    let _ = fs::remove_file(&guard);
                }
                return SpawnLock::Busy;
            }
            Err(_) => return SpawnLock::Unlocked,
        }
        let result = if lock_is_stale(path, stale_after) {
            let _ = fs::remove_file(path);
            // A starter that never saw the stale lock may have created its own
            // in the meantime; then `create_new` fails and we yield to it.
            match create_lock_file(path, &token) {
                Ok(()) => SpawnLock::Owned {
                    path: path.to_path_buf(),
                    token,
                },
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => SpawnLock::Busy,
                Err(_) => SpawnLock::Unlocked,
            }
        } else {
            SpawnLock::Busy
        };
        let _ = fs::remove_file(&guard);
        result
    }
}

impl Drop for SpawnLock {
    fn drop(&mut self) {
        if let SpawnLock::Owned { path, token } = self {
            // Only remove our own lock: a takeover may have replaced it.
            if fs::read_to_string(&*path).is_ok_and(|text| text == *token) {
                let _ = fs::remove_file(&*path);
            }
        }
    }
}

// ---- locating the Mnemo2 executable ---------------------------------------

#[cfg(windows)]
const APP_EXE: &str = "mnemo2.exe";
#[cfg(not(windows))]
const APP_EXE: &str = "mnemo2";

/// Inputs of the app-executable search, injectable for tests.
pub struct AppSearch {
    /// `MNEMO2_EXE`.
    pub env_exe: Option<PathBuf>,
    /// The directory holding this helper.
    pub helper_dir: PathBuf,
    /// `PATH`.
    pub path_var: Option<OsString>,
}

/// Find the Mnemo2 executable: `MNEMO2_EXE`, then the first line of
/// `<helper dir>/mnemo2-path.txt`, then `<helper dir>/mnemo2`, then `PATH`.
pub fn resolve_app(search: &AppSearch) -> Result<PathBuf, String> {
    if let Some(exe) = search.env_exe.as_ref().filter(|exe| exe.is_file()) {
        return Ok(exe.clone());
    }
    let hint_file = search.helper_dir.join("mnemo2-path.txt");
    if let Ok(text) = fs::read_to_string(&hint_file) {
        let hinted = text
            .trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty());
        if let Some(exe) = hinted.map(PathBuf::from).filter(|exe| exe.is_file()) {
            return Ok(exe);
        }
    }
    let sibling = search.helper_dir.join(APP_EXE);
    if sibling.is_file() {
        return Ok(sibling);
    }
    if let Some(path_var) = &search.path_var {
        if let Some(found) = std::env::split_paths(path_var)
            .map(|dir| dir.join(APP_EXE))
            .find(|candidate| candidate.is_file())
        {
            return Ok(found);
        }
    }
    Err(format!(
        "Mnemo2 isn't running for this project and could not be started: the Mnemo2 app was not found \
         (looked at MNEMO2_EXE, {}, {} and PATH). Build it with `npm run build:mnemo2:native`, \
         or set MNEMO2_EXE to the mnemo2 executable, then retry.",
        hint_file.display(),
        sibling.display()
    ))
}

#[cfg(windows)]
fn detach(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    // The stdio handles stopped being inheritable when `main` started (see
    // `stop_inheriting_stdio`), so the child cannot hold Claude's pipes.
}

/// Mark this process's own stdio handles non-inheritable. `main` does this
/// first thing, before anything is spawned.
///
/// `CreateProcess` passes EVERY inheritable handle to the child, not just the
/// ones named in STARTUPINFO. Claude Code hands `serve` inheritable pipe
/// handles, so a long-lived Mnemo2 started from here would keep Claude's MCP
/// pipe open after `serve` exits and hang the session's shutdown. This also
/// covers the plugin-cache copy that delegates `serve` to the `<config>/bin`
/// copy: the delegate inherits the parent's ORIGINAL handles, which only the
/// parent can fix. Inherited stdio for our own children still works: std
/// duplicates those handles as inheritable for each spawn.
#[cfg(windows)]
pub(crate) fn stop_inheriting_stdio() {
    use std::os::windows::io::AsRawHandle;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetHandleInformation(handle: *mut std::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
    ];
    for handle in handles {
        if !handle.is_null() {
            // SAFETY: a process-owned std handle; clearing its inherit flag
            // changes no ownership, and a failure only leaves it inheritable.
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

/// Elsewhere std closes every descriptor but stdio on exec, and `spawn_app`
/// replaces stdio: nothing to do.
#[cfg(not(windows))]
pub(crate) fn stop_inheriting_stdio() {}

#[cfg(unix)]
fn detach(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

/// Start `mnemo2 --hidden --project <root>` detached, with no stdio. It runs
/// from the config dir so it never pins the project folder as its cwd.
fn spawn_app(app: &Path, root: &Path, cwd: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(app);
    command
        .arg("--hidden")
        .arg("--project")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if cwd.is_dir() {
        command.current_dir(cwd);
    }
    detach(&mut command);
    command.spawn()
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
