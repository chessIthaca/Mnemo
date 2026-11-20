// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Machine-wide shared services: one process serving every Mnemo instance on
//! the machine.
//!
//! Several instances run side by side on one machine, and a heavyweight
//! backend (today Laya's Python `laya-serve`, later the model host) must not
//! be duplicated per instance. This module is the generic mechanism behind
//! that, modelled on a lease directory under `<config>/run/<service>/`:
//!
//! - `service.json` is the descriptor written by the ONE instance that
//!   spawned the service: [`ServiceInfo`] (pid, port, token, version,
//!   start time, models). It is always written atomically (temp file +
//!   rename), because another instance may read it at any moment.
//! - `lease-<instance pid>.json` is one file per attached instance; its
//!   existence means "a live instance uses this service". A lease whose pid
//!   is dead is stale and gets pruned ([`live_leases`]).
//! - `spawn.lock` serialises the spawn decision, so two instances starting at
//!   the same moment produce one server and one attach, never two servers.
//! - The watchdog ([`run_watchdog`]) is a detached process started by the
//!   spawning instance. It polls [`live_leases`] and ends the server tree
//!   45–60 s after the last lease disappears (the idle budget, counted in
//!   poll quanta), so a crash of every instance never leaves the service
//!   running forever. It exits when `service.json` disappears or names a
//!   different pid, so a stale watchdog can never kill a newer server.
//!
//! Detaching is what makes the service outlive its first instance: the
//! spawning instance must not put the child in its Windows kill-on-close job.
//! On Windows the child is spawned with `CREATE_NO_WINDOW |
//! CREATE_NEW_PROCESS_GROUP` — a HIDDEN console the child and every
//! console-subsystem grandchild (uv, python) inherit, where a console-less
//! `DETACHED_PROCESS` would make each grandchild pop a NEW Terminal window;
//! on Unix with `process_group(0)` (its own process group, so a group kill
//! reaches its descendants) — deliberately NOT `setsid`/`pre_exec`, which
//! would need a `libc` dependency; a GUI app has no controlling terminal, so
//! a new process group is enough to survive the parent's exit.
//!
//! Every function here is synchronous by design: the spawn path is a handful
//! of syscalls and one short health probe, and callers hop onto a blocking
//! task (`tokio::task::spawn_blocking`) rather than this module depending on a
//! runtime.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How long a spawn lock may sit untouched before another instance takes it
/// over (a spawner killed between `create` and `remove` must not wedge every
/// later start).
const SPAWN_LOCK_STALE: Duration = Duration::from_secs(60);

/// How long a spawn waits for the lock before proceeding anyway. Proceeding
/// unlocked is the deliberate SpawnLock behaviour (the MCP helper's
/// `SpawnLock`): a stuck lock must never block the app's startup — but the
/// closure is told `locked == false`, and a caller that did NOT win the lock
/// must never publish a shared service (an unlocked publisher could never be
/// cleaned up by the one watchdog the lock guarantees).
pub const SPAWN_LOCK_WAIT: Duration = Duration::from_secs(30);

/// The watchdog's default poll interval.
pub const WATCHDOG_POLL: Duration = Duration::from_secs(15);

/// The watchdog's default idle budget: this long without a live lease ends
/// the service. The budget is counted in poll quanta, so the kill lands
/// 45–60 s after the last lease disappears (the first silent tick may come
/// any time in the first 15 s window).
pub const WATCHDOG_IDLE: Duration = Duration::from_secs(60);

/// The service descriptor (`service.json`) — written by the instance that
/// spawned the service, read by every instance that may attach to it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceInfo {
    /// The server process id (the watchdog kills and watches this one).
    pub pid: u32,
    /// The loopback port the server answers on.
    pub port: u16,
    /// The bearer token a protected service requires (empty when none).
    pub token: String,
    /// The version of the Mnemo build that spawned it.
    pub version: String,
    /// Unix milliseconds when the service started (diagnostics / staleness).
    pub started_ms: u64,
    /// The pid-reuse guard: the server process's creation identity
    /// ([`crate::process_tree::process_creation_id`]), captured at spawn. Every
    /// kill path compares it before signalling — a recycled pid must never be
    /// killed by a stale descriptor.
    pub created: String,
    /// The model ids the service serves, in the server's own vocabulary.
    pub models: Vec<String>,
}

