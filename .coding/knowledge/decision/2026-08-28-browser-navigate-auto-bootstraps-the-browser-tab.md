+++
title = "browser_navigate auto-bootstraps the Browser tab (one-tool design)"
created = "2026-08-28"
status = "superseded"
+++

DECISION (plan 5ae26d22, merged to wt/agenticcoder 0868d14 + a0fde7c + d92c278, round-3 PASS): browser_navigate is the ONE agent tool that may bootstrap the Browser tab — its webview_page_auto_ensure invokes the app-layer ChildEnsurer hook (browser_webview_ensure_for_agent) when no child CDP target exists, creating the child at the stored rect + emitting browser://reveal (frontend revealRightPanelTab). Deliberately narrow: click/type/eval do NOT auto-create (never silently open a tab the user didn't ask for — their error guides to browser_navigate), and the read path keeps its app-UI fallback. Visibility: the created child preserves the frontend-reported tab_visible (shown immediately if the tab is already active; otherwise hidden until the reveal event selects the tab). The NeedsApproval gate on browser_navigate remains the user's consent mechanism for agent-initiated tab opening.
