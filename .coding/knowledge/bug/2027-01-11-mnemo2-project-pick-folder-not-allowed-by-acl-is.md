+++
title = "Mnemo2 \"project_pick_folder not allowed by ACL\" — isTauriRuntime too loose"
created = "2027-01-11"
+++

Symptom: clicking "open folder" in Mnemo2 printed `Command project_pick_folder not allowed by ACL`.

Root cause (verified by reading Tauri 2.11.5 src/webview/mod.rs:1819-1823 `if (plugin_command.is_some() || has_app_acl_manifest || !is_local)`): app commands are only ACL-checked when the page origin is non-local (or the app defines its own ACL manifest — Mnemo2 does not, there is no src-tauri/permissions). `mnemo2/src/lib/runtime.ts::isTauriRuntime` only tested `"__TAURI_INTERNALS__" in window`, which is TRUE inside Mnemo1's Browser-tab child WebView2 (a Tauri webview on a non-local origin, 127.0.0.1:5180 vs Mnemo1's devUrl localhost:5179). So Mnemo2's dev UI viewed there invoked `project_pick_folder` against Mnemo1's IPC, which never registered it, and the non-local origin triggered the ACL denial. The native Mnemo2 window (local devUrl) was never the failing path.

Fix: `runtime.ts` gained `probeMnemo2Host()` which invokes the Mnemo2-only handshake `mnemo2_host_ping` and returns false on any rejection. `store.ts` now gates loadProjects / openFolder / connectTeam on that probe and falls back to the explicit "needs the native shell" notice. The lead path already probed this way.

Regression test: `mnemo2/src/lib/store.host.test.ts` — "refuses the folder picker on a foreign Tauri host" asserts project_pick_folder is never invoked and the native-shell toast shows; the sibling test proves the real host still opens the folder. Full gates after the fix: Mnemo2 16 test files pass, svelte-check 0/0, build + check:no-mock pass, frontend 94 files pass.

Pointer: mnemo2/src/lib/runtime.ts, mnemo2/src/lib/store.ts, commit on wt/mnemo (Phase 2 step 8).
