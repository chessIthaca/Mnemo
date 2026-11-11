// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Headless-Chromium debug browser, driven over the Chrome DevTools Protocol.
//!
//! [`BrowserManager`] lazily spawns a single headless Chromium process (via
//! `chromiumoxide`) on first use and multiplexes it across any number of open
//! pages (tabs). It is shared between the agent tools (`src/tool/browser/`) and
//! the embedder API behind an `Arc`, so every caller drives the *same* browser.
//!
//! Lifecycle: the browser spawns on the first operation that needs it, and is
//! re-spawned transparently when the process dies (the handler task ends, the
//! stale state is cleared, and the next operation launches fresh). Every
//! launch-mode CDP round-trip is bounded by a timeout (~30s; ~60s for
//! navigate): a *wedged* browser — alive process, open connection,
//! unresponsive renderer, e.g. a page stuck in an infinite JS loop — is
//! force-reaped on timeout, and the next operation launches fresh, so a
//! wedge is a bounded error + automatic restart instead of an infinite hang.
//! A background watchdog (spawned with the first browser) reaps a dead
//! browser on its next tick — one that goes offline between operations no
//! longer lingers until the next one — probes liveness (two consecutive
//! probe timeouts force-reap an idle-wedged browser), and periodically
//! sweeps orphaned profile dirs: each profile carries a `.mnemo-live` marker
//! touched every tick, so live profiles (this process's or another
//! instance's) are never touched while dead managers' dirs are reaped
//! regardless of age. Call [`BrowserManager::close`] to shut it down. All
//! operations take an optional `page_id`; when omitted they act on the
//! *active* page (the most recently opened or switched-to page).
//!
//! Security: [`navigate`](BrowserManager::navigate) normalizes every input at
//! the single choke point shared by the agent tool and the UI: scheme-less
//! hostnames get an omnibox-style scheme (`https://`; `http://` for localhost
//! and IP literals), then the URL scheme allow-list (`http`, `https`, `data`,
//! `file`) is enforced. `file://` is deliberately on the list so local HTML
//! files can be debugged in the browser (a user-approved navigation renders
//! the file's own page); `javascript:`, `about:`, and other schemes stay
//! rejected so they can never cross the sandbox boundary or trigger SSRF via
//! the browser.
//!
//! Locking: the state `Mutex` is held only for short state mutations. CDP
//! round-trips (navigation, screenshots, eval, click, type, url/title reads)
//! run on cloned handles *outside* the lock, so a slow page cannot serialize
//! the other tools behind one mutex (Review C1). The one deliberate
//! exception is [`close`](BrowserManager::close): its kill runs inline
//! (outside the lock) so that `close()` returning means the child process is
//! dead — the Tauri exit hook and the profile-removal tests rely on that
//! contract.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::cdp::browser_protocol::log::EventEntryAdded;
use chromiumoxide::cdp::js_protocol::runtime::{EvaluateParams, EventConsoleApiCalled};
use chromiumoxide::listeners::EventStream;
use chromiumoxide::Page;
use futures::StreamExt as _;
use serde::Serialize;
use tokio::sync::{broadcast, Mutex};
use tokio::task::JoinHandle;

use crate::error::{Error, Result};

/// A snapshot of one open page, for tool results + the IPC layer.
#[derive(Debug, Clone, Serialize)]
pub struct PageInfo {
    /// The manager-assigned page id (a short uuid) used by every tool call.
    pub id: String,
    /// The page's current URL.
    pub url: String,
    /// The page's current title (empty until first load completes).
    pub title: String,
    /// Whether this is the page that id-less tool calls act on.
    pub active: bool,
}

/// One buffered console message / JS exception from a page.
#[derive(Debug, Clone, Serialize)]
pub struct ConsoleEntry {
    /// The severity level (`log`, `warn`, `error`, `exception`, ...).
    pub level: String,
    /// The rendered message text.
    pub text: String,
}

/// A live console event, pushed to subscribers the moment a page logs
/// something. `page_id` ties it to the page that emitted it.
#[derive(Debug, Clone, Serialize)]
pub struct ConsoleEvent {
    /// The page that logged this entry.
    pub page_id: String,
    /// The severity level.
    pub level: String,
    /// The rendered message text.
    pub text: String,
}

/// How many console entries are kept per page before the oldest is dropped.
const CONSOLE_CAP: usize = 500;

/// URL schemes the debug browser accepts — everything else is rejected so
/// exotic schemes (`javascript:`, `about:`, ...) can't drive SSRF through the
/// browser. `file` is deliberately on the list: local HTML files are a
/// primary debugging target, and a `file://` navigation renders only the
/// page the URL names (still gated behind the same approval flow as every
/// other navigation).
const ALLOWED_SCHEMES: [&str; 4] = ["http", "https", "data", "file"];

/// Name of the liveness marker file inside a browser profile dir. Created
/// when the profile is registered, touched by the watchdog every tick while
/// the profile belongs to a live browser, and backdated past
/// [`MARKER_GRACE`] when the profile is retired. The sweep reads its mtime
/// to tell a live manager's profile (in this or another process) from a
/// dead one's orphan — without any age cutoff.
const LIVE_MARKER: &str = ".mnemo-live";

