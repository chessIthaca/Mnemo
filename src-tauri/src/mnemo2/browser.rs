// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Mnemo2's native Browser pane. There is deliberately no CDP dependency.

use std::sync::{Arc, Mutex as StdMutex};

use serde::Serialize;
use tauri::webview::{NewWindowResponse, PageLoadEvent, Webview, WebviewBuilder};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl};
use tokio::sync::Mutex;

pub const BROWSER_EVENT_CHANNEL: &str = "mnemo2://browser-event";
pub const BROWSER_OPEN_CHANNEL: &str = "mnemo2://browser-open-request";
const CHILD_LABEL: &str = "mnemo2-browser";
const UNSUPPORTED: &str = "The embedded Browser pane requires WebView2 on Windows. Open the address in your system browser instead.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BrowserRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct BrowserState {
    webview: Option<Webview>,
    rect: BrowserRect,
    visible: bool,
    /// The sequence of the newest visibility request applied so far.
    visible_seq: u64,
    overlay_depth: u32,
}

impl BrowserState {
    /// Apply a sequenced visibility request; an older one that arrives late
    /// (async commands run concurrently) is ignored. Returns whether it applied.
    fn request_visible(&mut self, visible: bool, seq: Option<u64>) -> bool {
        if let Some(seq) = seq {
            if seq < self.visible_seq {
                return false;
            }
            self.visible_seq = seq;
        }
        self.visible = visible;
        true
    }

    fn should_show(&self) -> bool {
        self.visible
            && self.overlay_depth == 0
            && self.rect.width > 0
            && self.rect.height > 0
    }

