// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Headless-Chromium debug browser, driven over the Chrome DevTools Protocol.
//!
//! [`BrowserManager`] lazily spawns a single headless Chromium process (via
//! `chromiumoxide`) on first use and multiplexes it across any number of open
//! pages (tabs). It is shared between the agent tools (`src/tool/browser/`) and
//! the Tauri IPC layer (`src-tauri/src/ipc/browser.rs`) behind an `Arc`, so the
//! agent and the right-panel Browser tab drive the *same* browser.
//!
//! Lifecycle: the browser spawns on the first operation that needs it, and is
//! re-spawned transparently when the process dies (the handler task ends, the
//! stale state is cleared, and the next operation launches fresh). A
//! background watchdog (spawned with the first browser) reaps a dead browser
//! on its next tick — one that goes offline between operations no longer
//! lingers until the next one — and periodically sweeps orphaned profile
//! dirs: each profile carries a `.mnemo-live` marker touched every tick, so
//! live profiles (this process's or another instance's) are never touched
//! while dead managers' dirs are reaped regardless of age. Call
//! [`BrowserManager::close`] to shut it down. All operations take an optional
//! `page_id`; when omitted they act on the *active* page (the most recently
//! opened or switched-to page).
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
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// A live console event, pushed to subscribers (the IPC layer's forwarder) the
/// moment a page logs something. `page_id` ties it to the page that emitted it.
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
    /// The CDP-attached WebView2 browser (the live app webview the human plays
    /// the game in), connected via `Browser::connect` to the debug port. This
    /// is separate from the launch-mode `browser` above — the agent may use
    /// both (the headless browser for its own pages, the webview to inspect
    /// the human's game).
    webview: Option<Arc<Browser>>,
    /// The handler task driving the webview's CDP connection; its end signals
    /// the connection died (the app closed / the WebView2 went away).
    webview_handler: Option<JoinHandle<()>>,
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

/// The app-layer hook that creates the Browser tab's child WebView2 when the
/// agent needs it and none exists yet. The lib crate cannot create a Tauri
/// webview (no Tauri dependency), so the app layer (src-tauri) installs a
/// closure via [`BrowserManager::set_child_ensurer`] that calls the
/// `browser_webview_ensure_for_agent` command's logic; `webview_navigate`
/// invokes it with the normalized target URL when the CDP poll finds no
/// child target. The closure must be `Send + Sync` and returns the outcome
/// as a plain message (`Err(text)` on failure).
pub type ChildEnsurer = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = std::result::Result<(), String>> + Send>>
        + Send
        + Sync,
>;

/// A shared, lazily-spawned headless Chromium browser with multi-page support.
///
/// Clone-cheap (`Arc` inside); construct once and share across tools + IPC.
/// Also owns an optional CDP *attach* connection to the app's live WebView2
/// (the game the human plays) — see the `webview_*` methods.
pub struct BrowserManager {
    state: Arc<Mutex<State>>,
    /// Live console events, broadcast to subscribers (the Tauri forwarder).
    console_tx: broadcast::Sender<ConsoleEvent>,
    /// The CDP debug endpoint of the app's WebView2 (set via
    /// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` in dev builds). Defaults to the
    /// production port; tests override it to attach to a launched stand-in.
    webview_url: String,
    /// Whether the WebView2 CDP attach is enabled — i.e. whether the debug
    /// port is actually exposed. Debug builds always enable it (the env var is
    /// set unconditionally under `cfg(debug_assertions)`); release builds enable
    /// it only when the user opted in via the `enable_browser_inspection`
    /// config flag. When `false`, [`ensure_webview`](Self::ensure_webview)
    /// fails fast instead of probing a closed port for 30s. Set at startup
    /// from `main.rs` via [`set_webview_enabled`](Self::set_webview_enabled).
    webview_enabled: AtomicBool,
    /// The app-layer child-webview ensurer (see [`ChildEnsurer`]) — `None`
    /// until the Tauri app installs it (headless/test managers never do, so
    /// the auto-ensure path is inert there). Behind a sync `RwLock` because
    /// the setter takes `&self` and the ensurer is read per navigate.
    child_ensurer: std::sync::RwLock<Option<ChildEnsurer>>,
    /// The watchdog tick interval in milliseconds (see
    /// [`WATCHDOG_INTERVAL`]); atomic so tests can shorten it via `&self`
    /// before the first browser launch (the watchdog reads it once, at
    /// spawn time).
    reap_interval_ms: AtomicU64,
}

impl BrowserManager {
    /// Create a manager with no browser yet — it spawns on first use.
    pub fn new() -> Arc<Self> {
        let (console_tx, _) = broadcast::channel(512);
        Arc::new(Self {
            state: Arc::new(Mutex::new(State {
                browser: None,
                handler: None,
                pages: HashMap::new(),
                active: None,
                console: HashMap::new(),
                console_tasks: HashMap::new(),
                profile: None,
                webview: None,
                webview_handler: None,
                watchdog: None,
            })),
            console_tx,
            webview_url: format!("http://localhost:{}", crate::webview_args::cdp_port()),
            webview_enabled: AtomicBool::new(cfg!(debug_assertions)),
            child_ensurer: std::sync::RwLock::new(None),
            reap_interval_ms: AtomicU64::new(WATCHDOG_INTERVAL.as_millis() as u64),
        })
    }

    /// Install the app-layer child-webview ensurer (see [`ChildEnsurer`]).
    /// Called once at startup by the Tauri app (src-tauri); headless/test
    /// managers never install one, which leaves the auto-ensure path inert
    /// (the "call browser_navigate first" error still fires).
    pub fn set_child_ensurer(&self, ensurer: ChildEnsurer) {
        *self.child_ensurer.write().expect("child_ensurer poisoned") = Some(ensurer);
    }

    /// Subscribe to live console events (one receiver per subscriber).
    pub fn subscribe_console(&self) -> broadcast::Receiver<ConsoleEvent> {
        self.console_tx.subscribe()
    }

