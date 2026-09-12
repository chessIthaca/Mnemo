// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Browser tools — the headless debug browser (`offscreen_browser_*`) and the
//! live WebView2 Browser tab (`browser_*`), driven over CDP.
//!
//! All tools share one `Arc<BrowserManager>` (the same instance the Tauri IPC
//! layer uses), take an optional `page_id` (default = the active page), and are
//! `ToolCategory::Browser`. Browser tools are visible in every workflow
//! state: read tools are AutoRun; mutations (navigate/close/eval/click/type)
//! are NeedsApproval — every mutating call prompts the user, and that
//! approval gate (not state hiding) is the guard.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::browser::BrowserManager;
use crate::provider::ToolSchema;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// The shared deps every browser tool carries.
#[derive(Clone)]
pub(crate) struct Ctx {
    /// The shared headless browser.
    pub manager: Arc<BrowserManager>,
    /// The project sandbox (screenshot writes are validated through it).
    pub sandbox: Sandbox,
    /// Live mirror of `[general] enable_browser_inspection`.
    ///
    /// The six on-screen `browser_*` tools attach to the app's own WebView2
    /// over its CDP endpoint, which only exists when the user has opted in.
    /// With the flag off every call can only fail, so those tools hide
    /// themselves from the `tools` array (see `Tool::available`) instead of
    /// costing ~800 tokens per request to advertise an error. Shared and
    /// atomic so a Settings save takes effect on the next turn without
    /// rebuilding any registry. The off-screen `offscreen_browser_*` tools
    /// drive the headless browser and ignore this flag.
    pub inspection: Arc<std::sync::atomic::AtomicBool>,
}

/// The directory (sandbox-relative) screenshots are written to.
pub(crate) const SCREENSHOT_DIR: &str = ".coding/browser/screenshots";

/// Deserialize args or return an error `ToolResult`.
macro_rules! parse_args {
    ($t:ty, $args:expr) => {
        match serde_json::from_value::<$t>($args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        }
    };
}

/// Optional `page_id` argument shared by every tool.
#[derive(Debug, Deserialize)]
pub(crate) struct PageArg {
    /// The page to act on (default: the active page).
    #[serde(default)]
    pub page_id: Option<String>,
}

/// `offscreen_browser_navigate` — open a URL in a new page (NeedsApproval: navigates the
/// browser, a remote-side effect).
pub struct OffscreenNavigateTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenNavigateTool {
    fn name(&self) -> &str {
        "offscreen_browser_navigate"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_navigate",
            "Open a URL in a new headless-browser page and return its page_id + title. \
             Use the returned page_id with the other browser tools to screenshot, read the \
             console, or send input. Only http(s), data:, and file:// URLs are allowed \
             (file:// is for debugging local HTML files; javascript: and other schemes are \
             rejected); a scheme-less hostname like www.google.com is auto-prefixed with \
             https:// (http:// for localhost/IPs).",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The URL to open." }
                },
                "required": ["url"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            url: String,
        }
        let args = parse_args!(Args, args);
        match self.0.manager.navigate(&args.url).await {
            Ok(info) => ToolResult::success(format!(
                "opened page {} ({}): {}",
                info.id, info.title, info.url
            ))
            .with_data(serde_json::to_value(info).unwrap_or_default()),
            Err(e) => ToolResult::error(format!("offscreen_browser_navigate failed: {e}")),
        }
    }
}

/// `offscreen_browser_list_pages` — list open pages (AutoRun).
pub struct OffscreenListPagesTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenListPagesTool {
    fn name(&self) -> &str {
        "offscreen_browser_list_pages"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_list_pages",
            "List all open headless-browser pages with their page_id, URL, title, and \
             which is active. Read-only.",
            json!({ "type": "object", "properties": {} }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }
    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        match self.0.manager.list_pages().await {
            Ok(pages) => {
                if pages.is_empty() {
                    return ToolResult::success("no open pages");
                }
                let lines: Vec<String> = pages
                    .iter()
                    .map(|p| {
                        format!(
                            "{} {} ({}): {}",
                            if p.active { "*" } else { " " },
                            p.id,
                            p.title,
                            p.url
                        )
                    })
                    .collect();
                ToolResult::success(lines.join("\n"))
                    .with_data(serde_json::to_value(pages).unwrap_or_default())
            }
            Err(e) => ToolResult::error(format!("offscreen_browser_list_pages failed: {e}")),
        }
    }
}

