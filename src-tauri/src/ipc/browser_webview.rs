// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Native child-WebView2 lifecycle for the Browser tab (all builds).
//!
//! Replaces the Browser tab's `<iframe>` with a native child WebView2 embedded
//! in the existing "main" Tauri window. A child WebView2 is a separate OS-level
//! instance — it is never "framed," so sites that refuse framing via
//! `X-Frame-Options` / CSP `frame-ancestors` (Google, YouTube) load normally.
//! The human drives it via the URL bar (the frontend reports the browser-area
//! rect + calls navigate); the agent's `browser_*` tools attach to it via CDP on
//! the shared debug port (debug-only, unchanged from the iframe era).
//!
//! ## OS-account single sign-on
//! The webview signs in to AAD/MSA sites (e.g. forms.cloud.microsoft)
//! silently with the Windows primary account — Edge-equivalent — because
//! the vendored wry (`vendor/wry`, see PATCHES.md) sets
//! `AllowSingleSignOnUsingOSPrimaryAccount(true)` on the WebView2
//! environment options (Tauri 2 exposes no environment hook, so the flag
//! lives in the vendored crate; guarded by `tests/wry_sso_patch.rs`).
//!
//! ## Hard reload
//! The Reload button always bypasses the HTTP cache: the vendored wry
//! (`vendor/wry`, see PATCHES.md) issues CDP `Page.reload` with
//! `ignoreCache` — WebView2's `Reload()` serves cached subresources, so
//! pages came back out of sync with their server state (guarded by
//! `tests/wry_hard_reload_patch.rs`).
//!
//! ## Platform gate (Windows-only)
//! The child webview is a WebView2 instance and the `browser_*` tools attach to
//! it via its CDP debug endpoint — both only exist on Windows. On other
//! platforms [`browser_webview_ensure`] refuses with
//! [`UNSUPPORTED_BROWSER_TAB_MSG`] (the Browser tab shows that message as a
//! red panel, via [`browser_webview_supported`]) and the `browser_*` tools are
//! not registered at all (see `AgentLoopFactory`). Every other command here
//! is already a safe no-op when no webview exists.
//!
//! ## Z-order constraint
//! A child WebView2 is a separate HWND composited by the OS *above* the
//! parent window's HTML. Any HTML overlay crossing the browser rect (a
//! full-viewport modal, dropdown, tooltip) would be punched through. The
//! [`browser_webview_overlay_enter`]/[`browser_webview_overlay_exit`]
//! commands hide/show the child around full-viewport modals so the modal
//! renders on top. The overlay depth is a counter (not a bool) so nested
//! modals balance correctly under React strict-mode double-invoke.
//!
//! ## State
//! All mutable state lives behind one `tokio::sync::Mutex` in
//! [`BrowserWebviewState`]. The `Webview` handle is `Send` + `Sync` (it wraps
//! an OS HWND via wry's platform handle), so it is safe to hold across the
//! async commands. The overlay depth is an `AtomicU32` so enter/exit can be
//! decided without holding the webview lock (the show/hide call still takes
//! the lock, but the depth read is lock-free).
//!
//! ## Watchdog notes
//! Every controller operation (ensure create/move, navigate, stop, reload,
//! overlay enter/exit, rect apply, destroy) pushes a short note onto the hang
//! watchdog's activity ring via [`note`] — before the OS call, so the next
//! hang report names the in-flight operation (the top-ranked hang location
//! in the 2026-08-23 freeze diagnosis).

use std::sync::atomic::{AtomicU32, Ordering};

use tauri::webview::{Webview, WebviewBuilder};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl};

use mnemo::browser::normalize_url;

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// Push a short activity note onto the hang watchdog's ring (no-op when the
/// watchdog isn't running). Mirrors the `watchdog_note` feed in
/// `ipc/events.rs`: every child-webview controller operation notes what is
/// about to run, so the next hang report answers "was a controller call in
/// flight?" — the top-ranked hang location in the 2026-08-23 freeze
/// diagnosis (.coding/reviews/2026-08-23-freeze-diagnosis.md).
fn note(state: &IpcState, text: &str) {
    if let Some(watchdog) = &state.watchdog {
        watchdog.note(text);
    }
}

/// Extract the host portion of a normalized URL for watchdog notes
/// (`"scheme://host/path?query"` → `"host"`). Falls back to the whole input
/// when there is no `://` separator (e.g. `data:` URLs) or the authority is
/// empty (`file:///C:/x.html` — host-less file URLs), so a note is always
/// produced.
fn url_host(url: &str) -> &str {
    match url.split_once("://") {
        Some((_, rest)) => match rest.split(['/', '?', '#']).next() {
            Some(host) if !host.is_empty() => host,
            _ => url,
        },
        None => url,
    }
}

/// Watchdog note for the create arm of [`browser_webview_ensure`].
fn note_ensure_create(normalized: &str) -> String {
    format!("webview ensure create {}", url_host(normalized))
}

/// Watchdog note for [`browser_webview_navigate`].
fn note_navigate(normalized: &str) -> String {
    format!("webview navigate {}", url_host(normalized))
}

