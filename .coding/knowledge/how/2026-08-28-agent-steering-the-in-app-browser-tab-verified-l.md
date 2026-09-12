+++
title = "agent steering the in-app Browser tab (verified) + auto-bootstrap"
created = "2026-08-28"
+++

VERIFIED LIVE (2026-09-09, Complete state): the plan-bb8a0ca9 fix works — browser_navigate/click/type/eval execute in every workflow state and fully steer the user-visible Browser tab's child WebView2. End-to-end: snapshot → navigate example.com→info.cern.ch → eval JS (read location/title/h1) → click link → navigation followed. GOTCHA NOW FIXED (plan 5ae26d22): the lazy-create chicken-and-egg is gone — browser_navigate bootstraps the tab itself (BrowserManager child_ensurer hook → browser_webview_ensure_for_agent creates the child at the stored rect + emits browser://reveal → frontend reveals the tab). browser_click/type/eval still require an existing child (their error says "call browser_navigate first"). browser_click has NO app-UI fallback (snapshot/screenshot fall back to app UI when the tab is closed).