/// `offscreen_browser_close_page` — close a page (NeedsApproval).
pub struct OffscreenClosePageTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenClosePageTool {
    fn name(&self) -> &str {
        "offscreen_browser_close_page"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_close_page",
            "Close a headless-browser page (default: the active page). Closing the last \
             page leaves the browser process running.",
            json!({
                "type": "object",
                "properties": {
                    "page_id": { "type": "string", "description": "Page to close (default: active)." }
                }
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args = parse_args!(PageArg, args);
        match self.0.manager.close_page(args.page_id.as_deref()).await {
            Ok(()) => ToolResult::success("page closed"),
            Err(e) => ToolResult::error(format!("offscreen_browser_close_page failed: {e}")),
        }
    }
}

/// `offscreen_browser_screenshot` — capture a PNG to `.coding/browser/screenshots/` and
/// return the sandbox-relative path (AutoRun). Pair with `describe_image` so the
/// vision model can "see" the page.
pub struct OffscreenScreenshotTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenScreenshotTool {
    fn name(&self) -> &str {
        "offscreen_browser_screenshot"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_screenshot",
            "Capture a PNG screenshot of a headless-browser page (default: active). The PNG \
             is written to .coding/browser/screenshots/ and the sandbox-relative path is \
             returned — pass it to describe_image to have the vision model read the page. \
             Read-only (captures pixels; does not change page state).",
            json!({
                "type": "object",
                "properties": {
                    "page_id": { "type": "string", "description": "Page to capture (default: active)." }
                }
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args = parse_args!(PageArg, args);
        let png = match self.0.manager.screenshot(args.page_id.as_deref()).await {
            Ok(b) => b,
            Err(e) => {
                return ToolResult::error(format!("offscreen_browser_screenshot failed: {e}"))
            }
        };
        let page = args.page_id.unwrap_or_else(|| "active".to_string());
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let rel = format!("{SCREENSHOT_DIR}/{page}-{ts}.png");
        let sandbox = self.0.sandbox.clone();
        let rel_for_write = rel.clone();
        let written = tokio::task::spawn_blocking(move || {
            let path = sandbox.validate_for_creation(std::path::Path::new(&rel_for_write))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &png)?;
            Ok::<_, crate::error::Error>(())
        })
        .await;
        match written {
            Ok(Ok(())) => ToolResult::success(format!("screenshot saved: {rel}"))
                .with_data(json!({ "path": rel })),
            Ok(Err(e)) => ToolResult::error(format!("failed to write screenshot: {e}")),
            Err(e) => ToolResult::error(format!("screenshot write task failed: {e}")),
        }
    }
}

/// `offscreen_browser_console` — drain buffered console entries (AutoRun).
pub struct OffscreenConsoleTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenConsoleTool {
    fn name(&self) -> &str {
        "offscreen_browser_console"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_console",
            "Drain the buffered console messages + JS exceptions from a headless-browser \
             page (default: active). Returns one `{level, text}` per line. Read-only.",
            json!({
                "type": "object",
                "properties": {
                    "page_id": { "type": "string", "description": "Page to read (default: active)." }
                }
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args = parse_args!(PageArg, args);
        match self.0.manager.console(args.page_id.as_deref()).await {
            Ok(entries) => {
                if entries.is_empty() {
                    return ToolResult::success("console is empty");
                }
                let lines: Vec<String> = entries
                    .iter()
                    .map(|e| format!("[{}] {}", e.level, e.text))
                    .collect();
                ToolResult::success(lines.join("\n"))
                    .with_data(serde_json::to_value(entries).unwrap_or_default())
            }
            Err(e) => ToolResult::error(format!("offscreen_browser_console failed: {e}")),
        }
    }
}

/// `offscreen_browser_snapshot` — text dump of the page DOM (AutoRun). The cheap way to
/// "see" the page without a screenshot.
pub struct OffscreenSnapshotTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenSnapshotTool {
    fn name(&self) -> &str {
        "offscreen_browser_snapshot"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_snapshot",
            "Return the HTML/text content of a headless-browser page (default: active) as a \
             text dump — the cheap way to read the DOM without a screenshot. Read-only.",
            json!({
                "type": "object",
                "properties": {
                    "page_id": { "type": "string", "description": "Page to snapshot (default: active)." }
                }
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args = parse_args!(PageArg, args);
        match self.0.manager.snapshot(args.page_id.as_deref()).await {
            Ok(html) => ToolResult::success(html),
            Err(e) => ToolResult::error(format!("offscreen_browser_snapshot failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build a tool context over a temp sandbox with a fresh browser manager.
    fn ctx(dir: &std::path::Path) -> Ctx {
        Ctx {
            manager: BrowserManager::new(),
            sandbox: Sandbox::new(dir).unwrap(),
            inspection: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    const PAGE: &str = "data:text/html,<title>t</title><h1 id=\"x\">hello</h1>";

    #[tokio::test]
    async fn navigate_then_list_pages() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let nav = OffscreenNavigateTool(c.clone());
        let res = nav.execute(json!({"url": PAGE})).await;
        assert!(res.success, "navigate failed: {}", res.output);
        let page_id = res.data.as_ref().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let list = OffscreenListPagesTool(c.clone());
        let res = list.execute(json!({})).await;
        assert!(res.success);
        assert!(res.output.contains(&page_id), "list output: {}", res.output);
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn snapshot_returns_dom() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": PAGE}))
            .await;
        let snap = OffscreenSnapshotTool(c.clone());
        let res = snap.execute(json!({})).await;
        assert!(res.success, "snapshot failed: {}", res.output);
        assert!(res.output.contains("hello"), "snapshot: {}", res.output);
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn screenshot_writes_png_in_sandbox() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": PAGE}))
            .await;
        let shot = OffscreenScreenshotTool(c.clone());
        let res = shot.execute(json!({})).await;
        assert!(res.success, "screenshot failed: {}", res.output);
        let rel = res.data.as_ref().unwrap()["path"].as_str().unwrap();
        assert!(
            rel.ends_with(".png"),
            "structured data.path must carry a .png path, got: {rel}"
        );
        // The reported path must be inside the sandbox + actually exist.
        let full = dir.path().join(rel);
        assert!(
            full.exists(),
            "screenshot not written at {}",
            full.display()
        );
        // PNG magic bytes.
        let bytes = std::fs::read(&full).unwrap();
        assert_eq!(&bytes[..4], &[0x89, b'P', b'N', b'G'], "not a PNG");
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn console_empty_for_fresh_page() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": PAGE}))
            .await;
        let console = OffscreenConsoleTool(c.clone());
        let res = console.execute(json!({})).await;
        assert!(res.success);
        assert_eq!(res.output, "console is empty");
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn close_page_removes_it() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": PAGE}))
            .await;
        let close = OffscreenClosePageTool(c.clone());
        let res = close.execute(json!({})).await;
        assert!(res.success, "close failed: {}", res.output);
        let list = OffscreenListPagesTool(c.clone());
        let res = list.execute(json!({})).await;
        assert_eq!(res.output, "no open pages");
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn safety_levels_are_set() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        // Reads are AutoRun, mutations NeedsApproval — the ToolFilter gates
        // visibility from these.
        assert_eq!(
            OffscreenNavigateTool(c.clone()).safety(),
            SafetyLevel::NeedsApproval
        );
        assert_eq!(
            OffscreenClosePageTool(c.clone()).safety(),
            SafetyLevel::NeedsApproval
        );
        assert_eq!(
            OffscreenListPagesTool(c.clone()).safety(),
            SafetyLevel::AutoRun
        );
        assert_eq!(
            OffscreenScreenshotTool(c.clone()).safety(),
            SafetyLevel::AutoRun
        );
        assert_eq!(
            OffscreenConsoleTool(c.clone()).safety(),
            SafetyLevel::AutoRun
        );
        assert_eq!(
            OffscreenSnapshotTool(c.clone()).safety(),
            SafetyLevel::AutoRun
        );
        assert_eq!(
            OffscreenEvalTool(c.clone()).safety(),
            SafetyLevel::NeedsApproval
        );
        assert_eq!(
            OffscreenClickTool(c.clone()).safety(),
            SafetyLevel::NeedsApproval
        );
        assert_eq!(
            OffscreenTypeTool(c.clone()).safety(),
            SafetyLevel::NeedsApproval
        );
        assert_eq!(
            OffscreenSwitchPageTool(c).safety(),
            SafetyLevel::NeedsApproval
        );
    }

    const FORM: &str = "data:text/html,<input id=\"in\" type=\"text\"><button id=\"b\" onclick=\"document.getElementById('in').value='clicked'\">go</button>";

    #[tokio::test]
    async fn eval_returns_value() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": "data:text/html,<p>x</p>"}))
            .await;
        let eval = OffscreenEvalTool(c.clone());
        let res = eval.execute(json!({"expression": "1 + 2"})).await;
        assert!(res.success, "eval failed: {}", res.output);
        assert_eq!(res.data.as_ref().unwrap(), &json!(3));
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn click_and_type_roundtrip() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": FORM}))
            .await;

        // Type into the input, then read it back via eval.
        let ty = OffscreenTypeTool(c.clone());
        let res = ty
            .execute(json!({"selector": "#in", "text": "hello"}))
            .await;
        assert!(res.success, "type failed: {}", res.output);
        let eval = OffscreenEvalTool(c.clone());
        let res = eval
            .execute(json!({"expression": "document.getElementById('in').value"}))
            .await;
        assert_eq!(res.data.as_ref().unwrap(), &json!("hello"));

        // Click the button; the inline onclick sets the input to 'clicked'.
        let click = OffscreenClickTool(c.clone());
        let res = click.execute(json!({"selector": "#b"})).await;
        assert!(res.success, "click failed: {}", res.output);
        let res = eval
            .execute(json!({"expression": "document.getElementById('in').value"}))
            .await;
        assert_eq!(res.data.as_ref().unwrap(), &json!("clicked"));
        c.manager.close().await.unwrap();
    }

    #[tokio::test]
    async fn switch_page_changes_active() {
        let dir = tempdir().unwrap();
        let c = ctx(dir.path());
        let r1 = OffscreenNavigateTool(c.clone())
            .execute(json!({"url": "data:text/html,a"}))
            .await;
        let id1 = r1.data.as_ref().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let r2 = OffscreenNavigateTool(c.clone())
            .execute(json!({"url": "data:text/html,b"}))
            .await;
        let id2 = r2.data.as_ref().unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();

        let switch = OffscreenSwitchPageTool(c.clone());
        let res = switch.execute(json!({"page_id": id1})).await;
        assert!(res.success, "switch failed: {}", res.output);
        assert!(res.output.contains(&id1));

        // Error on unknown page id.
        let res = switch.execute(json!({"page_id": "nonexistent"})).await;
        assert!(!res.success);
        let _ = id2;
        c.manager.close().await.unwrap();
    }
}

// ---- Phase C: input / mutation tools ----

/// `offscreen_browser_eval` — run arbitrary JavaScript and return its JSON value
/// (NeedsApproval: arbitrary script execution in the page context).
pub struct OffscreenEvalTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenEvalTool {
    fn name(&self) -> &str {
        "offscreen_browser_eval"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_eval",
            "Evaluate a JavaScript expression in a headless-browser page (default: active) \
             and return its JSON value. Promises are awaited (awaitPromise) and results are \
             returned by value, so `Promise.resolve(42)` yields 42, not a promise handle. \
             The escape hatch for reading DOM state, calling functions, or mutating the page. \
             Use with care — arbitrary script execution.",
            json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string", "description": "The JS expression to evaluate." },
                    "page_id": { "type": "string", "description": "Page to run in (default: active)." }
                },
                "required": ["expression"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            expression: String,
            #[serde(default)]
            page_id: Option<String>,
        }
        let args = parse_args!(Args, args);
        match self
            .0
            .manager
            .eval(args.page_id.as_deref(), &args.expression)
            .await
        {
            Ok(value) => ToolResult::success(
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            )
            .with_data(value),
            Err(e) => ToolResult::error(format!("offscreen_browser_eval failed: {e}")),
        }
    }
}

/// `offscreen_browser_click` — click an element by CSS selector (NeedsApproval: mutates
/// page state).
pub struct OffscreenClickTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenClickTool {
    fn name(&self) -> &str {
        "offscreen_browser_click"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_click",
            "Click the element matching a CSS selector in a headless-browser page \
             (default: active). Use offscreen_browser_snapshot to find selectors first.",
            json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector of the element to click." },
                    "page_id": { "type": "string", "description": "Page to act on (default: active)." }
                },
                "required": ["selector"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            selector: String,
            #[serde(default)]
            page_id: Option<String>,
        }
        let args = parse_args!(Args, args);
        match self
            .0
            .manager
            .click(args.page_id.as_deref(), &args.selector)
            .await
        {
            Ok(()) => ToolResult::success(format!("clicked '{}'", args.selector)),
            Err(e) => ToolResult::error(format!("offscreen_browser_click failed: {e}")),
        }
    }
}

/// `offscreen_browser_type` — focus an element and type text into it (NeedsApproval:
/// mutates page state).
pub struct OffscreenTypeTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenTypeTool {
    fn name(&self) -> &str {
        "offscreen_browser_type"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_type",
            "Focus the element matching a CSS selector and type text into it in a \
             headless-browser page (default: active).",
            json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector of the input to type into." },
                    "text": { "type": "string", "description": "The text to type." },
                    "page_id": { "type": "string", "description": "Page to act on (default: active)." }
                },
                "required": ["selector", "text"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            selector: String,
            text: String,
            #[serde(default)]
            page_id: Option<String>,
        }
        let args = parse_args!(Args, args);
        match self
            .0
            .manager
            .type_text(args.page_id.as_deref(), &args.selector, &args.text)
            .await
        {
            Ok(()) => ToolResult::success(format!(
                "typed {} chars into '{}'",
                args.text.chars().count(),
                args.selector
            )),
            Err(e) => ToolResult::error(format!("offscreen_browser_type failed: {e}")),
        }
    }
}

/// `offscreen_browser_switch_page` — make a page the active page (NeedsApproval: changes
/// which page id-less operations act on).
pub struct OffscreenSwitchPageTool(pub(crate) Ctx);

#[async_trait]
impl Tool for OffscreenSwitchPageTool {
    fn name(&self) -> &str {
        "offscreen_browser_switch_page"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "offscreen_browser_switch_page",
            "Make a page the active page for subsequent id-less browser operations. Use \
             offscreen_browser_list_pages to see the open page ids.",
            json!({
                "type": "object",
                "properties": {
                    "page_id": { "type": "string", "description": "The page id to make active." }
                },
                "required": ["page_id"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            page_id: String,
        }
        let args = parse_args!(Args, args);
        match self.0.manager.switch_page(&args.page_id).await {
            Ok(info) => ToolResult::success(format!(
                "active page is now {} ({}): {}",
                info.id, info.title, info.url
            ))
            .with_data(serde_json::to_value(info).unwrap_or_default()),
            Err(e) => ToolResult::error(format!("offscreen_browser_switch_page failed: {e}")),
        }
    }
}

// ---- Live WebView2 (Browser tab) inspection tools ----
//
// These attach to the app's OWN WebView2 (the Browser tab's child webview —
// the one the human browses/plays in) via its CDP debug port, instead of
// driving the separate headless Chromium. They let the agent see and read the
// live page — screenshot it as the human sees it, eval JS against it, or dump
// its DOM.
// `browser_screenshot` and `browser_snapshot` are read-only (AutoRun): they capture
// pixels / serialize the DOM and open no new navigation surface. `browser_eval`
// runs arbitrary JS against the app's own live frame and is approval-gated
// (NeedsApproval) — it can navigate the app's main frame (e.g.
// `window.location.href = …`), so it is NOT confined to the URL scheme
// allow-list that `offscreen_browser_navigate` enforces.

/// `browser_screenshot` — capture a PNG of the live Browser tab's child
/// webview (the page the human sees) and return the sandbox-relative path
/// (AutoRun). Pair with `describe_image` so the vision model can "see" it.
pub struct BrowserScreenshotTool(pub(crate) Ctx);

#[async_trait]
impl Tool for BrowserScreenshotTool {
    fn name(&self) -> &str {
        "browser_screenshot"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn available(&self) -> bool {
        self.0.inspection.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "browser_screenshot",
            "Capture a PNG screenshot of the LIVE Browser tab's child webview — \
             a native child WebView2 rendered with its GPU (not the headless browser). \
             When the Browser tab is NOT open, falls back to the app's own UI so you can \
             see the app itself. The PNG \
             is written to .coding/browser/screenshots/ and the sandbox-relative path is \
             returned — pass it to describe_image to have the vision model read the page. \
             Read-only (captures pixels; does not change page state). Requires a debug build \
             (the CDP port is dev-only).",
            json!({ "type": "object", "properties": {} }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }
    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let png = match self.0.manager.webview_screenshot().await {
            Ok(b) => b,
            Err(e) => return ToolResult::error(format!("browser_screenshot failed: {e}")),
        };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let rel = format!("{SCREENSHOT_DIR}/browser-{ts}.png");
        let sandbox = self.0.sandbox.clone();
        let rel_for_write = rel.clone();
        let written = tokio::task::spawn_blocking(move || {
            let path = sandbox.validate_for_creation(std::path::Path::new(&rel_for_write))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, &png)?;
            Ok::<_, crate::error::Error>(())
        })
        .await;
        match written {
            Ok(Ok(())) => ToolResult::success(format!("browser screenshot saved: {rel}"))
                .with_data(json!({ "path": rel })),
            Ok(Err(e)) => ToolResult::error(format!("failed to write browser screenshot: {e}")),
            Err(e) => ToolResult::error(format!("browser screenshot write task failed: {e}")),
        }
    }
}

/// `browser_eval` — run arbitrary JavaScript against the live Browser tab's
/// child webview (the WebView2 main frame) and return its JSON value.
/// NeedsApproval: arbitrary script execution against the app's OWN live
/// WebView2 (not a throwaway headless browser) — strictly more sensitive than
/// `offscreen_browser_eval`, which is also approval-gated.
pub struct BrowserEvalTool(pub(crate) Ctx);

#[async_trait]
impl Tool for BrowserEvalTool {
    fn name(&self) -> &str {
        "browser_eval"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn available(&self) -> bool {
        self.0.inspection.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "browser_eval",
            "Evaluate a JavaScript expression against the LIVE Browser tab's child webview (a \
             native child WebView2 — the top-level page, not an iframe) and return its JSON \
             value. Promises are awaited (awaitPromise) and results are returned by value. The \
             escape hatch for reading live page state — score, position, inventory, etc. The \
             child webview IS the page (no iframe, no same-origin limit), so evals run in its \
             main frame directly. Arbitrary script execution against the live app — approval \
             required. Requires a debug build.",
            json!({
                "type": "object",
                "properties": {
                    "expression": { "type": "string", "description": "The JS expression to evaluate against the live child webview." }
                },
                "required": ["expression"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            expression: String,
        }
        let args = parse_args!(Args, args);
        match self.0.manager.webview_eval(&args.expression).await {
            Ok(value) => ToolResult::success(
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
            )
            .with_data(value),
            Err(e) => ToolResult::error(format!("browser_eval failed: {e}")),
        }
    }
}

/// `browser_snapshot` — text dump of the live Browser tab's page DOM
/// (AutoRun). The cheap way to "see" the page without a screenshot.
pub struct BrowserSnapshotTool(pub(crate) Ctx);

#[async_trait]
impl Tool for BrowserSnapshotTool {
    fn name(&self) -> &str {
        "browser_snapshot"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn available(&self) -> bool {
        self.0.inspection.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "browser_snapshot",
            "Return the HTML/text content of the LIVE Browser tab's child webview (a native \
             child WebView2 — the top-level page, not an iframe) as a text dump — the cheap way \
             to read the live DOM without a screenshot. When the Browser tab is NOT open, falls \
             back to the app's own UI so you can read the app itself. Read-only. Requires a debug build.",
            json!({ "type": "object", "properties": {} }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }
    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        match self.0.manager.webview_snapshot().await {
            Ok(html) => ToolResult::success(html),
            Err(e) => ToolResult::error(format!("browser_snapshot failed: {e}")),
        }
    }
}

/// `browser_navigate` — navigate the Browser tab's child webview to a URL
/// (NeedsApproval: navigates the visible tab the human is looking at).
///
/// The URL is normalized through the same scheme allow-list + omnibox
/// autodetection as `offscreen_browser_navigate`, so `google.com` loads
/// `https://google.com` and `javascript:` is rejected. `file://` URLs are
/// allowed for local HTML debugging. The
/// navigation drives the child webview directly via CDP `Page.navigate` (the
/// child is a separate top-level webview, not an iframe in the app's DOM).
/// Requires a debug build (the CDP port is dev-only).
pub struct BrowserNavigateTool(pub(crate) Ctx);

#[async_trait]
impl Tool for BrowserNavigateTool {
    fn name(&self) -> &str {
        "browser_navigate"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn available(&self) -> bool {
        self.0.inspection.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "browser_navigate",
            "Navigate the Browser tab's child webview (a native child WebView2) to a URL. The \
             URL is normalized (scheme-less hostnames like google.com get https://; http:// for \
             localhost/IPs) and the scheme allow-list (http, https, data, file) is enforced — \
             file:// is allowed for local HTML debugging; javascript: and other schemes are \
             rejected. The human sees the navigation immediately. Requires a debug build (the \
             CDP port is dev-only).",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "The URL to navigate the child webview to." }
                },
                "required": ["url"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            url: String,
        }
        let args = parse_args!(Args, args);
        match self.0.manager.webview_navigate(&args.url).await {
            Ok(landed) => ToolResult::success(format!("navigated the Browser tab to {landed}"))
                .with_data(json!({ "url": landed })),
            Err(e) => ToolResult::error(format!("browser_navigate failed: {e}")),
        }
    }
}

/// `browser_click` — click an element by CSS selector in the Browser tab's
/// child webview (NeedsApproval: mutates the visible page). The child webview
/// IS the top-level page (no iframe), so the click dispatches against its main
/// frame directly — no iframe offset (resolves the old coordinate-offset bug).
/// Use `browser_snapshot` or `browser_screenshot` to find selectors first. Requires a
/// debug build (the CDP port is dev-only).
pub struct BrowserClickTool(pub(crate) Ctx);

#[async_trait]
impl Tool for BrowserClickTool {
    fn name(&self) -> &str {
        "browser_click"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn available(&self) -> bool {
        self.0.inspection.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "browser_click",
            "Click the element matching a CSS selector in the Browser tab's child webview (a \
             native child WebView2 — the top-level page, not an iframe). The click is dispatched \
             via CDP input (a real mousePressed + mouseReleased at the element's center in the \
             child's main frame — no iframe offset), so it triggers the same handlers a human \
             click would. Use browser_snapshot or browser_screenshot to find selectors first. Requires \
             a debug build (the CDP port is dev-only).",
            json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector of the element to click (in the child webview)." }
                },
                "required": ["selector"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            selector: String,
        }
        let args = parse_args!(Args, args);
        match self.0.manager.webview_click(&args.selector).await {
            Ok(()) => ToolResult::success(format!("clicked '{}'", args.selector)),
            Err(e) => ToolResult::error(format!("browser_click failed: {e}")),
        }
    }
}

/// `browser_type` — focus an element by CSS selector in the Browser tab's
/// child webview and type text into it (NeedsApproval: mutates the visible
/// page). The child webview IS the top-level page (no iframe), so focus + type
/// dispatch against its main frame directly — no iframe offset. Requires a
/// debug build (the CDP port is dev-only).
pub struct BrowserTypeTool(pub(crate) Ctx);

#[async_trait]
impl Tool for BrowserTypeTool {
    fn name(&self) -> &str {
        "browser_type"
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Browser
    }
    fn deferred_group(&self) -> Option<&'static str> {
        Some("browser")
    }
    fn available(&self) -> bool {
        self.0.inspection.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "browser_type",
            "Focus the element matching a CSS selector in the Browser tab's child webview (a \
             native child WebView2 — the top-level page, not an iframe) and type text into it. \
             The element is focused via a CDP click, then the text is inserted via the CDP \
             Input.insertText domain (one call, not per-character). Requires a debug build (the \
             CDP port is dev-only).",
            json!({
                "type": "object",
                "properties": {
                    "selector": { "type": "string", "description": "CSS selector of the input to type into (in the child webview)." },
                    "text": { "type": "string", "description": "The text to type." }
                },
                "required": ["selector", "text"]
            }),
        )
    }
    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }
    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        #[derive(Deserialize)]
        struct Args {
            selector: String,
            text: String,
        }
        let args = parse_args!(Args, args);
        match self
            .0
            .manager
            .webview_type(&args.selector, &args.text)
            .await
        {
            Ok(()) => ToolResult::success(format!(
                "typed {} chars into '{}'",
                args.text.chars().count(),
                args.selector
            )),
            Err(e) => ToolResult::error(format!("browser_type failed: {e}")),
        }
    }
}

// ---- Phase E: screenshot → vision loop ----

#[cfg(test)]
mod vision_tests {
    use super::*;
    use crate::provider::vision::ImageDescriber;
    use crate::tool::agent::image_tools::describe_image_file;
    use tempfile::tempdir;

    /// Build a tool context over a temp sandbox with a fresh browser manager
    /// (duplicated from the tool tests so this module is self-contained).
    fn vctx(dir: &std::path::Path) -> Ctx {
        Ctx {
            manager: BrowserManager::new(),
            sandbox: Sandbox::new(dir).unwrap(),
            inspection: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    /// A mock vision model that records the data URL it was given.
    struct RecordingDescriber;

    #[async_trait]
    impl ImageDescriber for RecordingDescriber {
        async fn describe_image(
            &self,
            image_url: &str,
            _prompt: &str,
        ) -> crate::error::Result<String> {
            Ok(format!("described {} bytes", image_url.len()))
        }
    }

    /// The debugging payoff: an `offscreen_browser_screenshot` PNG path must feed straight
    /// into `describe_image` (both are sandbox-relative) so the vision model can
    /// "see" the page. This guards the two tools' contract from drifting.
    #[tokio::test]
    async fn screenshot_path_feeds_describe_image() {
        let dir = tempdir().unwrap();
        let c = vctx(dir.path());
        OffscreenNavigateTool(c.clone())
            .execute(json!({"url": "data:text/html,<h1>vision</h1>"}))
            .await;
        let shot = OffscreenScreenshotTool(c.clone());
        let res = shot.execute(json!({})).await;
        assert!(res.success, "screenshot failed: {}", res.output);
        let rel = res.data.as_ref().unwrap()["path"].as_str().unwrap();

        // Feed the sandbox-relative path to describe_image with a mock vision model.
        let vision = RecordingDescriber;
        let description = describe_image_file(&c.sandbox, &vision, rel, Some("What is shown?"))
            .await
            .expect("describe_image should accept the screenshot path");
        assert!(description.starts_with("described "), "got: {description}");
        c.manager.close().await.unwrap();
    }
}