/// The label used for the single child webview. Tauri forbids two webviews with
/// the same label, so this is a fixed constant (there is only ever one child).
pub const CHILD_WEBVIEW_LABEL: &str = "browser-child";

/// The error message for Browser-tab use on a platform without the
/// Windows-only WebView2/CDP stack. Shown as the red panel by the frontend
/// (which asks [`browser_webview_supported`] on mount) and returned by
/// [`browser_webview_ensure`] as a backend safety net.
pub const UNSUPPORTED_BROWSER_TAB_MSG: &str = "The embedded Browser tab requires Microsoft \
     WebView2 and its CDP debug protocol, which only exist on Windows. On this platform the \
     tab is disabled and the agent's browser_* inspection tools are not registered; the \
     headless offscreen_browser_* agent tools remain available.";

/// Whether the native child webview (and the `browser_*` CDP stack behind it) is
/// supported on this platform. The child webview is a WebView2 instance and
/// the `browser_*` tools attach to it via its CDP debug endpoint — both are
/// Windows-only — so this is a compile-time constant per target platform.
pub fn child_webview_supported() -> bool {
    cfg!(windows)
}

/// The gate for [`browser_webview_ensure`]: `Ok(())` when the child webview is
/// supported on this platform, `Err` carrying [`UNSUPPORTED_BROWSER_TAB_MSG`]
/// otherwise. Split out as a pure function so both branches are unit-testable
/// on any single platform.
fn ensure_platform_gate(supported: bool) -> Result<(), IpcError> {
    if supported {
        Ok(())
    } else {
        Err(IpcError::msg(UNSUPPORTED_BROWSER_TAB_MSG))
    }
}

/// The mutable state for the child webview, behind one async mutex.
pub struct BrowserWebviewState {
    /// The child webview handle, once created. `None` until the first
    /// `browser_webview_ensure` call succeeds.
    webview: Option<Webview>,
    /// The last-applied rect (x, y, w, h) in physical pixels. Tracked so
    /// `set_rect` can skip a no-op move/resize (avoids a redundant OS call
    /// on every ResizeObserver tick).
    rect: (i32, i32, u32, u32),
    /// Overlay depth: 0 = visible, >0 = hidden (a modal is open). Incremented
    /// by `overlay_enter`, decremented by `overlay_exit`. The webview is shown
    /// on exit only when depth returns to 0 AND `tab_visible` is true.
    overlay_depth: AtomicU32,
    /// Whether the Browser tab is the active tab. The webview is hidden when
    /// the tab is inactive (so it doesn't punch through other tabs' content).
    tab_visible: bool,
}

impl BrowserWebviewState {
    /// Construct the initial state: no webview, zero rect, depth 0, hidden.
    pub fn new() -> Self {
        Self {
            webview: None,
            rect: (0, 0, 0, 0),
            overlay_depth: AtomicU32::new(0),
            tab_visible: false,
        }
    }

    /// Whether the webview should currently be shown: the tab is active AND no
    /// overlay (modal) is hiding it.
    fn should_show(&self) -> bool {
        self.tab_visible && self.overlay_depth.load(Ordering::Acquire) == 0
    }

    /// Apply show/hide to the webview to match [`should_show`]. No-op if the
    /// webview hasn't been created yet.
    fn apply_visibility(&self) {
        if let Some(wv) = &self.webview {
            // show/hide are infallible from our perspective — a failure here
            // (e.g. the window was destroyed) is not actionable, so ignore it.
            if self.should_show() {
                let _ = wv.show();
            } else {
                let _ = wv.hide();
            }
        }
    }
}

impl Default for BrowserWebviewState {
    fn default() -> Self {
        Self::new()
    }
}

impl BrowserWebviewState {
    /// Drop the held webview handle (tears down the child webview on drop).
    /// Called from the app's `RunEvent::Exit` handler so no native HWND
    /// leaks. The field is private; this is the only way for `main.rs` to
    /// clear it without exposing the `Webview` type across module boundaries.
    pub(crate) fn take_webview(&mut self) {
        self.webview = None;
        self.rect = (0, 0, 0, 0);
        self.overlay_depth.store(0, Ordering::Release);
        self.tab_visible = false;
    }
}

/// The child webview's process-wide shared state: the async-guarded core
/// ([`BrowserWebviewState`]) plus a deferred-rect slot.
///
/// The deferred slot closes the self-heal hole of a `Busy`-dropped rect
/// update (review F3 of the 2026-08-18 RDP freeze fixes): when
/// [`try_apply_rect`] cannot take the main mutex it stores the desired rect
/// here, and the next command that acquires the mutex either supersedes it
/// (a newer rect report) or applies it (a non-rect command).
///
/// Lock ordering: the deferred slot is a leaf lock — it is only ever held
/// for a pair of statements and never while acquiring another lock, so
/// holding the main state guard and then taking the deferred lock (heal /
/// supersede) cannot deadlock.
pub struct BrowserWebviewShared {
    /// The main state, behind the async mutex every command takes.
    pub state: tokio::sync::Mutex<BrowserWebviewState>,
    /// The newest rect that was dropped because the main mutex was busy.
    deferred_rect: std::sync::Mutex<Option<(i32, i32, u32, u32)>>,
}

