+++
title = "React #185 render loop on startup — zustand 5 dropped useSyncExternalStoreWithSelector (fix a55ed24)"
created = "2027-01-11"
+++

Symptom: "Minified React error #185" on app startup (the ErrorBoundary surfaced it as "UI render error"), appearing after the zustand 4.5.7 -> 5.0.15 bump. Root cause: zustand 5 removed the useSyncExternalStoreWithSelector wrapper that its v4 useStore used, so selectors returning fresh objects/arrays re-rendered in a loop. Fix: the app store is built with createWithEqualityFn from zustand/traditional (commit a55ed24, PR #58 / wt/fix-zustand-snapshot). The loop was diagnosed by attaching to the running WebView2 over CDP — recipe in .coding/knowledge/how/2026-09-29-read-the-running-app-webview-console-via-cdp.md (post-fix console is clean). Frontend suite green after (92 files / 1314 tests then; 1319 now).
