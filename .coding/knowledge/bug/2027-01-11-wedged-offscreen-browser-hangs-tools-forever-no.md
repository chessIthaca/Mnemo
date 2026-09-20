+++
title = "wedged offscreen browser hangs tools forever — no CDP timeouts (plan ec425270)"
created = "2027-01-11"
+++

Symptom: offscreen_browser_* calls hang forever when the shared headless Chromium is wedged (alive process, open CDP, unresponsive renderer, e.g. infinite JS loop); the watchdog never reaps it. Root cause: no timeout on any launch-mode CDP round-trip in src/browser/mod.rs (navigate/list/close/switch/screenshot/snapshot/eval/click/type), and reap_dead_browser only fires on a finished handler task — a wedged browser keeps it alive. Fix (plan ec425270): per-op tokio::time::timeout wrapper (30s/60s) + force-reap + transparent respawn; watchdog Browser::version() liveness probe. Regression test: browser::tests::wedge_renderer_times_out_and_self_heals (cargo test --features browser wedge_renderer -- --ignored).