/// One attached instance's lease (`lease-<pid>.json`). `token` makes each
/// lease guard unique: a newer lease of the same pid rewrites the same
/// file, and only the guard whose token is still in the body may remove it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Lease {
    pid: u32,
    started_ms: u64,
    token: String,
}

/// Lease token source: process-unique even for two `take_lease` calls in the
/// same nanosecond (a process-local sequence number), and cross-process
/// unique because the pid is in it.
fn lease_token() -> String {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{nanos:x}-{}-{seq:x}", std::process::id())
}

/// `<config_dir>/run/<name>` — the lease directory of one shared service.
pub fn service_dir(config_dir: &Path, name: &str) -> PathBuf {
    config_dir.join("run").join(name)
}

/// Unix milliseconds now (best effort: 0 before the epoch).
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write `bytes` to `path` atomically: a sibling temp file, then a rename
/// (which replaces an existing file on both platforms). The service dir and
/// the file itself are restricted to the current user on the way — the same
/// user-only discipline `run_file.rs` applies to its bearer-token run file,
/// adopted now so step 2 can store a real token in `service.json`.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    if let Err(err) = crate::config::keys::restrict_dir_permissions(dir) {
        eprintln!(
            "warning: could not restrict '{}' to the current user: {err}",
            dir.display()
        );
    }
    let temp = dir.join(format!(
        "{}.{}.tmp",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "service".to_string()),
        std::process::id()
    ));
    {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        // Restrict BEFORE the bytes land, so a secret is never world-readable
        // even between write and rename.
        if let Err(err) = crate::config::keys::restrict_permissions(&temp) {
            eprintln!(
                "warning: could not restrict '{}' to the current user: {err}",
                temp.display()
            );
        }
        file.write_all(bytes)?;
        file.flush()?;
    }
    if let Err(err) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = crate::config::keys::restrict_permissions(path) {
        eprintln!(
            "warning: could not restrict '{}' to the current user: {err}",
            path.display()
        );
    }
    Ok(())
}

