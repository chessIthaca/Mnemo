+++
title = "browser_navigate can't bootstrap a never-navigated Browser tab — MERGED"
supersedes = "2026-08-28-browser-navigate-can-t-bootstrap-a-never-navigat"
created = "2026-08-28"
+++

BUG: browser_navigate can't bootstrap a never-navigated Browser tab (lazy child webview) — MERGED into main at a8a78e3 (2026-09-09), branch wt/agenticcoder deleted, not pushed. Fix: child_ensurer hook (set_child_ensurer) injected by the app layer; webview_navigate invokes it when no child target exists (creates child at the stored rect + emits browser://reveal), then re-polls; click/type/eval error text now points at browser_navigate. Regression: webview_click_without_child_says_to_navigate_first + webview_navigate_auto_ensures_missing_child.
