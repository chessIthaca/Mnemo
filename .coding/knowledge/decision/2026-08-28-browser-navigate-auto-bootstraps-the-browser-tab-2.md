+++
title = "browser_navigate auto-bootstraps the Browser tab (one-tool design) — MERGED"
supersedes = "2026-08-28-browser-navigate-auto-bootstraps-the-browser-tab"
created = "2026-08-28"
+++

DECISION (plan 5ae26d22, MERGED into main at a8a78e3 2026-09-09, branch wt/agenticcoder deleted, not pushed): browser_navigate is the ONE agent tool that may bootstrap the Browser tab — its webview_page_auto_ensure invokes the app-layer ChildEnsurer hook (browser_webview_ensure_for_agent) when no child CDP target exists, creating the child at the stored rect + emitting browser://reveal (frontend revealRightPanelTab). Deliberately narrow: click/type/eval do NOT auto-create (never silently open a tab the user didn't ask for — their error guides to browser_navigate), and the read path keeps its app-UI fallback. Visibility: the created child preserves the frontend-reported tab_visible (shown immediately if the tab is already active; otherwise hidden until the reveal event selects the tab). The NeedsApproval gate on browser_navigate remains the user's consent mechanism for agent-initiated tab opening.