/// How often the watchdog ticks: reap a dead launch-mode browser, touch the
/// live markers, and sweep orphaned profile dirs.
const WATCHDOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Timeout for ordinary launch-mode CDP operations (eval, click, type,
/// screenshot, snapshot, list/close/switch page). A healthy operation
/// completes in well under 1s; 30s only fires on a wedged renderer — the
/// false-fire cost is a browser restart + re-navigate.
const CDP_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Timeout for `navigate`'s `new_page` round-trip: loading a slow site can
/// legitimately take tens of seconds, so it gets twice the ordinary budget
/// (the false-fire cost is a restart + re-navigate).
const CDP_NAVIGATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Timeout for the watchdog's liveness probe — a cheap browser-level
/// `Browser.getVersion` round-trip (see [`BrowserManager::watchdog_loop`]).
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Consecutive probe timeouts before the watchdog force-reaps a browser as
/// wedged. Two strikes ride out a single transient hiccup.
const PROBE_STRIKES: u32 = 2;

/// A profile dir whose [`LIVE_MARKER`] is older than this is treated as an
/// orphan by the sweep, regardless of the dir's age. Generous on purpose:
/// markers are only touched once per [`WATCHDOG_INTERVAL`], and a heavily
/// loaded (or briefly suspended) machine may delay a live instance's touch
/// by several ticks — any doubt keeps the dir.
const MARKER_GRACE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Profile dirs owned by live managers *in this process*. The sweep never
/// touches these (live by definition); other processes' live profiles are
/// protected by their fresh [`LIVE_MARKER`] mtime instead.
static LIVE_PROFILES: std::sync::LazyLock<std::sync::Mutex<HashSet<PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// Register `dir` as owned by a live manager in this process and create its
/// [`LIVE_MARKER`]. Called under the state lock right after a successful
/// browser launch; the single file create is cheap enough to hold the lock
/// for.
fn register_live_profile(dir: &Path) {
    if let Ok(mut live) = LIVE_PROFILES.lock() {
        live.insert(dir.to_path_buf());
    }
    let _ = std::fs::File::create(dir.join(LIVE_MARKER));
}

/// Unregister `dir` and declare it dead: backdate its [`LIVE_MARKER`] past
/// [`MARKER_GRACE`] so the next sweep reaps the dir even if the removal
/// retry loop (or the `TempDir` drop) fails to take it away — the sweep is
/// the retry-forever safety net. Best-effort: a dir that is already gone
/// simply has no marker to backdate.
fn unregister_live_profile(dir: &Path) {
    if let Ok(mut live) = LIVE_PROFILES.lock() {
        live.remove(dir);
    }
    let _ = std::fs::File::options()
        .write(true)
        .open(dir.join(LIVE_MARKER))
        .and_then(|f| {
            f.set_modified(
                std::time::SystemTime::now() - MARKER_GRACE - std::time::Duration::from_secs(60),
            )
        });
}

/// Touch the [`LIVE_MARKER`] of every registered profile so other
/// processes' sweeps keep treating them as live. Best-effort: a profile
/// whose dir is already gone (removal race) stays registered until its
/// manager's state drops — harmless, the sweep just skips a missing dir.
fn touch_live_markers() {
    let Ok(live) = LIVE_PROFILES.lock() else { return };
    let now = std::time::SystemTime::now();
    for dir in live.iter() {
        let _ = std::fs::File::options()
            .write(true)
            .open(dir.join(LIVE_MARKER))
            .and_then(|f| f.set_modified(now));
    }
}

/// The live state behind the manager's lock.
struct State {
    /// The headless browser process handle, once spawned (`Arc` so CDP calls
    /// can run on a cloned handle after the lock is dropped).
    browser: Option<Arc<Browser>>,
    /// Launch generation: bumped every time `ensure_browser` stores a
    /// freshly launched browser. The `cdp` wrapper captures it when an op
    /// starts and `force_reap_wedged` no-ops on mismatch, so a late-firing
    /// timeout can never reap a browser that was respawned after the
    /// timed-out op's browser already died (review L2).
    generation: u64,
    /// The CDP event-handler task; its end signals the process died.
    handler: Option<JoinHandle<()>>,
    /// Open pages, keyed by manager-assigned id.
    pages: HashMap<String, Page>,
    /// The active page id (the one id-less operations act on).
    active: Option<String>,
    /// Per-page console ring buffer, fed by the per-page event forwarders.
    console: HashMap<String, std::collections::VecDeque<ConsoleEntry>>,
    /// The per-page console-event forwarder tasks, keyed by page id.
    console_tasks: HashMap<String, JoinHandle<()>>,
    /// The throwaway Chromium profile dir; dropping it removes the dir.
    profile: Option<tempfile::TempDir>,
    /// The background watchdog (reap dead browsers + sweep orphaned profile
    /// dirs), spawned on first browser launch and respawned if it ever
    /// dies. Deliberately NOT aborted by `close` — it keeps sweeping until
    /// the manager (and this state) drops.
    watchdog: Option<JoinHandle<()>>,
}

/// Unregister a still-owned profile when the state drops (a manager dropped
/// without `close`): the `TempDir` drop removes the dir right after, and the
/// registry entry must not outlive its dir.
impl Drop for State {
    fn drop(&mut self) {
        if let Some(profile) = &self.profile {
            unregister_live_profile(profile.path());
        }
    }
}

/// A shared, lazily-spawned headless Chromium browser with multi-page support.
///
/// Clone-cheap (`Arc` inside); construct once and share across tools + IPC.
pub struct BrowserManager {
    state: Arc<Mutex<State>>,
    /// Live console events, broadcast to subscribers.
    console_tx: broadcast::Sender<ConsoleEvent>,
    /// The watchdog tick interval in milliseconds (see
    /// [`WATCHDOG_INTERVAL`]); atomic so tests can shorten it via `&self`
    /// before the first browser launch (the watchdog reads it once, at
    /// spawn time).
    reap_interval_ms: AtomicU64,
    /// Test-only override for the CDP op/navigate timeouts (0 = use the
    /// [`CDP_OP_TIMEOUT`]/[`CDP_NAVIGATE_TIMEOUT`] defaults); atomic so
    /// tests can set it via `&self` before the first operation.
    cdp_timeout_ms: AtomicU64,
}

impl BrowserManager {
    /// Create a manager with no browser yet — it spawns on first use.
    pub fn new() -> Arc<Self> {
        let (console_tx, _) = broadcast::channel(512);
        Arc::new(Self {
            state: Arc::new(Mutex::new(State {
                browser: None,
                generation: 0,
                handler: None,
                pages: HashMap::new(),
                active: None,
                console: HashMap::new(),
                console_tasks: HashMap::new(),
                profile: None,
                watchdog: None,
            })),
            console_tx,
            reap_interval_ms: AtomicU64::new(WATCHDOG_INTERVAL.as_millis() as u64),
            cdp_timeout_ms: AtomicU64::new(0),
        })
    }

    /// Subscribe to live console events (one receiver per subscriber).
    pub fn subscribe_console(&self) -> broadcast::Receiver<ConsoleEvent> {
        self.console_tx.subscribe()
    }

    /// Test-only: shorten the watchdog tick so the reap path is observable
    /// within a test's runtime. Must be called before the first `navigate`
    /// (the watchdog reads the interval once, when it spawns).
    #[cfg(test)]
    pub(crate) fn set_reap_interval(&self, interval: std::time::Duration) {
        self.reap_interval_ms
            .store(interval.as_millis() as u64, Ordering::Relaxed);
    }

    /// Test-only: shorten the CDP op/navigate timeouts so a wedged-renderer
    /// repro fires in milliseconds instead of the production ~30s/60s. When
    /// set, the value overrides BOTH timeouts (data:-URL navigates complete
    /// in milliseconds, so tests can use one short value). Mirrors
    /// [`set_reap_interval`](Self::set_reap_interval).
    #[cfg(test)]
    pub(crate) fn set_cdp_timeout(&self, timeout: std::time::Duration) {
        self.cdp_timeout_ms
            .store(timeout.as_millis() as u64, Ordering::Relaxed);
    }

    /// Ensure the browser process is running, spawning it if needed — or, if
    /// the handler task has ended (the process died / the CDP connection
    /// dropped), clearing the stale state and re-spawning fresh. Callers hold
    /// the state lock; the launch itself is I/O but runs at most once per
    /// spawn/respawn, not per operation.
    async fn ensure_browser(state: &mut State) -> Result<()> {
        // Reap a dead browser (if any) so the launch below starts fresh; a
        // live one short-circuits here.
        if !Self::reap_dead_browser(state) && state.browser.is_some() {
            return Ok(());
        }

        // Best-effort sweep of orphaned profiles (dead managers' dirs, plus
        // legacy >1h leftovers from pre-marker versions). Fire-and-forget on
        // the blocking pool: it never touches the dir about to be created,
        // so it need not complete before launch — and running it inline held
        // the state lock across blocking `read_dir` + `remove_dir_all` calls
        // (Review M1-perf).
        let _handle = tokio::task::spawn_blocking(Self::sweep_orphan_profiles);

        let profile = tempfile::Builder::new()
            .prefix("mnemo-browser-")
            .tempdir_in(std::env::temp_dir())
            .map_err(|e| Error::Browser(format!("failed to create browser profile dir: {e}")))?;
        let (browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                // chromiumoxide 0.9 made `HeadlessMode` unreachable (the config
                // module is private; only BrowserConfig/BrowserConfigBuilder are
                // re-exported), so the typed setter can no longer be called from
                // outside the crate. The builder's `new_headless_mode()` sets
                // exactly HeadlessMode::New — the mode this app wants.
                .new_headless_mode()
                .user_data_dir(profile.path())
                // No plugins in this app: forbid extension loading explicitly
                // (the throwaway profile already blocks extensions; this makes
                // the policy explicit in case a profile dir is ever reused).
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .map_err(|e| Error::Browser(format!("invalid browser config: {e}")))?,
        )
        .await
        .map_err(|e| Error::Browser(format!("failed to launch headless Chromium: {e}")))?;
        // The profile is owned by a live browser from here until it is
        // retired (reap/close/drop): the registry entry + fresh marker keep
        // every sweep — this process's or another instance's — away from it.
        // Registered only after a successful launch so a failed one leaves
        // no registry entry behind (a fresh marker-less dir is protected by
        // the sweep's conservative 1h fallback anyway).
        register_live_profile(profile.path());
        let task = tokio::spawn(async move {
            // Drive the CDP connection; errors end the task (the watchdog
            // or the next operation detects the finished task and re-spawns).
            while handler.next().await.is_some() {}
        });
        state.browser = Some(Arc::new(browser));
        state.handler = Some(task);
        state.profile = Some(profile);
        // A fresh browser generation: late-firing timeouts from ops that
        // started against the previous browser must not reap this one
        // (review L2).
        state.generation += 1;
        Ok(())
    }

    /// Reap a dead launch-mode browser: take the stale handles, clear the
    /// page/console state, and kill the child + remove its profile on a
    /// background task. Returns whether a dead browser was reaped (`false`
    /// when none is spawned, or the live one is still alive). Callers hold
    /// the state lock; the kill runs off-lock (Review B1) — awaiting it here
    /// would hold the lock across it.
    ///
    /// Called from [`ensure_browser`](Self::ensure_browser) (lazily, on the
    /// next operation) *and* from the watchdog (every tick), so a browser
    /// that goes offline between operations is reaped without waiting for
    /// one.
    fn reap_dead_browser(state: &mut State) -> bool {
        if state.browser.is_none() {
            return false;
        }
        let dead = state.handler.as_ref().is_none_or(|h| h.is_finished());
        if !dead {
            return false;
        }
        // The process died (or the CDP connection dropped): drop the stale
        // handle + pages so a fresh launch starts from a clean slate
        // (Review C2), and kill the child + remove the dead profile on a
        // background task (Review B1).
        if let Some((browser, profile)) = Self::detach_browser(state) {
            Self::spawn_kill_task(browser, profile);
        }
        true
    }

    /// Detach the current launch-mode browser from `state`: abort the
    /// console forwarders, clear the page/console/active state, take (and
    /// abort) the handler task, and unregister the live profile. Returns
    /// the browser + profile for the CALLER to kill off-lock (Review B1 —
    /// awaiting a kill here would hold the state lock across it). Aborting
    /// the handler is a no-op when it already finished (the dead path).
    /// Callers hold the state lock.
    fn detach_browser(state: &mut State) -> Option<(Arc<Browser>, Option<tempfile::TempDir>)> {
        let browser = state.browser.take()?;
        for (_, task) in state.console_tasks.drain() {
            task.abort();
        }
        state.pages.clear();
        state.console.clear();
        state.active = None;
        if let Some(task) = state.handler.take() {
            task.abort();
        }
        let profile = state.profile.take();
        if let Some(profile) = &profile {
            // The dir no longer belongs to a live browser — declare it
            // dead so the sweep reaps it if the removal below never happens
            // or fails past its retry budget.
            unregister_live_profile(profile.path());
        }
        Some((browser, profile))
    }

    /// Kill a detached browser and remove its profile on a background task
    /// (Review B1 — callers hold, or are about to release, the state lock;
    /// awaiting the kill inline would hold it across the kill). Only
    /// removes the profile dir once the child is actually dead — a live
    /// child holds locks on it, so removal would fail forever (Review M1).
    fn spawn_kill_task(browser: Arc<Browser>, profile: Option<tempfile::TempDir>) {
        tokio::spawn(async move {
            if Self::kill_browser(browser).await {
                if let Some(profile) = profile {
                    Self::schedule_profile_removal(profile);
                }
            }
        });
    }

    /// Force-reap a WEDGED launch-mode browser: alive process, open CDP
    /// connection, unresponsive renderer (e.g. a page stuck in an infinite
    /// JS loop). [`reap_dead_browser`](Self::reap_dead_browser) never fires
    /// on such a browser — the wedge keeps its handler task alive, so the
    /// `is_finished` gate stays closed — hence this path skips the gate
    /// entirely and detaches unconditionally. The kill runs off-lock on a
    /// background task (Review B1). Returns whether a browser was reaped
    /// (a no-op when none is spawned); the next operation re-spawns fresh.
    ///
    /// `generation` is the launch generation the caller's operation
    /// started against; on mismatch a newer browser was launched in the
    /// meantime and reaping now would kill the healthy respawn — no-op
    /// (review L2).
    async fn force_reap_wedged(state: &Arc<Mutex<State>>, generation: u64) -> bool {
        let detached = {
            let mut state = state.lock().await;
            if state.generation != generation {
                // A newer browser was launched since the caller's op
                // started: reaping now would kill the healthy respawn, and
                // the op's own browser is already gone (its timeout,
                // another op's, or the watchdog reaped it). No-op (review
                // L2).
                return false;
            }
            Self::detach_browser(&mut state)
        };
        match detached {
            Some((browser, profile)) => {
                Self::spawn_kill_task(browser, profile);
                true
            }
            None => false,
        }
    }

    /// The effective CDP timeout for a round-trip whose production default
    /// is `default`: the test override
    /// ([`set_cdp_timeout`](Self::set_cdp_timeout)) when set, else
    /// `default`.
    fn cdp_timeout(&self, default: std::time::Duration) -> std::time::Duration {
        let ms = self.cdp_timeout_ms.load(Ordering::Relaxed);
        if ms > 0 {
            std::time::Duration::from_millis(ms)
        } else {
            default
        }
    }

    /// Run one launch-mode CDP round-trip bounded by `timeout`. The Ok path
    /// passes the operation's own result/error through unchanged; on
    /// timeout the wedged browser is force-reaped (killed; the next
    /// operation re-spawns fresh) and a bounded, actionable error is
    /// returned — without this, a wedged renderer (alive process, open
    /// connection, unresponsive page) hangs the calling tool forever.
    ///
    /// The launch generation is captured up front so the force-reap only
    /// ever targets the browser this operation ran against — a timeout
    /// firing after a respawn must not reap the fresh browser (review L2).
    async fn cdp<T>(
        &self,
        what: &str,
        timeout: std::time::Duration,
        fut: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let generation = self.state.lock().await.generation;
        match tokio::time::timeout(timeout, fut).await {
            Ok(result) => result,
            Err(_) => {
                Self::force_reap_wedged(&self.state, generation).await;
                Err(Error::Browser(format!(
                    "{what} timed out after {}s — the browser was unresponsive and has been \
                     restarted; re-navigate to reopen your pages",
                    timeout.as_secs()
                )))
            }
        }
    }

    /// Sweep orphaned browser profile dirs from the temp dir:
    /// `mnemo-browser-*` (and legacy `myharness-browser-*` dirs left by
    /// pre-rename versions).
    ///
    /// A dir is an orphan when no live manager owns it: dirs registered in
    /// this process are live by definition, and a dir whose [`LIVE_MARKER`]
    /// was touched within [`MARKER_GRACE`] belongs to a live manager in
    /// *another* process (another mnemo instance) — both are never touched.
    /// A dir with a stale marker belonged to a manager that has since died
    /// (crashed run, retired profile) and is removed regardless of its age —
    /// in-session orphans used to accumulate for a full hour or more. Dirs
    /// without a marker predate the marker scheme; they keep the
    /// conservative >1h age cutoff.
    ///
    /// Runs at browser launch and on every watchdog tick — before the
    /// watchdog existed it only ran at launch time, so a session whose
    /// browser stayed alive never swept at all.
    pub fn sweep_orphan_profiles() {
        let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
            return;
        };
        let now = std::time::SystemTime::now();
        // Snapshot the registry so the sweep never holds its lock across
        // blocking FS work (the watchdog's marker touch would block on it).
        let live: HashSet<PathBuf> = LIVE_PROFILES.lock().map(|l| l.clone()).unwrap_or_default();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("mnemo-browser-") && !name.starts_with("myharness-browser-") {
                continue;
            }
            let path = entry.path();
            // Live in this process — never touched.
            if live.contains(&path) {
                continue;
            }
            // A marker says another process's manager owns the dir while it
            // is fresh; a stale one means that manager died — reap now.
            let marker = path.join(LIVE_MARKER);
            if let Ok(meta) = std::fs::metadata(&marker) {
                // Any doubt (missing mtime, clock skew) keeps the dir.
                let fresh = meta.modified().map_or(true, |t| {
                    now.duration_since(t).map_or(true, |age| age < MARKER_GRACE)
                });
                if fresh {
                    continue;
                }
                let _ = std::fs::remove_dir_all(&path);
            } else {
                // No marker: pre-marker leftover — keep the conservative
                // 1h age cutoff.
                let stale = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|t| t < now - std::time::Duration::from_secs(3600))
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
    }

    /// Ensure the background watchdog is running. Spawned lazily on the
    /// first browser launch (a tokio runtime is guaranteed there —
    /// `BrowserManager::new` may run outside one); holds a `Weak` handle to
    /// the state so it exits once the manager drops, and is respawned if a
    /// previous watchdog ever died. Every tick it (1) reaps a dead
    /// launch-mode browser — the fix for offline browsers accumulating
    /// between operations, (2) touches the live markers, and (3) sweeps
    /// orphaned profile dirs. Deliberately NOT aborted by `close`: it keeps
    /// sweeping until the manager drops.
    async fn ensure_watchdog(&self) {
        let mut state = self.state.lock().await;
        if state.watchdog.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let interval =
            std::time::Duration::from_millis(self.reap_interval_ms.load(Ordering::Relaxed));
        let weak = Arc::downgrade(&self.state);
        state.watchdog = Some(tokio::spawn(Self::watchdog_loop(weak, interval)));
    }

    /// The watchdog body — see [`ensure_watchdog`](Self::ensure_watchdog).
    /// Free-running: exits only when the manager's state drops (the `Weak`
    /// upgrade fails) or the runtime shuts down.
    async fn watchdog_loop(weak: std::sync::Weak<Mutex<State>>, interval: std::time::Duration) {
        let mut strikes = 0u32;
        loop {
            tokio::time::sleep(interval).await;
            let Some(state) = weak.upgrade() else { break };
            let (browser, generation) = {
                let mut state = state.lock().await;
                Self::reap_dead_browser(&mut state);
                (state.browser.clone(), state.generation)
            };
            // Liveness probe: a wedged browser (alive process, open CDP
            // connection, unresponsive) keeps its handler task alive, so
            // reap_dead_browser never fires on it — this probe is the wedge
            // detector. It is browser-level (Browser.getVersion) so a busy
            // renderer doesn't false-positive; any answer — or a non-timeout
            // error, the connection closing while the handler dies, which
            // the next tick's reap_dead_browser handles — means live and
            // resets the strikes. Only a timeout strikes; two consecutive
            // strikes force-reap the wedged browser.
            if let Some(browser) = browser {
                if tokio::time::timeout(PROBE_TIMEOUT, browser.version())
                    .await
                    .is_err()
                {
                    strikes += 1;
                    if strikes >= PROBE_STRIKES {
                        Self::force_reap_wedged(&state, generation).await;
                        strikes = 0;
                    }
                } else {
                    strikes = 0;
                }
            } else {
                strikes = 0;
            }
            touch_live_markers();
            let _ = tokio::task::spawn_blocking(Self::sweep_orphan_profiles).await;
        }
    }

    /// Open `url` in a new page, make it active, and return its snapshot.
    ///
    /// Scheme-less hostnames (`www.google.com`, `localhost:3000`) get an
    /// omnibox-style scheme prepended; the URL must end up on the allowed
    /// scheme list (`http`, `https`, `data`, or `file`) — the single
    /// enforcement point for both the agent tool and the UI URL bar.
    pub async fn navigate(&self, url: &str) -> Result<PageInfo> {
        // Normalize first (omnibox-style scheme autodetection), then enforce
        // the scheme allow-list — the single choke point for tool + UI.
        let url = normalize_url(url)?;

        // Spawn (if needed) and clone the browser handle under the lock, then
        // drop it — the CDP round-trips run outside the lock (Review C1).
        let browser = {
            let mut state = self.state.lock().await;
            Self::ensure_browser(&mut state).await?;
            state
                .browser
                .as_ref()
                .expect("browser just spawned")
                .clone()
        };
        // With a browser alive, make sure the watchdog is running — it reaps
        // the browser when it goes offline between operations and keeps the
        // marker touches + profile sweep going. (ensure_browser works on a
        // `&mut State` borrow and cannot spawn it itself.)
        self.ensure_watchdog().await;

        let page = self
            .cdp(
                "navigate",
                self.cdp_timeout(CDP_NAVIGATE_TIMEOUT),
                async {
                    browser
                        .new_page(url.clone())
                        .await
                        .map_err(|e| Error::Browser(format!("failed to open page '{url}': {e}")))
                },
            )
            .await?;

        // Subscribe the page's console/exception event streams HERE (before
        // returning) so no event can arrive without a registered listener —
        // a spawned task might not be scheduled in time under load. On
        // failure, close the page and leave the rest of the state untouched
        // (a failed navigate must not clobber the active page — Review C3).
        let listeners = self
            .cdp(
                "navigate",
                self.cdp_timeout(CDP_NAVIGATE_TIMEOUT),
                async {
                    let console = page.event_listener::<EventConsoleApiCalled>().await;
                    let log = page.event_listener::<EventEntryAdded>().await;
                    Ok((console, log))
                },
            )
            .await;
        let (console_stream, log_stream) = match listeners {
            Ok((Ok(c), Ok(l))) => (c, l),
            // A timeout already force-reaped the browser — propagate its
            // actionable message instead of the subscribe error below.
            Err(e) => return Err(e),
            _ => {
                // Best-effort cleanup of the half-opened page — bounded
                // like every other round-trip, so even a hanging close
                // cannot stall the op (review L3).
                let _ = self
                    .cdp(
                        "navigate",
                        self.cdp_timeout(CDP_NAVIGATE_TIMEOUT),
                        async {
                            page.close().await.map_err(|e| {
                                Error::Browser(format!("page close failed: {e}"))
                            })
                        },
                    )
                    .await;
                return Err(Error::Browser(
                    "failed to subscribe console events for the new page".into(),
                ));
            }
        };

        // Insert the page + forwarder task under a short lock; the snapshot
        // (url/title) is read outside it.
        let id = short_id();
        {
            let mut state = self.state.lock().await;
            state.console.insert(id.clone(), Default::default());
            state.pages.insert(id.clone(), page.clone());
            state.active = Some(id.clone());
            let task_state = Arc::clone(&self.state);
            let tx = self.console_tx.clone();
            let pid = id.clone();
            let page_for_task = page.clone();
            let task = tokio::spawn(async move {
                Self::forward_console_events(
                    task_state,
                    tx,
                    pid,
                    page_for_task,
                    console_stream,
                    log_stream,
                )
                .await;
            });
            state.console_tasks.insert(id.clone(), task);
        }
        self.cdp(
            "navigate",
            self.cdp_timeout(CDP_NAVIGATE_TIMEOUT),
            Self::page_info(&page, &id, true),
        )
        .await
    }

    /// List all open pages. Pages that self-closed are pruned as they are
    /// discovered (their CDP calls fail).
    pub async fn list_pages(&self) -> Result<Vec<PageInfo>> {
        // Snapshot the page handles under the lock, then read url/title
        // outside it (Review C1).
        let (snapshot, active) = {
            let state = self.state.lock().await;
            let snapshot: Vec<(String, Page)> = state
                .pages
                .iter()
                .map(|(id, page)| (id.clone(), page.clone()))
                .collect();
            (snapshot, state.active.clone())
        };

        // The per-page url()/page_info round-trips are bounded as one unit:
        // a wedged renderer hangs its page's url() read, which the timeout
        // turns into a force-reap + bounded error instead of an infinite
        // hang. (The pruning lock inside is short and cancel-safe.)
        let mut out = self
            .cdp("list_pages", self.cdp_timeout(CDP_OP_TIMEOUT), async move {
                let mut out = Vec::new();
                for (id, page) in snapshot {
                    let is_active = active.as_deref() == Some(id.as_str());
                    // A dead/zombie page fails even its url() call — prune it.
                    if page.url().await.is_err() {
                        let mut state = self.state.lock().await;
                        state.pages.remove(&id);
                        state.console.remove(&id);
                        if let Some(task) = state.console_tasks.remove(&id) {
                            task.abort();
                        }
                        if state.active.as_deref() == Some(id.as_str()) {
                            state.active = state.pages.keys().next().cloned();
                        }
                        continue;
                    }
                    out.push(Self::page_info(&page, &id, is_active).await?);
                }
                Ok(out)
            })
            .await?;
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Close a page (defaults to the active page). Closing the last page does
    /// not kill the browser process.
    pub async fn close_page(&self, page_id: Option<&str>) -> Result<()> {
        let (id, page, task) = {
            let mut state = self.state.lock().await;
            let id = Self::resolve(&state, page_id)?;
            let page = state
                .pages
                .remove(&id)
                .ok_or_else(|| Error::NotFound(format!("no such page '{id}'")))?;
            state.console.remove(&id);
            let task = state.console_tasks.remove(&id);
            if state.active.as_deref() == Some(id.as_str()) {
                state.active = state.pages.keys().next().cloned();
            }
            (id, page, task)
        };
        self.cdp(
            "close_page",
            self.cdp_timeout(CDP_OP_TIMEOUT),
            async {
                page.close()
                    .await
                    .map_err(|e| Error::Browser(format!("failed to close page '{id}': {e}")))
            },
        )
        .await?;
        if let Some(task) = task {
            task.abort();
        }
        Ok(())
    }

    /// Make `page_id` the active page for subsequent id-less operations.
    pub async fn switch_page(&self, page_id: &str) -> Result<PageInfo> {
        let page = {
            let mut state = self.state.lock().await;
            let page = state
                .pages
                .get(page_id)
                .cloned()
                .ok_or_else(|| Error::NotFound(format!("no such page '{page_id}'")))?;
            state.active = Some(page_id.to_string());
            page
        };
        self.cdp(
            "switch_page",
            self.cdp_timeout(CDP_OP_TIMEOUT),
            Self::page_info(&page, page_id, true),
        )
        .await
    }

    /// Capture a PNG screenshot of the page (defaults to active).
    pub async fn screenshot(&self, page_id: Option<&str>) -> Result<Vec<u8>> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        self.cdp(
            "screenshot",
            self.cdp_timeout(CDP_OP_TIMEOUT),
            async {
                page.screenshot(chromiumoxide::page::ScreenshotParams::default())
                    .await
                    .map_err(|e| Error::Browser(format!("screenshot failed on '{id}': {e}")))
            },
        )
        .await
    }

    /// Drain the page's buffered console entries (defaults to active).
    ///
    /// The *agent tool* uses this destructive form (a read-then-clear is the
    /// "unread messages" semantic the LLM wants); the UI uses
    /// [`console_peek`](Self::console_peek) so a tab refresh cannot starve the
    /// agent of entries (Review C4/S6).
    pub async fn console(&self, page_id: Option<&str>) -> Result<Vec<ConsoleEntry>> {
        let mut state = self.state.lock().await;
        let id = Self::resolve(&state, page_id)?;
        let buf = state
            .console
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("no such page '{id}'")))?;
        Ok(buf.drain(..).collect())
    }

    /// Read the page's buffered console entries WITHOUT draining them (the
    /// UI's refresh path — the agent tool keeps its exclusive drain).
    pub async fn console_peek(&self, page_id: Option<&str>) -> Result<Vec<ConsoleEntry>> {
        let state = self.state.lock().await;
        let id = Self::resolve(&state, page_id)?;
        let buf = state
            .console
            .get(&id)
            .ok_or_else(|| Error::NotFound(format!("no such page '{id}'")))?;
        Ok(buf.iter().cloned().collect())
    }

    /// A text snapshot of the page's DOM (defaults to active) — the cheap way
    /// for the agent to "see" the page without a screenshot.
    pub async fn snapshot(&self, page_id: Option<&str>) -> Result<String> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        self.cdp(
            "snapshot",
            self.cdp_timeout(CDP_OP_TIMEOUT),
            async {
                page.content()
                    .await
                    .map_err(|e| Error::Browser(format!("snapshot failed on '{id}': {e}")))
            },
        )
        .await
    }

    /// Evaluate a JavaScript expression and return its JSON value. Promises
    /// are awaited (`awaitPromise`), and the result is returned by value so
    /// resolved primitives/objects come back as JSON rather than handles.
    pub async fn eval(&self, page_id: Option<&str>, expr: &str) -> Result<serde_json::Value> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        let params = EvaluateParams::builder()
            .expression(expr)
            .await_promise(true)
            .return_by_value(true)
            .build()
            .map_err(|e| Error::Browser(format!("invalid eval params: {e}")))?;
        let out = self
            .cdp(
                "eval",
                self.cdp_timeout(CDP_OP_TIMEOUT),
                async {
                    page.evaluate_expression(params)
                        .await
                        .map_err(|e| Error::Browser(format!("eval failed on '{id}': {e}")))
                },
            )
            .await?;
        Ok(out.value().cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Click the element matching `selector` (defaults to active page).
    pub async fn click(&self, page_id: Option<&str>, selector: &str) -> Result<()> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        self.cdp(
            "click",
            self.cdp_timeout(CDP_OP_TIMEOUT),
            async {
                let el = page.find_element(selector).await.map_err(|e| {
                    Error::Browser(format!("click: no element '{selector}' on '{id}': {e}"))
                })?;
                el.click()
                    .await
                    .map_err(|e| Error::Browser(format!("click failed on '{selector}': {e}")))?;
                Ok(())
            },
        )
        .await
    }

    /// Focus the element matching `selector` and type `text` into it.
    pub async fn type_text(&self, page_id: Option<&str>, selector: &str, text: &str) -> Result<()> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        self.cdp(
            "type",
            self.cdp_timeout(CDP_OP_TIMEOUT),
            async {
                let el = page
                    .find_element(selector)
                    .await
                    .map_err(|e| {
                        Error::Browser(format!("type: no element '{selector}' on '{id}': {e}"))
                    })?;
                el.click()
                    .await
                    .map_err(|e| Error::Browser(format!("focus failed on '{selector}': {e}")))?;
                el.type_str(text)
                    .await
                    .map_err(|e| Error::Browser(format!("type failed on '{selector}': {e}")))?;
                Ok(())
            },
        )
        .await
    }

    /// Shut the browser down and drop all pages + the throwaway profile. A
    /// subsequent operation re-spawns it lazily. We don't await
    /// `Browser::close` because its shutdown handshake races the aborted
    /// handler and errors spuriously; instead the child process is killed
    /// explicitly (see [`Self::kill_browser`]) before the profile dir is
    /// removed on a background retry loop.
    ///
    /// The kill runs inline (after the state lock is released) so that
    /// returning from `close()` means the child process is dead — the Tauri
    /// exit hook and the profile-removal tests rely on that contract (see
    /// the module locking note).
    pub async fn close(&self) -> Result<()> {
        // Short lock: abort the handlers and detach everything. The kill +
        // removal below run outside the lock — the kill can take up to ~5s
        // while an in-flight CDP operation's browser clone drains.
        let (browser, profile) = {
            let mut state = self.state.lock().await;
            if let Some(task) = state.handler.take() {
                task.abort();
            }
            for (_, task) in state.console_tasks.drain() {
                task.abort();
            }
            state.pages.clear();
            state.console.clear();
            state.active = None;
            (state.browser.take(), state.profile.take())
        };
        // The profile dir left the state — declare it dead right away so
        // the sweep reaps it if the removal below never runs (kill failure)
        // or fails past its retry budget.
        if let Some(profile) = &profile {
            unregister_live_profile(profile.path());
        }

        if let Some(browser) = browser {
            // Only schedule profile removal once the child is actually dead —
            // a live child holds locks on it, so removal would fail forever
            // (Review M1).
            if Self::kill_browser(browser).await {
                if let Some(profile) = profile {
                    Self::schedule_profile_removal(profile);
                }
            }
        }
        Ok(())
    }

    /// Kill a launched Chromium child process and wait for it to exit.
    /// Returns whether the kill was actually performed.
    ///
    /// `BrowserConfig::launch` in chromiumoxide 0.7 does NOT set
    /// `kill_on_drop` (despite its `Drop` comment claiming otherwise), so
    /// dropping a `Browser` leaves the child alive holding locks on the
    /// profile dir — and `remove_dir_all` then fails forever. Killing
    /// explicitly (kill + wait, inside `Browser::kill`) releases the OS file
    /// locks deterministically before the profile dir is removed.
    ///
    /// The browser is shared via `Arc`; killing requires sole ownership,
    /// which a concurrent CDP operation (e.g. `navigate`'s `new_page` call)
    /// may briefly hold. Those clones die with the CDP connection — the
    /// handler task is aborted / has finished before we get here — so
    /// `Arc::try_unwrap` is retried briefly instead of giving up at the first
    /// collision. If the retry budget is exhausted the child is left alone
    /// and `false` is returned so the caller skips profile removal: a live
    /// child still holds locks on it (Review M1).
    async fn kill_browser(browser: Arc<Browser>) -> bool {
        let mut browser = browser;
        for _ in 0..50 {
            match Arc::try_unwrap(browser) {
                Ok(mut owned) => {
                    let _ = owned.kill().await;
                    return true;
                }
                Err(arc) => browser = arc,
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        false
    }

    /// Remove a throwaway profile dir on a background retry loop. The OS
    /// releases the dead child's file locks asynchronously even after a
    /// waited kill (crashpad handlers, AV scans, ...), so `remove_dir_all`
    /// can fail transiently (Review B1). `TempDir::keep` hands ownership of
    /// the path to the loop; retries stop after ~15s. A plain OS thread (not
    /// a tokio task) because the removal must outlive the tokio runtime — a
    /// spawned task is cancelled at runtime shutdown (test end / app exit),
    /// which orphaned profile dirs.
    fn schedule_profile_removal(profile: tempfile::TempDir) {
        let path = profile.keep();
        std::thread::spawn(move || {
            for _ in 0..60 {
                if std::fs::remove_dir_all(&path).is_ok() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
        });
    }

    /// Resolve `page_id` (or the active page when `None`) to the page's id +
    /// a cloned handle, taking the lock only long enough to do the lookup
    /// (Review C1). The returned pair is owned — CDP calls run lock-free.
    async fn lookup(state: &Arc<Mutex<State>>, page_id: Option<&str>) -> Result<(String, Page)> {
        let st = state.lock().await;
        let id = Self::resolve(&st, page_id)?;
        let page = st
            .pages
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("no such page '{id}'")))?;
        Ok((id, page))
    }

    /// Resolve `page_id` (or the active page when `None`) to an existing id.
    fn resolve(state: &State, page_id: Option<&str>) -> Result<String> {
        match page_id {
            Some(id) => Ok(id.to_string()),
            None => state
                .active
                .clone()
                .ok_or_else(|| Error::Browser("no open page — call browser_navigate first".into())),
        }
    }

    /// Build a [`PageInfo`] snapshot from a page handle, reading url/title
    /// over CDP. Callers must NOT hold the state lock.
    async fn page_info(page: &Page, id: &str, active: bool) -> Result<PageInfo> {
        let url = page.url().await.ok().flatten().unwrap_or_default();
        let title = page.get_title().await.ok().flatten().unwrap_or_default();
        Ok(PageInfo {
            id: id.to_string(),
            url,
            title,
            active,
        })
    }

    /// Run one page's console-event loop: drain the two (already-subscribed)
    /// CDP event streams independently (one domain closing doesn't stop the
    /// other — Review C5), format each event into a [`ConsoleEvent`], push it
    /// into the page's ring buffer (capped), and broadcast it to live
    /// subscribers. Ends when both streams close or the task is aborted.
    async fn forward_console_events(
        state: Arc<Mutex<State>>,
        tx: broadcast::Sender<ConsoleEvent>,
        page_id: String,
        page: Page,
        console_stream: EventStream<EventConsoleApiCalled>,
        log_stream: EventStream<EventEntryAdded>,
    ) {
        // Explicitly enable both domains so events flow (the listener
        // registration usually does this, but enabling is cheap insurance).
        let _ = page
            .execute(chromiumoxide::cdp::js_protocol::runtime::EnableParams::default())
            .await;
        let _ = page
            .execute(chromiumoxide::cdp::browser_protocol::log::EnableParams::default())
            .await;

        let mut console_stream = Box::pin(console_stream.fuse());
        let mut log_stream = Box::pin(log_stream.fuse());
        let mut console_done = false;
        let mut log_done = false;
        loop {
            tokio::select! {
                ev = console_stream.next(), if !console_done => match ev {
                    Some(ev) => {
                        let event = ConsoleEvent {
                            page_id: page_id.clone(),
                            level: ev.r#type.as_ref().to_string(),
                            text: Self::format_console_args(&ev.args),
                        };
                        Self::record_console(&state, &tx, &page_id, event).await;
                    }
                    None => console_done = true,
                },
                ev = log_stream.next(), if !log_done => match ev {
                    Some(ev) => {
                        let event = ConsoleEvent {
                            page_id: page_id.clone(),
                            level: ev.entry.level.as_ref().to_string(),
                            text: ev.entry.text.clone(),
                        };
                        Self::record_console(&state, &tx, &page_id, event).await;
                    }
                    None => log_done = true,
                },
            }
            if console_done && log_done {
                break;
            }
        }
    }

    /// Format a `console.log(...)` call's arguments into one text line, like a
    /// devtools console would (joined descriptions; JSON strings unwrapped).
    fn format_console_args(
        args: &[chromiumoxide::cdp::js_protocol::runtime::RemoteObject],
    ) -> String {
        let parts: Vec<String> = args
            .iter()
            .map(|arg| {
                arg.description.clone().unwrap_or_else(|| match &arg.value {
                    // JSON strings carry quotes ("hello") — show the bare text
                    // like devtools does.
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                    None => arg
                        .unserializable_value
                        .as_ref()
                        .map(|u| u.as_ref().to_string())
                        .unwrap_or_default(),
                })
            })
            .collect();
        parts.join(" ")
    }

    /// Append a console event to its page's capped ring buffer and broadcast
    /// it to live subscribers. Locks state only briefly per event.
    async fn record_console(
        state: &Arc<Mutex<State>>,
        tx: &broadcast::Sender<ConsoleEvent>,
        page_id: &str,
        event: ConsoleEvent,
    ) {
        {
            let mut st = state.lock().await;
            if let Some(buf) = st.console.get_mut(page_id) {
                if buf.len() >= CONSOLE_CAP {
                    buf.pop_front();
                }
                buf.push_back(ConsoleEntry {
                    level: event.level.clone(),
                    text: event.text.clone(),
                });
            }
        }
        // Non-blocking: subscribers that fall behind are dropped (they can
        // always re-read the ring buffer via browser_console).
        let _ = tx.send(event);
    }
}

