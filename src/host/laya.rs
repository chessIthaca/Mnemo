// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Managed Laya runtime — download it in Settings, mnemo runs it.
//!
//! The classifier foundation (`crate::memory::classifier`) talks to a
//! `laya-serve` instance over HTTP. Laya is managed-only
//! (`[general.laya] enabled = true`) — this module makes that instance a
//! turnkey part of the install: it downloads its own uv, creates a
//! private venv, installs `laya[serve]`, pre-downloads the checkpoint
//! into a redirected HF cache, then runs `laya-serve` bound to a
//! loopback port and drives the shared
//! [`ClassifierStatus`](crate::memory::classifier::ClassifierStatus) to
//! `Ready`/`Failed`.
//!
//! Job 4 D5 (§5.1 :168, §5.8 :554): the Tauri-free core (moved out of the
//! app's IPC layer) — the manager, the pipeline, and the server
//! lifecycle are host-agnostic; a host app keeps only the
//! Tauri glue, and the headless `Setup::classifier_setup` feeds its
//! `EventReceiver<SetupEvent>` through the same sink.
//!
//! Opt-in-gated: `[general.laya]` absent or `enabled = false` means
//! nothing downloads, nothing runs. All child processes bind loopback
//! (`127.0.0.1`) only and are killed on manager drop.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use reqwest::Client;

use crate::config::{global_config_dir, LayaConfig};
use crate::host::shared_service::{self, LeaseGuard, ServiceInfo};
use crate::memory::classifier::{Classifier, ClassifierSlot, ClassifierStatus, LayaClassifier};
use crate::process_tree::KillOnCloseJob;

/// A status-change push (job 4 D5): the core writes the shared status
/// Arc AND hands every change to this sink — the headless
/// [`Setup::classifier_setup`](crate::headless::setup::Setup::classifier_setup)
/// feeds its `EventReceiver<SetupEvent>` (a host app forwards the same push
/// to its UI). `None` = no push (the status
/// Arc is still written).
pub type StatusSink = Arc<dyn Fn(&ClassifierStatus) + Send + Sync>;

/// Write the shared status AND push it to the sink (the moved core's
/// one write path — replaces the app's write + `app.emit` pairs).
fn push_status(
    sink: &Option<StatusSink>,
    status: &Arc<RwLock<ClassifierStatus>>,
    s: ClassifierStatus,
) {
    *status.write().expect("classifier status lock poisoned") = s.clone();
    if let Some(sink) = sink {
        sink(&s);
    }
}

/// Emit a download progress tick (the status write + the sink push).
fn emit_download(
    sink: &Option<StatusSink>,
    status: &Arc<RwLock<ClassifierStatus>>,
    label: &str,
    progress: f64,
) {
    push_status(
        sink,
        status,
        ClassifierStatus::Downloading {
            label: label.to_string(),
            progress,
        },
    );
}

/// The HuggingFace repo the checkpoint lives in (the English model is at
/// the repo root).
const LAYA_REPO: &str = "convaiinnovations/laya";

/// The uv GitHub release feed (one single-file binary asset per platform).
const UV_RELEASES_API: &str = "https://api.github.com/repos/astral-sh/uv/releases/latest";

/// The server bind address — always loopback, never exposed.
const MANAGED_HOST: &str = "127.0.0.1";

/// The machine-wide service name: `<config_dir>/run/laya` holds the lease
/// directory and the descriptor of the one server every instance shares.
const SERVICE_NAME: &str = "laya";

/// How long an attaching instance waits for the shared server's health probe.
const SHARED_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// How often a leasing instance checks that the shared server is still alive
/// (sharedCrashNoRecovery): pid first, plus a probe once it had answered.
const SHARED_MONITOR_INTERVAL: Duration = Duration::from_secs(30);

/// How many consecutive silent probes a READY shared server may miss before
/// the monitor recovers it: one slow probe is churn, not death.
const SHARED_MONITOR_STRIKES: u32 = 2;

/// One shared-server monitor tick's decision — shared by the loop task and
/// the tests ([`LayaManager::shared_tick`]).
#[derive(Debug, PartialEq, Eq)]
enum TickOutcome {
    /// The server is fine (or still loading): watch on.
    Keep,
    /// A previously answering server stayed silent: this many strikes in a
    /// row now. `Recover` only when the count reaches
    /// [`SHARED_MONITOR_STRIKES`].
    Strike(u32),
    /// The server is gone: replace it.
    Recover,
    /// The descriptor names a different server than the one this instance
    /// points at (another instance recovered and published a replacement):
    /// re-attach — same recovery, but the log says what happened.
    Moved,
}

/// The Jev-compatible route `laya-serve` serves.
const SYSTEMONE_PATH: &str = "/v1/systemone";

/// How long the readiness probe keeps waiting while the checkpoint
/// download is still making progress (bytes landing in the HF cache).
const PROBE_IDLE_BUDGET: Duration = Duration::from_secs(120);

/// Absolute cap on the readiness probe (first-run checkpoint download +
/// cold model build included).
const PROBE_ABSOLUTE_BUDGET: Duration = Duration::from_secs(900);

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// A Laya checkpoint the managed runtime can serve (Settings → Classifier).
#[derive(Debug, Clone)]
pub struct LayaCheckpoint {
    /// The short id used everywhere: `LAYA_MODELS`, the marker file name.
    pub id: &'static str,
    /// The human-facing name shown in Settings.
    pub name: &'static str,
    /// Approximate download size (MiB) — drives the setup progress bar.
    pub size_mb: u64,
    /// The repo subfolder (`None` = the repo root / English checkpoint).
    pub subfolder: Option<&'static str>,
}

/// The curated checkpoint catalog — exactly one entry (English).
pub fn checkpoint_catalog() -> &'static [LayaCheckpoint] {
    static CATALOG: &[LayaCheckpoint] = &[
        LayaCheckpoint {
            id: "english",
            name: "Laya (English)",
            size_mb: 810,
            subfolder: None,
        },
    ];
    CATALOG
}

/// The one and only checkpoint (English). Multilingual support was removed,
/// so the app always serves the English model.
pub fn english_checkpoint() -> &'static LayaCheckpoint {
    &checkpoint_catalog()[0]
}// ---------------------------------------------------------------------------
// Pure helpers (unit-tested; no I/O)
// ---------------------------------------------------------------------------

/// The uv release asset name for a platform (`std::env::consts::OS` /
/// `ARCH` pairs). `None` = platform without a uv binary asset.
pub fn uv_asset_name(os: &str, arch: &str) -> Option<&'static str> {
    Some(match (os, arch) {
        ("windows", "x86_64") => "uv-x86_64-pc-windows-msvc.zip",
        ("windows", "aarch64") => "uv-aarch64-pc-windows-msvc.zip",
        ("macos", "x86_64") => "uv-x86_64-apple-darwin.tar.gz",
        ("macos", "aarch64") => "uv-aarch64-apple-darwin.tar.gz",
        ("linux", "x86_64") => "uv-x86_64-unknown-linux-gnu.tar.gz",
        ("linux", "aarch64") => "uv-aarch64-unknown-linux-gnu.tar.gz",
        _ => return None,
    })
}