/// Read the service descriptor; `None` when absent or unreadable (a partial or
/// corrupted file means "no usable service", never a hard error).
pub fn read_service(dir: &Path) -> Option<ServiceInfo> {
    let text = fs::read_to_string(dir.join("service.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Publish the service descriptor atomically.
pub fn write_service(dir: &Path, info: &ServiceInfo) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(info)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    write_atomic(&dir.join("service.json"), &bytes)
}

/// Remove `service.json` only when it still names `server_pid` — a respawned
/// service's descriptor is never deleted by the previous server's owner.
pub fn remove_service_if_pid(dir: &Path, server_pid: u32) {
    if read_service(dir).is_some_and(|info| info.pid == server_pid) {
        let _ = fs::remove_file(dir.join("service.json"));
    }
}

/// One instance's lease on a shared service. Dropping it removes the lease
/// file — but only while its `token` is still the one the file carries: a
/// newer lease of the same pid (a re-attach by the same instance) rewrites
/// the file, and the older guard must not delete it.
pub struct LeaseGuard {
    path: PathBuf,
    pid: u32,
    token: String,
}

impl LeaseGuard {
    /// The instance pid this lease belongs to.
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let ours = fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Lease>(&text).ok())
            .is_some_and(|lease| lease.pid == self.pid && lease.token == self.token);
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Take (or refresh) an instance's lease on the service in `dir`. Each call
/// mints a fresh token, so an instance's older lease guard can never remove a
/// newer one's file.
pub fn take_lease(dir: &Path, instance_pid: u32) -> io::Result<LeaseGuard> {
    fs::create_dir_all(dir)?;
    let lease = Lease {
        pid: instance_pid,
        started_ms: now_ms(),
        token: lease_token(),
    };
    let bytes = serde_json::to_vec_pretty(&lease)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let path = dir.join(format!("lease-{instance_pid}.json"));
    write_atomic(&path, &bytes)?;
    Ok(LeaseGuard {
        path,
        pid: instance_pid,
        token: lease.token,
    })
}

/// The pids of the live leases in `dir`, with this instance included when it
/// holds one. Leases of dead pids (and unreadable ones) are pruned on the way.
pub fn live_leases(dir: &Path) -> Vec<u32> {
    let mut live = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return live;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(pid) = lease_pid(&path) else {
            continue;
        };
        if crate::process_tree::pid_alive(pid) {
            live.push(pid);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
    live.sort_unstable();
    live.dedup();
    live
}

/// The pid a `lease-<pid>.json` file names: its body when readable, its name
/// otherwise (so a truncated file still prunes cleanly).
fn lease_pid(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    if !(name.starts_with("lease-") && name.ends_with(".json")) {
        return None;
    }
    let from_name = name
        .strip_prefix("lease-")
        .and_then(|rest| rest.strip_suffix(".json"))
        .and_then(|digits| digits.parse::<u32>().ok());
    let from_body = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Lease>(&text).ok())
        .map(|lease| lease.pid);
    from_body.or(from_name)
}

/// Removes the spawn-lock file on drop, unless the lock was bypassed.
struct SpawnLockGuard {
    path: Option<PathBuf>,
    token: String,
    /// Did we actually CREATE the lock (`false` after the wait timed out)?
    held: bool,
}

impl Drop for SpawnLockGuard {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        if fs::read_to_string(&path).ok().as_deref() == Some(self.token.as_str()) {
            let _ = fs::remove_file(&path);
        }
    }
}

fn lock_age(path: &Path) -> Option<Duration> {
    let modified = fs::metadata(path).and_then(|meta| meta.modified()).ok()?;
    SystemTime::now().duration_since(modified).ok()
}

/// Run `f` while holding the service's spawn lock, so exactly one instance
/// spawns the service and the others see its descriptor.
///
/// The lock is a `create_new` file carrying a token. A lock older than
/// [`SPAWN_LOCK_STALE`] is considered abandoned and taken over (the
/// `create_new` attempt below is the atomic arbiter). After [`SPAWN_LOCK_WAIT`]
/// `f` runs anyway, but with `locked == false`: a stuck lock must never block
/// an instance's startup, AND a caller that did not win the lock must never
/// publish a shared service (without the lock there is no guarantee exactly
/// one watchdog watches it).
pub fn with_spawn_lock<T>(
    dir: &Path,
    f: impl FnOnce(bool) -> io::Result<T>,
) -> io::Result<T> {
    with_spawn_lock_wait(dir, SPAWN_LOCK_WAIT, f)
}

/// [`with_spawn_lock`] with an explicit lock wait (tests shrink it to keep
/// the unlocked leg fast).
pub fn with_spawn_lock_wait<T>(
    dir: &Path,
    wait: Duration,
    f: impl FnOnce(bool) -> io::Result<T>,
) -> io::Result<T> {
    let guard = acquire_spawn_lock(dir, wait)?;
    let locked = guard.held;
    f(locked)
}

fn acquire_spawn_lock(dir: &Path, wait: Duration) -> io::Result<SpawnLockGuard> {
    fs::create_dir_all(dir)?;
    let path = dir.join("spawn.lock");
    let token = format!("{}:{}", std::process::id(), now_ms());
    let deadline = Instant::now() + wait;
    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(token.as_bytes())?;
                return Ok(SpawnLockGuard {
                    path: Some(path),
                    token,
                    held: true,
                });
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err),
        }
        if lock_age(&path).is_some_and(|age| age > SPAWN_LOCK_STALE) {
            let _ = fs::remove_file(&path);
            continue;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "warning: the shared-service spawn lock in {} stayed busy; \
                 proceeding without it",
                dir.display()
            );
            return Ok(SpawnLockGuard {
                path: None,
                token,
                held: false,
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One loopback HTTP POST, with a hard timeout on connecting, writing and the
/// first response byte: `true` only for a `200`.
///
/// This is the health check an attaching instance runs against a shared
/// service. It is deliberately a plain socket call (no HTTP client, no
/// runtime) so it can run inside the synchronous spawn path.
pub fn http_post_ok(port: u16, path: &str, body: &str, timeout: Duration) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    let Ok(read) = stream.read(&mut buf) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..read]);
    head.starts_with("HTTP/1.") && head.split_whitespace().nth(1) == Some("200")
}

