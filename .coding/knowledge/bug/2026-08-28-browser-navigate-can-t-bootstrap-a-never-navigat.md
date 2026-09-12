+++
title = "browser_navigate can't bootstrap a never-navigated Browser tab (lazy child webview)"
created = "2026-08-28"
status = "superseded"
+++

BUG (plan 5ae26d22): browser_navigate fails "the child WebView2 has no page target to attach to — is the Browser tab open?" when the Browser tab was toggled on but never navigated; the agent cannot open the tab itself. Root cause: the child WebView2 is created lazily by frontend BrowserView.openUrl() → browser_webview_ensure; the agent-side BrowserManager (src/browser/mod.rs) only ATTACHES over CDP (webview_page → select_child_target) — no creation path exists outside the Tauri layer. Fix: child_ensurer hook (set_child_ensurer) injected by the app layer; webview_navigate invokes it when no child target exists (creates child at the stored rect + emits browser://reveal), then re-polls; click/type/eval error text now points at browser_navigate. Regression: webview_click_without_child_says_to_navigate_first (+ webview_navigate_auto_ensures_missing_child).
