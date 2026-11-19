// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `sccache` discovery and the shared build-cache server (plan d3336a8a step 7).
//!
//! One `sccache` server caches `rustc` output for every Mnemo2 instance (and
//! every agent session) on the machine, so two workers building the same
//! dependency graph compile it once instead of twice. There is no setting and
//! no toggle: when the binary is on `PATH` it is used, and a user's own
//! `RUSTC_WRAPPER` is never overridden.
//!
//! **The server has to outlive the command that used it.** Left alone,
//! `sccache` starts its server from the first `rustc` a wrapper runs, which
//! makes that server a child of the `cargo` — inside the shell or proof tree
//! kill — so it dies when that one command ends while other workers (and other
//! windows) are still compiling through the same port. Mnemo therefore starts
//! the server itself, from its own process and detached
//! ([`crate::process_tree::detached_command`]), and hands out the wrapper only
//! once a server is confirmed up — that check runs off the command path, so no
//! command is held up by it. `sccache`'s own idle timeout
//! (`SCCACHE_IDLE_TIMEOUT`, 600 s by default) ends the server later, so Mnemo
//! keeps no watchdog, and the confirmation is cached for [`RECHECK_AFTER`] so a
//! running server costs one idempotent probe a minute.
//!
//! The answer is resolved lazily at the first command spawn and refreshed on
//! every Settings read, so a `cargo install sccache` done mid-session is picked
//! up without a restart. A missing binary costs one cached `PATH` scan and
//! nothing else — the build cache is an optimisation, never a failure mode.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};

/// The environment variable `cargo` reads to pick the compiler wrapper.
const WRAPPER_VAR: &str = "RUSTC_WRAPPER";

/// The program that provides the shared Rust build cache.
const SCCACHE: &str = "sccache";

/// How long a server confirmation is trusted. A running server costs one
/// idempotent `--start-server` per window; a crashed one is noticed within a
/// minute.
const RECHECK_AFTER: Duration = Duration::from_secs(60);

/// How long one detached `sccache` helper may run before it is killed and the
/// check counts as failed.
///
/// A healthy `--start-server` returns in tens of milliseconds and
/// `--show-stats` in about the same, so this cap only bites on a wedged or
/// half-dead cache server — and then it is deliberately shorter than sccache's
/// own 15 s startup wait: an unconfirmed server just means this command runs
/// `rustc` directly, which is always correct.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);


/// The poll interval while waiting for a detached helper to exit.
const PROBE_POLL: Duration = Duration::from_millis(10);

/// The last `PATH` resolution: `None` before the first look, `Some(None)` when
/// the binary was absent, `Some(Some(path))` when it was found. Refreshed by
/// [`refresh`], so a mid-session install lands on the next command.
static RESOLVED: LazyLock<RwLock<Option<Option<PathBuf>>>> =
    LazyLock::new(|| RwLock::new(None));

/// The last build-cache server confirmation and when it was obtained.
static SERVER: LazyLock<RwLock<Option<ServerCheck>>> =
    LazyLock::new(|| RwLock::new(None));

/// The outcome of the last build-cache server check and when it ran.
struct ServerCheck {
    /// Whether a server was confirmed up (and the wrapper may be handed out).
    ok: bool,
    /// When the check ran; an answer older than [`RECHECK_AFTER`] is stale.
    at: Instant,
}

/// The cached `sccache` path, resolving it on first use.
///
/// This is the accessor every spawn path uses: the first command pays the
/// `PATH` scan, every later one reads the cache.
pub fn current() -> Option<PathBuf> {
    cached(&RESOLVED, lookup, false)
}

/// Look `sccache` up on `PATH` right now and cache the answer.
///
/// Called on every Settings read (`model_settings_get`), so a binary installed
/// after startup shows up in the Machine page without an app restart.
pub fn refresh() -> Option<PathBuf> {
    cached(&RESOLVED, lookup, true)
}

/// Look `sccache` up on `PATH` without touching the cache.
///
/// The one-shot form of [`current`]/[`refresh`]; the real lookup is
/// [`lookup`], and the cache behaviour is what the module's tests exercise.
pub fn resolve() -> Option<PathBuf> {
    lookup()
}

/// Set `RUSTC_WRAPPER` on `command` when the machine has `sccache` with a
/// running server and the inherited environment does not already name a
/// wrapper.
///
/// Applied to every spawned agent command, harness verification and Mnemo2
/// proof run: on a non-`cargo` command the variable is inert, and one code
/// path is simpler (and cannot disagree with itself) than a heavy-only rule.
/// A user's own `RUSTC_WRAPPER` — set or empty — always wins: Mnemo never
/// replaces a wrapper it did not choose, and never starts a server on its
/// behalf. Nothing here blocks: while the first check (or the one after a
/// stale answer) runs in the background, this command simply spawns without the
/// wrapper and the next one gets it.
pub fn apply_env(command: &mut tokio::process::Command) {
    let inherited = std::env::var_os(WRAPPER_VAR);
    let found = current();
    if let Some((key, value)) =
        wrapper_choice(inherited.as_deref(), found.as_deref(), ensure_server)
    {
        command.env(key, value);
    }
}