/// `name.exe` on Windows, `name` elsewhere.
pub fn exe_name(os: &str, name: &str) -> String {
    if os == "windows" {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Path of `tool` inside a venv (`Scripts\tool.exe` on Windows, `bin/tool`
/// elsewhere).
pub fn venv_bin(venv: &Path, os: &str, tool: &str) -> PathBuf {
    if os == "windows" {
        venv.join("Scripts").join(exe_name(os, tool))
    } else {
        venv.join("bin").join(tool)
    }
}

/// The `laya-serve` child environment: bind loopback on our port, preload
/// only the chosen checkpoint, and keep the HF cache inside the managed
/// dir (never the user's global cache).
pub fn server_env(checkpoint_id: &str, port: u16, hf_home: &Path) -> Vec<(String, String)> {
    vec![
        ("LAYA_HOST".to_string(), MANAGED_HOST.to_string()),
        ("LAYA_PORT".to_string(), port.to_string()),
        ("LAYA_MODELS".to_string(), checkpoint_id.to_string()),
        ("LAYA_PRELOAD".to_string(), "1".to_string()),
        ("HF_HOME".to_string(), hf_home.display().to_string()),
    ]
}

/// The python program that pre-downloads a checkpoint through the laya
/// package (setup step 3) — `laya.load` pulls exactly this checkpoint into
/// the `HF_HOME` cache.
pub fn preload_program(subfolder: Option<&str>) -> String {
    match subfolder {
        None => format!("import laya; laya.load({LAYA_REPO:?})"),
        Some(sf) => format!("import laya; laya.load({LAYA_REPO:?}, subfolder={sf:?})"),
    }
}

/// The readiness probe body — an empty `questions` dict is documented to
/// return the standard response (`"answers": {}`) without a forward pass.
pub fn probe_body() -> serde_json::Value {
    serde_json::json!({"state": {"body": "mnemo readiness probe"}, "questions": {}})
}

/// The readiness probe URL for a managed port.
pub fn probe_url(port: u16) -> String {
    format!("http://{MANAGED_HOST}:{port}{SYSTEMONE_PATH}")
}

/// Best-effort download fraction from sampled bytes vs. the catalog size
/// (never reports a full 1.0 — completion is the marker file, per the
/// embedder convention).
pub fn progress_fraction(bytes: u64, size_mb: u64) -> f64 {
    let expected = size_mb.saturating_mul(1024 * 1024);
    if expected == 0 {
        return 0.0;
    }
    (bytes as f64 / expected as f64).clamp(0.0, 0.99)
}

/// Recursively sum file sizes under a dir (0 if missing) — the same
/// sampling primitive the embedder downloads use (D6's `embed_download`
/// shares it: one impl in the host layer).
pub(crate) fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += dir_size(&p);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// On Windows, keep the child from flashing a console window; a no-op
/// elsewhere (per-platform branch, not a platform assumption). Shared by
/// the fine-tune child spawns in the app.
pub fn hide_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}// ---------------------------------------------------------------------------
// LayaManager
// ---------------------------------------------------------------------------

/// Test-only gate inside the real monitor task's disarm path: `entered` is set
/// after the lease read, then the task waits for `released` before storing
/// not-running. This makes the arm/disarm interleaving deterministic.
#[cfg(test)]
#[derive(Default)]
struct MonitorPause {
    entered: AtomicBool,
    released: AtomicBool,
}

/// Owns the managed Laya runtime: where it lives on disk, the running
/// `laya-serve` child, and its port. One instance per app (shared via
/// `IpcState` / the `Setup`), so the setup command, the startup hook, the
/// Settings rewire, and app exit all steer the same sidecar.
pub struct LayaManager {
    /// `<config_dir>/laya` — everything managed lives under here.
    base_dir: PathBuf,
    /// The shared status the consumers surface to Settings (the same
    /// `Arc` the brain's `classifier_status` holds).
    status: Arc<RwLock<ClassifierStatus>>,
    /// The `laya-serve` child this instance launched, if any, tagged with the
    /// generation it was spawned for. Guarded so `stop` can be called from app
    /// exit while a start task runs.
    child: Mutex<Option<ManagedChild>>,
    /// This instance's lease on the machine-wide shared server. Dropping it
    /// (via `stop` or the manager's own Drop) is how an instance leaves the
    /// shared service; the idle watchdog ends the server once the last lease
    /// is gone.
    lease: Mutex<Option<LeaseGuard>>,
    /// The model id this instance's server serves — the recovery path needs it
    /// to re-acquire the right server after a shared-server crash.
    models: Mutex<Option<String>>,
    /// The shared server this instance's CLASSIFIER points at: pid + port +
    /// creation identity, recorded at every attach/spawn. The monitor compares
    /// it with the live descriptor: a descriptor naming a DIFFERENT server
    /// means another instance recovered and published a replacement — this
    /// instance must re-attach instead of staying on a dead port
    /// (staleClientAfterRecovery).
    attached: Mutex<Option<AttachedServer>>,
    /// The live classifier slot (provided by the host layer), so the
    /// crash-recovery monitor can re-point the client at a replacement server.
    classifier_slot: Mutex<Option<ClassifierSlot>>,
    /// Is the shared-server liveness monitor task running? Guarded by a
    /// MUTEX, not an atomic, because the arming check and the exiting task's
    /// disarm decision must not interleave: a lease taken in the window
    /// between the task's lease read and its running=false store would
    /// otherwise end up with no monitor at all (monitorArmRace).
    monitor_running: Mutex<bool>,
    /// Test-only substitute for the installed launcher; production always
    /// resolves `laya_serve_path()`.
    #[cfg(test)]
    serve_override: Option<(PathBuf, Vec<String>)>,
    /// Test-only instance pid for the lease file name, so two managers in one
    /// test process behave like two instances.
    #[cfg(test)]
    instance_pid_override: Option<u32>,
    /// Test-only stand-in for the installed `mnemo2-mcp` watchdog launcher
    /// (`Some` also enables the shared-service path in tests).
    #[cfg(test)]
    watchdog_override: Option<PathBuf>,
    /// Test-only health-check substitute for a shared server.
    #[cfg(test)]
    probe_override: Option<Arc<dyn Fn(u16) -> bool + Send + Sync>>,
    /// Test-only shrink of the spawn-lock wait, so the unlocked leg of
    /// `acquire_on` is exercised in milliseconds, not 30 seconds.
    #[cfg(test)]
    spawn_wait_override: Option<Duration>,
    /// Test-only shrink of the shared-monitor interval, so the tests can
    /// drive the loop in milliseconds.
    #[cfg(test)]
    monitor_interval_override: Option<Duration>,
    /// Test-only pause between the exiting monitor's lease read and disarm.
    #[cfg(test)]
    monitor_pause: Option<Arc<MonitorPause>>,
    /// Test-only count of entered shared readiness tasks.
    #[cfg(test)]
    shared_ready_tasks: Arc<AtomicU64>,
    /// Test-only shrink of the readiness budgets (idle and absolute), so an
    /// expired wait is reachable in a test.
    #[cfg(test)]
    probe_budget_override: Option<Duration>,
    /// Monotonic spawn generation: a failed readiness probe stops only the
    /// child it spawned ([`LayaManager::stop_if_generation`]) — a newer
    /// child from a concurrent start (startup hook vs save-driven rewire
    /// vs setup autostart) must survive.
    child_generation: AtomicU64,
    /// Guard against two concurrent setup pipelines (double-click).
    setup_in_flight: AtomicBool,
}

/// A `laya-serve` this instance launched.
struct ManagedChild {
    child: Child,
    generation: u64,
    job: Option<KillOnCloseJob>,
    /// The pid-reuse guard, captured right after the spawn: every pid-based
    /// kill of this child is allowed only while the process creation identity
    /// still matches.
    created: String,
    /// A shared server outlives the instance that spawned it: `stop`/`Drop`
    /// release only this instance's lease and leave the server to its
    /// watchdog. [`ManagedChild::kill_owned`] still ends it, for a start of
    /// ours that never became ready — and only when no other instance leases
    /// it (see [`LayaManager::stop_if_generation`]).
    shared: bool,
}

impl ManagedChild {
    /// Leave the process running (shared) or end its tree (private).
    fn release(self) {
        if self.shared {
            // Dropping a `std::process::Child` never kills it; the watchdog
            // owns the lifecycle from here — the reaper thread only keeps
            // the exited server from lingering as a zombie.
            release_shared_child(self.child);
        } else {
            kill_tree(self.child, self.job, &self.created);
        }
    }

    /// End the tree unconditionally — this instance's own failed start.
    fn kill_owned(self) {
        kill_tree(self.child, self.job, &self.created);
    }
}

impl LayaManager {
    /// Create the manager rooted at the global config dir.
    pub fn new(status: Arc<RwLock<ClassifierStatus>>) -> Self {
        Self::with_base(global_config_dir().join("laya"), status)
    }

    /// Create the manager rooted at `base_dir` (tests use a tempdir; a
    /// `Setup` with a custom config dir roots at `<config_dir>/laya`).
    pub fn with_base(base_dir: PathBuf, status: Arc<RwLock<ClassifierStatus>>) -> Self {
        Self {
            base_dir,
            status,
            child: Mutex::new(None),
            lease: Mutex::new(None),
            models: Mutex::new(None),
            attached: Mutex::new(None),
            classifier_slot: Mutex::new(None),
            monitor_running: Mutex::new(false),
            #[cfg(test)]
            serve_override: None,
            #[cfg(test)]
            instance_pid_override: None,
            #[cfg(test)]
            watchdog_override: None,
            #[cfg(test)]
            probe_override: None,
            #[cfg(test)]
            spawn_wait_override: None,
            #[cfg(test)]
            monitor_interval_override: None,
            #[cfg(test)]
            monitor_pause: None,
            #[cfg(test)]
            shared_ready_tasks: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            probe_budget_override: None,
            child_generation: AtomicU64::new(0),
            setup_in_flight: AtomicBool::new(false),
        }
    }

    /// Substitute the server launcher in process-tree and shared-service
    /// tests.
    #[cfg(test)]
    fn with_serve_command(mut self, program: PathBuf, args: Vec<String>) -> Self {
        self.serve_override = Some((program, args));
        self
    }

    /// Make this manager act as another instance in the shared-service tests.
    #[cfg(test)]
    fn with_instance_pid(mut self, pid: u32) -> Self {
        self.instance_pid_override = Some(pid);
        self
    }

    /// Pretend the watchdog launcher is installed (`Some`) — which also
    /// enables the shared-service path in tests.
    #[cfg(test)]
    fn with_watchdog(mut self, program: PathBuf) -> Self {
        self.watchdog_override = Some(program);
        self
    }

    /// Substitute the shared-server health check.
    #[cfg(test)]
    fn with_probe(mut self, probe: impl Fn(u16) -> bool + Send + Sync + 'static) -> Self {
        self.probe_override = Some(Arc::new(probe));
        self
    }

    /// Shrink the spawn-lock wait (tests exercise the unlocked leg fast).
    #[cfg(test)]
    fn with_spawn_wait(mut self, wait: Duration) -> Self {
        self.spawn_wait_override = Some(wait);
        self
    }

    /// Shrink the shared-monitor interval (tests drive the loop fast).
    #[cfg(test)]
    fn with_monitor_interval(mut self, interval: Duration) -> Self {
        self.monitor_interval_override = Some(interval);
        self
    }

    /// Pause the real monitor task between its lease read and its disarm.
    #[cfg(test)]
    fn with_monitor_pause(mut self, pause: Arc<MonitorPause>) -> Self {
        self.monitor_pause = Some(pause);
        self
    }

    /// Shrink the readiness budgets (tests reach an expired wait fast);
    /// the absolute budget is twice the idle one.
    #[cfg(test)]
    fn with_probe_budget(mut self, idle: Duration) -> Self {
        self.probe_budget_override = Some(idle);
        self
    }

    /// The spawn-lock wait for `acquire_on`: the production default, or the
    /// test shrink.
    fn spawn_lock_wait(&self) -> Duration {
        #[cfg(test)]
        if let Some(wait) = self.spawn_wait_override {
            return wait;
        }
        shared_service::SPAWN_LOCK_WAIT
    }

    /// The managed root dir (`<config_dir>/laya`).
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// The shared status Arc this manager drives.
    pub fn status_arc(&self) -> &Arc<RwLock<ClassifierStatus>> {
        &self.status
    }

    /// `bin/uv[.exe]` inside the managed dir.
    pub fn uv_path(&self) -> PathBuf {
        self.base_dir
            .join("bin")
            .join(exe_name(std::env::consts::OS, "uv"))
    }

    /// The private venv (`venv/` inside the managed dir).
    pub fn venv_dir(&self) -> PathBuf {
        self.base_dir.join("venv")
    }

    /// The venv's python executable.
    pub fn venv_python(&self) -> PathBuf {
        venv_bin(&self.venv_dir(), std::env::consts::OS, "python")
    }

    /// The venv's `laya-serve` launcher.
    pub fn laya_serve_path(&self) -> PathBuf {
        venv_bin(&self.venv_dir(), std::env::consts::OS, "laya-serve")
    }

    /// The redirected HuggingFace cache (`laya.load` and the server both
    /// download checkpoints here via `HF_HOME`).
    pub fn hf_cache_dir(&self) -> PathBuf {
        self.base_dir.join("hf")
    }

    /// The server's stdout/stderr log (surfaced verbatim in error states).
    /// Per-PID: concurrent app instances share the config dir, and a
    /// shared name would have each spawn truncate the other's log.
    pub fn server_log_path(&self) -> PathBuf {
        self.base_dir
            .join(format!("server-{}.log", std::process::id()))
    }

    /// The per-checkpoint setup marker (`markers/<id>`).
    fn checkpoint_marker(&self, checkpoint_id: &str) -> PathBuf {
        self.base_dir.join("markers").join(checkpoint_id)
    }

    /// The startup fine-tune marker (`markers/finetune-<checkpoint>`): its
    /// content is the unix-seconds ts of the last completed fine-tune —
    /// the training rows newer than it form the next fine-tune's ≥50-row
    /// trigger (never its dataset — the run retrains on the full resolved
    /// corpus).
    pub fn finetune_marker_path(&self, checkpoint_id: &str) -> PathBuf {
        self.base_dir
            .join("markers")
            .join(format!("finetune-{checkpoint_id}"))
    }

    /// Was `checkpoint_id` downloaded by a successful setup (and is the
    /// runtime itself present)?
    pub fn is_checkpoint_installed(&self, checkpoint_id: &str) -> bool {
        self.checkpoint_marker(checkpoint_id).is_file() && self.laya_serve_path().is_file()
    }

    /// Release the setup guard.
    fn end_setup(&self) {
        self.setup_in_flight.store(false, Ordering::SeqCst);
    }

    /// Claim the setup guard; `false` when a setup is already running.
    pub fn begin_setup(&self) -> bool {
        self.setup_in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Leave the server: release the shared lease (and, for a private child,
    /// kill + reap it). Returns whether this manager held anything.
    ///
    /// A shared server is NEVER killed here, not even when this instance
    /// spawned it: another instance may hold a lease, and the idle watchdog
    /// ends the server once nobody does. Safe to call from app exit or a
    /// rewire.
    pub fn stop(&self) -> bool {
        let released = self
            .lease
            .lock()
            .expect("laya lease lock poisoned")
            .take()
            .is_some();
        let had = self
            .child
            .lock()
            .expect("laya child lock poisoned")
            .take();
        // Whatever the classifier pointed at is gone with it.
        *self.attached.lock().expect("laya attached lock poisoned") = None;
        match had {
            Some(child) => {
                child.release();
                true
            }
            None => released,
        }
    }

    /// Release this instance's lease on the shared server (this instance did
    /// not necessarily spawn the server it leased, so no child is touched).
    fn release_lease(&self) {
        *self.lease.lock().expect("laya lease lock poisoned") = None;
    }

    /// Hand the liveness monitor the live classifier slot, so a crash
    /// recovery can re-point the running client at a replacement server.
    /// The host layer (Setup) calls this whenever it fills the slot.
    pub fn provide_classifier_slot(&self, slot: Option<ClassifierSlot>) {
        *self
            .classifier_slot
            .lock()
            .expect("laya classifier slot lock poisoned") = slot;
    }

    /// The readiness budgets for this manager's waits: the production pair,
    /// or the test shrink.
    fn probe_budgets(&self) -> (Duration, Duration) {
        #[cfg(test)]
        if let Some(idle) = self.probe_budget_override {
            return (idle, idle * 2);
        }
        (PROBE_IDLE_BUDGET, PROBE_ABSOLUTE_BUDGET)
    }

    /// The shared-monitor interval: the production 30 s, or the test shrink.
    fn monitor_interval(&self) -> Duration {
        #[cfg(test)]
        if let Some(interval) = self.monitor_interval_override {
            return interval;
        }
        SHARED_MONITOR_INTERVAL
    }

    /// One monitor tick's decision (the loop task and the tests share it):
    ///
    /// - the descriptor is gone or its pid is dead → [`TickOutcome::Recover`];
    /// - the descriptor names a DIFFERENT server than the one this instance's
    ///   classifier points at → [`TickOutcome::Moved`]: another instance
    ///   recovered and published a replacement while our server died, and
    ///   staying on the old port would show `Ready` over a dead client
    ///   (staleClientAfterRecovery);
    /// - the server answers → [`TickOutcome::Keep`], and a `Failed` status is
    ///   promoted back to `Ready` (the self-heal the classifier performs on
    ///   its own calls, mirrored for the status: an attach whose readiness
    ///   wait expired still points its client at this server);
    /// - a server that HAD answered (status `Ready`) stays silent → a strike,
    ///   and [`SHARED_MONITOR_STRIKES`] strikes in a row → `Recover` — one
    ///   slow probe is churn, not death;
    /// - a live server that never answered yet (`Starting`, or a `Failed`
    ///   attach being watched) is loading, not gone → `Keep` — a probe
    ///   failure must never spawn a second server beside a cold start.
    fn shared_tick(&self, strikes: u32) -> TickOutcome {
        let Some(info) = shared_service::read_service(&self.service_dir()) else {
            return TickOutcome::Recover;
        };
        if !crate::process_tree::pid_alive(info.pid) {
            return TickOutcome::Recover;
        }
        // Our own server, or someone else's replacement? Every attach/spawn
        // path records what this instance points at; a descriptor naming
        // another server means another instance recovered while ours died.
        if let Some(ours) = self
            .attached
            .lock()
            .expect("laya attached lock poisoned")
            .as_ref()
        {
            if ours.pid != info.pid || ours.port != info.port {
                return TickOutcome::Moved;
            }
        }
        if self.probe_service(info.port) {
            let mut status = self.status.write().expect("classifier status lock poisoned");
            if *status == ClassifierStatus::Failed {
                *status = ClassifierStatus::Ready;
            }
            return TickOutcome::Keep;
        }
        let was_ready = *self
            .status
            .read()
            .expect("classifier status lock poisoned")
            == ClassifierStatus::Ready;
        if was_ready && strikes + 1 >= SHARED_MONITOR_STRIKES {
            TickOutcome::Recover
        } else if was_ready {
            TickOutcome::Strike(strikes + 1)
        } else {
            TickOutcome::Keep
        }
    }

    /// One recovery step, the monitor task's body (callable directly in
    /// tests): the shared server is gone, so acquire another one — attach to
    /// a server some other instance just published, or spawn one under the
    /// lock — and re-point the classifier slot at it. The readiness wait for
    /// a spawn or a still-loading attach is started fire-and-forget, so the
    /// recovery never blocks the monitor tick.
    async fn recover_shared_server(self: &Arc<Self>, models: &str) -> Option<u16> {
        let status = Arc::clone(&self.status);
        let slot = self
            .classifier_slot
            .lock()
            .expect("laya classifier slot lock poisoned")
            .clone();
        push_status(&None, &status, ClassifierStatus::Starting);
        let acquiring = Arc::clone(self);
        let wanted = models.to_string();
        let acquired =
            tokio::task::spawn_blocking(move || acquiring.acquire(&wanted)).await;
        match acquired {
            Ok(Ok(LayaServer::Attached { port })) => {
                point_slot(&slot, port, &status);
                eprintln!("info: attached to the replacement laya-serve on port {port}");
                push_status(&None, &status, ClassifierStatus::Ready);
                Some(port)
            }
            Ok(Ok(LayaServer::Attaching { port, pid })) => {
                point_slot(&slot, port, &status);
                let manager = Arc::clone(self);
                let wanted = models.to_string();
                tokio::spawn(async move {
                    await_shared_ready(&None, manager, &wanted, port, pid, slot, None).await;
                });
                Some(port)
            }
            Ok(Ok(LayaServer::Spawned { port, generation, .. })) => {
                point_slot(&slot, port, &status);
                let manager = Arc::clone(self);
                tokio::spawn(async move {
                    await_ready(&None, manager, port, generation, None).await;
                });
                Some(port)
            }
            Ok(Err(e)) => {
                eprintln!("error: replacing the shared laya-serve failed: {e}");
                push_status(&None, &status, ClassifierStatus::Failed);
                None
            }
            Err(join) => {
                eprintln!("error: the shared laya-serve recovery task failed: {join}");
                push_status(&None, &status, ClassifierStatus::Failed);
                None
            }
        }
    }

    /// The monitor's arm/disarm bookkeeping, shared by the loop task and the
    /// tests: decide under the ARM lock whether the monitor still has a
    /// server to watch (`Some(models)`) or disarms itself (`None`).
    ///
    /// Holding [`Self::monitor_running`] across "read the lease" and "store
    /// not-running" is what closes the arm race: the arming path takes the
    /// same lock, so a lease taken in between either (a) is seen HERE — the
    /// monitor keeps running for the new lease — or (b) disarms first and
    /// the armer then spawns a fresh task. No interleaving leaves a live
    /// lease with no monitor (monitorArmRace).
    fn monitor_lease_step(&self) -> Option<String> {
        let mut running = self
            .monitor_running
            .lock()
            .expect("laya monitor lock poisoned");
        let models = self
            .models
            .lock()
            .expect("laya models lock poisoned")
            .clone();
        let leased = self
            .lease
            .lock()
            .expect("laya lease lock poisoned")
            .is_some();
        if models.is_none() || !leased {
            #[cfg(test)]
            if let Some(pause) = &self.monitor_pause {
                // This hook intentionally blocks the REAL monitor task. Move
                // it off the async worker so the test runtime's timer and the
                // attaching task keep running.
                tokio::task::block_in_place(|| {
                    pause.entered.store(true, Ordering::SeqCst);
                    while !pause.released.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                });
            }
            *running = false;
            return None;
        }
        models
    }

    /// Is the shared-server liveness monitor task running? (Tests.)
    #[cfg(test)]
    pub(crate) fn is_monitor_running(&self) -> bool {
        *self
            .monitor_running
            .lock()
            .expect("laya monitor lock poisoned")
    }

    /// Number of shared readiness tasks entered (tests).
    #[cfg(test)]
    pub(crate) fn shared_ready_task_count(&self) -> u64 {
        self.shared_ready_tasks.load(Ordering::SeqCst)
    }

    /// The shared server this instance's classifier points at, as recorded at
    /// the last attach/spawn (tests assert the recovery re-points it).
    #[cfg(test)]
    fn attached_server(&self) -> Option<AttachedServer> {
        self.attached
            .lock()
            .expect("laya attached lock poisoned")
            .clone()
    }

    /// Watch the shared server this instance points its classifier at, so its
    /// crash never leaves the instance silently without a classifier
    /// (sharedCrashNoRecovery) and a late answer never stays `Failed`
    /// (leaselessAttacher): every [`SHARED_MONITOR_INTERVAL`] the descriptor's
    /// pid is checked — plus a probe — and a gone server is replaced through
    /// [`LayaManager::recover_shared_server`]. A `Failed` status whose server
    /// answers again is promoted back to `Ready` (the classifier self-heals on
    /// its own calls; the status must follow).
    ///
    /// One task per manager, started by every path that leaves this instance
    /// using a shared server (attach, spawn or an expired readiness wait). The
    /// task holds a [`std::sync::Weak`], never a strong `Arc`: a dropped
    /// manager must run its `Drop` — releasing the lease — instead of being
    /// pinned here forever (monitorPinsManager). It exits when the upgrade
    /// fails (the manager was dropped), when the lease or the model is gone
    /// (stopped, or a private server), and a later shared arm re-starts it.
    /// The classify-failure hook the design mentions is deliberately not
    /// wired: the classifier has no channel back into the manager, and the
    /// pid + probe check covers the same signal within one interval.
    pub fn ensure_shared_monitor(self: &Arc<Self>) {
        {
            // The arming check runs under the same lock as the exiting
            // task's disarm decision, so a lease taken while the old monitor
            // is deciding can never slip between the two and end up
            // unwatched (monitorArmRace).
            let mut running = self
                .monitor_running
                .lock()
                .expect("laya monitor lock poisoned");
            if *running {
                return;
            }
            *running = true;
        }
        let weak = Arc::downgrade(self);

        let interval = self.monitor_interval();
        tokio::spawn(async move {
            // Strikes a previously-answering server has collected: two
            // consecutive silent probes (SHARED_MONITOR_STRIKES) mean gone,
            // one slow probe does not.
            let mut strikes = 0u32;
            loop {
                tokio::time::sleep(interval).await;
                // Upgrade AFTER the sleep, so a manager dropped in the
                // meantime is not resurrected for this tick.
                let Some(manager) = weak.upgrade() else {
                    // Dropped: its Drop released the lease; nothing to reset.
                    return;
                };
                let Some(models) = manager.monitor_lease_step() else {
                    // No shared server any more (stopped, or a private one),
                    // decided under the arm lock: the paths that set up a new
                    // lease re-start the monitor, and a lease taken while we
                    // were deciding keeps THIS task running instead
                    // (monitorArmRace).
                    return;
                };
                // The tick probes a socket (up to 2 s): run it on the
                // blocking pool so it never parks a runtime worker
                // (awaitReadyDocAndBlockingProbe).
                let tick_manager = Arc::clone(&manager);
                let tick = tokio::task::spawn_blocking(move || {
                    tick_manager.shared_tick(strikes)
                })
                .await
                .unwrap_or_else(|err| {
                    eprintln!("warning: the shared laya-serve monitor tick failed: {err}");
                    TickOutcome::Keep
                });
                match tick {
                    TickOutcome::Keep => strikes = 0,
                    TickOutcome::Strike(count) => strikes = count,
                    TickOutcome::Moved => {
                        strikes = 0;
                        eprintln!(
                            "info: the shared laya-serve was replaced by another \
                             instance; re-attaching to the replacement"
                        );
                        manager.recover_shared_server(&models).await;
                    }
                    TickOutcome::Recover => {
                        strikes = 0;
                        eprintln!(
                            "info: the shared laya-serve this instance uses is \
                             gone; acquiring a replacement"
                        );
                        manager.recover_shared_server(&models).await;
                    }
                }
            }
        });
    }

    /// Stop the child only if it still belongs to `generation` — a failed
    /// readiness probe must never kill a newer child that a concurrent
    /// start recorded; it just reports failure and lets the winner run.
    ///
    /// A SHARED child this instance spawned is only ended when nobody else
    /// leases it any more: other instances may have attached to the still
    /// loading server (a cold start), and their lease keeps it alive —
    /// this start then just releases its own lease and lets the watchdog
    /// own the server (transitionLeaks).
    fn stop_if_generation(&self, generation: u64) -> bool {
        let had = self
            .child
            .lock()
            .expect("laya child lock poisoned")
            .take_if(|child| child.generation == generation);
        let Some(child) = had else {
            return false;
        };
        if child.shared {
            let dir = self.service_dir();
            // Release OUR lease first, then count what is left: another
            // instance attached to this still-loading server keeps it.
            self.release_lease();
            let others = shared_service::live_leases(&dir)
                .into_iter()
                .filter(|&pid| pid != self.instance_pid())
                .count();
            if others > 0 {
                eprintln!(
                    "info: this laya-serve start never became ready, but {others} \
                     other instance(s) lease it; leaving the shared server to them"
                );
                release_shared_child(child.child);
            } else {
                // Drop the descriptor first so no later instance attaches to
                // a server we are about to kill.
                shared_service::remove_service_if_pid(&dir, child.child.id());
                child.kill_owned();
            }
        } else {
            child.kill_owned();
        }
        true
    }

    /// Does the live child still belong to `generation`? A readiness probe
    /// that succeeds after its child was superseded (a save-driven stop +
    /// restart landed mid-probe) must not write `Ready` over the winner's
    /// lifecycle or hand callers a dead port.
    fn owns_generation(&self, generation: u64) -> bool {
        self.child
            .lock()
            .expect("laya child lock poisoned")
            .as_ref()
            .is_some_and(|child| child.generation == generation)
    }

    /// The pid + creation identity of the child this instance launched, if
    /// any (the descriptor publishes both, so every later kill of the shared
    /// server can prove the pid was never reused).
    fn child_identity(&self) -> Option<(u32, String)> {
        self.child
            .lock()
            .expect("laya child lock poisoned")
            .as_ref()
            .map(|child| (child.child.id(), child.created.clone()))
    }

    /// Pick a free loopback port (bind :0, read it, drop the listener).
    pub fn alloc_free_port() -> Option<u16> {
        std::net::TcpListener::bind((MANAGED_HOST, 0))
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
    }

    /// Spawn the managed `laya-serve` serving `models` on the
    /// pre-allocated `port` (the one the classifier client was built
    /// against) and record the child tagged with a fresh generation.
    /// `models` is the `LAYA_MODELS` value: a catalog checkpoint id, or a
    /// fine-tuned artifact directory path for the startup fine-tune
    /// hot-swap (best-effort — a laya-serve that cannot load it simply
    /// never gets ready).
    ///
    /// `shared` spawns a DETACHED server (a hidden console of its own, its
    /// own process group, never in this process's kill-on-close job) that
    /// outlives this instance
    /// — the machine-wide service; `false` is the private kill-on-close child
    /// as before. Callers reach this through [`LayaManager::acquire`], which
    /// holds the spawn lock and publishes the descriptor.
    ///
    /// Returns that generation — a caller whose readiness probe fails must
    /// pass it to [`LayaManager::stop_if_generation`] so a concurrent newer
    /// start survives. Does not wait for readiness (see [`await_ready`]).
    pub fn spawn_server(
        &self,
        models: &str,
        port: u16,
        shared: bool,
    ) -> std::io::Result<u64> {
        // Hold the child lock across stop → spawn → record: two concurrent
        // starts (save-driven rewire vs setup autostart vs startup hook)
        // serialize, so the slower task's record can never overwrite the
        // winner's (child, generation) and orphan a live child outside
        // exit/Drop cleanup. `spawn()` is a few syscalls — no await under
        // the lock, only a millisecond-scale stall.
        let mut slot = self.child.lock().expect("laya child lock poisoned");
        if let Some(previous) = slot.take() {
            // A restart of ours never kills a shared server another instance
            // may lease; a private child is ours alone (as before).
            if previous.shared {
                release_shared_child(previous.child);
            } else {
                kill_tree(previous.child, previous.job, &previous.created);
            }
        }
        let generation = self.child_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let serve = self.laya_serve_path();
        #[cfg(test)]
        let serve = self
            .serve_override
            .as_ref()
            .map(|(program, _)| program.clone())
            .unwrap_or(serve);
        if !serve.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "laya-serve is not installed — run the setup in Settings → Classifier",
            ));
        }
        std::fs::create_dir_all(self.base_dir())?;
        let log = std::fs::File::create(self.server_log_path())?;
        let mut cmd = Command::new(&serve);
        #[cfg(test)]
        if let Some((_, args)) = &self.serve_override {
            cmd.args(args);
        }
        cmd.current_dir(self.base_dir())
            .envs(server_env(models, port, &self.hf_cache_dir()))
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        // Unix: make the server the leader of its own process group so
        // kill_tree reaches its descendants as well as the root. Parent-death
        // cleanup on Unix is intentionally out of scope.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        if shared {
            // Detached: the shared service must outlive this instance, so it
            // never joins this process's kill-on-close job (or its console).
            shared_service::detach(&mut cmd);
        } else {
            hide_console_window(&mut cmd);
        }
        let child = cmd.spawn().map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("spawning {} failed: {e}", serve.display()),
            )
        })?;
        // The job handle lives in this host process. If Mnemo exits or is
        // force-ended the OS closes it, and KILL_ON_JOB_CLOSE ends the whole
        // Laya tree even when stop/Drop cannot run. A SHARED child gets no
        // job at all — that is what lets it serve the other instances.
        #[cfg(windows)]
        let job = if shared {
            None
        } else {
            use std::os::windows::io::AsRawHandle as _;
            let job = KillOnCloseJob::assign_raw(child.as_raw_handle());
            if job.is_none() {
                eprintln!(
                    "warning: could not assign laya-serve to a kill-on-close job; \
                     only the root process will be killed on stop"
                );
            }
            job
        };
        #[cfg(not(windows))]
        let job = None;
        let created = crate::process_tree::process_creation_id(child.id()).unwrap_or_default();
        if !shared {
            // A private child means this instance no longer uses the shared
            // server (a fine-tuned artifact, a model mismatch, or a spawn that
            // lost the spawn lock): release the lease so the watchdog can end
            // the shared one once its other holders are gone (transitionLeaks).
            self.release_lease();
        }
        *self
            .models
            .lock()
            .expect("laya models lock poisoned") = Some(models.to_string());
        *slot = Some(ManagedChild {
            child,
            generation,
            job,
            created,
            shared,
        });
        Ok(generation)
    }

    /// The machine-wide lease directory of the managed Laya service
    /// (`<config_dir>/run/laya`).
    pub fn service_dir(&self) -> PathBuf {
        shared_service::service_dir(
            self.base_dir.parent().unwrap_or(&self.base_dir),
            SERVICE_NAME,
        )
    }

    /// This instance's pid in the lease file name (tests override it so two
    /// managers in one test process behave like two instances).
    fn instance_pid(&self) -> u32 {
        #[cfg(test)]
        if let Some(pid) = self.instance_pid_override {
            return pid;
        }
        std::process::id()
    }

    /// The installed watchdog launcher (`mnemo2-mcp`), or a test substitute.
    ///
    /// `None` means no watchdog exists on this machine — a shared service
    /// could then never be ended once nobody used it, so the instance runs a
    /// private server instead (logged once by [`LayaManager::acquire_shared`]).
    fn watchdog_program(&self) -> Option<PathBuf> {
        #[cfg(test)]
        {
            self.watchdog_override.clone()
        }
        #[cfg(not(test))]
        {
            shared_service::watchdog_binary()
        }
    }

    /// The health check for a shared server: laya.rs's own readiness probe
    /// (the documented empty-questions POST) within
    /// [`SHARED_PROBE_TIMEOUT`].
    fn probe_service(&self, port: u16) -> bool {
        #[cfg(test)]
        if let Some(probe) = &self.probe_override {
            return probe(port);
        }
        shared_service::http_post_ok(
            port,
            SYSTEMONE_PATH,
            &probe_body().to_string(),
            SHARED_PROBE_TIMEOUT,
        )
    }

    /// End this instance's previous server because an attach replaces it: a
    /// previous PRIVATE child is this instance's alone and must not keep
    /// running (~1 GB of RAM for nothing); a previous shared child is simply
    /// handed to its watchdog (transitionLeaks).
    fn stop_previous_for_attach(&self) {
        let Some(previous) = self.child.lock().expect("laya child lock poisoned").take()
        else {
            return;
        };
        if previous.shared {
            release_shared_child(previous.child);
        } else {
            kill_tree(previous.child, previous.job, &previous.created);
        }
    }

    /// Attach to the shared server for `models` if one is live for that model,
    /// taking this instance's lease. `None` when there is no usable
    /// descriptor: none at all, one naming a dead pid, one serving another
    /// model (logged), or a lease that could not be taken.
    ///
    /// A LIVE server that does not answer the health probe yet is still
    /// ATTACHED — another instance's cold start is loading its model (minutes,
    /// not the probe's 2 seconds), and a second server beside it is exactly
    /// what the share exists to avoid (coldStartTwoServers). The caller sees
    /// [`LayaServer::Attaching`] and runs the readiness wait instead.
    fn try_attach(&self, models: &str) -> Option<LayaServer> {
        let dir = self.service_dir();
        let info = shared_service::read_service(&dir)?;
        if !info.models.iter().any(|model| model == models) {
            eprintln!(
                "info: the shared laya-serve serves {:?}, not {models:?}; \
                 running a private server",
                info.models
            );
            return None;
        }
        if !crate::process_tree::pid_alive(info.pid) {
            return None;
        }
        let ready = self.probe_service(info.port);
        let lease = match shared_service::take_lease(&dir, self.instance_pid()) {
            Ok(lease) => lease,
            Err(err) => {
                eprintln!("warning: could not lease the shared laya-serve: {err}");
                return None;
            }
        };
        // An attach replaces whatever this instance ran before.
        self.stop_previous_for_attach();
        *self.lease.lock().expect("laya lease lock poisoned") = Some(lease);
        *self
            .models
            .lock()
            .expect("laya models lock poisoned") = Some(models.to_string());
        // Record WHAT this instance now points at, so the monitor can tell
        // "my server is fine" from "the descriptor names someone else's
        // replacement" (staleClientAfterRecovery).
        *self
            .attached
            .lock()
            .expect("laya attached lock poisoned") = Some(AttachedServer::of(&info));
        if ready {
            Some(LayaServer::Attached { port: info.port })
        } else {
            eprintln!(
                "info: the shared laya-serve on port {} is still starting; \
                 attaching to it instead of spawning a second server",
                info.port
            );
            Some(LayaServer::Attaching {
                port: info.port,
                pid: info.pid,
            })
        }
    }

    /// May this instance create the shared service? Only for a catalog
    /// checkpoint (a fine-tuned artifact is instance-specific), only when a
    /// watchdog launcher exists, and only when the service dir is free — a
    /// live descriptor means another server owns it (attach to it instead).
    fn shared_spawn_allowed(&self, dir: &Path, models: &str) -> bool {
        let catalog = checkpoint_catalog()
            .iter()
            .any(|checkpoint| checkpoint.id == models);
        catalog
            && self.watchdog_program().is_some()
            && shared_service::read_service(dir)
                .is_none_or(|info| !crate::process_tree::pid_alive(info.pid))
    }

    /// Attach to the machine-wide shared server for `models` if one is live,
    /// taking this instance's lease. Blocking (one health probe, at most
    /// [`SHARED_PROBE_TIMEOUT`], and only when a descriptor exists).
    ///
    /// The startup attach leg calls this FIRST, so it knows the shared port
    /// before it builds the classifier client; a `None` means "spawn as
    /// before". An attach of a still-loading server returns
    /// [`LayaServer::Attaching`] — the caller points the client at the port,
    /// parks `Starting`, and waits for readiness.
    pub fn attach_shared(&self, models: &str) -> Option<LayaServer> {
        self.try_attach(models)
    }

    /// Get a server for `models`: attach to the machine-wide shared server
    /// when one is live for that model, otherwise spawn — shared (detached,
    /// with a watchdog) for a catalog checkpoint on a machine that has the
    /// watchdog launcher, private (the kill-on-close child, exactly as before)
    /// in every other case.
    ///
    /// `port` is a port the caller already built a classifier client against
    /// (the startup attach leg) and must then be used for the spawn; `None`
    /// allocates one. An attach ignores it — the shared server has its own.
    ///
    /// Serialised by the service spawn lock, so two instances starting at the
    /// same moment produce one server and one attach, never two servers.
    /// Blocking (a process spawn plus, on the attach leg, one health probe):
    /// call it from a blocking task.
    pub fn acquire_on(&self, models: &str, port: Option<u16>) -> std::io::Result<LayaServer> {
        if let Some(attached) = self.try_attach(models) {
            return Ok(attached);
        }
        let dir = self.service_dir();
        let mut outcome: Option<LayaServer> = None;
        let wait = self.spawn_lock_wait();
        shared_service::with_spawn_lock_wait(&dir, wait, |locked| {
            // Another instance may have won the race while we waited.
            if let Some(attached) = self.try_attach(models) {
                outcome = Some(attached);
                return Ok(());
            }
            let port = match port {
                Some(port) => port,
                None => Self::alloc_free_port().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::AddrNotAvailable,
                        "no free loopback port for the managed laya-serve",
                    )
                })?,
            };
            // An unlocked caller NEVER publishes a shared service: without the
            // lock there is no guarantee that exactly one watchdog watches it,
            // and an unwatched server would orphan forever
            // (unlockedSpawnOrphan). It attaches to a live descriptor or
            // spawns the private kill-on-close child.
            let shared = locked && self.shared_spawn_allowed(&dir, models);
            let catalog = checkpoint_catalog().iter().any(|c| c.id == models);
            if !shared && locked && catalog {
                // Locked but private: name the real reason.
                if self.watchdog_program().is_none() {
                    eprintln!(
                        "info: no shared-service watchdog launcher (mnemo2-mcp) was \
                         found; running a private laya-serve. Install Mnemo2's \
                         `mnemo2-mcp` next to the app to share one server between \
                         instances"
                    );
                } else {
                    eprintln!(
                        "info: the shared laya-serve slot is held by a live server \
                         for another model; running a private laya-serve"
                    );
                }
            } else if !shared && !locked {
                eprintln!(
                    "info: the shared laya-serve spawn lock stayed busy; running a \
                     private laya-serve and leaving the shared slot untouched"
                );
            }
            let generation = self.spawn_server(models, port, shared)?;
            if shared {
                self.publish_shared(&dir, models, port, generation)?;
            }
            // The classifier now points at whatever we just spawned — private
            // or shared — so record it exactly like an attach does
            // (staleClientAfterRecovery: the monitor must know what "my
            // server" is).
            if let Some((pid, created)) = self.child_identity() {
                *self
                    .attached
                    .lock()
                    .expect("laya attached lock poisoned") = Some(AttachedServer {
                    pid,
                    port,
                    created,
                });
            }
            outcome = Some(LayaServer::Spawned {
                port,
                generation,
                shared,
            });
            Ok(())
        })?;
        outcome.ok_or_else(|| {
            std::io::Error::other("the laya-serve spawn lock body produced no outcome")
        })
    }

    /// [`LayaManager::acquire_on`] with a freshly allocated port.
    pub fn acquire(&self, models: &str) -> std::io::Result<LayaServer> {
        self.acquire_on(models, None)
    }

    /// Publish the descriptor (with the child's creation identity, so every
    /// later kill of the shared server can prove the pid was never reused),
    /// take this instance's lease and start the watchdog for a just-spawned
    /// shared server.
    ///
    /// Any failure ends that server again (a server with no descriptor or no
    /// watchdog could never be ended) and returns the error, so the caller
    /// reports the start as failed instead of running an unowned process.
    fn publish_shared(
        &self,
        dir: &Path,
        models: &str,
        port: u16,
        generation: u64,
    ) -> std::io::Result<()> {
        let Some((pid, created)) = self.child_identity() else {
            return Err(std::io::Error::other(
                "the spawned laya-serve recorded no pid",
            ));
        };
        let info = ServiceInfo {
            pid,
            port,
            token: String::new(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_ms: shared_service::now_ms(),
            created,
            models: vec![models.to_string()],
        };
        let result = (|| {
            shared_service::write_service(dir, &info)?;
            let lease = shared_service::take_lease(dir, self.instance_pid())?;
            *self.lease.lock().expect("laya lease lock poisoned") = Some(lease);
            let program = self.watchdog_program().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no shared-service watchdog launcher",
                )
            })?;
            shared_service::spawn_detached(
                &program,
                &shared_service::watchdog_args(dir, pid),
                dir,
            )
        })();
        if result.is_err() {
            self.stop_if_generation(generation);
        }
        result
    }
}