/// Detaching is [`crate::process_tree::detach_command`], re-exported under the
/// name the shared-service spawn paths use: that function is THE one place the
/// hidden-console / own-process-group flags are defined (and it documents why
/// `DETACHED_PROCESS` is wrong — a console-less child makes every
/// console-subsystem grandchild pop a NEW Terminal window; the user saw a
/// swarm of them). The caller must NOT assign the child to a kill-on-close job
/// — surviving the spawn that started it is the whole point.
pub use crate::process_tree::detach_command as detach;

/// The detached spawn flags, re-exported for the shared-service tests and
/// their regression pin — defined in exactly one place,
/// [`crate::process_tree::DETACHED_CREATION_FLAGS`].
#[cfg(windows)]
pub use crate::process_tree::{DETACHED_CREATION_FLAGS, DETACHED_PROCESS};


/// Kill a detached service's whole tree (the watchdog has no job object to
/// lean on): `taskkill /T /F` on Windows, a group kill plus a root kill on
/// Unix. Returns whether the root could be signalled.
///
/// UNSAFE for a stale pid by design: callers that only know a recorded pid
/// must use [`kill_service_tree`], which proves the process identity first.
/// (Test cleanup of processes the test itself spawned is the exception.)
pub fn kill_detached_tree(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(unix)]
    {
        // `detach` made the service its own process-group leader, so the
        // negative pid signals its descendants first; the root kill covers a
        // service that was not group-led after all.
        let _ = Command::new("kill")
            .args(["-9", &format!("-{pid}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
    #[cfg(not(any(windows, unix)))]
    {
        false
    }
}

/// End a shared service's whole tree — but only once the pid's creation
/// identity still matches the recorded one, so a REUSED pid is never killed
/// (the server died, the OS recycled the pid, and a stale descriptor must not
/// signal an innocent process). Returns whether the kill was signalled.
///
/// When the identity cannot be determined the kill is skipped and logged: an
/// unproven pid is never signalled; the cost is one server left to its OS
/// lifetime.
pub fn kill_service_tree(pid: u32, created: &str) -> bool {
    if pid == 0 {
        return false;
    }
    match crate::process_tree::process_creation_id(pid) {
        Some(actual) if actual == created => kill_detached_tree(pid),
        Some(_) => {
            eprintln!(
                "warning: skipping the kill of pid {pid}: its creation time no \
                 longer matches the recorded one (the pid was reused)"
            );
            false
        }
        None => {
            eprintln!(
                "warning: skipping the kill of pid {pid}: its creation time \
                 could not be determined"
            );
            false
        }
    }
}

/// `name.exe` on Windows, `name` elsewhere — the installed-binary naming the
/// host layer shares with the Laya launcher.
fn exe_name(name: &str) -> String {
    crate::host::laya::exe_name(std::env::consts::OS, name)
}

/// The installed `mnemo2-mcp` binary the watchdog runs as.
///
/// It is the right host: a console binary (no Tauri/GUI initialisation) that
/// already dispatches subcommands, and the bundlers ship it next to the app
/// binary on Windows and inside `Mnemo2.app/Contents/MacOS` on macOS — so the
/// GUI instance finds it as a sibling of its own executable. `MNEMO2_MCP_EXE`
/// overrides the lookup (a portable install, a test harness).
pub fn watchdog_binary() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("MNEMO2_MCP_EXE") {
        let path = PathBuf::from(value);
        return path.is_file().then_some(path);
    }
    let me = std::env::current_exe().ok()?;
    let sibling = me.parent()?.join(exe_name("mnemo2-mcp"));
    sibling.is_file().then_some(sibling)
}

/// The argv of one watchdog process for the service in `dir`.
pub fn watchdog_args(dir: &Path, server_pid: u32) -> Vec<String> {
    vec![
        "laya-watchdog".to_string(),
        "--run-dir".to_string(),
        dir.display().to_string(),
        "--server-pid".to_string(),
        server_pid.to_string(),
    ]
}

/// Spawn `binary` detached with `args`, stdin/stdout discarded and stderr in
/// `<dir>/watchdog.log`. Dropping the `Child` leaves it running; a helper
/// thread reaps it when it exits so it never becomes a zombie.
pub fn spawn_detached(binary: &Path, args: &[String], dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let log = File::create(dir.join("watchdog.log"))?;
    let mut cmd = Command::new(binary);
    cmd.args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log));
    detach(&mut cmd);
    let mut child = cmd.spawn().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("spawning {} failed: {err}", binary.display()),
        )
    })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The watchdog loop: hold the service in `dir` open while a live instance