/// The `(name, value)` pair [`apply_env`] adds, with the server check injected
/// so tests can drive both answers.
///
/// An inherited wrapper wins before any probe runs, so a user who set their own
/// never pays for — or triggers — a server start.
fn wrapper_choice(
    inherited: Option<&OsStr>,
    found: Option<&Path>,
    server: impl FnOnce(&Path) -> bool,
) -> Option<(&'static str, OsString)> {
    if has_wrapper(inherited) {
        return None;
    }
    wrapper_env(None, found.filter(|path| server(path)))
}

/// The `(name, value)` pair to add to a child's environment, or `None`.
///
/// `inherited` is the parent process's `RUSTC_WRAPPER` (absent or blank = no
/// wrapper) and `found` is a resolved `sccache` path whose server is confirmed
/// up. Pure, so the three rules below are cheap to test without touching the
/// real `PATH`:
///
/// - confirmed server + no inherited wrapper → set `RUSTC_WRAPPER` to the path;
/// - any inherited wrapper → nothing (never override the user);
/// - no binary (or no server) → nothing.
pub fn wrapper_env(
    inherited: Option<&OsStr>,
    found: Option<&Path>,
) -> Option<(&'static str, OsString)> {
    if has_wrapper(inherited) {
        return None;
    }
    found.map(|path| (WRAPPER_VAR, path.as_os_str().to_os_string()))
}

/// Whether an inherited `RUSTC_WRAPPER` names a wrapper (blank = unset).
fn has_wrapper(inherited: Option<&OsStr>) -> bool {
    inherited.is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

/// The confirmed state of the shared build-cache server, refreshing it in the
/// background when the cached answer has aged out.
///
/// Never blocks the command path: an unknown or stale answer is `false` (this
/// one command runs `rustc` directly) and a detached check runs off-thread, so
/// the next command gets the wrapper. A running server therefore costs one
/// helper per [`RECHECK_AFTER`] window per process and nothing on the commands
/// themselves.
pub fn ensure_server(binary: &Path) -> bool {
    server_ok(&SERVER, Instant::now(), || start_refresh(binary))
}

/// The cache core behind [`ensure_server`]: a fresh answer is served from the
/// cache, otherwise `refresh` is asked to look again and this command is told
/// "not confirmed" until that look lands.
fn server_ok(
    state: &RwLock<Option<ServerCheck>>,
    now: Instant,
    refresh: impl FnOnce(),
) -> bool {
    if let Some((ok, at)) = state
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|check| (check.ok, check.at))
    {
        if now.saturating_duration_since(at) < RECHECK_AFTER {
            return ok;
        }
    }
    refresh();
    false
}

/// Refresh the confirmed server state on a background thread, one check at a
/// time.
///
/// The check starts the server from Mnemo — never from a command tree — and
/// stores the answer, a failure included, so a broken cache is retried after
/// the same window instead of on every command.
fn start_refresh(binary: &Path) {
    /// True while a background check runs, so a burst of commands runs one.
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let binary = binary.to_path_buf();
    std::thread::spawn(move || {
        let ok = start_server(&binary);
        if !ok {
            eprintln!(
                "warning: sccache is installed but no build server is running; \
                 RUSTC_WRAPPER left unset"
            );
        }
        *SERVER
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(ServerCheck {
            ok,
            at: Instant::now(),
        });
        BUSY.store(false, Ordering::Release);
    });
}

/// Make sure a server runs, detached from every command tree.
///
/// `--start-server` goes first because it both starts a missing server and
/// proves a running one answers (it is idempotent, and exits zero once the
/// server responds), so the healthy path costs one fast helper and no second
/// probe. `--show-stats` is the fallback for a version that reports an
/// already-running server as an error; it is never the first probe because a
/// half-dead server still answers it — exactly the state that hangs every
/// wrapper. Both go through [`crate::process_tree::detached_command`], so the
/// server is never inside a kill-on-close job or a command's process group.
fn start_server(binary: &Path) -> bool {
    run_probe(binary, &["--start-server"]).unwrap_or(false)
        || run_probe(binary, &["--show-stats"]).unwrap_or(false)
}