impl Default for BrowserManager {
    fn default() -> Self {
        let (console_tx, _) = broadcast::channel(512);
        Self {
            state: Arc::new(Mutex::new(State {
                browser: None,
                generation: 0,
                handler: None,
                pages: HashMap::new(),
                active: None,
                console: HashMap::new(),
                console_tasks: HashMap::new(),
                profile: None,
                watchdog: None,
            })),
            console_tx,
            reap_interval_ms: AtomicU64::new(WATCHDOG_INTERVAL.as_millis() as u64),
            cdp_timeout_ms: AtomicU64::new(0),
        }
    }
}

/// Normalize a URL input (omnibox-style scheme autodetection + scheme
/// allow-list). The single choke point shared by the agent tool and the UI URL
/// bar: scheme-less hostnames get an
/// omnibox-style scheme (`https://`; `http://` for localhost and IP
/// literals), then the URL scheme allow-list (`http`, `https`, `data`,
/// `file`) is enforced, so `javascript:` and other schemes can never cross
/// the sandbox boundary or trigger SSRF via the browser.
///
/// `http://…`, `https://…`, `data:…`, and `file://…` pass through unchanged;
/// a bare hostname (`www.google.com`, `localhost:3000`, `127.0.0.1:8080`)
/// gets `https://` — or `http://` for localhost and IP literals — prepended.
/// Everything else (`javascript:`, `about:`, drive paths, a `file:` prefix
/// without the `//` authority form, ...) is rejected.
pub fn normalize_url(url: &str) -> Result<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(format!("empty URL: '{url}'")));
    }
    let (candidate, rest) = match trimmed.split_once(':') {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => (String::new(), ""),
    };
    match candidate.as_str() {
        "http" | "https" | "data" => return Ok(trimmed.to_string()),
        // file: only in its standard `file://…` form (the authority-abandoned
        // `file:///C:/…` shape every browser accepts). A bare `file:x` stays
        // rejected so malformed inputs surface as errors instead of silently
        // becoming path-relative file URLs.
        "file" if rest.starts_with("//") => return Ok(trimmed.to_string()),
        // Any other `file:`-shaped input (`file:`, `file:foo`, `file:/x`) —
        // give a precise hint instead of the generic scheme rejection, which
        // would confusingly list `file` as allowed.
        "file" => {
            return Err(Error::InvalidInput(format!(
                "file URL '{url}' must use the file:///path form \
                 (e.g. file:///C:/page.html or file:///home/user/page.html)"
            )));
        }
        // A scheme-looking prefix that is not allowed (javascript:, about:,
        // mailto:, C:\…, ...) — keep the precise rejection. A colon whose
        // tail is only digits is a host:port, not a scheme, so
        // `localhost:3000` falls through to autodetection.
        other if is_scheme_like(other) && !rest.chars().all(|c| c.is_ascii_digit()) => {
            return Err(Error::InvalidInput(format!(
                "unsupported URL scheme '{other}' in '{url}' — only {} are allowed",
                ALLOWED_SCHEMES.join(", ")
            )));
        }
        _ => {}
    }
    let scheme = autodetect_scheme(trimmed).ok_or_else(|| {
        Error::InvalidInput(format!(
            "'{url}' is not a valid URL — expected http(s)://, data:, file://, \
             or a hostname like example.com"
        ))
    })?;
    Ok(format!("{scheme}://{trimmed}"))
}