impl BrowserWebviewShared {
    /// Construct the initial (empty) shared state.
    pub fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(BrowserWebviewState::new()),
            deferred_rect: std::sync::Mutex::new(None),
        }
    }

    /// Store `rect` as the newest dropped update (the `Busy` path of
    /// [`try_apply_rect`]).
    fn defer_rect(&self, rect: (i32, i32, u32, u32)) {
        *self.deferred_rect.lock().expect("deferred_rect poisoned") = Some(rect);
    }

    /// Clear any deferred rect WITHOUT applying it — called after a
    /// successful, newer rect report. Rect reports are coalesced to one per
    /// animation frame by the frontend, so a report that lands later is at
    /// least as new as anything deferred earlier; healing the parked rect
    /// then would replay a stale layout.
    fn supersede_deferred(&self) {
        *self.deferred_rect.lock().expect("deferred_rect poisoned") = None;
    }

    /// Apply any deferred rect to the held guard (`bw`) and clear it. Called
    /// by NON-rect commands that just acquired the main mutex, so a dropped
    /// report self-heals on the next command of any kind instead of leaving
    /// a stale rect with no re-send path. No-op when nothing was deferred or
    /// the deferred rect matches the current one.
    fn heal_deferred(&self, bw: &mut BrowserWebviewState) {
        let deferred = *self.deferred_rect.lock().expect("deferred_rect poisoned");
        if let Some(rect) = deferred {
            self.supersede_deferred();
            if bw.rect != rect {
                apply_rect(bw, rect);
            }
        }
    }
}