/// The shared server this instance's classifier points at — recorded at every
/// attach/spawn so the liveness monitor can tell "my server is fine" from
/// "the descriptor now names someone else's replacement" (a dead port under
/// a `Ready` status otherwise survives another instance's recovery).
#[derive(Clone, Debug, PartialEq, Eq)]
struct AttachedServer {
    pid: u32,
    port: u16,
    created: String,
}

impl AttachedServer {
    fn of(info: &shared_service::ServiceInfo) -> Self {
        Self {
            pid: info.pid,
            port: info.port,
            created: info.created.clone(),
        }
    }
}

/// How this instance got its Laya server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayaServer {
    /// Another instance already serves this model and ANSWERED the health
    /// probe before the lease was taken: ready by definition.
    Attached { port: u16 },
    /// Another instance's server is live for this model but still LOADING (a
    /// cold start): this instance leased it and must run the readiness wait
    /// (see [`await_shared_ready`]) instead of spawning a second server.
    Attaching { port: u16, pid: u32 },
    /// This instance launched the server: `shared` (detached, watchdog-owned)
    /// or the private kill-on-close child.
    Spawned {
        port: u16,
        generation: u64,
        shared: bool,
    },
}

impl LayaServer {
    /// The loopback port the classifier client must point at.
    pub fn port(self) -> u16 {
        match self {
            Self::Attached { port }
            | Self::Attaching { port, .. }
            | Self::Spawned { port, .. } => port,
        }
    }

