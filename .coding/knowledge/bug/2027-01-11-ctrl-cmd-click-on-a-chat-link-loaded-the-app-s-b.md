+++
title = "ctrl/cmd-click on a chat link loaded the app's Browser tab — escape hatch now sampled at press time (plan 8d1b8857)"
created = "2027-01-11"
+++

SYMPTOM (user report 2026-10-01): in the built app, ctrl/cmd-click on an http link in the chat loaded the URL in the app's own right-panel Browser tab instead of the OS default browser. Plain click was correct (that IS the tab), middle-click kept working.

ROOT CAUSE: the escape hatch was decided from the CLICK event's flags alone — `{ osBrowser: e.ctrlKey || e.metaKey }` in frontend/src/components/chat/MarkdownLink.tsx (the markdown anchor) and Message.tsx (the web_fetch URL chip). The engine decides a ctrl/middle-click gesture itself: WebView2 raises NewWindowRequested, and wry surfaces it to the host only through `new_window_req_handler` (vendor/wry/src/webview2/mod.rs:766-778), which this app never registers — so the click can arrive with the modifier already consumed. The hatch was lost, the click took the plain-click branch, and the store's requestBrowserOpen revealed the tab (the tab has exactly ONE trigger: openChatLink's non-osBrowser path, so the symptom proves the modifier read false). Middle-click survived because it never reads a modifier: MarkdownLink.tsx's onAuxClick hardcodes `osBrowser: true`.

FIX (plan 8d1b8857, branch wt/mnemo): NEW frontend/src/lib/linkGesture.ts — `noteLinkPress(e)` samples `ctrlKey || metaKey || button === 1` on EVERY press (mousedown precedes the engine's decision) and `takeLinkPress()` consumes it once per click. Both surfaces now wire `onMouseDown={(e) => { if (noteLinkPress(e)) e.preventDefault(); }}` (the preventDefault suppresses the engine's own gesture) and `onClick` reads `takeLinkPress() || e.ctrlKey || e.metaKey`, so it is correct whether or not the click carries the modifier. The record is module-scope rather than a per-element ref because Message.tsx renders the chip inside a `.map` and cannot own a per-chip ref.

TESTS: frontend/src/lib/linkGesture.test.ts (6 node-env cases, incl. the exact symptom — ctrl press followed by a modifier-less click resolves to the OS browser — plus consume-once/re-arm/stale-overwrite) and the source contracts `escape-hatch press wiring` in frontend/src/components/chat/MarkdownLink.test.tsx. RED before the wiring (2 contracts failed, exit 1) → GREEN after (30/30 targeted; full frontend suite 1329 tests exit 0; `npm run build` exit 0; root `cargo test` exit 0).

INVARIANT for future link work: never let the escape hatch depend on the click event's modifier flags — sample the PRESS (it is the seam the engine cannot consume), and keep the click flags only as a fallback.