/// leases it, and end it once nobody has for `idle`.
///
/// Exits (never panics) when `service.json` is gone, when it names a different
/// pid than the server this watchdog was started for (a respawn), when that
/// server is already dead, or after it killed an idle server. Removing the
/// descriptor happens only when it still names our pid.
pub fn run_watchdog(dir: &Path, server_pid: u32, poll: Duration, idle: Duration) -> io::Result<()> {
    let mut empty_for = Duration::ZERO;
    loop {
        let Some(info) = read_service(dir) else {
            // The service was stopped and cleaned up (or replaced).
            return Ok(());
        };
        if info.pid != server_pid {
            // A newer server owns the directory now: never kill it.
            return Ok(());
        }
        if !crate::process_tree::pid_alive(info.pid) {
            remove_service_if_pid(dir, server_pid);
            return Ok(());
        }
        if live_leases(dir).is_empty() {
            empty_for += poll;
            if empty_for >= idle {
                // Prove the pid is still the server this watchdog was started
                // for before signalling anything (the pid-reuse guard).
                kill_service_tree(info.pid, &info.created);
                remove_service_if_pid(dir, server_pid);
                return Ok(());
            }
        } else {
            empty_for = Duration::ZERO;
        }
        std::thread::sleep(poll);
    }
}

/// Parse and run the `laya-watchdog` command line (the hidden `mnemo2-mcp`
/// subcommand): `--run-dir <dir> --server-pid <pid>` plus optional
/// `--poll-ms`/`--idle-ms` (defaults [`WATCHDOG_POLL`]/[`WATCHDOG_IDLE`]).
/// `Err` carries a one-line reason for the console.
///
/// The `--run-dir` must be a service directory — `<config>/run/<service>` —
/// so a stray process cannot point the watchdog at an arbitrary directory,
/// and the only pid ever signalled is the one `service.json` names, whose
/// recorded creation time must match (see [`kill_service_tree`]).
pub fn run_watchdog_command(args: &[String]) -> Result<(), String> {
    let mut dir: Option<PathBuf> = None;
    let mut server_pid: Option<u32> = None;
    let mut poll = WATCHDOG_POLL;
    let mut idle = WATCHDOG_IDLE;
    let mut rest = args.iter();
    while let Some(flag) = rest.next() {
        let (flag, inline) = match flag.split_once('=') {
            Some((flag, value)) => (flag, Some(value.to_string())),
            None => (flag.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| rest.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--run-dir" => dir = Some(PathBuf::from(value()?)),
            "--server-pid" => {
                server_pid = Some(
                    value()?
                        .parse()
                        .map_err(|err| format!("--server-pid is not a pid: {err}"))?,
                )
            }
            "--poll-ms" => {
                poll = Duration::from_millis(
                    value()?
                        .parse()
                        .map_err(|err| format!("--poll-ms is not a number: {err}"))?,
                )
            }
            "--idle-ms" => {
                idle = Duration::from_millis(
                    value()?
                        .parse()
                        .map_err(|err| format!("--idle-ms is not a number: {err}"))?,
                )
            }
            other => return Err(format!("laya-watchdog: unknown argument {other}")),
        }
    }
    let dir = dir.ok_or_else(|| "laya-watchdog: --run-dir is required".to_string())?;
    if !is_service_dir(&dir) {
        return Err(format!(
            "laya-watchdog: --run-dir must be a service directory \
             (<config>/run/<service>): {}",
            dir.display()
        ));
    }
    let server_pid =
        server_pid.ok_or_else(|| "laya-watchdog: --server-pid is required".to_string())?;
    run_watchdog(&dir, server_pid, poll, idle)
        .map_err(|err| format!("laya-watchdog: {err}: {}", dir.display()))
}

/// Is `dir` shaped like a service directory — `<config>/run/<service>`, with
/// a plain single-component service name? This is the `laya-watchdog` command
/// line's guard: the watchdog must only ever operate on the lease area, never
/// on an arbitrary directory a caller names.
fn is_service_dir(dir: &Path) -> bool {
    let under_run = dir
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "run");
    let plain_name = dir.file_name().is_some_and(|name| {
        name.to_str().is_some_and(|name| {
            !name.is_empty()
                && !name.starts_with('.')
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
    });
    under_run && plain_name
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::process::Child;
    use std::time::{Duration, Instant};

    use super::*;

    /// A process that stays alive until it is killed — the stand-in for a
    /// service process in the liveness and watchdog tests.
    /// A process that stays alive until it is killed — the stand-in for a
    /// service process in the liveness and watchdog tests. Windows: no
    /// console window for the stub either (see [`detach`]).
    pub(crate) fn long_lived_child() -> Child {
        #[cfg(windows)]
        {
            let mut cmd = Command::new(
                std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
            );
            cmd.args(["/C", "ping -n 120 127.0.0.1 >NUL"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            use std::os::windows::process::CommandExt as _;
            cmd.creation_flags(DETACHED_CREATION_FLAGS);
            cmd.spawn().expect("spawn the stub service process")
        }
        #[cfg(not(windows))]
        {
            Command::new("/bin/sh")
                .args(["-c", "sleep 120"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn the stub service process")
        }
    }

    /// Always end a stub process, even when an assertion failed first.
    pub(crate) struct StubChild(pub Child);

    impl StubChild {
        pub(crate) fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for StubChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_dead(pid: u32, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            if !crate::process_tree::pid_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        !crate::process_tree::pid_alive(pid)
    }

    /// A well-formed descriptor for `pid`: the creation identity is the REAL
    /// one, so the watchdog's identity checks behave exactly as in production.
    fn test_service(pid: u32, port: u16, models: &[&str]) -> ServiceInfo {
        ServiceInfo {
            pid,
            port,
            token: String::new(),
            version: "test".into(),
            started_ms: now_ms(),
            created: crate::process_tree::process_creation_id(pid).unwrap_or_default(),
            models: models.iter().map(|model| model.to_string()).collect(),
        }
    }

    #[test]
    fn service_descriptor_round_trips_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_dir(dir.path(), "laya");
        assert_eq!(service, dir.path().join("run").join("laya"));
        write_service(
            &service,
            &ServiceInfo {
                pid: 4242,
                port: 8123,
                token: "t".into(),
                version: "2.6.0".into(),
                started_ms: 7,
                created: "creation-identity".into(),
                models: vec!["english".into()],
            },
        )
        .unwrap();
        let read = read_service(&service).unwrap();
        assert_eq!(read.pid, 4242);
        assert_eq!(read.models, vec!["english".to_string()]);
        assert_eq!(read.created, "creation-identity");
        // No temp file is left behind by the atomic write.
        let leftovers: Vec<_> = std::fs::read_dir(&service)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn dead_lease_is_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let stub = StubChild(long_lived_child());
        let pid = stub.pid();
        let lease = take_lease(dir.path(), pid).unwrap();
        // Leak the guard: the point is the PRUNE, not the drop path.
        std::mem::forget(lease);
        assert_eq!(live_leases(dir.path()), vec![pid]);
        drop(stub); // the instance "died"
        assert!(wait_dead(pid, Duration::from_secs(10)), "stub {pid} survived");
        assert!(live_leases(dir.path()).is_empty());
        assert!(!dir.path().join(format!("lease-{pid}.json")).exists());
    }

    #[test]
    fn lease_drop_removes_only_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let lease = take_lease(dir.path(), me).unwrap();
        assert_eq!(lease.pid(), me);
        assert!(dir.path().join(format!("lease-{me}.json")).exists());
        drop(lease);
        assert!(!dir.path().join(format!("lease-{me}.json")).exists());
    }

    /// Regression (leaseGenerations): a newer lease of the same pid rewrites
    /// the same file; the older guard's drop must not delete it, or the
    /// watchdog would kill a server another lease still uses.
    #[test]
    fn a_newer_lease_of_the_same_pid_survives_an_older_guards_drop() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        let old = take_lease(dir.path(), me).unwrap();
        let new = take_lease(dir.path(), me).unwrap();
        drop(old);
        assert!(
            dir.path().join(format!("lease-{me}.json")).exists(),
            "the older guard's drop removed the newer lease"
        );
        drop(new);
        assert!(!dir.path().join(format!("lease-{me}.json")).exists());
    }

    /// The descriptor and the lease files are readable by the current user
    /// only, exactly like the app's run file — a prerequisite for step 2
    /// storing a real bearer token in the descriptor.
    #[cfg(unix)]
    #[test]
    fn service_and_lease_files_are_user_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let service = service_dir(dir.path(), "laya");
        write_service(&service, &test_service(4242, 8123, &["english"])).unwrap();
        take_lease(&service, 4242).unwrap();
        let mode = |path: std::path::PathBuf| {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        };
        assert_eq!(mode(service.join("service.json")), 0o600);
        assert_eq!(mode(service.join("lease-4242.json")), 0o600);
    }

    /// Regression (unlockedSpawnOrphan): a caller that could not take the lock
    /// learns `locked == false` and must stay private.
    #[test]
    fn a_busy_spawn_lock_reports_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        // A lock somebody else holds, young enough to dodge the stale takeover.
        std::fs::write(dir.path().join("spawn.lock"), b"another-instance").unwrap();
        let locked = with_spawn_lock_wait(dir.path(), Duration::from_millis(100), |locked| {
            Ok(locked)
        })
        .unwrap();
        assert!(!locked, "the busy lock must be reported as not held");
        // The foreign lock is left exactly as it was.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("spawn.lock")).unwrap(),
            "another-instance"
        );
    }

    #[test]
    fn spawn_lock_serialises_and_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let ran = with_spawn_lock(dir.path(), |_| {
            assert!(dir.path().join("spawn.lock").exists());
            Ok(7)
        })
        .unwrap();
        assert_eq!(ran, 7);
        assert!(!dir.path().join("spawn.lock").exists());
    }

    #[test]
    fn spawn_lock_takes_over_a_stale_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        let lock = dir.path().join("spawn.lock");
        std::fs::write(&lock, b"dead-owner").unwrap();
        let old = SystemTime::now() - SPAWN_LOCK_STALE - Duration::from_secs(5);
        File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(with_spawn_lock(dir.path(), |_| Ok(true)).unwrap());
        assert!(!lock.exists());
    }

    #[test]
    fn http_post_ok_reads_only_a_200() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for status in ["200 OK", "500 Oops"] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 512];
                let _ = stream.read(&mut buf);
                let reply = format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\n\r\n{{}}");
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        let timeout = Duration::from_secs(2);
        assert!(http_post_ok(port, "/v1/systemone", "{}", timeout));
        assert!(!http_post_ok(port, "/v1/systemone", "{}", timeout));
        server.join().unwrap();
        // Nothing listens any more: a refused connection is a failed probe.
        assert!(!http_post_ok(port, "/v1/systemone", "{}", timeout));
    }

    /// Regression: a crash of every instance used to leave the shared server
    /// running forever. The watchdog ends it once no live lease remains.
    #[test]
    fn watchdog_stops_service_after_idle() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_dir(dir.path(), "laya");
        let stub = StubChild(long_lived_child());
        let pid = stub.pid();
        write_service(&service, &test_service(pid, 1, &["english"])).unwrap();
        let watch_dir = service.clone();
        let watchdog = std::thread::spawn(move || {
            run_watchdog(
                &watch_dir,
                pid,
                Duration::from_millis(25),
                Duration::from_millis(100),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline && read_service(&service).is_some() {
            std::thread::sleep(Duration::from_millis(25));
        }
        watchdog.join().expect("the watchdog thread").unwrap();
        assert!(read_service(&service).is_none(), "service.json survived");
        assert!(wait_dead(pid, Duration::from_secs(10)), "stub {pid} survived");
    }

    /// Regression (watchdogHardening): the descriptor's recorded creation time
    /// no longer matches the pid (the pid was reused) — the watchdog must
    /// remove the descriptor without signalling anything.
    #[test]
    fn watchdog_skips_the_kill_when_the_pid_was_reused() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_dir(dir.path(), "laya");
        let stub = StubChild(long_lived_child());
        let pid = stub.pid();
        let mut info = test_service(pid, 1, &["english"]);
        info.created = "a-creation-time-from-a-dead-process".into();
        write_service(&service, &info).unwrap();
        run_watchdog(
            &service,
            pid,
            Duration::from_millis(25),
            Duration::from_millis(100),
        )
        .unwrap();
        assert!(read_service(&service).is_none(), "service.json survived");
        assert!(
            crate::process_tree::pid_alive(pid),
            "the reused pid was killed anyway"
        );
    }

    /// A stale watchdog (its server was replaced) must never kill the newer
    /// server: it exits on the pid mismatch instead.
    #[test]
    fn watchdog_exits_when_the_service_names_another_pid() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_dir(dir.path(), "laya");
        let stub = StubChild(long_lived_child());
        let pid = stub.pid();
        write_service(&service, &test_service(pid, 1, &["english"])).unwrap();
        // Started for a different server than the one the descriptor names.
        run_watchdog(
            &service,
            pid + 1,
            Duration::from_millis(25),
            Duration::from_millis(50),
        )
        .unwrap();
        assert!(read_service(&service).is_some(), "the descriptor was removed");
        assert!(
            crate::process_tree::pid_alive(pid),
            "the newer server was killed"
        );
    }

    #[test]
    fn watchdog_command_line_parses_the_required_flags() {
        let dir = tempfile::tempdir().unwrap();
        let service = service_dir(dir.path(), "laya");
        write_service(&service, &test_service(0, 1, &[])).unwrap();
        // pid 0 = no such server: the watchdog cleans the descriptor and exits.
        let args: Vec<String> = vec![
            "--run-dir".to_string(),
            service.display().to_string(),
            "--server-pid".to_string(),
            "0".to_string(),
            "--poll-ms".to_string(),
            "10".to_string(),
            "--idle-ms".to_string(),
            "20".to_string(),
        ];
        run_watchdog_command(&args).expect("the watchdog command runs");
        assert!(read_service(&service).is_none());
        assert!(run_watchdog_command(&["--run-dir".to_string()]).is_err());
        assert!(run_watchdog_command(&[]).is_err());
    }

    /// Regression (watchdogHardening): a run dir outside the service area
    /// (`<config>/run/<service>`) is refused, so a stray process cannot aim
    /// the watchdog at an arbitrary directory.
    #[test]
    fn watchdog_command_refuses_a_run_dir_outside_the_service_area() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().display().to_string();
        let outside = vec![
            "--run-dir".to_string(),
            plain.clone(),
            "--server-pid".to_string(),
            "1".to_string(),
        ];
        assert!(run_watchdog_command(&outside).is_err());
        let dot = vec![
            "--run-dir".to_string(),
            format!("{plain}/run/.hidden"),
            "--server-pid".to_string(),
            "1".to_string(),
        ];
        assert!(run_watchdog_command(&dot).is_err());
        let traversal = vec![
            "--run-dir".to_string(),
            format!("{plain}/run/laya/.."),
            "--server-pid".to_string(),
            "1".to_string(),
        ];
        assert!(run_watchdog_command(&traversal).is_err());
    }

    /// Regression (detachedConsoleWindows): a detached child must carry a
    /// HIDDEN console, not `DETACHED_PROCESS` — a console-less child makes
    /// every console-subsystem grandchild pop a NEW Terminal window (the
    /// user saw a swarm of them from the shared stubs).
    #[cfg(windows)]
    #[test]
    fn detached_children_get_a_hidden_console_not_no_console() {
        assert_eq!(
            DETACHED_CREATION_FLAGS & DETACHED_PROCESS,
            0,
            "DETACHED_PROCESS must not be part of the detached spawn flags"
        );
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        assert_eq!(
            DETACHED_CREATION_FLAGS,
            CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
            "the detached spawn flags are a hidden console in a new group"
        );
    }

    /// Smoke test for the spawn mechanics (the flag-level regression above is
    /// the red-first pin): a detached `cmd /C` with a hidden console still runs
    /// and leaves its marker behind. The old console-less spawn failed with
    /// `0x800700e8`.
    #[cfg(windows)]
    #[test]
    fn a_detached_cmd_still_runs_with_the_hidden_console() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.txt");
        let mut cmd = Command::new(
            std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
        );
        cmd.args(["/C", "echo detached> marker.txt"])
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach(&mut cmd);
        let mut child = cmd.spawn().expect("the detached spawn runs");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(marker.is_file(), "the detached cmd never wrote its marker");
        let _ = child.wait();
    }
}