/// Whether `s` looks like a URL scheme (`[a-z][a-z0-9+.-]*`).
fn is_scheme_like(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

/// Pick the scheme a browser omnibox would choose for a scheme-less input,
/// or `None` when the input is not a plausible hostname/authority.
fn autodetect_scheme(input: &str) -> Option<&'static str> {
    // Whitespace never appears in a bare hostname — reject outright.
    if input.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let authority = input.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return None;
    }
    // IPv6 literal ([::1]:port) — loopback-family, so plain http.
    if authority.starts_with('[') {
        return Some("http");
    }
    // Split off an explicit :port — it must be all digits.
    let host = match authority.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
        Some(_) => return None,
        None => authority,
    };
    let host = host.trim_end_matches('.');
    if host.is_empty() {
        return None;
    }
    if host.eq_ignore_ascii_case("localhost") {
        return Some("http");
    }
    // All digits + dots → an IPv4 address → plain http.
    if host.contains('.') && host.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Some("http");
    }
    // A dotted hostname → https (the modern default; browsers try https first).
    if host.contains('.') {
        return Some("https");
    }
    None
}

/// A short, filesystem-safe page id.
fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn navigate_and_list_pages() {
        let manager = BrowserManager::new();
        let page = manager
            .navigate("data:text/html,<title>hello</title><h1>hi</h1>")
            .await
            .expect("navigate should succeed");
        assert!(page.active, "first page should be active");
        let pages = manager.list_pages().await.expect("list should succeed");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, page.id);
        manager.close().await.expect("close should succeed");
    }

    #[tokio::test]
    async fn resolve_errors_without_any_page() {
        let manager = BrowserManager::new();
        let err = manager
            .screenshot(None)
            .await
            .expect_err("screenshot with no page should error");
        assert!(err.to_string().contains("no open page"), "got: {err}");
    }

    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn console_events_stream_to_buffer_and_broadcast() {
        let manager = BrowserManager::new();
        let mut sub = manager.subscribe_console();
        let page = manager
            .navigate("data:text/html,<h1>ready</h1>")
            .await
            .expect("navigate should succeed");

        // Trigger console calls AFTER the per-page forwarder is subscribed
        // (load-time logs fire before any listener can exist).
        manager
            .eval(
                Some(&page.id),
                "console.log('hello'); console.warn('careful')",
            )
            .await
            .expect("eval should succeed");

        // The broadcast should deliver both events (generous deadline — the
        // suite runs many Chromium instances in parallel, so delivery can lag).
        let mut got: Vec<ConsoleEvent> = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            let have_all = {
                let texts: Vec<&str> = got.iter().map(|e| e.text.as_str()).collect();
                texts.contains(&"hello") && texts.contains(&"careful")
            };
            if have_all {
                break;
            }
            if let Ok(Ok(ev)) =
                tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await
            {
                got.push(ev)
            }
        }
        let texts: Vec<&str> = got.iter().map(|e| e.text.as_str()).collect();
        assert!(
            texts.contains(&"hello") && texts.contains(&"careful"),
            "expected both console events, got {got:?}"
        );

        // The ring buffer holds them; peek (UI path) preserves them, drain
        // (agent path) consumes them (Review C4).
        let peeked = manager
            .console_peek(None)
            .await
            .expect("console peek should succeed");
        let buf_texts: Vec<&str> = peeked.iter().map(|e| e.text.as_str()).collect();
        assert!(
            buf_texts.contains(&"hello") && buf_texts.contains(&"careful"),
            "peek should show both: {peeked:?}"
        );
        let drained = manager
            .console(None)
            .await
            .expect("console drain should succeed");
        assert_eq!(drained.len(), peeked.len(), "drain should match peek");
        let after = manager
            .console(None)
            .await
            .expect("second drain should succeed");
        assert!(after.is_empty(), "drain should empty the buffer");
        manager.close().await.expect("close should succeed");
    }

    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn eval_awaits_promises() {
        let manager = BrowserManager::new();
        manager
            .navigate("data:text/html,<p>x</p>")
            .await
            .expect("navigate should succeed");
        let v = manager
            .eval(None, "Promise.resolve(42)")
            .await
            .expect("eval should succeed");
        assert_eq!(
            v,
            serde_json::json!(42),
            "awaitPromise should resolve the value"
        );
        manager.close().await.expect("close should succeed");
    }

    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn navigate_loads_file_url() {
        // Pure HTML debugging end to end: a real on-disk HTML file loaded
        // through the file:// scheme — normalize_url passes it through and
        // headless Chromium renders it. Guards the deliberate file://
        // relaxation of the scheme allow-list.
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("page.html");
        std::fs::write(
            &path,
            "<!DOCTYPE html><html><head><title>local file</title></head>\
             <body><h1>from disk</h1></body></html>",
        )
        .expect("write temp html");
        // Build the canonical file:// form for both platforms: POSIX
        // absolute paths already start with '/', Windows drive paths don't.
        let display = path.to_string_lossy().replace('\\', "/");
        let url = if display.starts_with('/') {
            format!("file://{display}")
        } else {
            format!("file:///{display}")
        };
        let manager = BrowserManager::new();
        let page = manager
            .navigate(&url)
            .await
            .expect("file:// navigation should succeed");
        let title = manager
            .eval(Some(&page.id), "document.title")
            .await
            .expect("eval should succeed");
        assert_eq!(
            title,
            serde_json::json!("local file"),
            "file:// page should have rendered"
        );
        manager.close().await.expect("close should succeed");
    }

    #[tokio::test]
    async fn navigate_rejects_disallowed_schemes() {
        let manager = BrowserManager::new();
        // file:///… is intentionally NOT in this list anymore — it is allowed
        // for local HTML debugging (covered by the normalize_url tests and
        // the ignored file:// integration test). javascript:/about: must
        // still fail before anything spawns.
        for bad in ["javascript:alert(1)", "about:blank"] {
            let err = manager
                .navigate(bad)
                .await
                .expect_err(&format!("{bad} should be rejected"));
            assert!(
                err.to_string().contains("unsupported URL scheme"),
                "got: {err}"
            );
        }
        // The rejection must not have spawned anything or left pages behind.
        let pages = manager.list_pages().await.expect("list should succeed");
        assert!(pages.is_empty(), "no pages should exist: {pages:?}");
    }

    #[test]
    fn normalize_url_autodetects_bare_hostnames() {
        let cases = [
            ("www.google.com", "https://www.google.com"),
            ("google.com/path?q=1", "https://google.com/path?q=1"),
            ("sub.domain.co.uk/x", "https://sub.domain.co.uk/x"),
            ("example.com:8080", "https://example.com:8080"),
            ("example.com#frag", "https://example.com#frag"),
            ("example.com.", "https://example.com."),
            ("localhost:3000", "http://localhost:3000"),
            ("LOCALHOST:5173", "http://LOCALHOST:5173"),
            ("127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("10.0.0.1", "http://10.0.0.1"),
            ("[::1]:3000", "http://[::1]:3000"),
            ("  example.com  ", "https://example.com"),
            ("http://example.com", "http://example.com"),
            ("HTTPS://EXAMPLE.COM", "HTTPS://EXAMPLE.COM"),
            ("data:text/html,<h1>hi</h1>", "data:text/html,<h1>hi</h1>"),
            // file:// URLs pass through unchanged (pure HTML debugging) —
            // Windows drive, POSIX, UNC-host, and uppercase-scheme forms.
            ("file:///C:/temp/page.html", "file:///C:/temp/page.html"),
            ("file:///etc/hosts", "file:///etc/hosts"),
            ("file://server/share/x.html", "file://server/share/x.html"),
            ("FILE:///C:/temp/page.html", "FILE:///C:/temp/page.html"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                normalize_url(input).unwrap_or_else(|e| panic!("{input}: {e}")),
                expected,
                "input: {input}"
            );
        }
    }

    #[test]
    fn normalize_url_rejects_non_urls() {
        for bad in [
            "",
            "   ",
            "about:blank",
            "javascript:alert(1)",
            "mailto:user@example.com",
            "C:\\foo",
            "hello world",
            "example.com:8080abc",
            "localhost:abc",
            // file: is allowed only in its `file://…` form — malformed
            // prefixes must still be rejected. (`file://C:/page.html/` is
            // NOT rejected: `//C:` is the classic Windows host-style file
            // URL and browsers accept it.)
            "file:",
            "file:foo",
            "file:/C:/page.html",
        ] {
            assert!(normalize_url(bad).is_err(), "should reject: {bad:?}");
        }
        let err = normalize_url("").unwrap_err();
        assert!(err.to_string().contains("empty URL"), "got: {err}");
        let err = normalize_url("javascript:alert(1)").unwrap_err();
        assert!(
            err.to_string().contains("unsupported URL scheme"),
            "got: {err}"
        );
        let err = normalize_url("file:foo").unwrap_err();
        assert!(err.to_string().contains("file:///path form"), "got: {err}");
    }

    /// Regression (backlog 2ccf92b0): orphaned `mnemo-browser-*` profile dirs
    /// from dead managers must be reaped by the sweep even when younger than
    /// the old 1h cutoff — in-session orphans accumulated all session
    /// otherwise (the sweep only ran at launch time and only removed >1h-old
    /// dirs). Live profiles — this process's (registry) or another
    /// instance's (fresh marker) — must never be touched.
    #[test]
    fn sweep_reaps_orphans_but_never_live_profiles() {
        let mk = || {
            tempfile::Builder::new()
                .prefix("mnemo-browser-")
                .tempdir_in(std::env::temp_dir())
                .expect("tempdir")
        };
        let backdate = |dir: &Path, age: std::time::Duration| {
            std::fs::File::options()
                .write(true)
                .open(dir.join(LIVE_MARKER))
                .expect("marker")
                .set_modified(std::time::SystemTime::now() - age)
                .expect("backdate marker")
        };

        // A: ours, live — registered (register creates a fresh marker).
        let ours = mk();
        register_live_profile(ours.path());
        // B: another instance, live — not registered, fresh marker.
        let theirs = mk();
        std::fs::File::create(theirs.path().join(LIVE_MARKER)).expect("marker");
        // C: dead manager — not registered, marker backdated past grace.
        let dead = mk();
        std::fs::File::create(dead.path().join(LIVE_MARKER)).expect("marker");
        backdate(dead.path(), MARKER_GRACE + std::time::Duration::from_secs(60));
        // D: legacy leftover — no marker, fresh dir. The >1h-removal branch
        // is carried over unchanged from the pre-marker sweep and is not
        // re-tested here (a dir's mtime cannot be backdated portably from
        // std); the fresh-dir keep below pins the conservative fallback.
        let legacy = mk();
        // E: future-dated marker (clock stepped back between the owner's
        // touch and this sweep) — any doubt keeps the dir (review LOW-1).
        let skewed = mk();
        std::fs::File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(skewed.path().join(LIVE_MARKER))
            .expect("marker")
            .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(3600))
            .expect("future-date marker");

        BrowserManager::sweep_orphan_profiles();

        assert!(ours.path().exists(), "live (ours) profile must survive");
        assert!(
            theirs.path().exists(),
            "live (another instance) profile must survive"
        );
        assert!(legacy.path().exists(), "fresh legacy profile must survive");
        assert!(
            skewed.path().exists(),
            "future-dated marker (clock skew) must keep the dir"
        );
        assert!(
            !dead.path().exists(),
            "dead manager's profile must be reaped"
        );

        // Retiring our profile (what reap/close do) must make it sweepable
        // immediately — the backdated marker declares it dead.
        unregister_live_profile(ours.path());
        BrowserManager::sweep_orphan_profiles();
        assert!(!ours.path().exists(), "retired profile must be reaped");

        drop(theirs);
        drop(legacy);
        drop(skewed);
    }

    /// Regression (backlog 2ccf92b0): a browser that goes offline (process
    /// death / dropped CDP connection) must be reaped by the watchdog
    /// WITHOUT any subsequent browser operation — before the fix, the dead
    /// process + its profile dir lingered until the next operation (or app
    /// exit), so offline browsers accumulated throughout a session.
    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn watchdog_reaps_dead_browser_without_subsequent_operation() {
        let manager = BrowserManager::new();
        manager.set_reap_interval(std::time::Duration::from_millis(200));
        manager
            .navigate("data:text/html,<p>offline-browser-reap</p>")
            .await
            .expect("navigate");

        // Simulate the browser going offline: the CDP connection drops
        // (handler task ends) while the process may still be alive — and no
        // further operation ever comes.
        let profile_path = {
            let mut state = manager.state.lock().await;
            state.handler.take().expect("handler").abort();
            state.profile.as_ref().expect("profile").path().to_path_buf()
        };

        // The watchdog must reap it on its own: browser handle cleared...
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while manager.state.lock().await.browser.is_some() {
            assert!(
                std::time::Instant::now() < deadline,
                "watchdog did not reap the dead browser"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // ...then the kill + removal pipeline takes the profile dir away.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while profile_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "profile dir was not removed: {profile_path:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        manager.close().await.expect("close");
    }

    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn respawn_after_close() {
        let manager = BrowserManager::new();
        let first = manager
            .navigate("data:text/html,<p>one</p>")
            .await
            .expect("first navigate should succeed");
        manager.close().await.expect("close should succeed");
        // A fresh browser spawns on demand; old pages are gone.
        let second = manager
            .navigate("data:text/html,<p>two</p>")
            .await
            .expect("second navigate should succeed");
        assert_ne!(first.id, second.id);
        let pages = manager.list_pages().await.expect("list should succeed");
        assert_eq!(pages.len(), 1, "only the new page should exist: {pages:?}");
        manager.close().await.expect("close should succeed");
    }

    /// Regression (plan ec425270): a wedged renderer — alive browser process,
    /// open CDP connection, unresponsive page (e.g. stuck in an infinite JS
    /// loop) — must not hang launch-mode operations forever. The op must
    /// return a bounded error ("unresponsive" + "restart"), the wedged
    /// browser must be force-reaped, and the next operation must transparently
    /// respawn a fresh one (self-heal). Pre-fix, the eval never returns and
    /// the 10s outer guard fires — the repro.
    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn wedge_renderer_times_out_and_self_heals() {
        let manager = BrowserManager::new();
        // Short CDP timeout so the wedge fires in ~3s instead of ~30s — but
        // generous enough that a healthy cold-start navigate (browser
        // spawn + target creation) never false-fires under it.
        manager.set_cdp_timeout(std::time::Duration::from_secs(3));
        // Healthy ops must not false-fire under the short timeout.
        manager
            .navigate("data:text/html,<h1>hi</h1>")
            .await
            .expect("navigate should succeed under the short CDP timeout");

        // Wedge the renderer: the infinite loop blocks the page's main
        // thread, so the Runtime.evaluate response never arrives.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            manager.eval(None, "while(true){}"),
        )
        .await;
        let err = match outcome {
            // The outer guard fired: the eval never returned — the defect.
            Err(_) => {
                // Clean up the wedged browser (the fix force-reaps it
                // automatically) so the repro leaves no process behind.
                manager.close().await.expect("close should succeed");
                panic!("eval hung >10s on a wedged renderer — the defect");
            }
            Ok(Err(e)) => e,
            Ok(Ok(v)) => panic!("eval should fail on a wedged renderer, got {v:?}"),
        };
        let msg = err.to_string();
        assert!(msg.contains("unresponsive"), "got: {msg}");
        assert!(msg.contains("restart"), "got: {msg}");

        // Self-heal: the next operation respawns a fresh browser, and only
        // the new page exists (the wedged one died with its browser).
        let page = manager
            .navigate("data:text/html,<h1>again</h1>")
            .await
            .expect("navigate after the wedge should respawn a fresh browser");
        let pages = manager.list_pages().await.expect("list should succeed");
        assert_eq!(pages.len(), 1, "only the fresh page should exist: {pages:?}");
        assert_eq!(pages[0].id, page.id, "the fresh page should be listed");
        manager.close().await.expect("close should succeed");
    }

    /// The `cdp` wrapper passes a successful operation through unchanged —
    /// the timeout must not alter the op's own result or error.
    #[tokio::test]
    async fn cdp_wrapper_passes_ok_through() {
        let manager = BrowserManager::new();
        let out = manager
            .cdp(
                "eval",
                CDP_OP_TIMEOUT,
                async { Ok::<_, Error>(serde_json::json!(42)) },
            )
            .await
            .expect("an ok future must pass through");
        assert_eq!(out, serde_json::json!(42));
    }

    /// The `cdp` wrapper bounds a never-resolving operation: it must return
    /// an error mentioning "unresponsive" + "restart" instead of hanging
    /// forever (the defect). Force-reaping a browser-less manager is a
    /// no-op, so this needs no Chromium.
    #[tokio::test]
    async fn cdp_wrapper_times_out_pending_future() {
        let manager = BrowserManager::new();
        let err = manager
            .cdp::<()>(
                "eval",
                std::time::Duration::from_millis(50),
                std::future::pending(),
            )
            .await
            .expect_err("a never-resolving op must time out");
        let msg = err.to_string();
        assert!(msg.contains("unresponsive"), "got: {msg}");
        assert!(msg.contains("restart"), "got: {msg}");
    }

    /// The watchdog's liveness probe must not false-positive on a healthy
    /// browser: several probe ticks against a live, responsive browser must
    /// never accumulate [`PROBE_STRIKES`] (which would force-reap it).
    #[tokio::test]
    #[ignore = "integration: spawns a headless Chromium process; run with `cargo test -- --ignored`"]
    async fn watchdog_probe_does_not_reap_healthy_browser() {
        let manager = BrowserManager::new();
        manager.set_reap_interval(std::time::Duration::from_millis(250));
        manager
            .navigate("data:text/html,<p>probe</p>")
            .await
            .expect("navigate should succeed");
        // Several probe ticks against the healthy browser.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let pages = manager.list_pages().await.expect("list should succeed");
        assert_eq!(
            pages.len(),
            1,
            "the healthy browser must not be reaped by the probe: {pages:?}"
        );
        manager.close().await.expect("close should succeed");
    }

    /// Windows-only (user decision 2026-09-20): this test launches a real
    /// headless Chromium and polls 15s for the profile dir's removal — on
    /// the macOS CI leg (run 35521221353) the dir outlives the poll, and
    /// the root cause is not diagnosable from a Windows host. Gated
    /// rather than deadline-widened; Windows, where the app ships, keeps
    /// the coverage.
    #[cfg(windows)]
    #[tokio::test]
    async fn profile_dir_is_removed_on_close() {
        let manager = BrowserManager::new();
        manager
            .navigate("data:text/html,<p>x</p>")
            .await
            .expect("navigate should succeed");
        // Find the manager's profile dir while it's alive.
        let profile_path = {
            let st = manager.state.lock().await;
            st.profile
                .as_ref()
                .expect("profile should exist")
                .path()
                .to_path_buf()
        };
        assert!(profile_path.exists(), "profile dir should exist");
        manager.close().await.expect("close should succeed");
        // The removal runs on a short background retry loop (Chromium releases
        // file locks asynchronously) — poll with a deadline.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        while profile_path.exists() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert!(
            !profile_path.exists(),
            "profile dir should be removed on close (Review B1)"
        );
    }

}
