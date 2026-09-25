// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Managed Laya runtime — download it in Settings, Mnemo runs it.
//!
//! The classifier foundation (`src/memory/classifier.rs`) talks to a
//! `laya-serve` instance over HTTP. In managed mode
//! (`[general.laya] mode = "managed"`) this module makes that instance a
//! detail the user never sees, mirroring the bundled embedding models:
//!
//! 1. **Setup** ([`laya_setup`], Settings → Classifier): download the uv
//!    single-file binary (GitHub releases), create a private venv with a
//!    uv-managed CPython (`uv venv --python 3.12`), `uv pip install
//!    "laya[serve]"`, and pre-download the chosen checkpoint through the
//!    venv's python (`laya.load`) with `HF_HOME` redirected under the app
//!    config dir. Progress rides the shared [`ClassifierStatus`] (emitted as
//!    `classifier://status`), the same way embedder downloads ride
//!    `embedder://status`.
//! 2. **Runtime**: while Laya is enabled + managed + installed, the app
//!    spawns the venv's `laya-serve` bound to `127.0.0.1:<free port>` with
//!    `LAYA_MODELS=<checkpoint>` and probes `POST /v1/systemone` with an
//!    empty `questions` dict (documented to return `200` with
//!    `"answers": {}` and no forward pass) until it answers. Disabling,
//!    reconfiguring, or exiting the app stops the child.
//!
//! Everything lives under `<global_config_dir>/laya/` — `bin/uv[.exe]`,
//! `venv/`, `hf/` (the redirected HuggingFace cache), `server-<pid>.log`
//! (per-PID: concurrent app instances sharing the config dir never
//! truncate each other's log), and `markers/<checkpoint>` (the
//! per-checkpoint setup markers behind the catalog's `installed` flag).
//!
//! Managed mode stays opt-in-gated exactly like the foundation: with
//! `[general.laya]` absent or `enabled = false`, no setup runs, no server
//! spawns, and no classifier client exists.
//!
//! Why a Python sidecar at all: Laya's checkpoints ship as torch
//! transformers weights with a calibrated scoring head (option [MASK]
//! markers, head-budget splitting, per-question temperatures) and no ONNX
//! export — faithful in-process Rust inference is not possible today, so
//! the app hosts the reference implementation instead.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use mnemo::config::{global_config_dir, LayaConfig, LayaMode};
use mnemo::memory::classifier::{Classifier, ClassifierStatus, LayaClassifier};

use crate::ipc::state::IpcState;

/// The HuggingFace repo every checkpoint lives in (a `subfolder` selects
/// the non-English ones).
const LAYA_REPO: &str = "convaiinnovations/laya";

/// The uv GitHub release feed (one single-file binary asset per platform).
const UV_RELEASES_API: &str = "https://api.github.com/repos/astral-sh/uv/releases/latest";

/// The server bind address — always loopback, never exposed.
const MANAGED_HOST: &str = "127.0.0.1";

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
    /// The short id used everywhere: `LAYA_MODELS`, the config value, the
    /// marker file name.
    pub id: &'static str,
    /// The human-facing name shown in Settings.
    pub name: &'static str,
    /// Approximate download size (MiB) — drives the setup progress bar.
    pub size_mb: u64,
    /// The repo subfolder (`None` = the repo root / English checkpoint).
    pub subfolder: Option<&'static str>,
}

/// The curated checkpoint catalog (mirrors the embedder model catalog).
pub fn checkpoint_catalog() -> &'static [LayaCheckpoint] {
    static CATALOG: &[LayaCheckpoint] = &[
        LayaCheckpoint {
            id: "english",
            name: "Laya (English)",
            size_mb: 810,
            subfolder: None,
        },
        LayaCheckpoint {
            id: "multilingual",
            name: "Laya Multilingual (100+ languages)",
            size_mb: 650,
            subfolder: Some("multilingual"),
        },
    ];
    CATALOG
}