    /// Is this the machine-wide shared server (as opposed to a private child)?
    pub fn is_shared(self) -> bool {
        matches!(
            self,
            Self::Attached { .. }
                | Self::Attaching { .. }
                | Self::Spawned { shared: true, .. }
        )
    }
}

/// Hand a DETACHED shared server's child to a tiny reaper thread instead of
/// merely dropping it. On Unix a `std::process::Child` that is dropped
/// without a `wait` stays a ZOMBIE once the server exits, and `ps` still
/// lists a zombie — the spawning instance is the server's parent, so its
/// own liveness poll would report the crashed server as alive forever and
/// never recover it (see [`crate::process_tree::pid_alive`], which treats
/// zombies as dead). The server itself keeps running — its lifecycle
/// belongs to the watchdog, exactly as in
/// [`crate::host::shared_service::spawn_detached`]'s watchdog reaper.
fn release_shared_child(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Kill and reap the server's whole process tree (the shared core of `stop` /
/// `stop_if_generation` / the spawn critical section).
///
/// A job kill is the fast path where the child got a kill-on-close job — the
/// job is a kernel object bound to the real process, so it is identity-proof.
/// Every PID-BASED path (the `taskkill` fallback, the Unix group kill and
/// `Child::kill` on Unix) is gated on the process creation identity (`created`,
/// captured at spawn) still matching: pids are recycled, and a reused pid must
/// never be signalled. When the identity can no longer be proven the signals
/// are skipped and logged — `wait` still reaps.
fn kill_tree(mut child: Child, job: Option<KillOnCloseJob>, created: &str) {
    #[cfg(windows)]
    {
        if let Some(job) = job {
            job.kill();
            let _ = child.wait();
            return;
        }
    }
    #[cfg(not(windows))]
    drop(job);
    let pid = child.id();
    match crate::process_tree::process_creation_id(pid) {
        Some(actual) if actual == created => {}
        _ => {
            eprintln!(
                "warning: skipping the laya-serve tree kill for pid {pid}: its \
                 process identity could not be confirmed (pid reuse guard)"
            );
            let _ = child.wait();
            return;
        }
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // Mirror the shell tool's tree kill: `process_group(0)` made the
        // server's pid its process-group id, and the POSIX kill utility
        // accepts a negative pid to signal the whole group.
        let pgid = format!("-{pid}");
        let _ = Command::new("kill")
            .arg("-9")
            .arg(pgid)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The private child must never outlive the app (orphaned `laya-serve`
/// processes would hold the checkpoint in RAM for nothing) — the explicit
/// `RunEvent::Exit` stop is primary, this Drop is the safety net for every
/// other teardown path.
///
/// For a SHARED server the Drop is instead the safety net that releases this
/// instance's lease, so the idle watchdog ends the service once every instance
/// is gone; the server itself is deliberately never killed here.
impl Drop for LayaManager {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Debug without the child/lock details ([`crate::headless::setup::Setup`]'s
/// derived Debug needs it; the raw fields would be noise anyway).
impl std::fmt::Debug for LayaManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LayaManager")
            .field("base_dir", &self.base_dir)
            .field("child", &self.child.lock().map(|slot| slot.is_some()))
            .finish_non_exhaustive()
    }
}

/// A fresh (`Disabled`) status + manager pair — the fallback `IpcState`
/// paths (startup error, project picker) share the pair so a setup run
/// from Settings still surfaces progress through the shared status.
pub fn fresh_disabled() -> (Arc<RwLock<ClassifierStatus>>, Arc<LayaManager>) {
    let status = Arc::new(RwLock::new(ClassifierStatus::Disabled));
    (
        Arc::clone(&status),
        Arc::new(LayaManager::new(Arc::clone(&status))),
    )
}

/// Build the managed classifier client for a running sidecar — the same Laya
/// backend pointed at the loopback server.
pub fn build_managed_classifier(
    port: u16,
    status: Arc<RwLock<ClassifierStatus>>,
) -> Option<Arc<dyn Classifier>> {
    LayaClassifier::new(format!("http://{MANAGED_HOST}:{port}"), status)
        .map(|c| Arc::new(c) as Arc<dyn Classifier>)
        .map_err(|e| eprintln!("error: building the managed Laya client failed: {e}"))
        .ok()
}// ---------------------------------------------------------------------------
// Setup pipeline (Settings → Classifier → Download)
// ---------------------------------------------------------------------------

/// Download the uv single-file binary from the latest GitHub release into
/// `bin/uv[.exe]` (skipped when already present).
async fn ensure_uv(
    sink: &Option<StatusSink>,
    status: &Arc<RwLock<ClassifierStatus>>,
    manager: &LayaManager,
) -> Result<()> {
    if manager.uv_path().is_file() {
        return Ok(());
    }
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let asset = uv_asset_name(os, arch)
        .ok_or_else(|| anyhow!("no uv release asset for this platform ({os} {arch})"))?;
    let client = Client::builder()
        .user_agent("mnemo")
        .build()
        .context("building the uv download client")?;
    let meta: serde_json::Value = client
        .get(UV_RELEASES_API)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .context("fetching the uv release list from GitHub")?
        .json()
        .await
        .context("parsing the uv release list")?;
    let tag = meta["tag_name"].as_str().unwrap_or("latest").to_string();
    let (url, size, digest): (String, Option<u64>, Option<String>) = meta["assets"]
        .as_array()
        .and_then(|assets| {
            assets.iter().find_map(|a| {
                if a["name"].as_str() == Some(asset) {
                    Some((
                        a["browser_download_url"].as_str()?.to_string(),
                        a["size"].as_u64(),
                        a["digest"].as_str().map(str::to_string),
                    ))
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| anyhow!("the uv release {tag} carries no {asset} asset"))?;
    eprintln!("info: downloading uv {tag} ({asset})");

    std::fs::create_dir_all(manager.base_dir().join("bin"))?;
    let archive = manager.base_dir().join(format!("uv-download-{asset}"));
    let mut resp = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .with_context(|| format!("downloading uv {tag}"))?;
    let mut file = std::fs::File::create(&archive)
        .with_context(|| format!("creating {}", archive.display()))?;
    let mut got = 0u64;
    while let Some(chunk) = resp.chunk().await.context("reading the uv download")? {
        file.write_all(&chunk).context("writing the uv download")?;
        got += chunk.len() as u64;
        let progress = size.map_or(0.0, |total| {
            (got as f64 / total as f64).clamp(0.0, 0.99)
        });
        emit_download(sink, status, "uv", progress);
    }
    drop(file);
    if let Some(expected) = &digest {
        verify_sha256(&archive, expected)
            .context("the downloaded uv archive failed its sha256 digest check")?;
    }
    // Extract to a scratch name first, then rename into place: an app kill
    // mid-extract must never leave a partial binary at the fast-path
    // location (`uv_path().is_file()` would accept it forever after).
    let dest = manager.uv_path();
    let binary = exe_name(os, "uv");
    let scratch = manager
        .base_dir
        .join("bin")
        .join(format!(".extract-{binary}"));
    extract_uv_asset(&archive, &scratch, &binary)
        .with_context(|| format!("extracting {binary} from {asset}"))?;
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&scratch, &dest)
        .with_context(|| format!("finishing the uv install at {}", dest.display()))?;
    let _ = std::fs::remove_file(&archive);
    eprintln!("info: uv ready at {}", dest.display());
    Ok(())
}

/// Verify a downloaded file against a GitHub release asset digest
/// (`sha256:<hex>`; a bare hex digest is tolerated) — cheap hardening for a
/// binary the app will later execute.
fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    use sha2::{Digest as _, Sha256};
    let expected = expected.trim();
    let expected = expected.strip_prefix("sha256:").unwrap_or(expected);
    let mut hasher = Sha256::new();
    let mut file =
        std::fs::File::open(path).with_context(|| format!("hashing {}", path.display()))?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(expected) {
        return Err(anyhow!(
            "sha256 mismatch for {}: expected {expected}, got {got}",
            path.display()
        ));
    }
    Ok(())
}

/// Pull `binary` out of a uv release archive (`.zip` on Windows,
/// `.tar.gz` elsewhere) and write it to `dest`. Blocking; small (uv is a
/// single ~20–40 MB file).
fn extract_uv_asset(archive: &Path, dest: &Path, binary: &str) -> Result<()> {
    if archive.extension().is_some_and(|e| e == "zip") {
        let file = std::fs::File::open(archive)?;
        let mut zip = zip::ZipArchive::new(file)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            // zip 9 decodes an entry name fallibly; an undecodable name can
            // never be the (ASCII) `binary`, so skip that entry and keep
            // scanning — same as the pre-9 lossy-name compare did.
            let Ok(name) = entry.name() else { continue };
            if entry.is_dir() || name.rsplit('/').next() != Some(binary) {
                continue;
            }
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            return Ok(());
        }
        Err(anyhow!("{binary} not found inside {}", archive.display()))
    } else {
        let tar = flate2::read::GzDecoder::new(std::fs::File::open(archive)?);
        let mut tar_archive = tar::Archive::new(tar);
        for entry in tar_archive.entries()? {
            let mut entry = entry?;
            if !entry.header().entry_type().is_file()
                || entry.path()?.file_name().map(|f| f == binary) != Some(true)
            {
                continue;
            }
            let mut out = std::fs::File::create(dest)?;
            std::io::copy(&mut entry, &mut out)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755));
            }
            return Ok(());
        }
        Err(anyhow!("{binary} not found inside {}", archive.display()))
    }
}/// Run a child to completion, failing with a stderr/stdout tail on a
/// non-zero exit (the tail is what the Failed status surfaces).
fn run_logged(cmd: &mut Command, what: &str) -> Result<()> {
    hide_console_window(cmd);
    let out = cmd.output().with_context(|| format!("running {what}"))?;
    if out.status.success() {
        return Ok(());
    }
    let mut tail = format!("{} failed with {}:\n", what, out.status);
    for stream in [&out.stderr, &out.stdout] {
        let text = String::from_utf8_lossy(stream);
        let kept = text.chars().rev().take(600).collect::<Vec<_>>();
        let kept: String = kept.into_iter().rev().collect();
        tail.push_str(&kept);
    }
    Err(anyhow!("{tail}"))
}

/// The full setup pipeline: uv → venv → `laya[serve]` → checkpoint
/// pre-download → marker. Progress rides `ClassifierStatus` (pushed to
/// `sink` when one is attached).
async fn run_setup(
    sink: &Option<StatusSink>,
    status: &Arc<RwLock<ClassifierStatus>>,
    manager: &LayaManager,
    checkpoint: &LayaCheckpoint,
) -> Result<()> {
    push_status(sink, status, ClassifierStatus::Installing);
    std::fs::create_dir_all(manager.base_dir())?;

    // 1. uv itself.
    ensure_uv(sink, status, manager).await?;

    // 2 + 3. The venv + packages (network-bound, no progress signal —
    // the Installing badge covers them).
    if !manager.venv_python().is_file() {
        let mut venv = Command::new(manager.uv_path());
        venv.args(["venv", "--python", "3.12"]).arg(manager.venv_dir());
        let venv_dir = manager.venv_dir();
        tokio::task::spawn_blocking(move || run_logged(&mut venv, "creating the Laya Python environment"))
            .await
            .context("the venv task panicked")?
            .with_context(|| format!("creating the venv under {}", venv_dir.display()))?;
    }
    let mut install = Command::new(manager.uv_path());
    install
        .args(["pip", "install", "--python"])
        .arg(manager.venv_python())
        .arg("laya[serve]");
    let what = "installing laya[serve]";
    tokio::task::spawn_blocking(move || run_logged(&mut install, what))
        .await
        .context("the install task panicked")??;

    // 4. The checkpoint, through the laya package itself, into the
    // redirected HF cache — with a dir-size progress poller (the embedder
    // trick).
    emit_download(sink, status, checkpoint.id, 0.0);
    std::fs::create_dir_all(manager.hf_cache_dir())?;
    let poll_status = Arc::clone(status);
    let poll_sink = sink.clone();
    let poll_dir = manager.hf_cache_dir();
    let poll_label = checkpoint.id.to_string();
    let poll_size = checkpoint.size_mb;
    let poll_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let progress = progress_fraction(dir_size(&poll_dir), poll_size);
            emit_download(&poll_sink, &poll_status, &poll_label, progress);
        }
    });
    let mut preload = Command::new(manager.venv_python());
    preload
        .arg("-I")
        .arg("-c")
        .arg(preload_program(checkpoint.subfolder))
        .env("HF_HOME", manager.hf_cache_dir());
    let preload_joined = tokio::task::spawn_blocking(move || {
        run_logged(&mut preload, "downloading the checkpoint")
    })
    .await;
    // Abort the poller BEFORE propagating a JoinError: a panicked preload
    // must not leak the poller, which would keep clobbering the shared
    // status (a phantom progress bar) every 500 ms for the whole session.
    poll_handle.abort();
    preload_joined.context("the checkpoint download task panicked")??;

    // 5. Done — the marker flips the catalog's installed flag.
    std::fs::create_dir_all(manager.base_dir().join("markers"))?;
    std::fs::write(manager.base_dir().join("markers").join(checkpoint.id), b"ok")
        .context("writing the checkpoint marker")?;
    eprintln!("info: Laya runtime ready (checkpoint '{}')", checkpoint.id);
    Ok(())
}

