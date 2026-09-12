+++
title = "Browser tab reload is always a hard reload (CDP Page.reload ignoreCache)"
created = "2027-01-11"
+++

The Browser tab's Reload button always performs a hard reload that bypasses the HTTP cache (plan 71ab6646, 2027-01-11). Mechanism: the vendored wry 0.55.3 (vendor/wry, see PATCHES.md) patches the webview2 reload() to issue CDP Page.reload with {"ignoreCache":true} via ICoreWebView2::CallDevToolsProtocolMethod (the raw COM name — the "Async" suffix is only the .NET projection), falling back to plain Reload() when the call itself fails. Why vendored: WebView2 has no native hard-reload API, Chromium ignores location.reload(true), and tauri 2.11.5's Webview::reload exposes no hook — the same shape as the SSO patch. Blast radius: only the Browser tab's Reload button ever calls reload (the main + agent-chat webviews are never reloaded). Guard: src-tauri/tests/wry_hard_reload_patch.rs (source-contract — reload()'s primary path is the CDP call with Reload() only as the error fallback; fails on pristine wry 0.55.1). Renumber 0.55.2 → 0.55.3 (SSO patch unchanged; wry_sso_patch.rs renumber assertion updated). Frontend tooltip updated ("Reload the current page (bypasses cache)").