/// Resolve a checkpoint id (case-insensitive, whitespace-tolerant) to a
/// catalog entry. Unknown ids fall back to the English checkpoint — a
/// hand-edited config never breaks startup (strict validation lives in
/// `laya_setup`).
pub fn find_checkpoint(id: &str) -> &'static LayaCheckpoint {
    let wanted = id.trim().to_ascii_lowercase();
    checkpoint_catalog()
        .iter()
        .find(|c| c.id == wanted)
        .unwrap_or(&checkpoint_catalog()[0])
}

// ---------------------------------------------------------------------------
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
/// sampling primitive the embedder downloads use.
fn dir_size(path: &Path) -> u64 {
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
/// elsewhere (per-platform branch, not a platform assumption).
fn hide_console_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

// ---------------------------------------------------------------------------
// LayaManager
// ---------------------------------------------------------------------------

/// Owns the managed Laya runtime: where it lives on disk, the running
/// `laya-serve` child, and its port. One instance per app (shared via
/// `IpcState`), so the setup command, the startup hook, the Settings
/// rewire, and app exit all steer the same sidecar.
pub struct LayaManager {
    /// `<global_config_dir>/laya` — everything managed lives under here.
    base_dir: PathBuf,
    /// The shared status the IPC layer surfaces to Settings (the same
    /// `Arc` `IpcState.classifier_status` holds).
    status: Arc<RwLock<ClassifierStatus>>,
    /// The running `laya-serve` child, if any, tagged with the generation it
    /// was spawned for. Guarded so `stop` can be called from app exit while
    /// a start task runs.
    child: Mutex<Option<(Child, u64)>>,
    /// Monotonic spawn generation: a failed readiness probe stops only the
    /// child it spawned ([`LayaManager::stop_if_generation`]) — a newer
    /// child from a concurrent start (startup hook vs save-driven rewire
    /// vs setup autostart) must survive.
    child_generation: AtomicU64,
    /// Guard against two concurrent setup pipelines (double-click).
    setup_in_flight: AtomicBool,
}

impl LayaManager {
    /// Create the manager rooted at the global config dir.
    pub fn new(status: Arc<RwLock<ClassifierStatus>>) -> Self {
        Self::with_base(global_config_dir().join("laya"), status)
    }

    /// Create the manager rooted at `base_dir` (tests use a tempdir).
    pub fn with_base(base_dir: PathBuf, status: Arc<RwLock<ClassifierStatus>>) -> Self {
        Self {
            base_dir,
            status,
            child: Mutex::new(None),
            child_generation: AtomicU64::new(0),
            setup_in_flight: AtomicBool::new(false),
        }
    }

    /// The managed root dir (`<global_config_dir>/laya`).
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
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
    /// Per-PID: concurrent app instances share the global config dir, and a
    /// shared name would have each spawn truncate the other's log.
    pub fn server_log_path(&self) -> PathBuf {
        self.base_dir
            .join(format!("server-{}.log", std::process::id()))
    }

    /// The per-checkpoint setup marker (`markers/<id>`).
    fn checkpoint_marker(&self, checkpoint_id: &str) -> PathBuf {
        self.base_dir.join("markers").join(checkpoint_id)
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

    /// Stop the running child (kill + reap). Returns whether one was
    /// running. Safe to call from app exit or a rewire.
    pub fn stop(&self) -> bool {
        let had = self
            .child
            .lock()
            .expect("laya child lock poisoned")
            .take();
        match had {
            Some((child, _)) => {
                kill_child(child);
                true
            }
            None => false,
        }
    }

    /// Stop the child only if it still belongs to `generation` — a failed
    /// readiness probe must never kill a newer child that a concurrent
    /// start recorded; it just reports failure and lets the winner run.
    fn stop_if_generation(&self, generation: u64) -> bool {
        let had = self
            .child
            .lock()
            .expect("laya child lock poisoned")
            .take_if(|(_, g)| *g == generation);
        match had {
            Some((child, _)) => {
                kill_child(child);
                true
            }
            None => false,
        }
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
            .is_some_and(|(_, g)| *g == generation)
    }

    /// Pick a free loopback port (bind :0, read it, drop the listener).
    pub fn alloc_free_port() -> Option<u16> {
        std::net::TcpListener::bind((MANAGED_HOST, 0))
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port())
    }

    /// Spawn the managed `laya-serve` for `checkpoint_id` on the
    /// pre-allocated `port` (the one the classifier client was built
    /// against) and record the child tagged with a fresh generation.
    /// Returns that generation — a caller whose readiness probe fails must
    /// pass it to [`LayaManager::stop_if_generation`] so a concurrent newer
    /// start survives. Does not wait for readiness (see
    /// [`start_managed_server`]).
    pub fn spawn_server(&self, checkpoint_id: &str, port: u16) -> std::io::Result<u64> {
        // Hold the child lock across stop → spawn → record: two concurrent
        // starts (save-driven rewire vs setup autostart vs startup hook)
        // serialize, so the slower task's record can never overwrite the
        // winner's (child, generation) and orphan a live child outside
        // exit/Drop cleanup. `spawn()` is a few syscalls — no await under
        // the lock, only a millisecond-scale stall.
        let mut slot = self.child.lock().expect("laya child lock poisoned");
        if let Some((child, _)) = slot.take() {
            kill_child(child);
        }
        let generation = self.child_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let serve = self.laya_serve_path();
        if !serve.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "laya-serve is not installed — run the setup in Settings → Classifier",
            ));
        }
        std::fs::create_dir_all(self.base_dir())?;
        let log = std::fs::File::create(self.server_log_path())?;
        let mut cmd = Command::new(&serve);
        cmd.current_dir(self.base_dir())
            .envs(server_env(checkpoint_id, port, &self.hf_cache_dir()))
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        hide_console_window(&mut cmd);
        let child = cmd.spawn().map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("spawning {} failed: {e}", serve.display()),
            )
        })?;
        *slot = Some((child, generation));
        Ok(generation)
    }
}