impl Default for BrowserWebviewShared {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply `rect` to the held state guard: move/resize the webview (if any)
/// and record the rect as current.
fn apply_rect(bw: &mut BrowserWebviewState, rect: (i32, i32, u32, u32)) {
    if let Some(wv) = &bw.webview {
        let _ = wv.set_position(PhysicalPosition::new(rect.0, rect.1));
        let _ = wv.set_size(PhysicalSize::new(rect.2, rect.3));
    }
    bw.rect = rect;
}

/// Whether the native child webview for the Browser tab is supported on this
/// platform (Windows only — WebView2 + CDP; see [`child_webview_supported`]).
/// The frontend fetches this on Browser-tab mount to decide between the URL
/// bar and the unsupported-platform red panel.
#[tauri::command]
pub async fn browser_webview_supported() -> bool {
    child_webview_supported()
}

/// Ensure the child webview exists, positioned/sized at the given physical
/// rect, then navigate it to `url` (normalized). If the webview already
/// exists, just move/resize it (the caller separately calls navigate). This
/// is the create-on-first-use entry point the frontend calls on Browser-tab
/// mount + Open.
///
/// Coordinates are physical pixels (the frontend multiplies CSS pixels by
/// `devicePixelRatio` before calling).
#[tauri::command]
pub async fn browser_webview_ensure(
    app: AppHandle,
    state: State<'_, IpcState>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    url: String,
) -> Result<(), IpcError> {
    // Platform gate: the child webview is a WebView2 instance (Windows-only).
    // Refuse BEFORE creating anything; the frontend usually never gets here
    // because it renders the red panel instead (browser_webview_supported).
    ensure_platform_gate(child_webview_supported())?;
    let normalized = normalize_url(&url).map_err(IpcError::from)?;
    let mut bw = state.browser_webview.state.lock().await;
    if bw.webview.is_none() {
        // Note BEFORE the controller calls: if this wedge hangs the main
        // thread, the ring names the in-flight op in the hang report.
        note(state.inner(), &note_ensure_create(&normalized));
        // Create the child webview inside the "main" window. `add_child` lives
        // on `Window` (not `WebviewWindow`), so get the raw window handle.
        let window = app
            .get_window("main")
            .ok_or_else(|| IpcError::msg("main window not found"))?;
        let builder = WebviewBuilder::new(
            CHILD_WEBVIEW_LABEL,
            WebviewUrl::External(
                normalized
                    .parse()
                    .map_err(|e| IpcError::msg(format!("invalid url: {e}")))?,
            ),
        );
        let webview = window
            .add_child(
                builder,
                PhysicalPosition::new(x, y),
                PhysicalSize::new(w, h),
            )
            .map_err(|e| IpcError::msg(format!("failed to create child webview: {e}")))?;
        bw.webview = Some(webview);
        bw.rect = (x, y, w, h);
        bw.tab_visible = true;
        bw.apply_visibility();
    } else {
        // Already exists — just move/resize (navigate is a separate call).
        if bw.rect != (x, y, w, h) {
            note(state.inner(), "webview ensure move");
            apply_rect(&mut bw, (x, y, w, h));
        }
    }
    // The ensure report is the newest rect (mount/Open) — supersede, don't
    // heal, anything parked earlier (a fresh create also invalidates it).
    state.browser_webview.supersede_deferred();
    Ok(())
}

/// The Tauri event channel that asks the frontend to reveal + select the
/// Browser tab. Emitted when the agent bootstraps the child webview
/// ([`browser_webview_ensure_for_agent`]) so the human sees the navigation
/// the agent initiated instead of a hidden webview loading invisibly.
pub const BROWSER_REVEAL_CHANNEL: &str = "browser://reveal";

/// The shared core of [`browser_webview_ensure_for_agent`]: ensure the child
/// webview exists (created at the last frontend-reported rect, or a default
/// right-side panel when none was reported yet), keep it hidden until the
/// frontend confirms the tab is active, and emit [`BROWSER_REVEAL_CHANNEL`].
/// Split from the `#[tauri::command]` wrapper so the app layer's
/// agent-side child-ensurer hook (main.rs → `set_child_ensurer`) can call it
/// directly with `app.try_state::<IpcState>()` — no command dispatch needed.
pub(crate) async fn ensure_for_agent_impl(
    app: &AppHandle,
    state: &IpcState,
    url: &str,
) -> Result<(), IpcError> {
    // Platform gate: same as the human-facing ensure — on non-Windows this
    // refuses before creating anything (the frontend shows its red panel).
    ensure_platform_gate(child_webview_supported())?;
    let normalized = normalize_url(url).map_err(IpcError::from)?;
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3) — any
    // command acquiring the mutex is a heal point.
    state.browser_webview.heal_deferred(&mut bw);
    if bw.webview.is_none() {
        // Rect: the last frontend-reported placeholder rect when one exists
        // (the normal case — the tab was mounted at least once), else a
        // default right-side panel (~40% width, full height) derived from
        // the main window's inner size.
        let rect = if bw.rect != (0, 0, 0, 0) {
            bw.rect
        } else {
            let window = app
                .get_window("main")
                .ok_or_else(|| IpcError::msg("main window not found"))?;
            let size = window
                .inner_size()
                .map_err(|e| IpcError::msg(format!("main window size unavailable: {e}")))?;
            let w = (size.width / 5).max(1) * 2;
            let x = size.width.saturating_sub(w) as i32;
            (x, 0, w.max(1), size.height.max(1))
        };
        // Note BEFORE the controller call (see `note`).
        note(state, &note_ensure_create(&normalized));
        let window = app
            .get_window("main")
            .ok_or_else(|| IpcError::msg("main window not found"))?;
        let builder = WebviewBuilder::new(
            CHILD_WEBVIEW_LABEL,
            WebviewUrl::External(
                normalized
                    .parse()
                    .map_err(|e| IpcError::msg(format!("invalid url: {e}")))?,
            ),
        );
        // Created via add_child (visible by default — WebviewBuilder has no
        // visibility builder flag); `apply_visibility` below immediately
        // reconciles: hidden when the Browser tab is inactive (the agent must
        // never punch a native HWND through whatever tab the user is on) —
        // the reveal event + the frontend's set_tab_visible(true) flip it
        // visible — or shown immediately when the tab is already active.
        let webview = window
            .add_child(
                builder,
                PhysicalPosition::new(rect.0, rect.1),
                PhysicalSize::new(rect.2, rect.3),
            )
            .map_err(|e| IpcError::msg(format!("failed to create child webview: {e}")))?;
        bw.webview = Some(webview);
        bw.rect = rect;
        // Preserve the frontend-reported `tab_visible`: when the Browser tab
        // is ALREADY the active tab, BrowserView's mount effect has set it
        // true and `apply_visibility` below shows the new webview immediately
        // (the user is looking at the tab the agent is driving). When the tab
        // is inactive (the fresh-bootstrap case) it stays false — the webview
        // remains hidden until the reveal event selects the tab and
        // BrowserView's set_tab_visible(true) flips it visible.
        bw.apply_visibility();
        // Ask the frontend to reveal + select the Browser tab so the human
        // sees the page the agent just opened. Best-effort: a failure to
        // emit must not fail the navigation.
        let _ = app.emit(BROWSER_REVEAL_CHANNEL, normalized.clone());
    }
    // The ensure report is the newest layout fact — supersede, don't heal,
    // anything parked earlier (a fresh create also invalidates it).
    state.browser_webview.supersede_deferred();
    Ok(())
}

/// Ensure the child webview exists for an AGENT-initiated navigation (the
/// `browser_navigate` bootstrap, plan 5ae26d22): creates it at the last
/// frontend-reported rect (or a default right-side panel), keeps it hidden
/// until the frontend confirms the Browser tab is active, and emits
/// [`BROWSER_REVEAL_CHANNEL`] so the frontend reveals + selects the tab.
/// Unlike the human-facing [`browser_webview_ensure`], no rect/URL-bar
/// interaction is required — the agent supplies only the URL.
#[tauri::command]
pub async fn browser_webview_ensure_for_agent(
    app: AppHandle,
    state: State<'_, IpcState>,
    url: String,
) -> Result<(), IpcError> {
    ensure_for_agent_impl(&app, state.inner(), &url).await
}

/// The outcome of a best-effort rect update ([`browser_webview_set_rect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RectOutcome {
    /// The rect differed and was applied.
    Applied,
    /// The rect was unchanged — nothing to do.
    Noop,
    /// The state mutex was busy; the update was DROPPED (latest-wins — the
    /// next ResizeObserver tick re-sends the current rect).
    Busy,
}

