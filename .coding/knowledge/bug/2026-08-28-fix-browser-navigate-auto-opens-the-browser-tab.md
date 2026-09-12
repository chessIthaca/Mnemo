+++
title = "Fix: browser_navigate auto-opens the Browser tab + readable browser tool cards — MERGED"
supersedes = "5ae26d22"
created = "2026-08-28"
+++

BUG (plan 5ae26d22) — MERGED into main at a8a78e3 (2026-09-09, merge_to_main skill), branch wt/agenticcoder deleted, not pushed. Symptom: (1) browser_navigate fails "the child WebView2 has no page target to attach to — is the Browser tab open?" when the tab has never loaded a URL — the agent cannot open the tab itself; the user had to manually type a URL first. (2) Browser tool calls render in chat with no human-readable summary — browser_navigate shows no URL, browser_click no selector, browser_screenshot's PNG inline (already works via ToolImage) is the only visual; cards read "browser_navigate ✗" with raw JSON hidden behind expand. Regression: webview_navigate_auto_ensures_missing_child.
