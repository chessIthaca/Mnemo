+++
title = "the Browser tab URL box tracks the child webview via browser://url-changed — MERGED into main (5a86e9d)"
supersedes = "2027-01-11-the-browser-tab-url-box-tracks-the-child-webview"
created = "2027-01-11"
+++

MERGED into main at 5a86e9d (5a86e9d1157a497a8f6fdc567c29d584dda774eb) on 2027-01-24 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 33ad6ec; merge-fix 9d7cfbc: the on_page_load closure reads payload.url() — tauri 2.11.5 hands the closure (Webview, PageLoadPayload)) — supersedes this record's earlier unmerged marker. WHAT: the shared child_webview_builder attaches wry's on_page_load hook emitting browser://url-changed (object payload via browser_url_payload) on every page load; a module-scope listener writes store.browserUrl (no mount-timing gap); BrowserView syncs the box on change and mount. Knowledge file: .coding/knowledge/spec/2027-01-11-the-browser-tab-url-box-tracks-the-child-webview.md.