/// Try to move/resize the child webview to `rect` WITHOUT ever waiting on
/// the state mutex. The frontend reports rects once per coalesced
/// ResizeObserver/window-resize frame, so updates are latest-wins: when the
/// mutex is held (typically a controller call in flight — which can block
/// for a long time on a wedged compositor during an RDP session switch),
/// the update is parked in the deferred slot and returns `Busy` — the next
/// command that acquires the mutex supersedes or heals it (review F3), so
/// the drop can never leave a stale layout. `lock().await` would instead
/// queue suspended IPC tasks behind the wedge (R3 of
/// .coding/reviews/2026-08-18-rdp-freeze-diagnosis.md).
///
/// `on_apply` fires immediately BEFORE the move/resize controller calls on
/// the `Applied` path only — the caller pushes the watchdog note there, so
/// a hang during the controller call names the in-flight op in the ring
/// (review L3: noting after the apply could not).
fn try_apply_rect(
    shared: &BrowserWebviewShared,
    rect: (i32, i32, u32, u32),
    on_apply: impl FnOnce(),
) -> RectOutcome {
    let mut bw = match shared.state.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            shared.defer_rect(rect);
            return RectOutcome::Busy;
        }
    };
    if bw.rect == rect {
        // This report matches the current rect; anything deferred earlier is
        // older and must not be healed over it.
        shared.supersede_deferred();
        return RectOutcome::Noop;
    }
    on_apply();
    apply_rect(&mut bw, rect);
    // This report is newer than (or equal to) anything deferred earlier —
    // it supersedes the parked rect instead of healing a stale one.
    shared.supersede_deferred();
    RectOutcome::Applied
}

/// Move/resize the existing child webview to the given physical rect
/// (best-effort, latest-wins). No-op if the webview hasn't been created yet.
/// The frontend coalesces reports to one per animation frame; when the state
/// mutex is busy this DROPS the update instead of queueing behind the holder
/// — the next frame re-sends the rect (R3, 2026-08-18 RDP freeze diagnosis).
#[tauri::command]
pub async fn browser_webview_set_rect(
    state: State<'_, IpcState>,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
) -> Result<(), IpcError> {
    let _ = try_apply_rect(&state.browser_webview, (x, y, w, h), || {
        // Fired right before the move/resize controller calls, on the
        // Applied path only — rect reports arrive once per coalesced
        // ResizeObserver frame, and a note per report (Noop included) would
        // flood the ring.
        note(state.inner(), "webview rect apply");
    });
    Ok(())
}

/// Navigate the child webview to `url` (normalized). The URL is normalized via
/// the shared `normalize_url` choke point (scheme-less hostnames get an
/// omnibox scheme; the `http`/`https`/`data`/`file` allow-list is enforced —
/// `file://` URLs load local HTML files for debugging), so this is the single
/// entry point for navigation — never pass an unnormalized URL.
#[tauri::command]
pub async fn browser_webview_navigate(
    state: State<'_, IpcState>,
    url: String,
) -> Result<(), IpcError> {
    let normalized = normalize_url(&url).map_err(IpcError::from)?;
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3) — any
    // command acquiring the mutex is a heal point.
    state.browser_webview.heal_deferred(&mut bw);
    if let Some(wv) = &bw.webview {
        // Note BEFORE the controller call (see `note`).
        note(state.inner(), &note_navigate(&normalized));
        // `Webview::navigate` (tauri 2.11.5) loads the URL in the webview
        // without a destroy/recreate cycle. `tauri::Url` is a re-export of
        // `url::Url` (no separate crate dependency needed).
        let parsed: tauri::Url = normalized
            .parse()
            .map_err(|e| IpcError::msg(format!("invalid url: {e}")))?;
        wv.navigate(parsed)
            .map_err(|e| IpcError::msg(format!("navigate failed: {e}")))?;
    }
    Ok(())
}

/// Stop the child webview's in-flight page load. Tauri 2.11.5's `Webview` has
/// no `stop()` method, so this evaluates `window.stop()` in the page — the
/// JavaScript stop-button equivalent (standard in Chromium/WebView2). Safe
/// no-op when no webview exists yet.
#[tauri::command]
pub async fn browser_webview_stop(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3) — any
    // command acquiring the mutex is a heal point.
    state.browser_webview.heal_deferred(&mut bw);
    if let Some(wv) = &bw.webview {
        // Note BEFORE the controller call (see `note`).
        note(state.inner(), "webview stop");
        // `Webview::eval` (tauri 2.11.5) runs the JS in the page; there is
        // no native stop(), so `window.stop()` is the halt mechanism.
        wv.eval("window.stop()")
            .map_err(|e| IpcError::msg(format!("stop failed: {e}")))?;
    }
    Ok(())
}