    fn apply_visibility(&self) {
        if let Some(webview) = &self.webview {
            if self.should_show() {
                let _ = webview.show();
            } else {
                let _ = webview.hide();
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct BrowserMetadata {
    url: String,
    title: String,
    loading: bool,
}

impl Default for BrowserMetadata {
    fn default() -> Self {
        Self {
            url: "about:blank".to_string(),
            title: String::new(),
            loading: false,
        }
    }
}

/// The native browser's observable URL/title/load state.
#[derive(Clone, Debug, Serialize)]
pub struct BrowserSnapshot {
    /// Whether a native child WebView2 is available on this target.
    pub supported: bool,
    /// The committed or currently loading URL.
    pub url: String,
    /// The page's real document title.
    pub title: String,
    /// Whether a page load is in progress.
    pub loading: bool,
}

/// One lazily created child webview plus latest-wins layout state.
#[derive(Default)]
pub struct BrowserController {
    state: Mutex<BrowserState>,
    deferred_rect: StdMutex<Option<BrowserRect>>,
    metadata: Arc<StdMutex<BrowserMetadata>>,
}

fn supported() -> bool {
    cfg!(windows)
}

fn metadata(controller: &BrowserController) -> Result<BrowserMetadata, String> {
    controller
        .metadata
        .lock()
        .map(|metadata| metadata.clone())
        .map_err(|_| "the browser state is unavailable".to_string())
}

fn snapshot(controller: &BrowserController) -> Result<BrowserSnapshot, String> {
    let metadata = metadata(controller)?;
    Ok(BrowserSnapshot {
        supported: supported(),
        url: metadata.url,
        title: metadata.title,
        loading: metadata.loading,
    })
}

fn emit(app: &AppHandle, metadata: &Arc<StdMutex<BrowserMetadata>>, loading: bool) {
    if let Ok(mut state) = metadata.lock() {
        state.loading = loading;
        let payload = BrowserSnapshot {
            supported: supported(),
            url: state.url.clone(),
            title: state.title.clone(),
            loading,
        };
        let _ = app.emit(BROWSER_EVENT_CHANNEL, payload);
    }
}

fn normalized_http_url(input: &str) -> Result<String, String> {
    let normalized = mnemo::browser::normalize_url(input).map_err(|err| err.to_string())?;
    let url = tauri::Url::parse(&normalized).map_err(|err| format!("invalid address: {err}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("the Browser pane accepts only HTTP(S) addresses".to_string());
    }
    Ok(url.to_string())
}

fn apply_rect(state: &mut BrowserState, rect: BrowserRect) -> Result<(), String> {
    if let Some(webview) = &state.webview {
        webview
            .set_position(PhysicalPosition::new(rect.x, rect.y))
            .map_err(|err| format!("could not move the browser pane: {err}"))?;
        webview
            .set_size(PhysicalSize::new(rect.width, rect.height))
            .map_err(|err| format!("could not resize the browser pane: {err}"))?;
    }
    state.rect = rect;
    Ok(())
}

fn defer_rect(controller: &BrowserController, rect: BrowserRect) {
    if let Ok(mut deferred) = controller.deferred_rect.lock() {
        *deferred = Some(rect);
    }
}

fn heal_rect(controller: &BrowserController, state: &mut BrowserState) -> Result<(), String> {
    let deferred = controller
        .deferred_rect
        .lock()
        .map_err(|_| "the browser layout state is unavailable".to_string())?
        .take();
    if let Some(rect) = deferred {
        if state.rect != rect {
            apply_rect(state, rect)?;
        }
    }
    Ok(())
}

/// Whether the current platform supports the embedded native pane.
#[tauri::command]
pub fn browser_supported() -> bool {
    supported()
}

/// Read the real page state, including its document title.
#[tauri::command]
pub fn browser_snapshot(state: State<'_, BrowserController>) -> Result<BrowserSnapshot, String> {
    snapshot(&state)
}

/// Create the native child if needed and apply its physical-pixel bounds.
#[tauri::command]
pub async fn browser_ensure(
    app: AppHandle,
    state: State<'_, BrowserController>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    url: String,
) -> Result<BrowserSnapshot, String> {
    if !supported() {
        return Err(UNSUPPORTED.to_string());
    }
    let url = normalized_http_url(&url)?;
    let rect = BrowserRect {
        x,
        y,
        width,
        height,
    };
    let mut browser = state.state.lock().await;
    heal_rect(&state, &mut browser)?;
    if browser.webview.is_none() {
        let window = app
            .get_window("main")
            .ok_or_else(|| "the main Mnemo2 window is unavailable".to_string())?;
        let rect = if rect.width == 0 || rect.height == 0 {
            let size = window
                .inner_size()
                .map_err(|err| format!("could not read the main window size: {err}"))?;
            BrowserRect {
                x: (size.width / 2) as i32,
                y: 0,
                width: (size.width / 2).max(1),
                height: size.height.max(1),
            }
        } else {
            rect
        };
        let parsed = tauri::Url::parse(&url).map_err(|err| format!("invalid address: {err}"))?;
        let metadata = state.metadata.clone();
        let load_app = app.clone();
        let load_metadata = metadata.clone();
        let title_app = app.clone();
        let title_metadata = metadata.clone();
        let open_app = app.clone();
        let builder = crate::webview_udf::apply(
            WebviewBuilder::new(CHILD_LABEL, WebviewUrl::External(parsed)),
            CHILD_LABEL,
        )
        .on_navigation(|url| matches!(url.scheme(), "http" | "https"))
        .on_page_load(move |_webview, payload| {
            if let Ok(mut metadata) = load_metadata.lock() {
                metadata.url = payload.url().to_string();
                metadata.loading = matches!(payload.event(), PageLoadEvent::Started);
            }
            emit(
                &load_app,
                &load_metadata,
                matches!(payload.event(), PageLoadEvent::Started),
            );
        })
        .on_document_title_changed(move |_webview, title| {
            if let Ok(mut metadata) = title_metadata.lock() {
                metadata.title = title;
            }
            emit(&title_app, &title_metadata, false);
        })
        .on_new_window(move |url, _features| {
            let payload = BrowserSnapshot {
                supported: supported(),
                url: url.to_string(),
                title: String::new(),
                loading: true,
            };
            let _ = open_app.emit(BROWSER_OPEN_CHANNEL, payload);
            NewWindowResponse::Deny
        });
        let webview = window
            .add_child(
                builder,
                PhysicalPosition::new(rect.x, rect.y),
                PhysicalSize::new(rect.width, rect.height),
            )
            .map_err(|err| format!("could not create the native Browser pane: {err}"))?;
        browser.webview = Some(webview);
        browser.rect = rect;
    } else if browser.rect != rect {
        apply_rect(&mut browser, rect)?;
    }
    browser.apply_visibility();
    snapshot(&state)
}

/// Navigate in the real webview, creating it only when the pane is available.
#[tauri::command]
pub async fn browser_navigate(
    app: AppHandle,
    state: State<'_, BrowserController>,
    url: String,
) -> Result<BrowserSnapshot, String> {
    let url = normalized_http_url(&url)?;
    let (rect, existing) = {
        let mut browser = state.state.lock().await;
        heal_rect(&state, &mut browser)?;
        (browser.rect, browser.webview.clone())
    };
    if let Some(webview) = existing {
        let parsed = tauri::Url::parse(&url).map_err(|err| format!("invalid address: {err}"))?;
        webview
            .navigate(parsed)
            .map_err(|err| format!("could not navigate the Browser pane: {err}"))?;
        snapshot(&state)
    } else {
        browser_ensure(
            app,
            state,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            url,
        )
        .await
    }
}

/// Use Chromium's real session history, never a frontend URL stack.
#[tauri::command]
pub async fn browser_history(
    state: State<'_, BrowserController>,
    direction: String,
) -> Result<(), String> {
    let browser = state.state.lock().await;
    let script = match direction.as_str() {
        "back" => "history.back()",
        "forward" => "history.forward()",
        _ => return Err("unknown browser history direction".to_string()),
    };
    if let Some(webview) = &browser.webview {
        webview
            .eval(script)
            .map_err(|err| format!("could not change browser history: {err}"))?;
    }
    Ok(())
}

/// Reload the real page. The shared vendored wry performs a hard reload on Windows.
#[tauri::command]
pub async fn browser_reload(state: State<'_, BrowserController>) -> Result<(), String> {
    let browser = state.state.lock().await;
    if let Some(webview) = &browser.webview {
        webview
            .reload()
            .map_err(|err| format!("could not reload the Browser pane: {err}"))?;
    }
    Ok(())
}

/// Latest-wins layout updates never queue behind a wedged controller operation.
#[tauri::command]
pub async fn browser_set_rect(
    state: State<'_, BrowserController>,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let rect = BrowserRect {
        x,
        y,
        width,
        height,
    };
    let Ok(mut browser) = state.state.try_lock() else {
        defer_rect(&state, rect);
        return Ok(());
    };
    if browser.rect != rect {
        apply_rect(&mut browser, rect)?;
    }
    if let Ok(mut deferred) = state.deferred_rect.lock() {
        *deferred = None;
    }
    browser.apply_visibility();
    Ok(())
}

/// Keep the native HWND behind every other tab, drawer, or screenshot preview.
/// `seq` orders the requests: the native pane always reflects the newest one,
/// even when an older "show" is processed after a newer "hide".
#[tauri::command]
pub async fn browser_set_visible(
    state: State<'_, BrowserController>,
    visible: bool,
    seq: Option<u64>,
) -> Result<(), String> {
    let mut browser = state.state.lock().await;
    if !browser.request_visible(visible, seq) {
        return Ok(());
    }
    // Hide first: a layout heal must never be the reason a hide is skipped.
    browser.apply_visibility();
    heal_rect(&state, &mut browser)?;
    browser.apply_visibility();
    Ok(())
}

/// Balance HTML modal overlays so they cannot be punched through by the HWND.
#[tauri::command]
pub async fn browser_overlay(
    state: State<'_, BrowserController>,
    enter: bool,
) -> Result<(), String> {
    let mut browser = state.state.lock().await;
    heal_rect(&state, &mut browser)?;
    if enter {
        browser.overlay_depth = browser.overlay_depth.saturating_add(1);
    } else {
        browser.overlay_depth = browser.overlay_depth.saturating_sub(1);
    }
    browser.apply_visibility();
    Ok(())
}

impl BrowserController {
    /// Close the child and forget its project-local navigation state.
    pub async fn close(&self) -> Result<(), String> {
        let mut browser = self.state.lock().await;
        let result = if let Some(webview) = browser.webview.take() {
            let _ = webview.hide();
            webview
                .close()
                .map_err(|err| format!("could not close the Browser pane: {err}"))
        } else {
            Ok(())
        };
        *browser = BrowserState::default();
        if let Ok(mut deferred) = self.deferred_rect.lock() {
            *deferred = None;
        }
        if let Ok(mut metadata) = self.metadata.lock() {
            *metadata = BrowserMetadata::default();
        }
        result
    }
}

/// Close the native child through the typed command surface.
#[tauri::command]
pub async fn browser_close(state: State<'_, BrowserController>) -> Result<(), String> {
    state.close().await
}

/// macOS and other fallback: open the same validated address in the system browser.
#[tauri::command]
pub fn browser_open_external(url: String) -> Result<(), String> {
    let url = normalized_http_url(&url)?;
    open::that_detached(url).map_err(|err| format!("could not open the system browser: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visibility_accounts_for_tab_rect_and_nested_overlays() {
        let mut state = BrowserState {
            visible: true,
            rect: BrowserRect {
                x: 1,
                y: 2,
                width: 300,
                height: 400,
            },
            ..Default::default()
        };
        assert!(state.should_show());
        state.overlay_depth = 2;
        assert!(!state.should_show());
        state.overlay_depth = state.overlay_depth.saturating_sub(1);
        assert!(!state.should_show());
        state.overlay_depth = state.overlay_depth.saturating_sub(1);
        assert!(state.should_show());
        state.visible = false;
        assert!(!state.should_show());
        state.visible = true;
        state.rect.width = 0;
        assert!(!state.should_show());
    }

    /// Regression: show/hide are separate async commands with no ordering, so
    /// a "show" sent before a tab switch could be applied after the switch's
    /// "hide" and leave the native pane covering the Plan or Diff tab.
    #[test]
    fn a_late_older_visibility_request_never_overrides_a_newer_one() {
        let mut state = BrowserState {
            rect: BrowserRect {
                x: 1,
                y: 2,
                width: 300,
                height: 400,
            },
            ..Default::default()
        };
        assert!(state.request_visible(true, Some(1)));
        assert!(state.request_visible(false, Some(2)));
        assert!(!state.request_visible(true, Some(1)), "the stale show is ignored");
        assert!(!state.should_show());
        assert!(state.request_visible(true, Some(3)));
        assert!(state.should_show());
        // Unsequenced callers keep the old last-write-wins behavior.
        assert!(state.request_visible(false, None));
        assert!(!state.should_show());
    }

    #[test]
    fn navigation_is_http_only_and_normalizes_local_addresses() {
        assert_eq!(
            normalized_http_url("localhost:3000").expect("local address"),
            "http://localhost:3000/"
        );
        assert!(normalized_http_url("file:///C:/secret.txt").is_err());
        assert!(normalized_http_url("javascript:alert(1)").is_err());
    }

    #[tokio::test]
    async fn busy_layout_updates_defer_and_heal() {
        let controller = BrowserController::default();
        let rect = BrowserRect {
            x: 4,
            y: 5,
            width: 100,
            height: 200,
        };
        {
            let _guard = controller.state.lock().await;
            assert!(controller.state.try_lock().is_err());
            defer_rect(&controller, rect);
        }
        let mut state = controller.state.lock().await;
        heal_rect(&controller, &mut state).expect("heal rect");
        assert_eq!(state.rect, rect);
        assert!(controller.deferred_rect.lock().expect("deferred").is_none());
    }
}
