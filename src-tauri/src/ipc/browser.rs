// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Headless-browser Tauri commands.
//!
//! These expose the shared [`BrowserManager`](mnemo::browser::BrowserManager)
//! to the frontend. NOTE: after the Browser tab was unified into the interactive
//! iframe (the human plays; the agent drives it via the `browser_*` tools), the
//! headless Chromium became an **agent-only** surface — the agent's
//! `offscreen_browser_*` tools call `BrowserManager` directly (not via these
//! IPC commands). The
//! commands below (`browser_pages` / `browser_screenshot_latest` /
//! `browser_console` / `browser_open`) + the console forwarder are therefore
//! currently UI-orphaned; they're kept for a future headless debug panel. The
//! one command the unified Browser tab still uses is `browser_normalize_url`
//! (the iframe URL bar).

use std::sync::Arc;

use base64::Engine as _;
use tauri::{AppHandle, Emitter, State};

use mnemo::browser::{ConsoleEntry, PageInfo};

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The Tauri event channel for live console events (Rust → frontend).
pub const BROWSER_CONSOLE_CHANNEL: &str = "browser://console";

/// Spawn a forwarder that pushes every live console event from the shared
/// [`BrowserManager`](mnemo::browser::BrowserManager) to the frontend as a
/// `browser://console` Tauri event. The task ends when the app exits or the
/// manager is dropped (the broadcast closes).
pub fn spawn_console_forwarder(app: AppHandle, browser: Arc<mnemo::browser::BrowserManager>) {
    // Use Tauri's global async runtime, NOT a bare tokio::spawn: this runs in
    // the .setup() closure on the main thread, where no Tokio reactor context
    // exists. tokio::spawn panics there ("there is no reactor running") and
    // kills the app at startup.
    tauri::async_runtime::spawn(async move {
        let mut rx = browser.subscribe_console();
        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let _ = app.emit(BROWSER_CONSOLE_CHANNEL, ev);
                }
                // A lagging UI can always re-read the ring buffer via
                // browser_console — skip the backlog rather than block.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// List every open browser page (tab), most-recently-activated marked
/// `active`. Returns an empty vec when the browser has not spawned yet or the
/// call fails — the Browser tab renders an empty state rather than an error.
#[tauri::command]
pub async fn browser_pages(state: State<'_, IpcState>) -> Result<Vec<PageInfo>, IpcError> {
    state
        .runtime
        .browser
        .list_pages()
        .await
        .map_err(IpcError::from)
}

/// Get the latest screenshot of `page_id` as a base64-encoded PNG, for the
/// Browser tab's `<img src="data:image/png;base64,...">`. Returns `None` when
/// the page doesn't exist or the screenshot fails (e.g. the browser died) —
/// the tab keeps showing the previous frame instead of an error.
#[tauri::command]
pub async fn browser_screenshot_latest(
    page_id: String,
    state: State<'_, IpcState>,
) -> Result<Option<String>, IpcError> {
    match state.runtime.browser.screenshot(Some(&page_id)).await {
        Ok(png) => Ok(Some(base64::engine::general_purpose::STANDARD.encode(png))),
        Err(_) => Ok(None),
    }
}

/// Get the buffered console messages for `page_id` (logs, warnings, errors,
/// JS exceptions) WITHOUT draining them — the UI peeks, the agent tool drains,
/// so a tab refresh can never starve the agent of entries. Returns an empty
/// vec on error or when the page doesn't exist — the tab renders "no console
/// output" rather than an error.
#[tauri::command]
pub async fn browser_console(
    page_id: String,
    state: State<'_, IpcState>,
) -> Result<Vec<ConsoleEntry>, IpcError> {
    state
        .runtime
        .browser
        .console_peek(Some(&page_id))
        .await
        .or_else(|_| Ok(Vec::new()))
}

/// Open `url` in a new browser page (making it the active page) and return
/// its snapshot. Spawns the headless Chromium process on first use.
#[tauri::command]
pub async fn browser_open(url: String, state: State<'_, IpcState>) -> Result<PageInfo, IpcError> {
    state
        .runtime
        .browser
        .navigate(&url)
        .await
        .map_err(IpcError::from)
}

/// Normalize a URL the same way `browser_open` / `browser_navigate` do:
/// scheme-less hostnames (`google.com`) get an omnibox-style scheme
/// (`https://`; `http://` for localhost/IPs), then the scheme allow-list
/// (`http`, `https`, `data`, `file`) is enforced — `file://` URLs load local
/// HTML files for debugging. The Browser tab's URL bar calls this before
/// setting the webview URL, so `google.com` loads google (not the app's SPA
/// fallback). Returns the normalized URL or an error string.
#[tauri::command]
pub async fn browser_normalize_url(url: String) -> Result<String, IpcError> {
    mnemo::browser::normalize_url(&url).map_err(IpcError::from)
}
