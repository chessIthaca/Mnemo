+++
title = "CDP port is per-instance — pick_cdp_port + OnceLock single source of truth"
created = "2027-01-11"
+++

The WebView2 CDP remote-debugging port is PER-INSTANCE, not fixed (plan 89daef6f / commit d01e098, backlog 55ba23b1):

1. **Allocation**: `pick_cdp_port()` (src/webview_args.rs) probes 9222..=9231 on 127.0.0.1 (bind a TcpListener, drop it), then any ephemeral port, last resort DEFAULT_CDP_PORT. A single instance keeps the documented 9222; a second instance gets the next free port — its WebView2 env no longer fails to bind silently and its `ensure_webview` no longer attaches to the FIRST instance's WebView2.
2. **Single source of truth**: `static CDP_PORT: OnceLock<u16>` in webview_args.rs — `set_cdp_port` called from main.rs's `#[cfg(windows)]` block BEFORE the Tauri builder (set-before-read guaranteed: no BrowserManager exists yet); `cdp_port()` (default 9222) is read by `BrowserManager::new`/`Default` for `webview_url` and by `build_webview2_args(Some(port), occlusion)` for the env var. No round-trip unit test for the accessors (a test setting the OnceLock would poison parallel tests) — startup-only by construction.
3. **Accepted TOCTOU**: a tiny probe-then-bind window remains (two instances launching within milliseconds could still collide) — documented in pick_cdp_port's doc comment, deemed acceptable.
4. **Unchanged**: the security gate (`cfg!(debug_assertions) || enable_browser_inspection`), the unauthenticated-localhost warning, and `is_app_url` (it never special-cased 9222 — the backlog item's claim was wrong; it matches only tauri:// schemes, the 5179 dev devUrl, about:blank, chrome://).

Regression tests: `cdp_port_avoids_an_occupied_default` + `pick_cdp_port_returns_bindable_port` (src/webview_args.rs). Related: DECISION "browser profile lifecycle — watchdog + marker invariants" (same module family); reviews .coding/reviews/2026-09-12-89daef6f-cdp-port-multi-instance-review.md + -round2.md.