/// Reload the child webview's current page — always a HARD reload that
/// bypasses the HTTP cache (`Webview::reload`, tauri 2.11.5, whose vendored
/// wry issues CDP `Page.reload` with `ignoreCache` — see
/// `vendor/wry/PATCHES.md` and `tests/wry_hard_reload_patch.rs`; a plain
/// `Reload()` serves cached subresources, so pages came back out of sync).
/// Safe no-op when no webview exists yet.
#[tauri::command]
pub async fn browser_webview_reload(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3) — any
    // command acquiring the mutex is a heal point.
    state.browser_webview.heal_deferred(&mut bw);
    if let Some(wv) = &bw.webview {
        // Note BEFORE the controller call (see `note`).
        note(state.inner(), "webview reload");
        wv.reload()
            .map_err(|e| IpcError::msg(format!("reload failed: {e}")))?;
    }
    Ok(())
}

/// Set whether the Browser tab is the active tab. When inactive, the child
/// webview is hidden (so it doesn't punch through other tabs' content). When
/// active, it's shown only if no overlay (modal) is hiding it.
#[tauri::command]
pub async fn browser_webview_set_tab_visible(
    state: State<'_, IpcState>,
    visible: bool,
) -> Result<(), IpcError> {
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3).
    state.browser_webview.heal_deferred(&mut bw);
    bw.tab_visible = visible;
    bw.apply_visibility();
    Ok(())
}

/// Enter a modal overlay: increment the overlay depth and hide the child
/// webview (a full-viewport modal would otherwise be punched through by the
/// native HWND). Nested modals balance via the counter.
#[tauri::command]
pub async fn browser_webview_overlay_enter(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3).
    state.browser_webview.heal_deferred(&mut bw);
    note(state.inner(), "webview overlay enter");
    bw.overlay_depth.fetch_add(1, Ordering::AcqRel);
    bw.apply_visibility();
    Ok(())
}

/// Exit a modal overlay: decrement the overlay depth and show the child webview
/// only when depth returns to 0 AND the tab is visible.
#[tauri::command]
pub async fn browser_webview_overlay_exit(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let mut bw = state.browser_webview.state.lock().await;
    // Heal any rect dropped while the mutex was busy (review F3).
    state.browser_webview.heal_deferred(&mut bw);
    note(state.inner(), "webview overlay exit");
    // Saturating subtract so a stray exit (e.g. React strict-mode double-cleanup)
    // can't underflow to u32::MAX and wedge the webview hidden forever.
    let prev = bw.overlay_depth.fetch_sub(1, Ordering::AcqRel);
    if prev == 0 {
        // Underflow guard: restore to 0 instead of wrapping.
        bw.overlay_depth.store(0, Ordering::Release);
    }
    bw.apply_visibility();
    Ok(())
}