/// Run one short-lived `sccache` helper detached and report whether it exited
/// zero.
///
/// `None` when it could not be spawned or did not finish within
/// [`PROBE_TIMEOUT`] — the helper is killed then, so a wedged probe cannot hold
/// a command up. The server it may have started is untouched: it is not in our
/// process tree.
fn run_probe(binary: &Path, args: &[&str]) -> Option<bool> {
    let mut child = crate::process_tree::detached_command(binary)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(PROBE_POLL),
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// The real lookup: `which` honours `PATH` and, on Windows, `PATHEXT`.
fn lookup() -> Option<PathBuf> {
    which::which(SCCACHE).ok()
}

/// The cache core: an unexpired cached answer short-circuits unless `refresh`,
/// otherwise `lookup` runs and its answer (including "absent") is stored.
fn cached(
    cache: &RwLock<Option<Option<PathBuf>>>,
    lookup: impl FnOnce() -> Option<PathBuf>,
    refresh: bool,
) -> Option<PathBuf> {
    if !refresh {
        if let Some(answer) = cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return answer;
        }
    }
    let answer = lookup();
    *cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(answer.clone());
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found on `PATH` and no wrapper inherited: the wrapper is set.
    #[test]
    fn wrapper_set_when_found() {
        let path = PathBuf::from("/opt/sccache/bin/sccache");
        let (key, value) =
            wrapper_env(None, Some(&path)).expect("a found binary must be used");
        assert_eq!(key, WRAPPER_VAR);
        assert_eq!(value, path.as_os_str().to_os_string());
        // A blank inherited value is "no wrapper", not a user choice.
        assert!(wrapper_env(Some(OsStr::new("  ")), Some(&path)).is_some());
    }

    /// The user's own wrapper is never replaced, found binary or not.
    #[test]
    fn user_wrapper_not_overridden() {
        let path = PathBuf::from("/opt/sccache/bin/sccache");
        assert_eq!(
            wrapper_env(Some(OsStr::new("sccache-custom")), Some(&path)),
            None
        );
    }

    /// No binary found: nothing is set, however the environment looks.
    #[test]
    fn not_found_sets_nothing() {
        assert_eq!(wrapper_env(None, None), None);
        assert_eq!(wrapper_env(Some(OsStr::new("mine")), None), None);
    }

    /// The injected lookup: the first answer is cached (a later "found" is not
    /// seen) and `refresh` replaces it (a mid-session install is picked up).
    #[test]
    fn cache_resolves_once_and_refreshes() {
        let cache = RwLock::new(None);
        let installed = PathBuf::from("/new/sccache");
        assert_eq!(
            cached(&cache, || None, false),
            None,
            "the first look caches the absence"
        );
        assert_eq!(
            cached(&cache, || Some(installed.clone()), false),
            None,
            "a cached 'not found' is not re-looked"
        );
        assert_eq!(
            cached(&cache, || Some(installed.clone()), true),
            Some(installed.clone()),
            "refresh picks up a newly installed binary"
        );
        assert_eq!(
            cached(&cache, || None, false),
            Some(installed),
            "the refreshed answer is what the next spawn reads"
        );
    }
}

/// The server gate: no wrapper unless a build-cache server is confirmed up,
/// and a user's own wrapper skips the check entirely.
#[cfg(test)]
mod server_tests {
    use std::cell::Cell;

    use super::*;

    /// A failed start (injected) leaves `RUSTC_WRAPPER` unset.
    #[test]
    fn no_wrapper_when_the_server_does_not_start() {
        let path = Path::new("/opt/sccache/bin/sccache");
        let probes = Cell::new(0);
        let chosen = wrapper_choice(None, Some(path), |_| {
            probes.set(probes.get() + 1);
            false
        });
        assert_eq!(
            chosen, None,
            "a binary without a confirmed server must not be handed out"
        );
        assert_eq!(probes.get(), 1, "the start is attempted exactly once");
    }

    /// A confirmed server (injected) hands out the wrapper.
    #[test]
    fn wrapper_set_when_the_server_check_passes() {
        let path = Path::new("/opt/sccache/bin/sccache");
        let chosen = wrapper_choice(None, Some(path), |binary| binary == path);
        assert_eq!(
            chosen,
            Some((WRAPPER_VAR, path.as_os_str().to_os_string()))
        );
    }

    /// A user's own wrapper short-circuits before the server is even probed.
    #[test]
    fn user_wrapper_skips_the_server_probe() {
        let path = Path::new("/opt/sccache/bin/sccache");
        let chosen = wrapper_choice(Some(OsStr::new("sccache-custom")), Some(path), |_| {
            panic!("a user's own wrapper must not trigger a server start")
        });
        assert_eq!(chosen, None);
    }

    /// No binary found: the server is never probed either.
    #[test]
    fn no_binary_skips_the_server_probe() {
        let chosen = wrapper_choice(None, None, |_| {
            panic!("no binary means no server check")
        });
        assert_eq!(chosen, None);
    }

    /// A fresh answer is served from the cache; an unknown or stale one asks
    /// for a background refresh, so that command is told "not confirmed".
    #[test]
    fn a_fresh_answer_is_reused_and_a_stale_one_refreshes() {
        let state = RwLock::new(None);
        let refreshes = Cell::new(0);
        let look = || refreshes.set(refreshes.get() + 1);
        let t0 = Instant::now();
        assert!(
            !server_ok(&state, t0, look),
            "nothing is confirmed before the first check lands"
        );
        assert_eq!(refreshes.get(), 1, "the first command starts a check");
        *state.write().expect("state") = Some(ServerCheck { ok: true, at: t0 });
        assert!(
            server_ok(&state, t0 + Duration::from_secs(30), look),
            "a fresh positive answer is served from the cache"
        );
        assert_eq!(refreshes.get(), 1, "a fresh answer does not re-check");
        assert!(
            !server_ok(&state, t0 + Duration::from_secs(61), look),
            "a stale answer is never trusted"
        );
        assert_eq!(refreshes.get(), 2, "a stale answer is re-checked");
    }
}