    /// Set whether the WebView2 CDP attach is enabled. Called once at startup
    /// from `main.rs`: debug builds leave the default (`true`); release builds
    /// enable it only when the user opted in via the
    /// `enable_browser_inspection` config flag. When `false`,
    /// [`ensure_webview`](Self::ensure_webview) fails fast instead of probing
    /// a closed port for 30s.
    pub fn set_webview_enabled(&self, enabled: bool) {
        self.webview_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Test-only constructor: like [`new`](Self::new) but with a custom WebView2
    /// CDP endpoint, so a test can attach to a launched-Chromium stand-in on an
    /// ephemeral port instead of the default port.
    #[cfg(test)]
    pub(crate) fn new_with_webview_url(url: String) -> Arc<Self> {
        let (console_tx, _) = broadcast::channel(512);
        Arc::new(Self {
            state: Arc::new(Mutex::new(State {
                browser: None,
                handler: None,
                pages: HashMap::new(),
                active: None,
                console: HashMap::new(),
                console_tasks: HashMap::new(),
                profile: None,
                webview: None,
                webview_handler: None,
                watchdog: None,
            })),
            console_tx,
            webview_url: url,
            webview_enabled: AtomicBool::new(true),
            child_ensurer: std::sync::RwLock::new(None),
            reap_interval_ms: AtomicU64::new(WATCHDOG_INTERVAL.as_millis() as u64),
        })
    }

    /// Test-only: shorten the watchdog tick so the reap path is observable
    /// within a test's runtime. Must be called before the first `navigate`
    /// (the watchdog reads the interval once, when it spawns).
    #[cfg(test)]
    pub(crate) fn set_reap_interval(&self, interval: std::time::Duration) {
        self.reap_interval_ms
            .store(interval.as_millis() as u64, Ordering::Relaxed);
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
        let _ = tokio::task::spawn_blocking(Self::sweep_orphan_profiles);

        let profile = tempfile::Builder::new()
            .prefix("mnemo-browser-")
            .tempdir_in(std::env::temp_dir())
            .map_err(|e| Error::Browser(format!("failed to create browser profile dir: {e}")))?;
        let (browser, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(chromiumoxide::browser::HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
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
        // (Review C2). Kill the child and remove the dead profile on a
        // background task (Review B1).
        state.handler.take();
        if let Some(browser) = state.browser.take() {
            let profile = state.profile.take();
            if let Some(profile) = &profile {
                // The dir no longer belongs to a live browser — declare it
                // dead so the sweep reaps it if the removal below never
                // happens or fails past its retry budget.
                unregister_live_profile(profile.path());
            }
            tokio::spawn(async move {
                // Only remove the profile dir once the child is actually
                // dead — a live child holds locks on it, so removal
                // would fail forever (Review M1).
                if Self::kill_browser(browser).await {
                    if let Some(profile) = profile {
                        Self::schedule_profile_removal(profile);
                    }
                }
            });
        }
        for (_, task) in state.console_tasks.drain() {
            task.abort();
        }
        state.pages.clear();
        state.console.clear();
        state.active = None;
        true
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
    /// Runs at browser launch, on every watchdog tick, and once at app
    /// startup (src-tauri `main.rs`) — before the watchdog existed it only
    /// ran at launch time, so a session whose browser stayed alive never
    /// swept at all.
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
        loop {
            tokio::time::sleep(interval).await;
            let Some(state) = weak.upgrade() else { break };
            {
                let mut state = state.lock().await;
                Self::reap_dead_browser(&mut state);
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

        let page = browser
            .new_page(url.clone())
            .await
            .map_err(|e| Error::Browser(format!("failed to open page '{url}': {e}")))?;

        // Subscribe the page's console/exception event streams HERE (before
        // returning) so no event can arrive without a registered listener —
        // a spawned task might not be scheduled in time under load. On
        // failure, close the page and leave the rest of the state untouched
        // (a failed navigate must not clobber the active page — Review C3).
        let (console_stream, log_stream) = match (
            page.event_listener::<EventConsoleApiCalled>().await,
            page.event_listener::<EventEntryAdded>().await,
        ) {
            (Ok(c), Ok(l)) => (c, l),
            _ => {
                let _ = page.close().await;
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
        Self::page_info(&page, &id, true).await
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
        page.close()
            .await
            .map_err(|e| Error::Browser(format!("failed to close page '{id}': {e}")))?;
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
        Self::page_info(&page, page_id, true).await
    }

    /// Capture a PNG screenshot of the page (defaults to active).
    pub async fn screenshot(&self, page_id: Option<&str>) -> Result<Vec<u8>> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        page.screenshot(chromiumoxide::page::ScreenshotParams::default())
            .await
            .map_err(|e| Error::Browser(format!("screenshot failed on '{id}': {e}")))
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
        page.content()
            .await
            .map_err(|e| Error::Browser(format!("snapshot failed on '{id}': {e}")))
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
        let out = page
            .evaluate_expression(params)
            .await
            .map_err(|e| Error::Browser(format!("eval failed on '{id}': {e}")))?;
        Ok(out.value().cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Click the element matching `selector` (defaults to active page).
    pub async fn click(&self, page_id: Option<&str>, selector: &str) -> Result<()> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        let el = page.find_element(selector).await.map_err(|e| {
            Error::Browser(format!("click: no element '{selector}' on '{id}': {e}"))
        })?;
        el.click()
            .await
            .map_err(|e| Error::Browser(format!("click failed on '{selector}': {e}")))?;
        Ok(())
    }

    /// Focus the element matching `selector` and type `text` into it.
    pub async fn type_text(&self, page_id: Option<&str>, selector: &str, text: &str) -> Result<()> {
        let (id, page) = Self::lookup(&self.state, page_id).await?;
        let el = page
            .find_element(selector)
            .await
            .map_err(|e| Error::Browser(format!("type: no element '{selector}' on '{id}': {e}")))?;
        el.click()
            .await
            .map_err(|e| Error::Browser(format!("focus failed on '{selector}': {e}")))?;
        el.type_str(text)
            .await
            .map_err(|e| Error::Browser(format!("type failed on '{selector}': {e}")))?;
        Ok(())
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
            // Detach from the live WebView2 (abort the handler, drop the
            // connected browser). Dropping a *connected* Browser does NOT
            // kill a process — it only closes the CDP WebSocket — so this is
            // safe even though the WebView2 belongs to the app itself.
            if let Some(task) = state.webview_handler.take() {
                task.abort();
            }
            drop(state.webview.take());
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

    // ---- Live WebView2 (CDP attach) ----
    //
    // The methods below attach to the *app's own* WebView2 (the one the human
    // plays the game in) via its CDP debug port, instead of launching a
    // separate headless Chromium. `Browser::connect` opens a WebSocket to the
    // port the `--remote-debugging-port` arg exposed (set debug-only in
    // `main()`); the agent then inspects the live game state. This is separate
    // from the launch-mode browser above — the agent may use both.

    /// Ensure a CDP connection to the live WebView2 exists, (re)connecting if it
    /// is missing or has died. Returns a cloned handle to the connected
    /// browser. The fast path (already connected + alive) takes the state lock
    /// only briefly; the slow path (connect + 30s retry) runs **outside** the
    /// lock so a missing WebView2 cannot stall the headless browser (Review
    /// B1). Short-circuits when the CDP port is not exposed (debug builds
    /// always expose it; release builds only when opted in via
    /// `enable_browser_inspection`) — probing the CDP endpoint for 30s would
    /// only waste time (Review R1).
    async fn ensure_webview(&self) -> Result<Arc<Browser>> {
        let url = self.webview_url.clone();
        // Fast path: already connected + alive — clone the handle under a short
        // lock and return immediately.
        {
            let state = self.state.lock().await;
            if let Some(b) = &state.webview {
                let dead = state
                    .webview_handler
                    .as_ref()
                    .is_none_or(|h| h.is_finished());
                if !dead {
                    return Ok(Arc::clone(b));
                }
            }
        }
        // Release builds never expose the CDP port — fail fast instead of
        // probing the CDP endpoint for 30s (Review R1). The flag is set at
        // startup: debug builds always enable it; release builds enable it only
        // when the user opted in via `enable_browser_inspection`.
        if !self.webview_enabled.load(Ordering::Relaxed) {
            return Err(Error::Browser(
                "game inspection is disabled — enable 'Agent browser inspection' in Settings → Advanced and restart the app (the CDP port is dev-only unless opted in)".into(),
            ));
        }
        // Slow path: connect + retry OUTSIDE the lock so the headless browser
        // is not blocked while we wait for the WebView2 to come up.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let connect =
                tokio::time::timeout(std::time::Duration::from_secs(15), Browser::connect(&url));
            match connect.await {
                Ok(Ok((browser, mut handler))) => {
                    let task = tokio::spawn(async move { while handler.next().await.is_some() {} });
                    let connected = Arc::new(browser);
                    // Re-acquire the lock to store the result. Another caller
                    // may have connected concurrently — if so, use theirs and
                    // drop ours (only one handler task should survive).
                    let mut state = self.state.lock().await;
                    let dead = state
                        .webview_handler
                        .as_ref()
                        .is_none_or(|h| h.is_finished());
                    if let Some(existing) = &state.webview {
                        if !dead {
                            // A concurrent winner already connected — use
                            // theirs and drop ours: abort our handler so its
                            // CDP WebSocket closes instead of leaking the
                            // task + connection forever.
                            task.abort();
                            return Ok(Arc::clone(existing));
                        }
                    }
                    // Clear any stale state, then store ours.
                    if let Some(task) = state.webview_handler.take() {
                        task.abort();
                    }
                    state.webview = Some(Arc::clone(&connected));
                    state.webview_handler = Some(task);
                    return Ok(connected);
                }
                Ok(Err(_)) | Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Ok(Err(e)) => {
                    return Err(Error::Browser(format!(
                        "failed to attach to the WebView2 CDP endpoint at {url}: {e}"
                    )))
                }
                Err(_) => {
                    return Err(Error::Browser(format!(
                        "timed out attaching to the WebView2 CDP endpoint at {url} — \
                         is the app running in a debug build?"
                    )))
                }
            }
        }
    }

    /// Ensure the webview connection exists and return the CHILD webview's
    /// page target (the navigated external site), NOT the app's main page.
    /// With the child-WebView2 architecture there are 2+ targets on the shared
    /// debug port: the app's main page (`tauri://localhost` /
    /// `http://tauri.localhost` / the devUrl `http://localhost:5179` in dev)
    /// and the child webview (the navigated external site). The child is the
    /// one the agent's `browser_*` tools must drive. Polls briefly because target
    /// discovery can lag the connect. The connect (if needed) runs outside the
    /// state lock; only the page enumeration does.
    async fn webview_page(&self) -> Result<Page> {
        let browser = self.ensure_webview().await?;
        // The connected browser may need a moment to discover the existing
        // page — poll until the CHILD target appears (mirrors the spike).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let found = browser.pages().await.map_err(|e| {
                Error::Browser(format!("failed to enumerate WebView2 targets: {e}"))
            })?;
            if let Some(child) = Self::select_child_target(&found).await {
                return Ok(child);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Browser(
                    "the child WebView2 has no page target to attach to — is the Browser tab \
                     open? (call browser_navigate first — it creates the child webview \
                     automatically)"
                        .into(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Resolve the page target for a MUTATION that may bootstrap the child
    /// webview: identical to [`Self::webview_page`], except that when the
    /// child target is missing AND the app layer installed a
    /// [`ChildEnsurer`](crate::browser::ChildEnsurer), the ensurer is invoked
    /// once (creating the child webview + revealing the Browser tab) and the
    /// poll is retried. Only `webview_navigate` routes through here —
    /// click/type/eval must not silently open the tab (the user may not want
    /// it opened) and the read path has its own app-page fallback.
    async fn webview_page_auto_ensure(&self, normalized_url: &str) -> Result<Page> {
        let browser = self.ensure_webview().await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        // Whether the bootstrap was attempted (hook or not) — the one-shot
        // guard below.
        let mut attempted = false;
        // Whether a hook existed and returned Ok — differentiates the
        // deadline error: after a successful create the "call
        // browser_navigate first" guidance would be a lie (that call just
        // ran and the child still never registered — e.g. a stale IPC-side
        // handle after a renderer crash, or a child stuck at about:blank,
        // which `is_app_url` classifies as the app page).
        let mut ensurer_ran = false;
        loop {
            let found = browser.pages().await.map_err(|e| {
                Error::Browser(format!("failed to enumerate WebView2 targets: {e}"))
            })?;
            if let Some(child) = Self::select_child_target(&found).await {
                return Ok(child);
            }
            // No child target. On the first miss, ask the app layer to create
            // the child webview at its stored rect (a no-op when the app layer
            // never installed the hook — the no-hook error below is then
            // returned). The creation is async; the retry loop's 100ms sleeps
            // cover the child's CDP target registration.
            if !attempted {
                attempted = true;
                let ensurer = self
                    .child_ensurer
                    .read()
                    .expect("child_ensurer poisoned")
                    .clone();
                if let Some(ensurer) = ensurer {
                    ensurer_ran = true;
                    let url = normalized_url.to_string();
                    if let Err(e) = (ensurer)(url).await {
                        return Err(Error::Browser(format!(
                            "failed to create the Browser tab's child webview: {e}"
                        )));
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Browser(if ensurer_ran {
                    "the Browser tab's child webview did not register a CDP page \
                     target within 10s — it may be stale; open the Browser tab and \
                     load any URL once manually, then retry"
                        .into()
                } else {
                    "the child WebView2 has no page target to attach to — is the Browser tab \
                     open? (call browser_navigate first — it creates the child webview \
                     automatically)"
                        .into()
                }));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Resolve the page target for a READ-ONLY operation (screenshot /
    /// snapshot): the CHILD webview when the Browser tab is open, else the
    /// app's own main page — so the agent can see the app's UI even when the
    /// Browser tab hasn't been opened. Mutations (eval/navigate/click/type)
    /// never take this path — they must not operate on the app's own page.
    /// Polls briefly for target discovery (a fresh CDP connection takes a
    /// moment to enumerate existing targets), mirroring [`Self::webview_page`].
    async fn webview_page_for_read(&self) -> Result<Page> {
        let browser = self.ensure_webview().await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let found = browser.pages().await.map_err(|e| {
                Error::Browser(format!("failed to enumerate WebView2 targets: {e}"))
            })?;
            // Read all URLs up front so the target choice is a pure decision
            // over strings ([`Self::select_read_target`] — unit-testable).
            let urls: Vec<String> = {
                let mut v = Vec::with_capacity(found.len());
                for p in &found {
                    v.push(p.url().await.ok().flatten().unwrap_or_default());
                }
                v
            };
            if let Some(i) = Self::select_read_target(&urls) {
                return Ok(found[i].clone());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Browser(
                    "no WebView2 page target to attach to — is the app window open?".into(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// The pure decision behind [`Self::webview_page_for_read`]: which target
    /// a READ-ONLY snapshot attaches to, given the enumerated target URLs.
    ///
    /// 1. The first non-app page — the navigated child (Browser tab open).
    /// 2. An `about:blank` page when a REAL app page exists alongside — the
    ///    child webview is transiently `about:blank` while its initial
    ///    navigation is in flight, and reading it beats silently falling back
    ///    to the app's UI.
    /// 3. The app's own page (the read-only fallback).
    ///
    /// `None` only when no usable page exists at all (all URLs are
    /// `chrome://`-style internal pages or the list is empty).
    fn select_read_target(urls: &[String]) -> Option<usize> {
        // (1) The navigated child — any non-app URL.
        if let Some(i) = urls.iter().position(|u| !Self::is_app_url(u)) {
            return Some(i);
        }
        // (2) A blank page alongside a REAL app page is the transient child.
        // Without a real app page the blank IS the app (the test stand-in /
        // a single blank window) — keep the fallback below.
        let has_real_app = urls.iter().any(|u| Self::is_real_app_page(u));
        if has_real_app {
            if let Some(i) = urls.iter().position(|u| u == "about:blank") {
                return Some(i);
            }
        }
        // (3) The app page fallback.
        urls.iter().position(|u| Self::is_app_url(u))
    }

    /// Pick the CHILD webview's page target out of a list of CDP targets: the
    /// one whose URL is NOT the app's main page. The app page is
    /// `tauri://localhost` / `http://tauri.localhost` (release) or the devUrl
    /// `http://localhost:5179` (dev); the child is the navigated external site
    /// (including a separate project's dev server, e.g. `http://localhost:3000`
    /// — a documented primary use case). Returns `None` when no non-app page
    /// exists (the Browser tab hasn't been opened yet) so `webview_page()`
    /// errors instead of silently operating on the app's own UI.
    async fn select_child_target(pages: &[Page]) -> Option<Page> {
        // The app page's URL starts with `tauri://`, `http://tauri.localhost`,
        // or (in dev) the devUrl host. The child is any page whose URL does NOT
        // match one of those app prefixes.
        for p in pages {
            let url = p.url().await.ok().flatten().unwrap_or_default();
            if !Self::is_app_url(&url) {
                return Some(p.clone());
            }
        }
        // No non-app page found — the Browser tab hasn't been opened yet (no
        // child webview created). Return None so webview_page() errors with a
        // clear "is the Browser tab open?" message instead of silently
        // returning the app's own page (which would let browser_* operate on the
        // app's UI).
        None
    }

    /// Whether a URL is the app's own main page (not the child webview).
    /// `tauri://localhost` / `http://tauri.localhost` (release) or
    /// `http://localhost:5179` (dev devUrl). Also matches `about:blank` and
    /// `chrome://` pages — a launched-Chromium test's default page is
    /// `about:blank`, and treating it as "not the child" lets the
    /// target-selection test distinguish the app stand-in from the child
    /// stand-in. The read-only fallback ([`Self::select_read_target`])
    /// additionally treats an `about:blank` page alongside a REAL app page as
    /// the transient child (its initial navigation is still in flight).
    ///
    /// NOTE: only the *specific* dev devUrl port (5179) is matched, NOT every
    /// `http://localhost:<port>` — the child commonly navigates to a separate
    /// project's dev server (e.g. `http://localhost:3000`), which must be
    /// selected as the child target, not misclassified as the app page.
    fn is_app_url(url: &str) -> bool {
        url.starts_with("tauri://")
            || url.starts_with("http://tauri.localhost")
            || url.starts_with("https://tauri.localhost")
            // Dev devUrl only (the Vite dev server on port 5179). Hardcoding
            // 5179 mirrors the default CDP port. The child navigates to
            // external sites + separate dev servers (localhost:3000, etc.),
            // which must NOT be treated as the app page.
            || url.starts_with("http://localhost:5179")
            || url == "about:blank"
            || url.starts_with("chrome://")
    }

    /// Whether a URL is a REAL app page (not the ambiguous `about:blank` /
    /// `chrome://` stand-ins). Used by [`Self::select_read_target`] to tell a
    /// transiently-blank child webview apart from a lone blank window.
    fn is_real_app_page(url: &str) -> bool {
        url.starts_with("tauri://")
            || url.starts_with("http://tauri.localhost")
            || url.starts_with("https://tauri.localhost")
            || url.starts_with("http://localhost:5179")
    }

    /// Capture a PNG screenshot of the live WebView2 (the game as the human
    /// sees it). The screenshot shows the app's main page, including the game
    /// iframe rendered with the WebView2's GPU.
    ///
    /// READ-ONLY FALLBACK: when the Browser tab isn't open (no child webview
    /// exists), this falls back to the app's own page — so the agent can see
    /// the app's UI itself. Mutations never fall back (see
    /// [`Self::webview_page_for_read`]).
    pub async fn webview_screenshot(&self) -> Result<Vec<u8>> {
        let page = self.webview_page_for_read().await?;
        page.screenshot(chromiumoxide::page::ScreenshotParams::default())
            .await
            .map_err(|e| Error::Browser(format!("WebView2 screenshot failed: {e}")))
    }

    /// Evaluate a JavaScript expression against the child webview's main page
    /// and return its JSON value. Promises are awaited and the result returned
    /// by value. The child webview IS the top-level page (no iframe), so there
    /// is no same-origin limit — evals run in the child's main frame directly.
    pub async fn webview_eval(&self, expr: &str) -> Result<serde_json::Value> {
        let page = self.webview_page().await?;
        let params = EvaluateParams::builder()
            .expression(expr)
            .await_promise(true)
            .return_by_value(true)
            .build()
            .map_err(|e| Error::Browser(format!("invalid eval params: {e}")))?;
        let out = page
            .evaluate_expression(params)
            .await
            .map_err(|e| Error::Browser(format!("WebView2 eval failed: {e}")))?;
        Ok(out.value().cloned().unwrap_or(serde_json::Value::Null))
    }

    /// A text snapshot of the child webview's main-page DOM — the cheap way for
    /// the agent to "see" the page's content without a screenshot. The child IS
    /// the top-level page (no iframe), so this returns the navigated site's
    /// full HTML.
    ///
    /// READ-ONLY FALLBACK: when the Browser tab isn't open (no child webview
    /// exists), this falls back to the app's own page — so the agent can read
    /// the app's UI itself.
    pub async fn webview_snapshot(&self) -> Result<String> {
        let page = self.webview_page_for_read().await?;
        page.content()
            .await
            .map_err(|e| Error::Browser(format!("WebView2 snapshot failed: {e}")))
    }

    /// Navigate the Browser tab's child webview to `url`. Normalizes the URL
    /// through the same [`normalize_url`] choke point (so the scheme allow-list
    /// + omnibox autodetection apply), then drives the child webview's page
    /// directly via CDP `Page.navigate` — no iframe `src` eval (the child is a
    /// separate top-level webview, not an iframe in the app's DOM).
    ///
    /// BOOTSTRAP: when no child webview exists yet (the Browser tab was
    /// toggled on but never navigated), the app-layer
    /// [`ChildEnsurer`](crate::browser::ChildEnsurer) hook is invoked once to
    /// create the child webview + reveal the tab, then the attach poll is
    /// retried — so `browser_navigate` is the one tool that can open the
    /// Browser tab by itself (plan 5ae26d22).
    pub async fn webview_navigate(&self, url: &str) -> Result<String> {
        let normalized = normalize_url(url)?;
        let page = self.webview_page_auto_ensure(&normalized).await?;
        page.goto(&normalized)
            .await
            .map_err(|e| Error::Browser(format!("WebView2 navigate failed: {e}")))?;
        Ok(normalized)
    }

    /// Click the element matching `selector` in the child webview's main
    /// frame. Gets the element's bounding rect via a main-frame eval (no
    /// `context_id`, no iframe offset — the child IS the page), then dispatches
    /// a mousePressed + mouseReleased at its center. Requires the CDP port to
    /// be exposed (debug builds, or release builds with
    /// `enable_browser_inspection` enabled).
    pub async fn webview_click(&self, selector: &str) -> Result<()> {
        let page = self.webview_page().await?;
        let (x, y) = self
            .element_center(&page, selector, /*focus*/ false)
            .await?;
        self.dispatch_click(&page, x, y).await
    }

    /// Focus the element matching `selector` in the child webview's main frame
    /// and type `text` into it. Focuses via a click (mousePressed + Released at
    /// the element's center), then inserts the text via the CDP
    /// `Input.insertText` domain (one call, no per-character key dispatch).
    /// The text is inserted at the caret (appends, like `browser_type` — does
    /// NOT clear the existing value). Requires the CDP port to be exposed
    /// (debug builds, or release builds with `enable_browser_inspection`
    /// enabled).
    pub async fn webview_type(&self, selector: &str, text: &str) -> Result<()> {
        use chromiumoxide::cdp::browser_protocol::input::InsertTextParams;

        let page = self.webview_page().await?;
        let (x, y) = self.element_center(&page, selector, /*focus*/ true).await?;
        self.dispatch_click(&page, x, y).await?;
        page.execute(InsertTextParams::new(text))
            .await
            .map_err(|e| Error::Browser(format!("type: insertText failed: {e}")))?;
        Ok(())
    }

    /// Compute the center of an element matching `selector` in the child
    /// webview's main frame, for CDP `Input.dispatchMouseEvent`. CDP mouse
    /// coordinates are relative to the browser viewport, and the child webview
    /// IS the top-level page (no iframe), so `getBoundingClientRect()` returns
    /// page-level coordinates directly — no iframe offset needed (resolves
    /// review H1, which was the iframe-offset bug).
    ///
    /// `focus` additionally calls `el.focus()` (for `webview_type`, so
    /// `InsertText` lands in the right element).
    async fn element_center(
        &self,
        page: &chromiumoxide::Page,
        selector: &str,
        focus: bool,
    ) -> Result<(f64, f64)> {
        use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;

        let sel_json = serde_json::to_string(selector)
            .map_err(|e| Error::Browser(format!("selector encode failed: {e}")))?;
        let focus_clause = if focus { "el.focus();" } else { "" };
        let center_expr = format!(
            "(function(){{var el=document.querySelector({sel_json});if(!el){{throw new Error('no element matches {sel_json}');}}{focus_clause}var r=el.getBoundingClientRect();return JSON.stringify({{x:r.x+r.width/2,y:r.y+r.height/2}});}})()"
        );
        let center_val = page
            .evaluate_expression(
                EvaluateParams::builder()
                    .expression(&center_expr)
                    .return_by_value(true)
                    .build()
                    .map_err(|e| Error::Browser(format!("invalid center eval params: {e}")))?,
            )
            .await
            .map_err(|e| Error::Browser(format!("element rect eval failed: {e}")))?;
        let center_str = center_val
            .value()
            .and_then(|v| v.as_str().map(String::from))
            .ok_or_else(|| Error::Browser("element rect eval returned no string".into()))?;
        let center: serde_json::Value = serde_json::from_str(&center_str)
            .map_err(|e| Error::Browser(format!("element rect JSON parse failed: {e}")))?;
        let cx = center["x"]
            .as_f64()
            .ok_or_else(|| Error::Browser("missing element x coord".into()))?;
        let cy = center["y"]
            .as_f64()
            .ok_or_else(|| Error::Browser("missing element y coord".into()))?;
        Ok((cx, cy))
    }

    /// Dispatch a mousePressed + mouseReleased at page-level `(x, y)`.
    async fn dispatch_click(&self, page: &chromiumoxide::Page, x: f64, y: f64) -> Result<()> {
        use chromiumoxide::cdp::browser_protocol::input::{
            DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
        };
        page.execute(
            DispatchMouseEventParams::builder()
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .r#type(DispatchMouseEventType::MousePressed)
                .click_count(1)
                .build()
                .map_err(|e| Error::Browser(format!("invalid mousePressed params: {e}")))?,
        )
        .await
        .map_err(|e| Error::Browser(format!("mousePressed failed: {e}")))?;
        page.execute(
            DispatchMouseEventParams::builder()
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .r#type(DispatchMouseEventType::MouseReleased)
                .click_count(1)
                .build()
                .map_err(|e| Error::Browser(format!("invalid mouseReleased params: {e}")))?,
        )
        .await
        .map_err(|e| Error::Browser(format!("mouseReleased failed: {e}")))?;
        Ok(())
    }

    /// Detach from the live WebView2 (abort the handler, drop the connected
    /// browser). A subsequent `webview_*` call reconnects lazily. Unlike
    /// [`close`](Self::close), this leaves the launch-mode headless browser
    /// untouched.
    pub async fn close_webview(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        if let Some(task) = state.webview_handler.take() {
            task.abort();
        }
        drop(state.webview.take());
        Ok(())
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
                handler: None,
                pages: HashMap::new(),
                active: None,
                console: HashMap::new(),
                console_tasks: HashMap::new(),
                profile: None,
                webview: None,
                webview_handler: None,
                watchdog: None,
            })),
            console_tx,
            webview_url: format!("http://localhost:{}", crate::webview_args::cdp_port()),
            webview_enabled: AtomicBool::new(cfg!(debug_assertions)),
            child_ensurer: std::sync::RwLock::new(None),
            reap_interval_ms: AtomicU64::new(WATCHDOG_INTERVAL.as_millis() as u64),
        }
    }
}

/// Normalize a URL input (omnibox-style scheme autodetection + scheme
/// allow-list). The single choke point shared by the agent tool, the UI URL
/// bar, and the child-webview navigation: scheme-less hostnames get an
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
            match tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv()).await {
                Ok(Ok(ev)) => got.push(ev),
                _ => {}
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

    /// Regression (review C2): `is_app_url` must match ONLY the specific dev
    /// devUrl port (5179), NOT every `http://localhost:<port>`. The child
    /// commonly navigates to a separate project's dev server (e.g.
    /// `http://localhost:3000`) — a documented primary use case — and that
    /// must be selected as the child target, not misclassified as the app page
    /// (which would make `browser_*` silently operate on the app's own UI).
    #[test]
    fn is_app_url_matches_only_dev_devurl_port() {
        // App pages (must be treated as the app, NOT the child).
        assert!(BrowserManager::is_app_url("tauri://localhost"));
        assert!(BrowserManager::is_app_url("http://tauri.localhost/"));
        assert!(BrowserManager::is_app_url(
            "https://tauri.localhost/index.html"
        ));
        assert!(BrowserManager::is_app_url("http://localhost:5179"));
        assert!(BrowserManager::is_app_url("http://localhost:5179/"));
        assert!(BrowserManager::is_app_url("about:blank"));
        assert!(BrowserManager::is_app_url("chrome://newtab/"));

        // Child pages (must NOT be treated as the app — these are the child
        // webview's navigated URLs, including localhost dev servers that are
        // NOT the app's own dev server).
        assert!(!BrowserManager::is_app_url("http://localhost:3000"));
        assert!(!BrowserManager::is_app_url("http://localhost:3000/game"));
        assert!(!BrowserManager::is_app_url("http://localhost:8080"));
        assert!(!BrowserManager::is_app_url("http://localhost:9222"));
        assert!(!BrowserManager::is_app_url("https://www.google.com"));
        assert!(!BrowserManager::is_app_url("http://example.com:8080"));
        assert!(!BrowserManager::is_app_url("data:text/html,<h1>hi</h1>"));
    }

    /// The read-only target decision ([`BrowserManager::select_read_target`]):
    /// the navigated child wins, a transiently-blank child beats the app
    /// fallback, a lone blank page IS the app (the test stand-in), and the
    /// app page is the last resort.
    #[test]
    fn select_read_target_prefers_child_then_transient_blank_then_app() {
        // (1) The navigated child wins outright.
        assert_eq!(
            BrowserManager::select_read_target(&[
                "tauri://localhost".to_string(),
                "https://example.com".to_string(),
            ]),
            Some(1)
        );
        // (2) A blank page alongside a REAL app page is the transient child
        // (its initial navigation is still in flight) — read it instead of
        // silently falling back to the app UI.
        assert_eq!(
            BrowserManager::select_read_target(&[
                "tauri://localhost".to_string(),
                "about:blank".to_string(),
            ]),
            Some(1)
        );
        // (3) A lone blank page IS the app (the launched-Chromium test
        // stand-in) — no transient child exists, so the fallback applies.
        assert_eq!(
            BrowserManager::select_read_target(&["about:blank".to_string()]),
            Some(0)
        );
        // (4) No blank, no child → the app page fallback.
        assert_eq!(
            BrowserManager::select_read_target(&["tauri://localhost".to_string()]),
            Some(0)
        );
        // (5) Nothing usable → None (caller errors with a clear message).
        assert_eq!(BrowserManager::select_read_target(&[]), None);
        // (6) A chrome:// page is an app stand-in (like about:blank, it
        // belongs to the launched-Chromium test harness, not a real child) —
        // the fallback applies.
        assert_eq!(
            BrowserManager::select_read_target(&["chrome://newtab/".to_string()]),
            Some(0)
        );
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

    /// GO/NO-GO spike for the interactive-browser architecture: prove that
    /// `chromiumoxide::Browser::connect` can attach to an *already-running*
    /// Chromium via its HTTP debug endpoint, enumerate its page targets, and
    /// eval/screenshot against the live page — the exact flow attaching to the
    /// Tauri WebView2 will require. The launched browser is a stand-in for the
    /// WebView2; `Browser::connect("http://localhost:PORT")` auto-fetches
    /// `/json/version` to resolve the WebSocket URL.
    #[tokio::test]
    #[ignore = "integration: launches + connects to a real Chromium process; run with `cargo test -- --ignored`"]
    async fn browser_connect_attaches_to_running_chromium() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};
        use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
        use chromiumoxide::page::ScreenshotParams;

        // (a) Launch a Chromium exposing an *ephemeral* debug port — the
        // stand-in for the Tauri WebView2 we'll attach to in the real app.
        // port(0) lets the OS pick a free port, avoiding fixed-port flakiness
        // when a leftover Chromium from a crashed run briefly holds the socket.
        let profile = tempfile::Builder::new()
            .prefix("mnemo-spike-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // (b) Open a page with identifiable content.
        let _page = launched
            .new_page("data:text/html,<title>spike</title><h1 id=x>hello</h1>")
            .await
            .expect("new_page should succeed");

        // (c) Connect to the running browser via its HTTP debug endpoint.
        // chromiumoxide captured the real ws:// URL (with the ephemeral port)
        // from Chromium's stderr at launch; derive the HTTP endpoint from it so
        // Browser::connect fetches /json/version to resolve the WebSocket URL —
        // exactly the flow for attaching to the Tauri WebView2's
        // --remote-debugging-port.
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let (connected, mut handler2) = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            Browser::connect(http_url),
        )
        .await
        .expect("connect should resolve within 15s")
        .expect("connect should succeed");
        let connected_task = tokio::spawn(async move { while handler2.next().await.is_some() {} });

        // (d) Enumerate targets. The connected browser may need a moment to
        // discover the existing page — poll until pages() is non-empty.
        let pages = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let found = connected.pages().await.expect("pages should enumerate");
                if !found.is_empty() || tokio::time::Instant::now() >= deadline {
                    break found;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        assert!(!pages.is_empty(), "should find at least one page target");

        // (e) Find the page whose title is "spike" and eval against it —
        // prove we're inspecting the live DOM of the running browser.
        let mut spike_page = None;
        for p in &pages {
            if p.get_title().await.ok().flatten().as_deref() == Some("spike") {
                spike_page = Some(p.clone());
                break;
            }
        }
        let spike_page = spike_page.expect("should find the spike page among targets");
        let params = EvaluateParams::builder()
            .expression("document.getElementById('x').textContent")
            .await_promise(true)
            .return_by_value(true)
            .build()
            .expect("eval params build");
        let result = spike_page
            .evaluate_expression(params)
            .await
            .expect("eval should succeed");
        let text = result.value().cloned().unwrap_or(serde_json::Value::Null);
        assert_eq!(
            text,
            serde_json::json!("hello"),
            "eval should read the live DOM"
        );

        // (f) Screenshot — prove visual capture works against the attached target.
        let shot = spike_page
            .screenshot(ScreenshotParams::default())
            .await
            .expect("screenshot should succeed");
        assert!(!shot.is_empty(), "screenshot should be non-empty");

        // (g) Cleanup: dropping the connected browser only closes the CDP
        // WebSocket (no child process). The launched browser's child must be
        // killed explicitly — chromiumoxide 0.7 never sets kill_on_drop, so a
        // bare drop leaves Chromium alive holding locks on the profile dir
        // (Review B1). Removal retries because the OS releases the dead
        // child's file locks asynchronously.
        drop(connected);
        connected_task.abort();
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// The connect-mode `webview_*` methods attach to an already-running
    /// Chromium via CDP and screenshot/eval/snapshot its live page — the exact
    /// flow the agent uses to inspect the game running in the app's WebView2.
    /// A launched Chromium on an ephemeral port stands in for the WebView2.
    #[tokio::test]
    #[ignore = "integration: launches a real Chromium process; run with `cargo test -- --ignored`"]
    async fn webview_connect_screenshot_eval_snapshot() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};

        // (a) Launch a Chromium on an ephemeral debug port — the stand-in for
        // the app's WebView2.
        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // (b) Navigate the browser's existing first page to identifiable
        // content the agent will inspect. (In production the WebView2 has
        // exactly one page — the app UI — so webview_page() returns the first;
        // we mirror that here rather than creating a second page with
        // new_page, which webview_page() would not pick up.) The page may take
        // a moment to appear after launch, so poll for it.
        let first = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let found = launched.pages().await.expect("pages should enumerate");
                if let Some(p) = found.into_iter().next() {
                    break p;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("launched browser should have a default page");
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        first
            .goto("data:text/html,<title>game</title><h1 id=x>hello</h1>")
            .await
            .expect("goto should succeed");

        // (c) Derive the HTTP debug endpoint from the ws URL chromiumoxide
        // captured at launch (ephemeral port), and build a manager pointed at it.
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url.clone());

        // (d) webview_eval reads the live DOM.
        let v = manager
            .webview_eval("document.getElementById('x').textContent")
            .await
            .expect("webview_eval should succeed");
        assert_eq!(
            v,
            serde_json::json!("hello"),
            "eval should read the live DOM"
        );

        // (e) webview_screenshot captures a non-empty PNG.
        let shot = manager
            .webview_screenshot()
            .await
            .expect("webview_screenshot should succeed");
        assert!(!shot.is_empty(), "screenshot should be non-empty");

        // (f) webview_snapshot returns the page HTML (contains our marker).
        let html = manager
            .webview_snapshot()
            .await
            .expect("webview_snapshot should succeed");
        assert!(
            html.contains("hello"),
            "snapshot should contain the page content"
        );

        // (g) close_webview detaches; a subsequent call reconnects lazily.
        manager
            .close_webview()
            .await
            .expect("close_webview should succeed");
        let v2 = manager
            .webview_eval("document.getElementById('x').textContent")
            .await
            .expect("reconnect + eval should succeed");
        assert_eq!(
            v2,
            serde_json::json!("hello"),
            "eval after reconnect should still read the live DOM"
        );

        // (h) Cleanup: drop the manager (detaches from the launched browser's
        // debug port) + kill the launched browser explicitly — chromiumoxide
        // 0.7 never sets kill_on_drop, so a bare drop leaves Chromium alive
        // holding locks on the profile dir (Review B1).
        drop(manager);
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

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

    /// Spike: confirm chromiumoxide 0.7's CDP frame API + input dispatch work
    /// against a connected (attach-mode) browser — the foundation for the
    /// agent's `browser_click` / `browser_type` tools that drive the live
    /// Browser tab's child webview. A launched Chromium on an ephemeral port
    /// stands in for the app's WebView2 (mirrors
    /// `webview_connect_screenshot_eval_snapshot`).
    ///
    /// Verifies: (a) `page.frames()` enumerates the iframe; (b) a frame's
    /// execution context can eval JS in the iframe's DOM; (c) CDP input
    /// dispatch (`DispatchMouseEventParams`) clicks an element inside the
    /// iframe. If any type is missing, the compiler fails here and the plan
    /// pivots step 5 to same-origin-only click/type.
    ///
    /// `#[ignore]`: this is a heavy integration test that spawns a real
    /// Chromium process + navigates an iframe asynchronously. It passes in
    /// isolation but is unreliable under the full suite's accumulated Chromium
    /// load (frame/context registration races under resource contention). Run
    /// manually with `cargo test --lib browser::tests::webview_frame_and_input_cdp_surface -- --ignored`.
    /// The API confirmation is the point; the reliable regression guards are
    /// `webview_connect_screenshot_eval_snapshot` (connect path) +
    /// `webview_navigate_sets_iframe_src` (navigate + scheme rejection).
    #[tokio::test]
    #[ignore = "heavy CDP-attach iframe integration test; run with --ignored"]
    async fn webview_frame_and_input_cdp_surface() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};
        use chromiumoxide::cdp::browser_protocol::input::{
            DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
        };
        use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;

        // (a) Launch a Chromium stand-in on an ephemeral debug port.
        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-spike-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // (b) Navigate the first page to a simple doc with an empty iframe,
        // then set the iframe's src to a data: URL via a main-frame eval
        // (avoids the quoting nightmare of nesting HTML+JS inside a data:
        // URL). The iframe's button sets an input value on click — the agent
        // will click that button.
        let first = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let found = launched.pages().await.expect("pages should enumerate");
                if let Some(p) = found.into_iter().next() {
                    break p;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("launched browser should have a default page");
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        first
            .goto("data:text/html,<title>spike</title><iframe id='f'></iframe>")
            .await
            .expect("goto should succeed");
        // Navigate the iframe to a data: URL with a button whose onclick
        // writes a marker into the input.
        first
            .evaluate(
                r#"document.getElementById('f').src = 'data:text/html,<input id="i"><button id="b">go</button><script>document.getElementById("b").onclick=function(){document.getElementById("i").value="clicked"}</script>'"#,
            )
            .await
            .expect("set iframe src should succeed");

        // (c) Derive the HTTP debug endpoint + build a manager pointed at it.
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url);

        // (d) frames() enumerates the iframe (FrameId list, length >= 2: the
        // main frame + the iframe).
        let page = manager.webview_page().await.expect("webview_page");
        let frames = page
            .frames()
            .await
            .expect("frames() should enumerate the iframe");
        assert!(
            frames.len() >= 2,
            "expected at least 2 frames (main + iframe), got {}",
            frames.len()
        );

        // (e) Find the iframe frame's execution context + eval in it to read
        // the input's initial value (empty). The iframe's src loads
        // asynchronously, so the frame + its execution context may not exist
        // yet — poll, re-enumerating frames + re-fetching the context each
        // iteration (both can change when the iframe navigates).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let ctx = loop {
            let frames = page.frames().await.unwrap_or_default();
            let iframe_frame = match frames.get(1) {
                Some(f) => f.clone(),
                None => {
                    if tokio::time::Instant::now() >= deadline {
                        panic!("iframe frame never appeared");
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    continue;
                }
            };
            if let Some(ctx) = page
                .frame_execution_context(iframe_frame)
                .await
                .ok()
                .flatten()
            {
                let ready = page
                    .evaluate_expression(
                        EvaluateParams::builder()
                            .expression("document.getElementById('i') !== null")
                            .context_id(ctx)
                            .return_by_value(true)
                            .build()
                            .expect("valid ready-check params"),
                    )
                    .await
                    .map(|r| r.value().and_then(|v| v.as_bool()).unwrap_or(false))
                    .unwrap_or(false);
                if ready {
                    break ctx;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("iframe content never became ready");
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        };
        let val = page
            .evaluate_expression(
                EvaluateParams::builder()
                    .expression("document.getElementById('i').value")
                    .context_id(ctx)
                    .return_by_value(true)
                    .build()
                    .expect("valid eval params"),
            )
            .await
            .expect("eval in iframe context should succeed");
        assert_eq!(
            val.value().cloned().unwrap_or(serde_json::Value::Null),
            serde_json::json!(""),
            "input should start empty"
        );

        // (f) Get the button's bounding rect (in the iframe's context), then
        // dispatch a mouse click at its center via CDP input dispatch.
        let rect_val = page
            .evaluate_expression(
                EvaluateParams::builder()
                    .expression(
                        "(() => { const r = document.getElementById('b').getBoundingClientRect(); \
                         return JSON.stringify({x:r.x+r.width/2, y:r.y+r.height/2}); })()",
                    )
                    .context_id(ctx)
                    .return_by_value(true)
                    .build()
                    .expect("valid rect eval params"),
            )
            .await
            .expect("rect eval should succeed");
        let rect_str = rect_val
            .value()
            .and_then(|v| v.as_str())
            .expect("rect should be a JSON string")
            .to_string();
        let rect: serde_json::Value = serde_json::from_str(&rect_str).expect("rect JSON");
        let x = rect["x"].as_f64().expect("x coord");
        let y = rect["y"].as_f64().expect("y coord");

        // mousePressed + mouseReleased at the button's center.
        page.execute(
            DispatchMouseEventParams::builder()
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .r#type(DispatchMouseEventType::MousePressed)
                .click_count(1)
                .build()
                .expect("valid mousePressed params"),
        )
        .await
        .expect("mousePressed should dispatch");
        page.execute(
            DispatchMouseEventParams::builder()
                .x(x)
                .y(y)
                .button(MouseButton::Left)
                .r#type(DispatchMouseEventType::MouseReleased)
                .click_count(1)
                .build()
                .expect("valid mouseReleased params"),
        )
        .await
        .expect("mouseReleased should dispatch");

        // (g) The click set the input value to 'clicked' — confirm via eval.
        let after_val = page
            .evaluate_expression(
                EvaluateParams::builder()
                    .expression("document.getElementById('i').value")
                    .context_id(ctx)
                    .return_by_value(true)
                    .build()
                    .expect("valid after-eval params"),
            )
            .await
            .expect("after-click eval should succeed");
        assert_eq!(
            after_val
                .value()
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            serde_json::json!("clicked"),
            "the CDP click should have set the input value"
        );

        // (h) Cleanup.
        drop(manager);
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// `webview_page()` must select the CHILD webview's target, not the app's
    /// main page. With the child-WebView2 architecture there are 2+ targets on
    /// the shared debug port: the app page + the child (the navigated site).
    /// This test launches Chromium, opens two pages (an "app" stand-in +
    /// a "child" stand-in), connects via `new_with_webview_url`, and asserts
    /// `webview_page()` returns the child (title "child"), not the app.
    #[tokio::test]
    #[ignore = "integration: launches a real Chromium process; run with `cargo test -- --ignored`"]
    async fn webview_page_selects_child_target() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};

        // (a) Launch a Chromium on an ephemeral debug port — the stand-in for
        // the app's WebView2 (which hosts both the app page + the child).
        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-sel-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // (b) The launched browser's default page is about:blank — the app
        // stand-in (is_app_url returns true for about:blank, so it's skipped
        // by select_child_target). Open a SECOND page as the child stand-in
        // with a data: URL (NOT an app URL → selected as the child).
        let app_page = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let found = launched.pages().await.expect("pages should enumerate");
                if let Some(p) = found.into_iter().next() {
                    break p;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("launched browser should have a default page");
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        let _child_page = launched
            .new_page("data:text/html,<title>child</title><h1 id=x>hello</h1>")
            .await
            .expect("new_page should succeed");

        // (c) Derive the HTTP debug endpoint + build a manager pointed at it.
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url);

        // (d) webview_page() must return the CHILD (title "child"), not the app
        // (about:blank). Poll briefly because the second page may take a moment
        // to register as a target.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        let child = loop {
            let page = manager.webview_page().await.expect("webview_page");
            let title = page.get_title().await.ok().flatten().unwrap_or_default();
            if title == "child" {
                break page;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("webview_page never returned the child target; last title: {title}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        };

        // (e) Confirm we're inspecting the child's DOM (not the app's).
        use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;
        let v = child
            .evaluate_expression(
                EvaluateParams::builder()
                    .expression("document.getElementById('x').textContent")
                    .await_promise(true)
                    .return_by_value(true)
                    .build()
                    .expect("eval params build"),
            )
            .await
            .expect("eval should succeed");
        assert_eq!(
            v.value().cloned().unwrap_or(serde_json::Value::Null),
            serde_json::json!("hello"),
            "eval should read the CHILD's DOM"
        );
        let _ = app_page;

        // (f) Cleanup.
        drop(manager);
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// The READ-ONLY fallback: when the Browser tab is NOT open (no child
    /// webview exists), `webview_snapshot`/`webview_screenshot` fall back to
    /// the app's own page — the main UI — instead of erroring "is the Browser
    /// tab open?". The mutations (`webview_page`) must still refuse to touch
    /// the app page. A launched Chromium stand-in plays the app's WebView2
    /// with a single about:blank page (an app URL, so `select_child_target`
    /// finds no child); the fallback must resolve that page for read-only ops.
    #[tokio::test]
    #[ignore = "integration: launches a real Chromium process; run with `cargo test -- --ignored`"]
    async fn webview_read_falls_back_to_app_page_when_child_missing() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};

        // (a) Launch a Chromium on an ephemeral debug port — the stand-in for
        // the app's WebView2 with ONLY the app page (no Browser tab opened).
        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-fb-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // (b) The launched browser's default page is about:blank — an app URL
        // (is_app_url → true), so no child target exists.
        {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let found = launched.pages().await.expect("pages should enumerate");
                if !found.is_empty() {
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("launched browser should have a default page");
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        // (c) Derive the HTTP debug endpoint + build a manager pointed at it.
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url);

        // (d) The read-only snapshot falls back to the app page: it resolves
        // instead of erroring with "is the Browser tab open?".
        let html = manager
            .webview_snapshot()
            .await
            .expect("webview_snapshot must fall back to the app page");
        assert!(
            html.contains("<html") || html.contains("<!DOCTYPE"),
            "snapshot should return the app page's HTML, got: {html:?}"
        );
        let shot = manager
            .webview_screenshot()
            .await
            .expect("webview_screenshot must fall back to the app page");
        assert!(!shot.is_empty(), "screenshot should be non-empty");

        // (e) The child-only mutation path still refuses the app page — the
        // browser_* mutations must never operate on the app's own UI.
        let err = manager
            .webview_page()
            .await
            .expect_err("webview_page must refuse the app page");
        assert!(
            err.to_string().contains("is the Browser tab open?"),
            "unexpected error: {err}"
        );

        // (f) Cleanup.
        drop(manager);
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// `webview_click` + `webview_type` drive the child webview's main frame
    /// via CDP input dispatch — no iframe, no offset, no race. A launched
    /// Chromium stand-in plays the app's WebView2: the default page is
    /// about:blank (the app stand-in), and a second page is the child with a
    /// button (sets an input on click) + an input the agent types into. The
    /// agent clicks the button, then types into the input, and a main-frame
    /// eval confirms both landed.
    #[tokio::test]
    #[ignore = "integration: launches a real Chromium process; run with `cargo test -- --ignored`"]
    async fn webview_click_and_type_drive_child_webview() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};
        use chromiumoxide::cdp::js_protocol::runtime::EvaluateParams;

        // (a) Launch a Chromium stand-in on an ephemeral debug port.
        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-ct-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });

        // (b) The default page is about:blank (the app stand-in — is_app_url
        // returns true for it). Open a SECOND page as the child with a button
        // (sets input #i on click) + an input #i2 the agent types into.
        let _app_page = {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let found = launched.pages().await.expect("pages should enumerate");
                if let Some(p) = found.into_iter().next() {
                    break p;
                }
                if tokio::time::Instant::now() >= deadline {
                    panic!("launched browser should have a default page");
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        };
        let _child = launched
            .new_page(
                "data:text/html,<title>ct</title>\
                 <input id='i'><input id='i2'>\
                 <button id='b'>go</button>\
                 <script>document.getElementById('b').onclick=function(){document.getElementById('i').value='clicked'}</script>",
            )
            .await
            .expect("new_page should succeed");

        // (c) Derive the HTTP debug endpoint + build a manager pointed at it.
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url);

        // (d) Wait for the child page to be selectable (the button exists)
        // before clicking. webview_page() selects the child (data: URL, not
        // about:blank); poll until the button is ready in the child's main
        // frame (no iframe, no context_id — the child IS the page).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            let page = manager.webview_page().await.expect("webview_page");
            let ready = page
                .evaluate_expression(
                    EvaluateParams::builder()
                        .expression("document.getElementById('b') !== null")
                        .return_by_value(true)
                        .build()
                        .expect("valid ready-check params"),
                )
                .await
                .map(|r| r.value().and_then(|v| v.as_bool()).unwrap_or(false))
                .unwrap_or(false);
            if ready {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("child content never became ready");
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }

        // (e) webview_click the button — its onclick sets input #i to 'clicked'.
        manager
            .webview_click("#b")
            .await
            .expect("webview_click should succeed");

        // (f) Confirm the click landed (read input #i via the child's main
        // frame). Poll briefly because the click handler runs async.
        let mut clicked = false;
        for _ in 0..30 {
            let page = manager.webview_page().await.expect("webview_page");
            let val = page
                .evaluate_expression(
                    EvaluateParams::builder()
                        .expression("document.getElementById('i').value")
                        .return_by_value(true)
                        .build()
                        .expect("valid eval params"),
                )
                .await
                .map(|r| r.value().cloned().unwrap_or(serde_json::Value::Null))
                .unwrap_or(serde_json::Value::Null);
            if val == serde_json::json!("clicked") {
                clicked = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            clicked,
            "the CDP click should have set input #i to 'clicked'"
        );

        // (g) webview_type into input #i2 (a fresh, empty input — no clear
        // needed). InsertText should set it to 'typed'.
        manager
            .webview_type("#i2", "typed")
            .await
            .expect("webview_type should succeed");
        let mut typed = false;
        for _ in 0..30 {
            let page = manager.webview_page().await.expect("webview_page");
            let val = page
                .evaluate_expression(
                    EvaluateParams::builder()
                        .expression("document.getElementById('i2').value")
                        .return_by_value(true)
                        .build()
                        .expect("valid eval params"),
                )
                .await
                .map(|r| r.value().cloned().unwrap_or(serde_json::Value::Null))
                .unwrap_or(serde_json::Value::Null);
            if val == serde_json::json!("typed") {
                typed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(typed, "the CDP type should have set input #i2 to 'typed'");

        // (h) Cleanup.
        drop(manager);
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// Regression (plan 5ae26d22, bug 1): with a CDP endpoint alive but NO
    /// child webview target (the freshly-toggled Browser tab before its first
    /// navigation), the MUTATION methods must fail with an error that points
    /// the agent at `browser_navigate` — the tool that bootstraps the child —
    /// instead of the bare "is the Browser tab open?" message. A launched
    /// Chromium stands in for the app's WebView2; its lone default
    /// `about:blank` page is an app URL (see `is_app_url`), so no child target
    /// exists. Fails without the fix (generic message), passes with it.
    #[tokio::test]
    #[ignore = "integration: launches a real Chromium process; run with `cargo test -- --ignored`"]
    async fn webview_click_without_child_says_to_navigate_first() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};

        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url);

        let err = manager
            .webview_click("#nothing")
            .await
            .expect_err("click must fail when no child webview exists");
        assert!(
            err.to_string().contains("call browser_navigate first"),
            "the error should tell the agent browser_navigate bootstraps the tab, got: {err}"
        );

        // Cleanup (Review B1: kill the launched browser explicitly).
        drop(manager);
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// Regression (plan 5ae26d22, bug 1 — the fix itself): `webview_navigate`
    /// must BOOTSTRAP a missing child webview via the app-layer
    /// [`ChildEnsurer`](crate::browser::ChildEnsurer) hook — create it, then
    /// attach and navigate. A launched Chromium stands in for the app's
    /// WebView2: its lone default `about:blank` page is an app URL, so no
    /// child target exists until the ensurer (a stand-in for
    /// `browser_webview_ensure_for_agent`) creates one. Fails without the fix
    /// (navigate errors "no page target"; the ensurer is never invoked).
    #[tokio::test]
    #[ignore = "integration: launches a real Chromium process; run with `cargo test -- --ignored`"]
    async fn webview_navigate_auto_ensures_missing_child() {
        use chromiumoxide::browser::{Browser, BrowserConfig, HeadlessMode};
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

        let profile = tempfile::Builder::new()
            .prefix("mnemo-webview-")
            .tempdir_in(std::env::temp_dir())
            .expect("tempdir");
        let (launched, mut handler) = Browser::launch(
            BrowserConfig::builder()
                .headless_mode(HeadlessMode::New)
                .user_data_dir(profile.path().to_path_buf())
                .port(0)
                .args(vec!["--disable-extensions".to_string()])
                .build()
                .expect("valid browser config"),
        )
        .await
        .expect("launch should succeed");
        let launched_task = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let http_url = {
            let ws = launched.websocket_address();
            let after_scheme = ws.strip_prefix("ws://").unwrap_or(ws);
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            format!("http://{host_port}")
        };
        let manager = BrowserManager::new_with_webview_url(http_url.clone());

        // Install the ensurer stand-in: it "creates the child webview" by
        // opening a new page on the stand-in browser at the requested URL
        // (exactly what browser_webview_ensure_for_agent does for the real
        // child WebView2) and records that it ran. The ensurer gets its own
        // connect()-derived handle (Arc<Browser> — the launch handle is not
        // Clone), with its CDP handler task pumped alongside.
        let (helper, mut helper_handler) = Browser::connect(&http_url)
            .await
            .expect("ensurer should connect to the stand-in browser");
        let helper = Arc::new(helper);
        let helper_task =
            tokio::spawn(async move { while helper_handler.next().await.is_some() {} });
        let ensured = Arc::new(AtomicBool::new(false));
        let ensured_flag = Arc::clone(&ensured);
        manager.set_child_ensurer(Arc::new(move |url: String| {
            let browser = Arc::clone(&helper);
            let flag = Arc::clone(&ensured_flag);
            Box::pin(async move {
                browser
                    .new_page(&url)
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("stand-in ensure failed: {e}"))?;
                flag.store(true, AtomicOrdering::SeqCst);
                Ok(())
            })
        }));

        let url = "data:text/html,<title>child</title><h1 id=x>bootstrapped</h1>";
        let landed = manager
            .webview_navigate(url)
            .await
            .expect("navigate should bootstrap the missing child webview");
        assert_eq!(landed, url, "navigate returns the normalized URL");
        assert!(
            ensured.load(AtomicOrdering::SeqCst),
            "the child ensurer should have been invoked exactly for this bootstrap"
        );

        // The bootstrapped child is now the attach target: eval reads its DOM.
        let v = manager
            .webview_eval("document.getElementById('x').textContent")
            .await
            .expect("webview_eval should attach to the bootstrapped child");
        assert_eq!(
            v,
            serde_json::json!("bootstrapped"),
            "eval should read the bootstrapped child's DOM"
        );

        // Cleanup (Review B1: kill the launched browser explicitly).
        drop(manager);
        helper_task.abort();
        let mut launched = launched;
        let _ = launched.kill().await;
        launched_task.abort();
        let path = profile.keep();
        for _ in 0..60 {
            if std::fs::remove_dir_all(&path).is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}