/// Destroy the child webview (drop the handle). Called on app exit (the
/// RunEvent::Exit handler in main.rs) so no HWND leaks. Safe to call when no
/// webview exists.
#[tauri::command]
pub async fn browser_webview_destroy(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let mut bw = state.browser_webview.state.lock().await;
    note(state.inner(), "webview destroy");
    // Dropping the Webview handle closes the child webview (wry tears down the
    // platform webview on drop).
    bw.webview = None;
    bw.rect = (0, 0, 0, 0);
    bw.overlay_depth.store(0, Ordering::Release);
    bw.tab_visible = false;
    // Clear any parked rect — it targeted the just-destroyed webview.
    state.browser_webview.supersede_deferred();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overlay counter must balance: enter → depth 1, exit → depth 0.
    /// And `should_show` must reflect the depth (hidden while >0, shown on 0
    /// when the tab is visible). This uses a bare `BrowserWebviewState` (no
    /// webview handle) so it's a pure-logic test — `apply_visibility` is a
    /// no-op when `webview` is `None`.
    #[test]
    fn overlay_counter_balances_and_gates_visibility() {
        let mut bw = BrowserWebviewState::new();
        bw.tab_visible = true;

        // Initially: depth 0, tab visible → should show.
        assert_eq!(bw.overlay_depth.load(Ordering::Acquire), 0);
        assert!(bw.should_show());

        // Enter overlay: depth 1 → hidden.
        bw.overlay_depth.fetch_add(1, Ordering::AcqRel);
        bw.apply_visibility();
        assert_eq!(bw.overlay_depth.load(Ordering::Acquire), 1);
        assert!(!bw.should_show());

        // Exit overlay: depth 0 → shown again.
        let prev = bw.overlay_depth.fetch_sub(1, Ordering::AcqRel);
        assert_eq!(prev, 1);
        if prev == 0 {
            bw.overlay_depth.store(0, Ordering::Release);
        }
        bw.apply_visibility();
        assert_eq!(bw.overlay_depth.load(Ordering::Acquire), 0);
        assert!(bw.should_show());
    }

    /// A stray exit (more exits than enters, e.g. React strict-mode
    /// double-cleanup) must NOT underflow to u32::MAX and wedge the webview
    /// hidden forever. The saturating guard restores depth to 0.
    #[test]
    fn overlay_exit_underflow_does_not_wedge() {
        let mut bw = BrowserWebviewState::new();
        bw.tab_visible = true;

        // Stray exit with depth already 0: must clamp to 0, not underflow.
        let prev = bw.overlay_depth.fetch_sub(1, Ordering::AcqRel);
        assert_eq!(prev, 0);
        if prev == 0 {
            bw.overlay_depth.store(0, Ordering::Release);
        }
        bw.apply_visibility();
        assert_eq!(bw.overlay_depth.load(Ordering::Acquire), 0);
        assert!(
            bw.should_show(),
            "stray exit must not wedge the webview hidden"
        );
    }

    /// `should_show` is false when the tab is inactive, even with depth 0.
    #[test]
    fn tab_hidden_overrides_depth() {
        let mut bw = BrowserWebviewState::new();
        bw.tab_visible = false;
        bw.overlay_depth.store(0, Ordering::Release);
        assert!(!bw.should_show());

        // Tab visible again → shown.
        bw.tab_visible = true;
        assert!(bw.should_show());
    }

    /// R3 regression: when the state mutex is held (a controller call in
    /// flight), a rect update must report `Busy` and NOT queue behind the
    /// lock — the old `lock().await` piled suspended IPC tasks up behind a
    /// possibly wedged controller call during an RDP session switch.
    #[tokio::test]
    async fn set_rect_reports_busy_when_lock_held() {
        let shared = BrowserWebviewShared::new();
        let _guard = shared.state.lock().await;
        assert_eq!(
            try_apply_rect(&shared, (1, 2, 3, 4), || {}),
            RectOutcome::Busy
        );
    }

    /// R3 regression: an unchanged rect is a no-op; a changed rect is applied
    /// (the stored rect updates even with no webview handle yet, mirroring
    /// the old behavior on a not-yet-created webview). The `on_apply` note
    /// hook must fire on Applied ONLY — before the controller call — and
    /// never on Noop (review L3 ordering contract).
    #[tokio::test]
    async fn set_rect_noop_on_same_rect_then_applies_changed() {
        let shared = BrowserWebviewShared::new();
        let notes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fired = |n: &std::sync::Arc<std::sync::atomic::AtomicUsize>| {
            let n = n.clone();
            move || {
                n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        };
        assert_eq!(
            try_apply_rect(&shared, (1, 2, 3, 4), fired(&notes)),
            RectOutcome::Applied
        );
        assert_eq!(
            try_apply_rect(&shared, (1, 2, 3, 4), fired(&notes)),
            RectOutcome::Noop
        );
        assert_eq!(
            try_apply_rect(&shared, (5, 6, 7, 8), fired(&notes)),
            RectOutcome::Applied
        );
        assert_eq!(
            notes.load(std::sync::atomic::Ordering::Relaxed),
            2,
            "on_apply must fire on Applied only (never Noop)"
        );
        assert_eq!(shared.state.lock().await.rect, (5, 6, 7, 8));
    }

    /// F3 regression (2026-08-18 review): a `Busy`-dropped rect must
    /// self-heal. It is parked in the deferred slot; the next NON-rect
    /// command that acquires the mutex applies it (heal), while a newer
    /// successful rect report supersedes it (no stale replay).
    #[tokio::test]
    async fn busy_rect_defers_then_heals_on_next_command() {
        let shared = BrowserWebviewShared::new();

        // Mutex busy → the rect is deferred, not lost.
        {
            let _guard = shared.state.lock().await;
            assert_eq!(
                try_apply_rect(&shared, (1, 2, 3, 4), || {}),
                RectOutcome::Busy
            );
        }

        // A non-rect command acquires the mutex and heals: the deferred rect
        // is applied even though no new report arrived.
        {
            let mut bw = shared.state.lock().await;
            shared.heal_deferred(&mut bw);
            assert_eq!(bw.rect, (1, 2, 3, 4), "deferred rect must self-heal");
        }

        // A stale deferred rect must NOT replay over a newer report: park
        // (9,9,9,9) behind a busy mutex, then let a newer report (5,6,7,8)
        // land successfully — the final rect is the newer one and the slot
        // is empty (a later heal is a no-op).
        {
            let _guard = shared.state.lock().await;
            assert_eq!(
                try_apply_rect(&shared, (9, 9, 9, 9), || {}),
                RectOutcome::Busy
            );
        }
        assert_eq!(
            try_apply_rect(&shared, (5, 6, 7, 8), || {}),
            RectOutcome::Applied
        );
        assert_eq!(shared.state.lock().await.rect, (5, 6, 7, 8));
        {
            let mut bw = shared.state.lock().await;
            shared.heal_deferred(&mut bw);
            assert_eq!(bw.rect, (5, 6, 7, 8), "stale deferred rect must not replay");
        }
    }

    /// The platform gate must be a compile-time constant tied to the Windows
    /// target — the child webview is a WebView2 instance (Windows-only). This
    /// documents the invariant; the macOS CI build proves the other side.
    #[test]
    fn child_webview_supported_tracks_windows_target() {
        assert_eq!(child_webview_supported(), cfg!(windows));
    }

    /// Both branches of the ensure gate: a supported platform passes through
    /// cleanly; an unsupported one gets the explanatory message the Browser
    /// tab also surfaces as its red panel. `IpcError` exposes `kind` +
    /// `message` directly (no `Display`), so assert on the fields.
    #[test]
    fn ensure_platform_gate_both_branches() {
        assert!(ensure_platform_gate(true).is_ok());
        let err = ensure_platform_gate(false).expect_err("gate must fail when unsupported");
        assert_eq!(err.kind, "error");
        assert!(err.message.contains("Windows"), "message: {}", err.message);
        assert!(
            err.message.contains("browser_*"),
            "message: {}",
            err.message
        );
        assert_eq!(err.message, UNSUPPORTED_BROWSER_TAB_MSG);
    }

    /// `url_host` extracts just the host from normalized URLs and falls back
    /// to the input for unexpected shapes (a note is always produced).
    #[test]
    fn url_host_extracts_host_or_falls_back() {
        assert_eq!(url_host("https://example.com/some/path?q=1"), "example.com");
        assert_eq!(url_host("http://localhost:8080/x"), "localhost:8080");
        assert_eq!(url_host("https://a.b/#frag"), "a.b");
        // No scheme separator → the whole string is the fallback.
        assert_eq!(url_host("data:text/html,hi"), "data:text/html,hi");
    }

    /// The watchdog note builders produce the documented short strings —
    /// they are what a hang report's activity ring will show, so the exact
    /// wording is contract.
    #[test]
    fn watchdog_note_builders_produce_documented_strings() {
        assert_eq!(
            note_ensure_create("https://example.com/page"),
            "webview ensure create example.com"
        );
        assert_eq!(
            note_navigate("http://localhost:8080/"),
            "webview navigate localhost:8080"
        );
    }

    /// The controller-op notes must land in the hang watchdog's activity
    /// ring oldest-first (mirrors the `watchdog_note_tests` in events.rs;
    /// `State<'_, IpcState>` is impractical to construct, so this asserts
    /// the ring side of the contract with the real builders).
    #[test]
    fn activity_ring_receives_webview_notes_oldest_first() {
        let ring = crate::watchdog::ActivityRing::new(8);
        ring.push(note_ensure_create("https://example.com/"));
        ring.push(note_navigate("https://example.com/"));
        ring.push("webview overlay enter".to_string());
        ring.push("webview rect apply".to_string());
        assert_eq!(
            ring.snapshot(),
            vec![
                "webview ensure create example.com".to_string(),
                "webview navigate example.com".to_string(),
                "webview overlay enter".to_string(),
                "webview rect apply".to_string(),
            ]
        );
    }

    /// Source contract (backlog 3b395d27): the stop/reload commands need
    /// Tauri `State`, so the wiring is pinned as a source contract (mirrors
    /// `backlog_retry_routes_through_guarded_requeue`): stop evaluates
    /// `window.stop()` (tauri 2.11.5 has no native stop()), reload calls
    /// `Webview::reload` (a hard reload — the vendored-wry patch behind it
    /// is guarded by `tests/wry_hard_reload_patch.rs`), and both follow the
    /// navigate pattern —
    /// `heal_deferred`, the `if let Some(wv)` no-op gate, and the watchdog
    /// note BEFORE the controller call. Registration in main.rs's
    /// invoke_handler is pinned too — a command missing from the handler is
    /// a silent runtime 404 for the frontend.
    #[test]
    fn stop_and_reload_commands_follow_navigate_pattern() {
        let src = include_str!("browser_webview.rs");
        // Slice out each command body so the assertions inspect the real
        // call site — asserting against the whole file would be
        // self-referential (this test's own literals live in `src` too).
        let command_body = |name: &str| {
            let start = src
                .find(name)
                .unwrap_or_else(|| panic!("{name} command present"));
            let end = src[start..]
                .find("\n}\n")
                .map(|i| start + i)
                .unwrap_or(src.len());
            &src[start..end]
        };
        let stop = command_body("pub async fn browser_webview_stop");
        let reload = command_body("pub async fn browser_webview_reload");
        for (name, body, controller) in [
            ("stop", stop, "eval(\"window.stop()\")"),
            ("reload", reload, ".reload()"),
        ] {
            assert!(
                body.contains("heal_deferred(&mut bw)"),
                "{name} must heal a Busy-dropped rect (review F3)"
            );
            assert!(
                body.contains("if let Some(wv) = &bw.webview"),
                "{name} must be a safe no-op when no webview exists"
            );
            let note_at = body
                .find("note(state.inner()")
                .unwrap_or_else(|| panic!("{name} must push a watchdog note"));
            let call_at = body
                .find(controller)
                .unwrap_or_else(|| panic!("{name} must call {controller}"));
            assert!(
                note_at < call_at,
                "{name}: watchdog note must land BEFORE the controller call"
            );
        }
        // Exact note wording is contract — it is what a hang report's
        // activity ring shows.
        assert!(stop.contains("\"webview stop\""));
        assert!(reload.contains("\"webview reload\""));
        // Both commands must be registered in the invoke_handler.
        let main_src = include_str!("../main.rs");
        assert!(
            main_src.contains("ipc::browser_webview::browser_webview_stop"),
            "browser_webview_stop must be registered in main.rs invoke_handler"
        );
        assert!(
            main_src.contains("ipc::browser_webview::browser_webview_reload"),
            "browser_webview_reload must be registered in main.rs invoke_handler"
        );
    }
}