/// Kill + reap a child (the shared core of `stop` / `stop_if_generation` /
/// the spawn critical section).
fn kill_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The child must never outlive the app (orphaned `laya-serve` processes
/// would hold the checkpoint in RAM for nothing) — the explicit
/// `RunEvent::Exit` stop is primary, this Drop is the safety net for every
/// other teardown path.
impl Drop for LayaManager {
    fn drop(&mut self) {
        self.stop();
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

/// Build the managed classifier client for a running server — the
/// foundation's `build_classifier` covers external mode; managed mode
/// points the same backend at the loopback sidecar.
pub fn build_managed_classifier(
    port: u16,
    status: Arc<RwLock<ClassifierStatus>>,
) -> Option<Arc<dyn Classifier>> {
    LayaClassifier::new(format!("http://{MANAGED_HOST}:{port}"), status)
        .map(|c| Arc::new(c) as Arc<dyn Classifier>)
        .map_err(|e| eprintln!("error: building the managed Laya client failed: {e}"))
        .ok()
}

// ---------------------------------------------------------------------------
// Setup pipeline (Settings → Classifier → Download)
// ---------------------------------------------------------------------------

/// Emit a uv-binary download progress tick.
fn emit_download(app: &Option<AppHandle>, status: &Arc<RwLock<ClassifierStatus>>, label: &str, progress: f64) {
    let s = ClassifierStatus::Downloading {
        label: label.to_string(),
        progress,
    };
    *status.write().expect("classifier status lock poisoned") = s.clone();
    if let Some(app) = app {
        let _ = app.emit("classifier://status", &s);
    }
}

/// Download the uv single-file binary from the latest GitHub release into
/// `bin/uv[.exe]` (skipped when already present).
async fn ensure_uv(
    app: &Option<AppHandle>,
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
    let client = reqwest::Client::builder()
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
        emit_download(app, status, "uv", progress);
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
            if entry.is_dir() || entry.name().rsplit('/').next() != Some(binary) {
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
}

/// Run a child to completion, failing with a stderr/stdout tail on a
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
/// pre-download → marker. Progress rides `ClassifierStatus`.
async fn run_setup(
    app: &Option<AppHandle>,
    status: &Arc<RwLock<ClassifierStatus>>,
    manager: &LayaManager,
    checkpoint: &LayaCheckpoint,
) -> Result<()> {
    let s = ClassifierStatus::Installing;
    *status.write().expect("classifier status lock poisoned") = s.clone();
    if let Some(app) = app {
        let _ = app.emit("classifier://status", &s);
    }
    std::fs::create_dir_all(manager.base_dir())?;

    // 1. uv itself.
    ensure_uv(app, status, manager).await?;

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
    emit_download(app, status, checkpoint.id, 0.0);
    std::fs::create_dir_all(manager.hf_cache_dir())?;
    let poll_status = Arc::clone(status);
    let poll_app = app.clone();
    let poll_dir = manager.hf_cache_dir();
    let poll_label = checkpoint.id.to_string();
    let poll_size = checkpoint.size_mb;
    let poll_handle = tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let progress = progress_fraction(dir_size(&poll_dir), poll_size);
            emit_download(&poll_app, &poll_status, &poll_label, progress);
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

/// The autostart decision for a finished setup, derived from the LIVE
/// config plus the checkpoint the setup just downloaded. Returns
/// `(autostart, laya_off)`: `autostart` only when the config enables
/// managed mode AND this checkpoint is the configured one (downloading a
/// different checkpoint must never silently swap the live model — the
/// save path makes that switch explicit); `laya_off` — the only case
/// where a post-setup `Disabled` is the honest status — when Laya is off
/// entirely. An enabled external-mode config keeps its live classifier
/// untouched.
fn setup_autostart_decision(laya: &LayaConfig, downloaded_id: &str) -> (bool, bool) {
    let configured = laya
        .checkpoint
        .as_deref()
        .map(|c| c.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "english".to_string());
    let laya_off = !laya.enabled;
    let autostart = !laya_off && laya.mode == LayaMode::Managed && configured == downloaded_id;
    (autostart, laya_off)
}

/// Fire-and-forget setup: download the runtime + `checkpoint`, then start
/// the server so the freshly enabled classifier goes live without a
/// restart. The autostart decision is re-derived from the LIVE config
/// (`config`) when the setup finishes — see [`setup_autostart_decision`]
/// — never from a snapshot taken at download start: a mid-setup save
/// (disable, checkpoint switch, mode flip) must always win over the
/// just-finished download.
pub fn spawn_setup_task(
    app: Option<AppHandle>,
    manager: Arc<LayaManager>,
    checkpoint: &'static LayaCheckpoint,
    config: Arc<tokio::sync::Mutex<mnemo::config::Config>>,
    classifier_slot: Option<Arc<RwLock<Option<Arc<dyn Classifier>>>>>,
) {
    tauri::async_runtime::spawn(async move {
        let status = Arc::clone(&manager.status);
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
        let result = run_setup(&app, &status, &manager, checkpoint).await;
        manager.end_setup();
        // Re-evaluate from the live config at completion time.
        let (autostart, laya_off) = {
            let cfg = config.lock().await;
            setup_autostart_decision(&cfg.general.general.laya, checkpoint.id)
        };
        match result {
            Ok(()) if autostart => {
                let started = match LayaManager::alloc_free_port() {
                    Some(port) => {
                        start_managed_server(&app, Arc::clone(&manager), checkpoint, port).await
                    }
                    None => {
                        eprintln!("error: no free loopback port for the managed laya-serve");
                        let s = ClassifierStatus::Failed;
                        *status.write().expect("classifier status lock poisoned") = s.clone();
                        if let Some(app) = &app {
                            let _ = app.emit("classifier://status", &s);
                        }
                        None
                    }
                };
                // Swap the live classifier slot to the fresh sidecar
                // endpoint (until now it was None — managed mode without
                // an install).
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
                    let s = ClassifierStatus::Disabled;
                    *status.write().expect("classifier status lock poisoned") = s.clone();
                    if let Some(app) = &app {
                        let _ = app.emit("classifier://status", &s);
                    }
                } else {
                    // Restore what the setup's progress ticks overwrote —
                    // typically the live classifier's Ready.
                    let s = pre_setup_status.clone();
                    *status.write().expect("classifier status lock poisoned") = s.clone();
                    if let Some(app) = &app {
                        let _ = app.emit("classifier://status", &s);
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
                let s = ClassifierStatus::Failed;
                *status.write().expect("classifier status lock poisoned") = s.clone();
                if let Some(app) = &app {
                    let _ = app.emit("classifier://status", &s);
                }
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
async fn wait_until_ready(
    app: &Option<AppHandle>,
    status: &Arc<RwLock<ClassifierStatus>>,
    port: u16,
    hf_dir: &Path,
    checkpoint: Option<&LayaCheckpoint>,
) -> bool {
    let Ok(client) = reqwest::Client::builder()
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
        if elapsed >= PROBE_ABSOLUTE_BUDGET {
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
                emit_download(app, status, c.id, progress);
            }
        } else if idle >= PROBE_IDLE_BUDGET {
            return false;
        }
    }
}

/// Start (or restart) the managed `laya-serve` on `port` and drive the
/// status to `Ready`/`Failed`. Returns the port on success — the caller
/// (startup hook, setup command, or Settings rewire) builds/points the
/// classifier client against it.
pub async fn start_managed_server(
    app: &Option<AppHandle>,
    manager: Arc<LayaManager>,
    checkpoint: &LayaCheckpoint,
    port: u16,
) -> Option<u16> {
    let status = Arc::clone(&manager.status);
    let s = ClassifierStatus::Starting;
    *status.write().expect("classifier status lock poisoned") = s.clone();
    if let Some(app) = app {
        let _ = app.emit("classifier://status", &s);
    }
    match manager.spawn_server(checkpoint.id, port) {
        Ok(generation) => {
            let hf_dir = manager.hf_cache_dir();
            let ready = wait_until_ready(app, &status, port, &hf_dir, Some(checkpoint)).await;
            if ready {
                // Confirm this start still owns the child before declaring
                // Ready: a save-driven stop + restart can land between the
                // probe's success and this write, and the winner must keep
                // its own lifecycle (and the callers must not swap to this
                // — possibly dead — port).
                if manager.owns_generation(generation) {
                    let s = ClassifierStatus::Ready;
                    *status.write().expect("classifier status lock poisoned") = s.clone();
                    if let Some(app) = app {
                        let _ = app.emit("classifier://status", &s);
                    }
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
                    let s = ClassifierStatus::Failed;
                    *status.write().expect("classifier status lock poisoned") = s.clone();
                    if let Some(app) = app {
                        let _ = app.emit("classifier://status", &s);
                    }
                } else {
                    eprintln!(
                        "info: the Laya readiness probe was superseded by a newer \
                         start; keeping its status"
                    );
                }
                None
            }
        }
        Err(e) => {
            eprintln!("error: starting the managed laya-serve failed: {e:#}");
            let s = ClassifierStatus::Failed;
            *status.write().expect("classifier status lock poisoned") = s.clone();
            if let Some(app) = app {
                let _ = app.emit("classifier://status", &s);
            }
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

/// The wire form of a catalog entry (Settings → Classifier).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LayaCheckpointWire {
    /// The checkpoint id (`"english"` | `"multilingual"`).
    pub id: String,
    /// Human-facing name.
    pub name: String,
    /// Approximate download size (MiB).
    pub size_mb: u64,
    /// Was this checkpoint downloaded by a completed setup?
    pub installed: bool,
}

/// List the managed-Laya checkpoints with their installed flags.
#[tauri::command]
pub async fn laya_catalog(
    state: State<'_, IpcState>,
) -> Result<Vec<LayaCheckpointWire>, String> {
    let manager = &state.runtime.laya;
    Ok(checkpoint_catalog()
        .iter()
        .map(|c| LayaCheckpointWire {
            id: c.id.to_string(),
            name: c.name.to_string(),
            size_mb: c.size_mb,
            installed: manager.is_checkpoint_installed(c.id),
        })
        .collect())
}

/// Download the managed Laya runtime (uv + venv + `laya[serve]` +
/// `checkpoint`). Fire-and-forget: progress arrives via
/// `classifier://status`; when the current config already enables managed
/// Laya, the server starts on success.
#[tauri::command]
pub async fn laya_setup(
    app: AppHandle,
    state: State<'_, IpcState>,
    checkpoint: String,
) -> Result<(), String> {
    let manager = Arc::clone(&state.runtime.laya);
    let Some(cp) = checkpoint_catalog()
        .iter()
        .find(|c| c.id == checkpoint.trim().to_ascii_lowercase())
    else {
        return Err(format!("unknown checkpoint: {checkpoint}"));
    };
    if !manager.begin_setup() {
        return Err("a Laya setup is already running".into());
    }
    // The autostart decision is re-derived from the LIVE config when the
    // (multi-minute) setup finishes — see `setup_autostart_decision` — so
    // no snapshot rides along from download start.
    spawn_setup_task(
        Some(app),
        manager,
        cp,
        state.project.config.clone(),
        Some(state.runtime.classifier.clone()),
    );
    Ok(())
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
        let env = server_env("multilingual", 8123, hf);
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
                .unwrap()
        };
        assert_eq!(get("LAYA_HOST"), "127.0.0.1");
        assert_eq!(get("LAYA_PORT"), "8123");
        assert_eq!(get("LAYA_MODELS"), "multilingual");
        assert_eq!(get("LAYA_PRELOAD"), "1");
        assert_eq!(get("HF_HOME"), hf.display().to_string());
    }

    #[test]
    fn preload_program_targets_the_repo_and_subfolder() {
        let english = preload_program(None);
        assert!(english.contains("import laya"));
        assert!(english.contains("\"convaiinnovations/laya\""));
        assert!(!english.contains("subfolder"));
        let multi = preload_program(Some("multilingual"));
        assert!(multi.contains("subfolder=\"multilingual\""));
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
    fn find_checkpoint_is_tolerant_and_defaults_to_english() {
        assert_eq!(find_checkpoint("english").id, "english");
        assert_eq!(find_checkpoint(" Multilingual ").id, "multilingual");
        // Unknown ids fall back to english rather than breaking a
        // hand-edited config.
        assert_eq!(find_checkpoint("nonsense").id, "english");
        assert_eq!(checkpoint_catalog().len(), 2);
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
        assert!(!manager.is_checkpoint_installed("multilingual"));
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
    }

    // -- extract_uv_asset: the archive boundary must stay traversal-safe --

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
        tar.append(&mut link, std::io::empty()).unwrap();
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
        use mnemo::config::{LayaConfig, LayaMode};
        let mut cfg = LayaConfig::default();
        // Off entirely: no autostart, and Disabled is the honest status.
        assert_eq!(setup_autostart_decision(&cfg, "english"), (false, true));
        // Enabled + managed + the configured (defaulted) checkpoint.
        cfg.enabled = true;
        cfg.mode = LayaMode::Managed;
        assert_eq!(setup_autostart_decision(&cfg, "english"), (true, false));
        // A different checkpoint was downloaded: never swap silently.
        assert_eq!(setup_autostart_decision(&cfg, "multilingual"), (false, false));
        // Explicit checkpoint match (whitespace/case normalized).
        cfg.checkpoint = Some(" Multilingual ".into());
        assert_eq!(setup_autostart_decision(&cfg, "multilingual"), (true, false));
        // Enabled but external: a live external classifier stays untouched.
        cfg.mode = LayaMode::External;
        assert_eq!(setup_autostart_decision(&cfg, "english"), (false, false));
    }
}
