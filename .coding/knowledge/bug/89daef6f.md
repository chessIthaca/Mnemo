+++
title = "Safeguard the WebView2 CDP port against multiple app instances"
created = "2027-01-11"
+++

Symptom: When two or more mnemo instances run at once — both live, both with the browser online (debug builds always expose CDP; release builds with the `enable_browser_inspection` opt-in) — they conflict on the web browser port. The CDP remote-debugging port is hardcoded 9222 (src/webview_args.rs::build_webview2_args ~line 30; src/browser/mod.rs::BrowserManager::new ~line 288 and Default ~line 1601). The first instance's WebView2 env binds 9222; the second's bind fails silently (a second env renders but gets no CDP — proven in .coding/scoping-native-mix-architecture.md), and the second instance's `ensure_webview` then attaches to the FIRST instance's WebView2 — cross-instance inspection/control of the wrong browser tab. · regression test: cdp_port_avoids_an_occupied_default

Full record for plan 89daef6f (see .coding/plans/89daef6f.md for the plan file).

regression test: cdp_port_avoids_an_occupied_default · path .coding/plans/89daef6f.md · branch wt/agenticcoding @ d01e098 (unmerged — exists only on this branch)