/// Is `status` one of THIS setup's own in-progress states (`Installing`, or
/// a `Downloading` tick for one of its assets — "uv" or the checkpoint
/// id)? The completion arm's restore is conditioned on this: anything else
/// in the status (a rewire's `Starting`, a terminal
/// `Ready`/`Failed`/`Disabled`, a live classifier's self-heal, a second
/// setup's phases) belongs to a concurrent writer and must never be
/// overwritten by a stale snapshot.
fn is_setup_progress(status: &ClassifierStatus, checkpoint_id: &str) -> bool {
    match status {
        ClassifierStatus::Installing => true,
        ClassifierStatus::Downloading { label, .. } => {
            label == checkpoint_id || label == "uv"
        }
        _ => false,
    }
}

/// The autostart decision for a finished setup, derived from the LIVE config
/// when the setup completes. Returns `(autostart, laya_off)`: `autostart`
/// only when the config enables Laya (the runtime is managed-only, so an
/// enabled config always wants the sidecar); `laya_off` — the only case
/// where a post-setup `Disabled` is the honest status — when Laya is off
/// entirely. Re-deriving from the live config means a mid-setup save always
/// wins over the just-finished download.
fn setup_autostart_decision(laya: &LayaConfig) -> (bool, bool) {
    let laya_off = !laya.enabled;
    (!laya_off, laya_off)
}/// Fire-and-forget setup: download the runtime + `checkpoint`, then start
/// the server so the freshly enabled classifier goes live without a
/// restart. The autostart decision is re-derived from the LIVE config
/// (`config`) when the setup finishes — see [`setup_autostart_decision`]
/// — never from a snapshot taken at download start: a mid-setup disable
/// must always win over the just-finished download.
pub fn spawn_setup_task(
    sink: Option<StatusSink>,
    manager: Arc<LayaManager>,
    checkpoint: &'static LayaCheckpoint,
    config: Arc<tokio::sync::Mutex<crate::config::Config>>,
    classifier_slot: Option<ClassifierSlot>,
) {
    tokio::spawn(async move {
        let status = Arc::clone(&manager.status);
        // The autostart leg can attach to (or spawn) the shared server; its
        // liveness monitor needs the slot to re-point the client at a
        // replacement.
        manager.provide_classifier_slot(classifier_slot.clone());
        // The setup's Installing/Downloading ticks overwrite the shared
        // status even while a live classifier serves another checkpoint —
        // snapshot the pre-setup status so the no-autostart completion arm
        // can restore it instead of leaving a phantom ~99% download bar
        // stuck (which would also disable every Download button via the
        // busy flag).
        let pre_setup_status = status
            .read()
            .expect("classifier status lock poisoned")
            .clone();
        let result = run_setup(&sink, &status, &manager, checkpoint).await;
        manager.end_setup();
        // Re-evaluate from the live config at completion time.
        let (autostart, laya_off) = {
            let cfg = config.lock().await;
            setup_autostart_decision(&cfg.general.general.laya)
        };
        match result {
            Ok(()) if autostart => {
                let started =
                    start_managed_server(&sink, Arc::clone(&manager), checkpoint).await;
                // Swap the live classifier slot to the fresh sidecar
                // endpoint (until now it was None — enabled without an
                // install).
                if let (Some(slot), Some(port)) = (&classifier_slot, started) {
                    *slot.write().expect("classifier slot lock poisoned") =
                        build_managed_classifier(port, Arc::clone(&status));
                }
            }
            Ok(()) => {
                // Setup finished without autostart: `Disabled` is only the
                // honest status when Laya is actually off in the config —
                // otherwise leave any live classifier alone instead of
                // clobbering its Ready status.
                if laya_off {
                    push_status(&sink, &status, ClassifierStatus::Disabled);
                } else {
                    // Restore ONLY what this setup itself overwrote: heal
                    // the status back to the pre-setup value when it still
                    // shows one of our own progress states. Any other value
                    // (a rewire's Starting, a terminal Ready/Failed/
                    // Disabled, a live classifier's self-heal) belongs to a
                    // concurrent writer — restoring a stale snapshot over it
                    // could pin a dishonest status with no self-heal path.
                    let current = status
                        .read()
                        .expect("classifier status lock poisoned")
                        .clone();
                    if is_setup_progress(&current, checkpoint.id) {
                        let s = pre_setup_status.clone();
                        push_status(&sink, &status, s);
                    }
                    eprintln!(
                        "info: Laya runtime ready (checkpoint '{}') — the live config \
                         does not select it; the live classifier is untouched",
                        checkpoint.id
                    );
                }
            }
            Err(e) => {
                eprintln!("error: Laya setup failed: {e:#}");
                push_status(&sink, &status, ClassifierStatus::Failed);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Server lifecycle (auto-start / auto-stop)
// ---------------------------------------------------------------------------

/// Wait until the freshly spawned server answers the readiness probe.
///
/// While the checkpoint download is still making progress (the HF cache
/// growing), the idle budget resets — a first `LAYA_MODELS`-scoped start
/// after enabling without a matching setup can legitimately download
/// hundreds of MiB before the socket ever opens. The absolute cap still
/// bounds the wait.
/// The budgets the callers pass in (production: [`PROBE_IDLE_BUDGET`] /
/// [`PROBE_ABSOLUTE_BUDGET`]; tests shrink them to keep an expiry quick).
/// Threading them as parameters keeps this free function pure and lets the
/// test seam live on the manager.
async fn wait_until_ready(
    sink: &Option<StatusSink>,
    status: &Arc<RwLock<ClassifierStatus>>,
    port: u16,
    hf_dir: &Path,
    checkpoint: Option<&LayaCheckpoint>,
    budgets: (Duration, Duration),
) -> bool {
    let (idle_budget, absolute_budget) = budgets;
    let Ok(client) = Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    let url = probe_url(port);
    let body = probe_body();
    let mut last_size = dir_size(hf_dir);
    let mut idle = Duration::ZERO;
    let mut elapsed = Duration::ZERO;
    let tick = Duration::from_millis(750);
    loop {
        // The absolute cap bounds the TOTAL wait unconditionally — a writer
        // that keeps growing the HF dir (a wedged retry loop, a second app
        // instance sharing this cache) must not reset it forever.
        if elapsed >= absolute_budget {
            return false;
        }
        if let Ok(resp) = client.post(&url).json(&body).send().await {
            if resp.status().is_success() {
                return true;
            }
        }
        tokio::time::sleep(tick).await;
        idle += tick;
        elapsed += tick;
        let now_size = dir_size(hf_dir);
        if now_size > last_size {
            last_size = now_size;
            idle = Duration::ZERO;
            // Keep the UI honest: this wait IS a download.
            if let Some(c) = checkpoint {
                let progress = progress_fraction(now_size, c.size_mb);
                emit_download(sink, status, c.id, progress);
            }
        } else if idle >= idle_budget {
            return false;
        }
    }
}

/// Start (or join) the managed `laya-serve` for `checkpoint` and drive the
/// status to `Ready`/`Failed`. Returns the port on success — the caller
/// (setup autostart, Settings rewire, or the startup fine-tune hot-swap)
/// builds/points the classifier client against it.
pub async fn start_managed_server(
    sink: &Option<StatusSink>,
    manager: Arc<LayaManager>,
    checkpoint: &LayaCheckpoint,
) -> Option<u16> {
    start_server_models(sink, manager, checkpoint.id, Some(checkpoint)).await
}

/// Start (or join) the managed `laya-serve` serving `models` (a catalog
/// checkpoint id, or a fine-tuned artifact directory path — the startup
/// fine-tune hot-swap), driving the status Starting → Ready/Failed.
/// `progress` names the catalog checkpoint for download-progress reporting
/// (`None` for a fine-tuned artifact, which needs no download). Returns the
/// port on success.
///
/// Attaches to the machine-wide shared server when one serves `models` —
/// including one that is still LOADING (see [`await_shared_ready`]); see
/// [`LayaManager::acquire`] for when a shared server is spawned instead.
pub async fn start_server_models(
    sink: &Option<StatusSink>,
    manager: Arc<LayaManager>,
    models: &str,
    progress: Option<&LayaCheckpoint>,
) -> Option<u16> {
    let status = Arc::clone(&manager.status);
    push_status(sink, &status, ClassifierStatus::Starting);
    let acquire_manager = Arc::clone(&manager);
    let acquire_models = models.to_string();
    let acquired =
        tokio::task::spawn_blocking(move || acquire_manager.acquire(&acquire_models)).await;
    match acquired {
        Ok(Ok(LayaServer::Attached { port })) => {
            // A shared server answered its health probe before we leased it:
            // it is ready by definition.
            eprintln!("info: attached to the shared laya-serve on port {port}");
            manager.ensure_shared_monitor();
            push_status(sink, &status, ClassifierStatus::Ready);
            Some(port)
        }
        Ok(Ok(LayaServer::Attaching { port, pid })) => {
            await_shared_ready(sink, manager, models, port, pid, None, progress).await
        }
        Ok(Ok(server @ LayaServer::Spawned { generation, .. })) => {
            let waiting = Arc::clone(&manager);
            let port = await_ready(sink, waiting, server.port(), generation, progress).await;
            if server.is_shared() && port.is_some() {
                manager.ensure_shared_monitor();
            }
            port
        }
        Ok(Err(e)) => {
            eprintln!("error: starting the managed laya-serve failed: {e}");
            push_status(sink, &status, ClassifierStatus::Failed);
            None
        }
        Err(join) => {
            eprintln!("error: the managed laya-serve start task failed: {join}");
            push_status(sink, &status, ClassifierStatus::Failed);
            None
        }
    }
}

/// Wait for a shared server another instance is still LOADING (this instance
/// leased it through [`LayaManager::acquire`] without a 2-second health
/// answer), then drive the status to `Ready`.
///
/// The wait budget is the ordinary readiness budget ([`wait_until_ready`]):
/// a model load takes minutes, not seconds. When the server DIES while
/// loading, this instance falls back to acquiring another one — attach to a
/// freshly published server, or spawn one under the lock (cold start by us
/// now) — and re-points `slot` at every port change. A server that stays
/// alive but silent for the whole budget keeps the lease and the monitor:
/// the classifier still points at that port and self-heals to `Ready` on the
/// first successful call, so the status is `Failed` only until the monitor's
/// probe answers — and the watchdog may never end a server a live instance
/// uses (leaselessAttacher).
pub async fn await_shared_ready(
    sink: &Option<StatusSink>,
    manager: Arc<LayaManager>,
    models: &str,
    port: u16,
    pid: u32,
    slot: Option<ClassifierSlot>,
    progress: Option<&LayaCheckpoint>,
) -> Option<u16> {
    #[cfg(test)]
    manager.shared_ready_tasks.fetch_add(1, Ordering::SeqCst);
    let status = Arc::clone(&manager.status);
    let hf_dir = manager.hf_cache_dir();
    let mut current = port;
    let mut current_pid = pid;
    for attempt in 0..2 {
        let ready = wait_until_ready(sink, &status, current, &hf_dir, progress, manager.probe_budgets())
            .await;
        if ready {
            eprintln!("info: the shared laya-serve is ready on port {current}");
            manager.ensure_shared_monitor();
            push_status(sink, &status, ClassifierStatus::Ready);
            return Some(current);
        }
        if crate::process_tree::pid_alive(current_pid) {
            // The server is ALIVE but never answered within the readiness
            // budget. The classifier still points at its port and self-heals
            // to Ready on the first successful call — so this instance KEEPS
            // the lease (the watchdog must not end a server a live instance
            // uses, whatever the status says) and the monitor, which promotes
            // the Failed status back to Ready once the probe answers
            // (leaselessAttacher).
            eprintln!(
                "error: the shared laya-serve on port {current} never answered \
                 within the readiness budget; keeping the lease and watching it"
            );
            manager.ensure_shared_monitor();
            push_status(sink, &status, ClassifierStatus::Failed);
            return None;
        }
        // The server died while loading: acquire another one. The second
        // attempt does not recurse further — a server that dies twice in a
        // row is a machine problem, and `Failed` is the honest answer.
        eprintln!(
            "info: the shared laya-serve on port {current} died while \
             starting; acquiring a replacement"
        );
        manager.release_lease();
        if attempt == 1 {
            push_status(sink, &status, ClassifierStatus::Failed);
            return None;
        }
        let acquiring = Arc::clone(&manager);
        let wanted = models.to_string();
        let acquired =
            tokio::task::spawn_blocking(move || acquiring.acquire(&wanted)).await;
        match acquired {
            Ok(Ok(LayaServer::Attached { port })) => {
                point_slot(&slot, port, &status);
                eprintln!("info: attached to the shared laya-serve on port {port}");
                manager.ensure_shared_monitor();
                push_status(sink, &status, ClassifierStatus::Ready);
                return Some(port);
            }
            Ok(Ok(LayaServer::Attaching { port, pid })) => {
                point_slot(&slot, port, &status);
                current = port;
                current_pid = pid;
                continue;
            }
            Ok(Ok(server @ LayaServer::Spawned { generation, .. })) => {
                let port = server.port();
                point_slot(&slot, port, &status);
                let waiting = Arc::clone(&manager);
                let port = await_ready(sink, waiting, port, generation, progress).await;
                if server.is_shared() && port.is_some() {
                    manager.ensure_shared_monitor();
                }
                return port;
            }
            Ok(Err(e)) => {
                eprintln!("error: replacing the shared laya-serve failed: {e}");
                push_status(sink, &status, ClassifierStatus::Failed);
                return None;
            }
            Err(join) => {
                eprintln!("error: the replacement start task failed: {join}");
                push_status(sink, &status, ClassifierStatus::Failed);
                return None;
            }
        }
    }
    push_status(sink, &status, ClassifierStatus::Failed);
    None
}

/// Point the classifier slot at `port` — every recovery path moves the LIVE
/// client, not just the status (the old client would keep talking to a dead
/// port).
fn point_slot(
    slot: &Option<ClassifierSlot>,
    port: u16,
    status: &Arc<RwLock<ClassifierStatus>>,
) {
    if let Some(slot) = slot {
        *slot.write().expect("classifier slot lock poisoned") =
            build_managed_classifier(port, Arc::clone(status));
    }
}

/// Wait for a just-spawned `laya-serve` (on `port`, generation `generation`)
/// to answer, then drive the status to `Ready`/`Failed`. Returns the port on
/// success.
///
/// This is an async readiness wait, not a one-shot check: it can last for the
/// manager's probe budgets while a model downloads or loads. Callers that must
/// stay responsive spawn it as a task; the actual socket probes use async
/// `reqwest`, and the monitor's plain-socket probe runs on `spawn_blocking`, so
/// neither blocks a Tokio worker (awaitReadyDocAndBlockingProbe).
///
/// Shared with the startup attach leg, which has already built the classifier
/// client against `port` and only needs the status to follow the child.
pub async fn await_ready(
    sink: &Option<StatusSink>,
    manager: Arc<LayaManager>,
    port: u16,
    generation: u64,
    progress: Option<&LayaCheckpoint>,
) -> Option<u16> {
    let status = Arc::clone(&manager.status);
    let hf_dir = manager.hf_cache_dir();
    let ready =
        wait_until_ready(sink, &status, port, &hf_dir, progress, manager.probe_budgets()).await;
    if ready {
        // Confirm this start still owns the child before declaring
        // Ready: a save-driven stop + restart can land between the
        // probe's success and this write, and the winner must keep
        // its own lifecycle (and the callers must not swap to this
        // — possibly dead — port).
        if manager.owns_generation(generation) {
            push_status(sink, &status, ClassifierStatus::Ready);
            eprintln!("info: managed laya-serve ready on port {port}");
            Some(port)
        } else {
            eprintln!(
                "info: the Laya start on port {port} was superseded before \
                 readiness; the newer start owns the lifecycle"
            );
            None
        }
    } else {
        // Scoped teardown: kill only the child THIS start spawned —
        // a concurrent newer start (save-driven rewire) must survive
        // an orphaned probe's failure path AND keep its own status
        // (no Failed overwrite when the generation no longer
        // matched: the winner is healthy).
        if manager.stop_if_generation(generation) {
            eprintln!(
                "error: managed laya-serve never answered on port {port} \
                 (see {})",
                manager.server_log_path().display()
            );
            push_status(sink, &status, ClassifierStatus::Failed);
        } else {
            eprintln!(
                "info: the Laya readiness probe was superseded by a newer \
                 start; keeping its status"
            );
        }
        None
    }
}

/// Finish a start the caller already filled the classifier client for: the
/// spawn decision runs on a blocking task, and when the spawn lock shows that
/// another instance won the race, the caller's `slot` is re-pointed at the
/// shared server instead (a client left on the port we allocated would never
/// answer).
///
/// Used by the startup attach leg, which fills the slot synchronously (so
/// startup never waits) and then hands the start here.
pub async fn finish_start(
    sink: &Option<StatusSink>,
    manager: Arc<LayaManager>,
    models: &str,
    port: u16,
    slot: Option<ClassifierSlot>,
    progress: Option<&LayaCheckpoint>,
) -> Option<u16> {
    let status = Arc::clone(&manager.status);
    let acquiring = Arc::clone(&manager);
    let wanted = models.to_string();
    let acquired =
        tokio::task::spawn_blocking(move || acquiring.acquire_on(&wanted, Some(port))).await;
    match acquired {
        Ok(Ok(LayaServer::Attached { port: shared })) => {
            if shared != port {
                point_slot(&slot, shared, &status);
            }
            eprintln!("info: attached to the shared laya-serve on port {shared}");
            manager.ensure_shared_monitor();
            push_status(sink, &status, ClassifierStatus::Ready);
            Some(shared)
        }
        Ok(Ok(LayaServer::Attaching {
            port: shared,
            pid,
        })) => {
            // Another instance's server is live but still loading: the client
            // points at its port, the status is Starting, and the shared
            // readiness wait below drives Ready (or falls back if it dies).
            point_slot(&slot, shared, &status);
            await_shared_ready(sink, manager, models, shared, pid, slot, progress).await
        }
        Ok(Ok(server @ LayaServer::Spawned { generation, .. })) => {
            let waiting = Arc::clone(&manager);
            let port = await_ready(sink, waiting, server.port(), generation, progress).await;
            if server.is_shared() && port.is_some() {
                manager.ensure_shared_monitor();
            }
            port
        }
        Ok(Err(e)) => {
            eprintln!("error: starting the managed laya-serve failed: {e}");
            push_status(sink, &status, ClassifierStatus::Failed);
            None
        }
        Err(join) => {
            eprintln!("error: the managed laya-serve start task failed: {join}");
            push_status(sink, &status, ClassifierStatus::Failed);
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (pure helpers + offline fs paths only — no network, no uv)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uv_asset_name_covers_the_supported_platforms_and_rejects_unknown() {
        assert_eq!(
            uv_asset_name("windows", "x86_64"),
            Some("uv-x86_64-pc-windows-msvc.zip")
        );
        assert_eq!(
            uv_asset_name("windows", "aarch64"),
            Some("uv-aarch64-pc-windows-msvc.zip")
        );
        assert_eq!(
            uv_asset_name("macos", "x86_64"),
            Some("uv-x86_64-apple-darwin.tar.gz")
        );
        assert_eq!(
            uv_asset_name("macos", "aarch64"),
            Some("uv-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            uv_asset_name("linux", "x86_64"),
            Some("uv-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            uv_asset_name("linux", "aarch64"),
            Some("uv-aarch64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(uv_asset_name("haiku", "x86_64"), None);
    }

    #[test]
    fn exe_name_and_venv_bin_follow_the_os_layout() {
        assert_eq!(exe_name("windows", "uv"), "uv.exe");
        assert_eq!(exe_name("macos", "uv"), "uv");
        let venv = Path::new("/tmp/venv");
        assert_eq!(
            venv_bin(venv, "windows", "python"),
            PathBuf::from("/tmp/venv/Scripts/python.exe")
        );
        assert_eq!(
            venv_bin(venv, "windows", "laya-serve"),
            PathBuf::from("/tmp/venv/Scripts/laya-serve.exe")
        );
        assert_eq!(
            venv_bin(venv, "macos", "laya-serve"),
            PathBuf::from("/tmp/venv/bin/laya-serve")
        );
    }

    #[test]
    fn server_env_binds_loopback_with_the_checkpoint_and_redirected_cache() {
        let hf = Path::new("/app/laya/hf");
        let env = server_env("english", 8123, hf);
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        assert_eq!(get("LAYA_HOST"), "127.0.0.1");
        assert_eq!(get("LAYA_PORT"), "8123");
        assert_eq!(get("LAYA_MODELS"), "english");
        assert_eq!(get("LAYA_PRELOAD"), "1");
        assert_eq!(get("HF_HOME"), hf.display().to_string());
    }

    #[test]
    fn preload_program_targets_the_repo_and_subfolder() {
        let english = preload_program(None);
        assert!(english.contains("import laya"));
        assert!(english.contains("\"convaiinnovations/laya\""));
        assert!(!english.contains("subfolder"));
        let sub = preload_program(Some("some-subfolder"));
        assert!(sub.contains("subfolder=\"some-subfolder\""));
    }

    #[test]
    fn probe_targets_the_loopback_systemone_route() {
        assert_eq!(probe_url(8123), "http://127.0.0.1:8123/v1/systemone");
        let body = probe_body();
        // The documented empty-questions probe: a 200 with no forward pass.
        assert_eq!(body["questions"], serde_json::json!({}));
        assert!(body["state"]["body"].as_str().is_some());
    }

    #[test]
    fn progress_fraction_stays_in_the_zero_to_99_band() {
        assert_eq!(progress_fraction(0, 810), 0.0);
        assert_eq!(progress_fraction(123, 0), 0.0);
        let half = progress_fraction(810 * 512 * 1024, 810);
        assert!((half - 0.5).abs() < 0.01, "half was {half}");
        assert_eq!(progress_fraction(u64::MAX, 810), 0.99);
    }

    #[test]
    fn the_catalog_is_exactly_the_english_checkpoint() {
        assert_eq!(english_checkpoint().id, "english");
        assert_eq!(checkpoint_catalog().len(), 1);
    }

    #[test]
    fn markers_gate_the_installed_flag() {
        let dir = tempfile::tempdir().unwrap();
        let manager = LayaManager::with_base(
            dir.path().to_path_buf(),
            Arc::new(RwLock::new(ClassifierStatus::Disabled)),
        );
        assert!(!manager.is_checkpoint_installed("english"));
        std::fs::create_dir_all(dir.path().join("markers")).unwrap();
        std::fs::write(dir.path().join("markers").join("english"), b"ok").unwrap();
        // Marker alone is not enough — the runtime must exist too.
        assert!(!manager.is_checkpoint_installed("english"));
        std::fs::create_dir_all(
            manager
                .laya_serve_path()
                .parent()
                .expect("laya-serve parent dir"),
        )
        .unwrap();
        std::fs::write(manager.laya_serve_path(), b"stub").unwrap();
        assert!(manager.is_checkpoint_installed("english"));
    }

    #[test]
    fn dir_size_sums_nested_files_and_tolerates_missing_dirs() {
        assert_eq!(dir_size(Path::new("/nonexistent-mnemo-test")), 0);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 1024]).unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested").join("b.bin"), vec![0u8; 2048]).unwrap();
        assert_eq!(dir_size(dir.path()), 1024 + 2048);
    }

    /// Kill the complete fake-server tree before the manager's regular stop,
    /// so a failed assertion can never strand a ping holding the tempdir.
    #[cfg(windows)]
    struct ManagerTreeCleanup<'a> {
        manager: &'a LayaManager,
        parent_pid: u32,
        grandchild_pid: Option<u32>,
    }

    #[cfg(windows)]
    impl Drop for ManagerTreeCleanup<'_> {
        fn drop(&mut self) {
            for (pid, tree) in [
                (self.parent_pid, true),
                (self.grandchild_pid.unwrap_or(0), false),
            ] {
                if pid == 0 {
                    continue;
                }
                let mut command = std::process::Command::new("taskkill");
                command.args(["/PID", &pid.to_string(), "/F"]);
                if tree {
                    command.arg("/T");
                }
                hide_console_window(&mut command);
                let _ = command.stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
            self.manager.stop();
        }
    }

    #[cfg(windows)]
    fn pid_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: fixed query flags, no handle inheritance; the handle is
        // closed on every path below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: the handle comes from OpenProcess above; query access
        // suffices for GetExitCodeProcess.
        let mut exit_code: u32 = 0;
        let probed = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(handle) };
        probed != 0 && exit_code == STILL_ACTIVE as u32
    }

    #[cfg(windows)]
    fn assert_dead_within(pid: u32, description: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if !pid_alive(pid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !pid_alive(pid),
            "{description} (pid {pid}) survived LayaManager::stop"
        );
    }

    #[cfg(windows)]
    fn child_pid(parent: u32) -> u32 {
        let script = format!(
            "$end = [DateTime]::UtcNow.AddSeconds(5); do {{ \
             $p = Get-CimInstance Win32_Process -Filter 'ParentProcessId = {parent}' \
             | Select-Object -First 1; if ($p) {{ \
             [Console]::Out.Write($p.ProcessId); exit 0 }}; \
             Start-Sleep -Milliseconds 100 }} while ([DateTime]::UtcNow -lt $end); exit 1"
        );
        let mut command = std::process::Command::new("powershell");
        command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        hide_console_window(&mut command);
        let output = command
            .output()
            .expect("query the fake server's grandchild");
        assert!(
            output.status.success(),
            "grandchild query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("grandchild pid is UTF-8")
            .trim()
            .parse()
            .expect("grandchild query returned a pid")
    }

    /// Regression: killing only laya-serve left its interpreter descendant
    /// alive, holding the checkpoint after an orderly manager stop.
    #[cfg(windows)]
    #[test]
    fn stopping_the_manager_kills_the_server_tree() {
        let dir = tempfile::tempdir().unwrap();
        let program = PathBuf::from(
            std::env::var_os("COMSPEC").expect("COMSPEC points at the Windows command processor"),
        );
        let manager = LayaManager::with_base(
            dir.path().to_path_buf(),
            Arc::new(RwLock::new(ClassifierStatus::Disabled)),
        )
        .with_serve_command(
            program,
            vec!["/C".to_string(), "ping -n 60 127.0.0.1 >NUL".to_string()],
        );
        manager
            .spawn_server("english", 38123, false)
            .expect("spawn the fake laya-serve tree");
        let parent_pid = manager
            .child
            .lock()
            .expect("laya child lock poisoned")
            .as_ref()
            .expect("spawn recorded the fake server")
            .child
            .id();

        let mut cleanup = ManagerTreeCleanup {
            manager: &manager,
            parent_pid,
            grandchild_pid: None,
        };
        let grandchild_pid = child_pid(parent_pid);
        cleanup.grandchild_pid = Some(grandchild_pid);
        assert!(pid_alive(parent_pid), "the cmd parent must start alive");
        assert!(
            pid_alive(grandchild_pid),
            "the ping grandchild must start alive"
        );

        assert!(manager.stop(), "the fake server was running");

        assert_dead_within(parent_pid, "fake laya-serve parent");
        assert_dead_within(grandchild_pid, "fake python.exe grandchild");
    }

    #[test]
    fn stop_without_a_child_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let manager = LayaManager::with_base(
            dir.path().to_path_buf(),
            Arc::new(RwLock::new(ClassifierStatus::Disabled)),
        );
        assert!(!manager.stop());
        // Stopping twice is safe (the child slot was already empty).
        assert!(!manager.stop());
        // A scoped stop on an empty slot (any generation) is a no-op too.
        assert!(!manager.stop_if_generation(7));
        // An empty slot owns no generation.
        assert!(!manager.owns_generation(7));
    }    // -- extract_uv_asset: the archive boundary must stay traversal-safe --

    fn write_zip_fixture(path: &Path, entries: &[(&str, &[u8])]) {
        let file = std::fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = Default::default();
        for (name, data) in entries {
            zip.start_file(*name, opts).unwrap();
            std::io::Write::write_all(&mut zip, data).unwrap();
        }
        zip.finish().unwrap();
    }

    fn write_targz_fixture(
        path: &Path,
        regular: &[(&str, &[u8])],
        symlink: (&str, &str),
    ) {
        let file = std::fs::File::create(path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        // The symlink lands FIRST: extraction must skip it, not follow it.
        let mut link = tar::Header::new_gnu();
        link.set_size(0);
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_mode(0o777);
        // The decoy must genuinely be NAMED like the binary (set_path is
        // the entry name; set_link_name is only the link target).
        link.set_path(symlink.0).unwrap();
        link.set_link_name(symlink.1).unwrap();
        link.set_cksum();
        tar.append(&link, std::io::empty()).unwrap();
        for (name, data) in regular {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, name, *data).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
    }

    #[test]
    fn extract_uv_asset_zip_picks_the_named_file_and_ignores_entry_paths() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("uv.zip");
        write_zip_fixture(
            &archive,
            &[
                ("../evil.bin", b"evil"),
                ("readme.txt", b"readme"),
                ("uv", b"UV-BYTES"),
            ],
        );
        let dest = dir.path().join("bin").join("uv");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        extract_uv_asset(&archive, &dest, "uv").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"UV-BYTES");
        // Traversal-safety: entry names are never used as write paths, so
        // nothing escaped the dest dir.
        assert!(!dir.path().join("evil.bin").exists());
    }

    #[test]
    fn extract_uv_asset_tar_skips_symlinks_and_matches_the_nested_layout() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("uv.tar.gz");
        // uv's tarballs ship the binary nested (uv-<triple>/uv) — the
        // file-name match must find it, and must skip the decoy symlink.
        write_targz_fixture(
            &archive,
            &[("uv-x86_64-apple-darwin/uv", b"TAR-UV")],
            ("uv", "/etc/passwd"),
        );
        let dest = dir.path().join("bin").join("uv");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        extract_uv_asset(&archive, &dest, "uv").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"TAR-UV");
    }

    #[test]
    fn extract_uv_asset_errors_when_the_binary_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("uv.zip");
        write_zip_fixture(&archive, &[("only-a-readme.txt", b"nothing")]);
        let dest = dir.path().join("bin").join("uv");
        assert!(extract_uv_asset(&archive, &dest, "uv").is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn verify_sha256_accepts_prefixed_digests_and_rejects_mismatches() {
        // sha256("abc")
        const ABC: &str =
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("payload.bin");
        std::fs::write(&f, b"abc").unwrap();
        verify_sha256(&f, &format!("sha256:{ABC}")).unwrap();
        // Case-insensitive hex, and a bare digest without the prefix.
        verify_sha256(&f, ABC.to_ascii_uppercase().as_str()).unwrap();
        assert!(verify_sha256(&f, "sha256:00000000000000000000000000000000").is_err());
    }

    #[test]
    fn setup_autostart_decision_follows_the_live_config() {
        let mut cfg = LayaConfig::default();
        // Off entirely: no autostart, and Disabled is the honest status.
        assert_eq!(setup_autostart_decision(&cfg), (false, true));
        // Enabled: the host owns the runtime, so the finished setup autostarts.
        cfg.enabled = true;
        assert_eq!(setup_autostart_decision(&cfg), (true, false));
    }

    #[test]
    fn is_setup_progress_covers_only_this_setups_own_states() {
        assert!(is_setup_progress(&ClassifierStatus::Installing, "english"));
        assert!(is_setup_progress(
            &ClassifierStatus::Downloading {
                label: "english".into(),
                progress: 0.5
            },
            "english"
        ));
        assert!(is_setup_progress(
            &ClassifierStatus::Downloading {
                label: "uv".into(),
                progress: 0.1
            },
            "english"
        ));
        // A different label's download belongs to a different setup.
        assert!(!is_setup_progress(
            &ClassifierStatus::Downloading {
                label: "other".into(),
                progress: 0.5
            },
            "english"
        ));
        // Every other state belongs to a concurrent writer.
        assert!(!is_setup_progress(&ClassifierStatus::Ready, "english"));
        assert!(!is_setup_progress(&ClassifierStatus::Failed, "english"));
        assert!(!is_setup_progress(&ClassifierStatus::Starting, "english"));
        assert!(!is_setup_progress(&ClassifierStatus::Disabled, "english"));
    }

    // -----------------------------------------------------------------------
    // Shared-service leases (no Python: a scripted process stands in for
    // `laya-serve` and for the instances that lease it)
    // -----------------------------------------------------------------------

    /// A long-lived placeholder process standing in for `laya-serve` or for
    /// another instance. The lease code is about real pids, so the tests use
    /// real ones.
    struct StubProcess(Child);

    impl StubProcess {
        fn start() -> Self {
            #[cfg(windows)]
            {
                let mut cmd = Command::new(
                    std::env::var_os("COMSPEC")
                        .expect("COMSPEC points at the Windows command processor"),
                );
                cmd.args(["/C", "ping -n 120 127.0.0.1 >NUL"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                // No console flash for the stubs either (the same fix as the
                // real spawns — see `shared_service::detach`).
                hide_console_window(&mut cmd);
                let child = cmd.spawn().expect("start the stub process");
                Self(child)
            }
            #[cfg(not(windows))]
            {
                let child = Command::new("/bin/sh")
                    .args(["-c", "sleep 120"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("start the stub process");
                Self(child)
            }
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

    /// End whatever shared server a test left behind: a manager never kills a
    /// shared server (that is the point of the lease), so the test has to.
    struct SharedServiceCleanup {
        dir: PathBuf,
    }

    impl Drop for SharedServiceCleanup {
        fn drop(&mut self) {
            if let Some(info) = shared_service::read_service(&self.dir) {
                // Identity-checked kill first; a test stub whose creation time
                // could not be recorded is still ended — the test spawned it,
                // so it owns it.
                if !shared_service::kill_service_tree(info.pid, &info.created) {
                    let _ = shared_service::kill_detached_tree(info.pid);
                }
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A well-formed descriptor for `pid`: the creation identity is the REAL
    /// one, so the watchdog's and the kill paths' identity checks behave in
    /// tests exactly as in production.
    fn test_descriptor(pid: u32, port: u16, models: &[&str]) -> ServiceInfo {
        ServiceInfo {
            pid,
            port,
            token: String::new(),
            version: "test".into(),
            started_ms: shared_service::now_ms(),
            created: crate::process_tree::process_creation_id(pid).unwrap_or_default(),
            models: models.iter().map(|model| model.to_string()).collect(),
        }
    }

    /// A manager that acts as one instance (`instance_pid`, a live stub
    /// process) and spawns real, scripted processes instead of Python.
    fn shared_test_manager(base: &Path, instance_pid: u32) -> LayaManager {
        shared_test_manager_with_status(
            base,
            instance_pid,
            Arc::new(RwLock::new(ClassifierStatus::Disabled)),
        )
    }

    /// [`shared_test_manager`] around an existing status Arc (the recovery
    /// tests assert on it).
    fn shared_test_manager_with_status(
        base: &Path,
        instance_pid: u32,
        status: Arc<RwLock<ClassifierStatus>>,
    ) -> LayaManager {
        let (program, args) = stub_launcher();
        LayaManager::with_base(base.to_path_buf(), status)
            .with_serve_command(program.clone(), args)
            .with_watchdog(program)
            .with_instance_pid(instance_pid)
    }

    /// Wait until a stub pid is really gone (killed and reaped).
    fn stub_dead_within(pid: u32, within: Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if !crate::process_tree::pid_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        !crate::process_tree::pid_alive(pid)
    }


    /// The scripted stand-in for `laya-serve`: a process that stays alive.
    fn stub_launcher() -> (PathBuf, Vec<String>) {
        #[cfg(windows)]
        {
            (
                PathBuf::from(
                    std::env::var_os("COMSPEC")
                        .expect("COMSPEC points at the Windows command processor"),
                ),
                vec!["/C".to_string(), "ping -n 120 127.0.0.1 >NUL".to_string()],
            )
        }
        #[cfg(not(windows))]
        {
            (
                PathBuf::from("/bin/sh"),
                vec!["-c".to_string(), "sleep 120".to_string()],
            )
        }
    }

    #[test]
    fn second_instance_attaches_instead_of_spawning() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let first_instance = StubProcess::start();
        let second_instance = StubProcess::start();

        let first =
            shared_test_manager(&base, first_instance.pid()).with_probe(|_| true);
        let spawned = first.acquire("english").expect("the first instance spawns");
        assert!(
            matches!(spawned, LayaServer::Spawned { shared: true, .. }),
            "the first instance must spawn the shared server, got {spawned:?}"
        );
        let info = shared_service::read_service(&service).expect("descriptor written");
        assert_eq!(info.port, spawned.port());
        assert_eq!(info.models, vec!["english".to_string()]);

        let second =
            shared_test_manager(&base, second_instance.pid()).with_probe(|_| true);
        let attached = second.acquire("english").expect("the second instance attaches");
        assert!(
            matches!(attached, LayaServer::Attached { .. }),
            "the second instance must attach, got {attached:?}"
        );
        assert_eq!(attached.port(), info.port);
        // Attaching spawned nothing: the descriptor still names the one server.
        assert_eq!(shared_service::read_service(&service).unwrap().pid, info.pid);
        let mut leases = vec![first_instance.pid(), second_instance.pid()];
        leases.sort_unstable();
        assert_eq!(shared_service::live_leases(&service), leases);
    }

    #[test]
    fn stop_releases_only_this_lease() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(&service, &test_descriptor(server.pid(), live_port, &["english"]))
            .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance_a = StubProcess::start();
        let instance_b = StubProcess::start();
        let first = shared_test_manager(&base, instance_a.pid()).with_probe(|_| true);
        let second = shared_test_manager(&base, instance_b.pid()).with_probe(|_| true);
        assert!(matches!(
            first.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert!(matches!(
            second.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));

        assert!(first.stop());
        assert_eq!(
            shared_service::live_leases(&service),
            vec![instance_b.pid()],
            "stop must release only this instance's lease"
        );
        assert!(
            shared_service::read_service(&service).is_some(),
            "one instance leaving must not remove the shared descriptor"
        );
        assert!(
            crate::process_tree::pid_alive(server.pid()),
            "one instance leaving must never kill the shared server"
        );
        second.stop();
        assert!(shared_service::live_leases(&service).is_empty());
    }

    /// Regression (coldStartTwoServers): a LIVE shared server of the same
    /// model that does not answer the health probe yet (another instance's
    /// cold start) is LEASED, not replaced by a second server.
    #[test]
    fn cold_start_attaches_to_a_live_unready_server() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        // A port this test HOLDS silent (a gate never opened): the REAL health
        // probe (no override) refuses the attach — while the descriptor's pid
        // stays alive — and no parallel test can steal the port.
        let (unready_port, _never_answered) = gated_answer_server();
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), unready_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let manager = shared_test_manager(&base, instance.pid());
        let acquired = manager.acquire("english").expect("the unready server is leased");
        assert!(
            matches!(acquired, LayaServer::Attaching { .. }),
            "a live unready server must be attached (Starting), got {acquired:?}"
        );
        assert_eq!(acquired.port(), unready_port);
        assert_eq!(
            shared_service::live_leases(&service),
            vec![instance.pid()],
            "the unready server must be leased"
        );
        assert!(
            manager.child_identity().is_none(),
            "no second server may be spawned beside the cold start"
        );
        assert_eq!(shared_service::read_service(&service).unwrap().pid, server.pid());
    }

    /// A descriptor naming a DEAD pid (the shared server crashed) is replaced
    /// by a respawn under the spawn lock — attach sees nothing to lease.
    #[test]
    fn a_dead_descriptor_is_replaced_by_a_shared_respawn() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let dead = StubProcess::start();
        let dead_pid = dead.pid();
        drop(dead); // the shared server crashed and was reaped
        assert!(stub_dead_within(dead_pid, Duration::from_secs(10)));
        let dead_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(dead_pid, dead_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let manager = shared_test_manager(&base, instance.pid());
        let acquired = manager.acquire("english").expect("a respawn happens");
        assert!(
            matches!(acquired, LayaServer::Spawned { shared: true, .. }),
            "a dead descriptor must be replaced by a shared respawn, got {acquired:?}"
        );
        let info = shared_service::read_service(&service).expect("the respawn is published");
        assert_ne!(info.pid, dead_pid);
        assert!(crate::process_tree::pid_alive(info.pid));
        assert_eq!(shared_service::live_leases(&service), vec![instance.pid()]);
    }

    /// Regression (unlockedSpawnOrphan): a caller that lost the spawn-lock
    /// race never publishes a shared server — it stays private and leaves the
    /// other model's descriptor untouched.
    #[test]
    fn an_unlocked_call_spawns_privately_and_leaves_the_descriptor() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["other-model"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        // A young foreign spawn lock: neither stale takeover nor acquisition
        // applies, so the caller waits out a tiny test shrink of the wait.
        std::fs::create_dir_all(&service).unwrap();
        std::fs::write(service.join("spawn.lock"), b"another-instance").unwrap();
        let instance = StubProcess::start();
        let manager = shared_test_manager(&base, instance.pid())
            .with_spawn_wait(Duration::from_millis(150));
        let acquired = manager.acquire("english").expect("an unlocked call still spawns");
        assert!(
            matches!(acquired, LayaServer::Spawned { shared: false, .. }),
            "an unlocked caller must stay private, got {acquired:?}"
        );
        assert_eq!(
            shared_service::read_service(&service).unwrap().pid,
            server.pid(),
            "the other model's descriptor must stay untouched"
        );
        assert!(
            shared_service::live_leases(&service).is_empty(),
            "a private spawn is not a shared one"
        );
    }

    #[test]
    fn model_mismatch_does_not_attach() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["fine-tuned-artifact"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        // The probe would answer (so the model check is what blocks the
        // attach), and the model this instance wants is the catalog one.
        let manager = shared_test_manager(&base, instance.pid()).with_probe(|_| true);
        let acquired = manager.acquire("english").expect("a mismatch spawns");
        assert!(
            !matches!(acquired, LayaServer::Attached { .. }),
            "a model mismatch must never attach, got {acquired:?}"
        );
        assert_eq!(shared_service::read_service(&service).unwrap().pid, server.pid());
        assert!(shared_service::live_leases(&service).is_empty());
    }

    /// Regression (transitionLeaks a): spawning a PRIVATE child while holding
    /// a shared lease releases the lease.
    #[test]
    fn a_private_spawn_releases_a_stale_shared_lease() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let manager = Arc::new(
            shared_test_manager(&base, instance.pid()).with_probe(|_| true),
        );
        assert!(matches!(
            manager.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert_eq!(shared_service::live_leases(&service), vec![instance.pid()]);

        // The fine-tuned hot-swap spawns a private server: the shared lease
        // must not outlive that transition.
        let private_port = LayaManager::alloc_free_port().expect("a free port");
        manager
            .spawn_server("fine-tuned-artifact", private_port, false)
            .expect("the private spawn runs");
        assert!(
            shared_service::live_leases(&service).is_empty(),
            "a private spawn must release the shared lease"
        );
    }

    /// Regression (transitionLeaks b): a successful attach stops this
    /// instance's previous PRIVATE child instead of leaving it running.
    #[test]
    fn an_attach_stops_the_previous_private_child() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let manager = shared_test_manager(&base, instance.pid()).with_probe(|_| true);
        let private_port = LayaManager::alloc_free_port().expect("a free port");
        manager
            .spawn_server("english", private_port, false)
            .expect("the private spawn runs");
        let previous_pid = manager
            .child_identity()
            .expect("the private child is recorded")
            .0;
        assert!(crate::process_tree::pid_alive(previous_pid));

        // A healthy shared server appears (another instance's).
        let server = StubProcess::start();
        let shared_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), shared_port, &["english"]),
        )
        .unwrap();
        assert!(matches!(
            manager.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert!(
            stub_dead_within(previous_pid, Duration::from_secs(10)),
            "the attach must stop the previous private child (pid {previous_pid})"
        );
        assert!(manager.child_identity().is_none());
    }

    /// Regression (transitionLeaks c): a failed start of a SHARED child spares
    /// it while another instance holds a lease on it.
    #[test]
    fn stop_if_generation_spares_a_shared_child_with_other_leases() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let first_instance = StubProcess::start();
        let second_instance = StubProcess::start();
        let manager_a = shared_test_manager(&base, first_instance.pid());
        let spawned = manager_a.acquire("english").expect("the shared spawn");
        let LayaServer::Spawned {
            generation, shared, ..
        } = spawned
        else {
            panic!("the first instance must spawn the shared server, got {spawned:?}");
        };
        assert!(shared);
        let server_pid = shared_service::read_service(&service).unwrap().pid;

        // A cold-starting second instance leases the still-loading server.
        let manager_b =
            shared_test_manager(&base, second_instance.pid()).with_probe(|_| false);
        assert!(matches!(
            manager_b.acquire("english").unwrap(),
            LayaServer::Attaching { .. }
        ));

        // The spawner's readiness probe "failed": the other lease must spare
        // the server.
        assert!(manager_a.stop_if_generation(generation));
        assert!(
            crate::process_tree::pid_alive(server_pid),
            "a shared child another instance leases must be spared"
        );
        assert!(
            shared_service::read_service(&service).is_some(),
            "the spared server keeps its descriptor"
        );
        assert_eq!(
            shared_service::live_leases(&service),
            vec![second_instance.pid()],
            "the failed start releases only its own lease"
        );
    }

    /// The other arm of transitionLeaks c: with NO other lease, a failed
    /// shared start ends the server and removes the descriptor.
    #[test]
    fn stop_if_generation_ends_an_unshared_shared_child() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let manager = shared_test_manager(&base, instance.pid());
        let spawned = manager.acquire("english").expect("the shared spawn");
        let LayaServer::Spawned { generation, .. } = spawned else {
            panic!("the instance must spawn the shared server, got {spawned:?}")
        };
        let server_pid = shared_service::read_service(&service).unwrap().pid;
        assert!(manager.stop_if_generation(generation));
        assert!(
            stub_dead_within(server_pid, Duration::from_secs(10)),
            "an unshared failed start must end the server (pid {server_pid})"
        );
        assert!(shared_service::read_service(&service).is_none());
    }

    /// A stub HTTP server whose answers a gate flag controls: while the gate
    /// is closed every probe connection is accepted and held silent — a
    /// `laya-serve` still loading its model — and once the test opens the
    /// gate every connection is answered `200` immediately. Deterministic by
    /// construction (no wall-clock "load" window a parallel test run can
    /// outrun), and because the socket is HELD, no other test's port
    /// allocation can ever collide with it.
    fn gated_answer_server() -> (u16, Arc<std::sync::atomic::AtomicBool>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let gate = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opened = Arc::clone(&gate);
        std::thread::spawn(move || {
            for _ in 0..200 {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                // Drain the request (headers + small JSON body).
                let mut buf = [0u8; 2048];
                let mut seen = Vec::new();
                for _ in 0..16 {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            seen.extend_from_slice(&buf[..n]);
                            if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }
                while !opened.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(25));
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                );
            }
        });
        (port, gate)
    }

    /// Regression (coldStartTwoServers): a live-but-unready shared server is
    /// leased, and the ordinary readiness wait drives the status to `Ready`
    /// once it answers — no second server is spawned at any point.
    #[tokio::test]
    async fn an_unready_shared_server_becomes_ready_when_it_answers() {
        let (port, gate) = gated_answer_server();

        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let status = Arc::new(RwLock::new(ClassifierStatus::Disabled));
        let manager = Arc::new(shared_test_manager_with_status(
            &base,
            instance.pid(),
            Arc::clone(&status),
        ));
        let acquired = {
            let blocking = Arc::clone(&manager);
            tokio::task::spawn_blocking(move || blocking.acquire("english"))
                .await
                .expect("the blocking acquire joins")
                .expect("the unready server is leased")
        };
        assert!(
            matches!(acquired, LayaServer::Attaching { .. }),
            "the loading server must be attached, got {acquired:?}"
        );
        let LayaServer::Attaching { port, pid } = acquired else {
            unreachable!("just asserted Attaching")
        };

        // The server finishes loading (the gate opens), and the ordinary
        // readiness budgets deliver the answer well within them.
        gate.store(true, std::sync::atomic::Ordering::SeqCst);
        let ready = crate::host::laya::await_shared_ready(
            &None,
            Arc::clone(&manager),
            "english",
            port,
            pid,
            None,
            None,
        )
        .await;
        assert_eq!(ready, Some(port), "the shared server must become ready");
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Ready
        );
        assert!(
            manager.child_identity().is_none(),
            "no second server may have been spawned"
        );
    }

    /// Regression (sharedCrashNoRecovery): a leasing instance notices a dead
    /// shared server and acquires a replacement (attach to a freshly
    /// published one, or spawn under the lock) — the classifier never stays
    /// silently pointed at a dead port.
    #[tokio::test]
    async fn a_crashed_shared_server_is_replaced() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let status = Arc::new(RwLock::new(ClassifierStatus::Ready));
        let answering = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let manager = Arc::new(
            shared_test_manager_with_status(&base, instance.pid(), Arc::clone(&status))
                .with_probe({
                    let answering = Arc::clone(&answering);
                    move |_| answering.load(std::sync::atomic::Ordering::SeqCst)
                }),
        );
        // The instance leased the healthy server.
        assert!(matches!(
            manager.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert_eq!(manager.shared_tick(0), TickOutcome::Keep);

        // The shared server crashes.
        let crashed_pid = server.pid();
        drop(server);
        answering.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(stub_dead_within(crashed_pid, Duration::from_secs(10)));
        assert_eq!(
            manager.shared_tick(0),
            TickOutcome::Recover,
            "the crash must be detected within one monitor interval"
        );

        let replacement = manager.recover_shared_server("english").await;
        let port = replacement.expect("a replacement server");
        let info = shared_service::read_service(&service).expect("the replacement descriptor");
        assert_ne!(info.pid, crashed_pid, "the descriptor must be replaced");
        assert!(crate::process_tree::pid_alive(info.pid));
        assert_eq!(port, info.port);
        assert_eq!(shared_service::live_leases(&service), vec![instance.pid()]);
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Starting,
            "a spawned replacement is Starting until its readiness probe answers"
        );
    }

    /// The monitor tick's classification on its own: what counts as gone,
    /// what counts as a strike, what keeps waiting, and the Failed→Ready
    /// promotion when the server answers again.
    #[test]
    fn shared_tick_classifies_the_health() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let port = LayaManager::alloc_free_port().expect("a free port");
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        // No descriptor at all: gone.
        let status = Arc::new(RwLock::new(ClassifierStatus::Ready));
        let gone =
            shared_test_manager_with_status(&base, std::process::id(), Arc::clone(&status))
                .with_probe(|_| true);
        assert_eq!(gone.shared_tick(0), TickOutcome::Recover);

        shared_service::write_service(&service, &test_descriptor(server.pid(), port, &["english"]))
            .unwrap();
        // Dead pid: gone.
        let dead_pid = server.pid();
        drop(server);
        assert!(stub_dead_within(dead_pid, Duration::from_secs(10)));
        assert_eq!(gone.shared_tick(0), TickOutcome::Recover);

        // Live server, Ready, answering: keep.
        let server = StubProcess::start();
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), port, &["english"]),
        )
        .unwrap();
        let healthy = shared_test_manager_with_status(
            &base,
            std::process::id(),
            Arc::new(RwLock::new(ClassifierStatus::Ready)),
        )
        .with_probe(|_| true);
        assert_eq!(healthy.shared_tick(0), TickOutcome::Keep);

        // Live server, Ready, silent: the first probe is a strike, the second
        // declares it gone (one slow probe is churn, not death).
        let silent = shared_test_manager_with_status(
            &base,
            std::process::id(),
            Arc::new(RwLock::new(ClassifierStatus::Ready)),
        )
        .with_probe(|_| false);
        assert_eq!(silent.shared_tick(0), TickOutcome::Strike(1));
        assert_eq!(silent.shared_tick(1), TickOutcome::Recover);

        // Live server, still loading (Starting), silent: keep waiting.
        let loading = shared_test_manager_with_status(
            &base,
            std::process::id(),
            Arc::new(RwLock::new(ClassifierStatus::Starting)),
        )
        .with_probe(|_| false);
        assert_eq!(loading.shared_tick(0), TickOutcome::Keep);

        // A Failed attach whose server answers again is promoted to Ready.
        let status = Arc::new(RwLock::new(ClassifierStatus::Failed));
        let healed = shared_test_manager_with_status(
            &base,
            std::process::id(),
            Arc::clone(&status),
        )
        .with_probe(|_| true);
        assert_eq!(healed.shared_tick(0), TickOutcome::Keep);
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Ready,
            "an answering server must promote a Failed status back to Ready"
        );

        // A descriptor naming a DIFFERENT server than the one this instance
        // points at is a Moved: another instance recovered while ours died.
        let instance = StubProcess::start();
        let moved = shared_test_manager(&base, instance.pid()).with_probe(|_| true);
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), port, &["english"]),
        )
        .unwrap();
        assert!(matches!(
            moved.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert_eq!(moved.shared_tick(0), TickOutcome::Keep);
        let replacement = StubProcess::start();
        let other_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(replacement.pid(), other_port, &["english"]),
        )
        .unwrap();
        assert_eq!(
            moved.shared_tick(0),
            TickOutcome::Moved,
            "a descriptor naming another server must move, not keep"
        );
    }

    /// Regression (monitorHardening): one silent probe on a Ready server does
    /// not recover it; the second consecutive one does.
    #[tokio::test]
    async fn two_silent_probes_recover_a_ready_server_and_one_does_not() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let status = Arc::new(RwLock::new(ClassifierStatus::Ready));
        let manager = Arc::new(
            shared_test_manager_with_status(&base, instance.pid(), Arc::clone(&status))
                .with_probe(|_| false) // the server went silent
                .with_monitor_interval(Duration::from_millis(100)),
        );
        // The instance holds a lease on the (now silent) server; the status
        // this test drives by hand stays Ready, which is what the monitor
        // reads.
        assert!(matches!(
            manager.acquire("english").unwrap(),
            LayaServer::Attaching { .. }
        ));
        assert!(!manager.is_monitor_running());

        manager.ensure_shared_monitor();
        // One tick: a strike, not a recovery — the server and the status are
        // untouched.
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            shared_service::read_service(&service).unwrap().pid,
            server.pid(),
            "one silent probe must not replace the server"
        );
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Ready
        );

        // The second consecutive silent probe fires the recovery (observable
        // as the recovery's Starting status; the wedged server is re-attached,
        // not killed).
        let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
        while tokio::time::Instant::now() < deadline {
            if *status.read().expect("classifier status lock poisoned")
                == ClassifierStatus::Starting
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Starting,
            "two consecutive silent probes must fire the recovery"
        );
    }

    /// Regression (monitorPinsManager, lease exit): releasing the lease ends
    /// the monitor task.
    #[tokio::test]
    async fn monitor_exits_when_the_lease_is_released() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let manager = Arc::new(
            shared_test_manager(&base, instance.pid())
                .with_probe(|_| true)
                .with_monitor_interval(Duration::from_millis(50)),
        );
        assert!(matches!(
            manager.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        manager.ensure_shared_monitor();
        assert!(manager.is_monitor_running());

        manager.stop();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while manager.is_monitor_running() && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            !manager.is_monitor_running(),
            "the monitor must exit once the lease is released"
        );
    }

    /// Regression (monitorPinsManager, Weak exit): a manager dropped without
    /// `stop()` still runs its `Drop` (its lease file disappears — the monitor
    /// must not pin the Arc), and the monitor then ends without re-acquiring
    /// servers under a dropped manager.
    #[tokio::test]
    async fn a_dropped_manager_lets_its_drop_run_and_ends_the_monitor() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let server_pid = server.pid();
        let live_port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server.pid(), live_port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let lease_path = service.join(format!("lease-{}.json", instance.pid()));
        let manager = Arc::new(
            shared_test_manager(&base, instance.pid())
                .with_probe(|_| true)
                .with_monitor_interval(Duration::from_millis(50)),
        );
        assert!(matches!(
            manager.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert!(lease_path.exists());
        manager.ensure_shared_monitor();

        // Drop the manager: Drop releases the lease (the file disappears) —
        // only possible when the monitor does NOT hold a strong Arc.
        drop(manager);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while lease_path.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            !lease_path.exists(),
            "the manager's Drop must run despite the monitor (the lease file would \
             otherwise pin the server forever)"
        );

        // The monitor is gone too: killing the server afterwards (nothing
        // watches it) does not make a dropped manager re-acquire.
        drop(server);
        assert!(stub_dead_within(server_pid, Duration::from_secs(10)));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            shared_service::read_service(&service)
                .expect("nobody replaces the descriptor")
                .pid,
            server_pid,
            "the ended monitor must not re-acquire under a dropped manager"
        );
    }

    /// Regression (leaselessAttacher): an attach whose readiness wait expires
    /// on a LIVE server keeps the lease and the monitor (the watchdog may not
    /// end a server a live instance still points at), and the monitor promotes
    /// the Failed status back to Ready when the server finally answers.
    #[tokio::test]
    async fn an_expired_wait_keeps_the_lease_and_the_monitor_recovers() {
        // A server whose answers the test gates: silent while the readiness
        // wait expires (and the 2 s health probe times out), answering once
        // the gate opens — while the monitor watches.
        let (port, gate) = gated_answer_server();

        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let server_pid = server.pid();
        shared_service::write_service(&service, &test_descriptor(server_pid, port, &["english"]))
            .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance = StubProcess::start();
        let status = Arc::new(RwLock::new(ClassifierStatus::Disabled));
        let manager = Arc::new(
            shared_test_manager_with_status(&base, instance.pid(), Arc::clone(&status))
                .with_probe_budget(Duration::from_millis(300))
                .with_monitor_interval(Duration::from_millis(200)),
        );
        let acquired = {
            let blocking = Arc::clone(&manager);
            tokio::task::spawn_blocking(move || blocking.acquire("english"))
                .await
                .expect("the blocking acquire joins")
                .expect("the unready server is leased")
        };
        assert!(matches!(acquired, LayaServer::Attaching { .. }));

        // The readiness wait expires on the live server.
        let waited = await_shared_ready(
            &None,
            Arc::clone(&manager),
            "english",
            acquired.port(),
            match acquired {
                LayaServer::Attaching { pid, .. } => pid,
                _ => unreachable!("just asserted Attaching"),
            },
            None,
            None,
        )
        .await;
        assert_eq!(waited, None, "the wait must expire on a silent server");
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Failed
        );
        // The lease and the monitor SURVIVE the expired wait.
        assert_eq!(
            shared_service::live_leases(&service),
            vec![instance.pid()],
            "an expired wait on a live server must keep the lease"
        );
        assert!(manager.is_monitor_running());

        // The server finally answers (the test opens the gate): the monitor
        // promotes the status.
        gate.store(true, std::sync::atomic::Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while *status.read().expect("classifier status lock poisoned") != ClassifierStatus::Ready
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(
            *status.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Ready,
            "the monitor must promote the expired attach when the server answers"
        );
        assert_eq!(shared_service::live_leases(&service), vec![instance.pid()]);
        assert!(
            manager.is_monitor_running(),
            "the monitor remains armed for later failures after the promotion"
        );
    }

    /// Regression (staleClientAfterRecovery): after one instance recovers and
    /// publishes a replacement, the OTHER instance's monitor sees the
    /// descriptor name a different server, re-attaches, and re-points its
    /// classifier at the new port — instead of staying `Ready` on the dead
    /// one.
    #[tokio::test]
    async fn a_replaced_server_re_points_the_other_instances() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let server = StubProcess::start();
        let server_pid = server.pid();
        let port = LayaManager::alloc_free_port().expect("a free port");
        shared_service::write_service(
            &service,
            &test_descriptor(server_pid, port, &["english"]),
        )
        .unwrap();
        let _cleanup = SharedServiceCleanup {
            dir: service.clone(),
        };
        let instance_a = StubProcess::start();
        let instance_b = StubProcess::start();
        let status_b = Arc::new(RwLock::new(ClassifierStatus::Disabled));
        let manager_a = Arc::new(
            shared_test_manager(&base, instance_a.pid()).with_probe(|_| true),
        );
        let manager_b = Arc::new(
            shared_test_manager_with_status(&base, instance_b.pid(), Arc::clone(&status_b))
                .with_probe(|_| true)
                .with_monitor_interval(Duration::from_millis(100)),
        );
        assert!(matches!(
            manager_a.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        assert!(matches!(
            manager_b.acquire("english").unwrap(),
            LayaServer::Attached { .. }
        ));
        // B's monitor stays DISARMED until A has published: otherwise B can
        // win the recovery itself, and the pid-death check would bypass the
        // Moved path this test is meant to exercise.

        // The shared server dies; instance A recovers and publishes the
        // replacement (the exact path another instance's monitor would run).
        drop(server);
        assert!(stub_dead_within(server_pid, Duration::from_secs(10)));
        manager_a
            .recover_shared_server("english")
            .await
            .expect("instance A replaces the server");
        let info = shared_service::read_service(&service).expect("the replacement descriptor");
        assert_ne!(info.pid, server_pid);
        // Prove the exact branch before letting the real monitor run it.
        assert_eq!(manager_b.shared_tick(0), TickOutcome::Moved);
        manager_b.ensure_shared_monitor();

        // Instance B's monitor moves to the replacement: its recorded server
        // becomes the new (pid, port), the status drives Ready, and it holds
        // a lease — not a dead port under a Ready badge. (The attached record
        // is written inside the attach, the Ready status after it: wait for
        // BOTH, or the poll can catch the gap.)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let moved = manager_b
                .attached_server()
                .is_some_and(|ours| ours.pid == info.pid && ours.port == info.port);
            let ready = *status_b.read().expect("classifier status lock poisoned")
                == ClassifierStatus::Ready;
            if moved && ready {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "instance B must re-attach to the replacement and go Ready"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mut leases = vec![instance_a.pid(), instance_b.pid()];
        leases.sort_unstable();
        assert_eq!(shared_service::live_leases(&service), leases);
        assert_eq!(
            *status_b.read().expect("classifier status lock poisoned"),
            ClassifierStatus::Ready,
            "the re-attach must drive the status like any attach"
        );
    }

    /// Regression (monitorArmRace): pause the REAL monitor after it has read
    /// an absent lease but before it disarms, then take a lease on another
    /// thread. With the old atomic arming check the attacher saw `true` and
    /// returned, after which the old task stored `false` and exited — leaving
    /// the new lease unwatched. The mutex makes the attacher wait for the
    /// disarm and then arm a fresh monitor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lease_taken_during_the_disarm_re_arms_the_real_monitor() {
        let home = tempfile::tempdir().unwrap();
        let base = home.path().join("laya");
        let service = shared_service::service_dir(home.path(), "laya");
        let pause = Arc::new(MonitorPause::default());
        let manager = Arc::new(
            LayaManager::with_base(base, Arc::new(RwLock::new(ClassifierStatus::Disabled)))
                .with_monitor_interval(Duration::from_millis(10))
                .with_monitor_pause(Arc::clone(&pause)),
        );
        // No lease yet, but a known model gets the real task as far as the
        // lease read and its disarm decision.
        *manager.models.lock().expect("laya models lock poisoned") = Some("english".into());
        manager.ensure_shared_monitor();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !pause.entered.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            pause.entered.load(Ordering::SeqCst),
            "the real monitor task must reach the pause between read and disarm"
        );

        // Take the lease, then block trying to arm while the old monitor still
        // holds the arm lock at the pause. No server process is needed for
        // this bookkeeping interleaving.
        let attaching = Arc::clone(&manager);
        let arm = tokio::task::spawn_blocking(move || {
            let lease = shared_service::take_lease(&service, std::process::id())
                .expect("the new lease is written");
            *attaching.lease.lock().expect("laya lease lock poisoned") = Some(lease);
            attaching.ensure_shared_monitor();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        pause.released.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(5), arm)
            .await
            .expect("the armer must not stay blocked after the pause is released")
            .expect("the arming task joins");
        assert!(
            manager.is_monitor_running(),
            "a lease taken during the disarm must leave a monitor armed"
        );
        manager.stop();
    }
}