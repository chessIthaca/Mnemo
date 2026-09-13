## Verdict: PASS

Reviewed plan 5ce69497 "Second mnemo instance shows blank white window — per-instance WebView2 user data folder" (bug_fixing, all 4 steps complete, in Reviewing) on branch wt/mnemo. Scope: git diff HEAD (15 modified files) + all untracked files (webview_udf.rs, instance_marker.rs, tests/webview_udf.rs, InstanceConflictDialog.tsx + test, plan + knowledge + analysis + browser-debugging.md). All 8 review asks checked; zero findings.

Full analysis in the sections below.

## 1. Mechanism correctness

### UDF isolation (mutex primary/secondary)
- `install_default_data_dir` (webview_udf.rs:35) runs at main.rs:254 — inside the window-build block, **before** the first WebviewBuilder (main.rs:258). The two child-webview sites (browser_webview.rs:336, :425) are created lazily on demand, strictly after startup. So the folder is decided before ANY webview exists.
- OnceLock semantics hold: `set_webview_data_dir` has exactly one caller (install_default_data_dir, on the secondary path); the primary never calls it, so `webview_data_dir()` stays `None` for the whole process — never set mid-run, never flipped. `apply` reads the same static every time → all webviews of one instance share one profile. ✓
- `acquire_default_profile_mutex` (webview_udf.rs:77): kernel-serialized `CreateMutexW`; fresh acquire = primary (passes through → SDK default UDF), `ERROR_ALREADY_EXISTS` = secondary (per-pid dir). Handle deliberately leaked (`Box::leak`) so the mutex object survives the process lifetime — later instances observe it. Failure degrades to primary/default (documented best-effort; never blocks startup). ✓
- Builder-wrap coverage: repo-wide search for `WebviewBuilder::new` finds exactly the 3 production sites (main.rs:259, browser_webview.rs:336, :425), each wrapped with `webview_udf::apply(` — no site missed. ✓
- `mutex_name_for` (webview_args.rs) is deterministic per default-UDF path (DefaultHasher over the path), which is per app identity; the `Local\` session namespace keeps different logon sessions apart. Legal mutex name (no separators — asserted in unit test). ✓

### Marker / conflict path
- Ordering: main.rs reads the incumbent (`conflict_for` → `read_marker`) BEFORE `InstanceMarker::new_self().write(&backlog_root)` — a second instance sees the first instance's marker. Both best-effort (`let _ =` write). ✓
- `conflict_for` (instance_marker.rs:69): absent marker → None; own pid → None (crash re-launch); dead pid → None (stale); live other pid → conflict. Covered by unit tests with injected predicate. Marker path uses `Project::CODING_DIR_NAME` (".coding") — consistent with the rest of the project. ✓
- Liveness probe (`instance_pid_alive`, startup.rs): Windows `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` by **exact pid** — name-less, so it cannot match the host or any unrelated process. The host-match impossibility holds rigorously: a marker whose pid equals the current process's pid is the own-pid case, which `conflict_for` excludes before the probe runs. A recycled pid pointing at an unrelated live process yields a spurious warning only — user-dismissable, benign, same residual class as the documented LWW race. No dangerous match is possible. An access-denied OpenProcess returns false → warning suppressed (false negative on a warning-only feature — acceptable). Non-Windows uses `ps -p <pid>` once per launch; no new dependency. ✓
- Conflict rides the existing startup snapshot: `instance_conflict` added to `StartupSnapshot` (startup.rs:28), computed once into `AgentRuntimeContext` (state.rs:148), surfaced at startup.rs:152. Frontend tolerates older backends (`snap.instance_conflict ?? null`). ✓

## 2. Regression tests

- **Genuinely fails without the fix:** on the pre-fix tree there is no `webview_udf.rs` and no `webview_udf::apply(` substring anywhere — `agent_chat_builder_uses_per_instance_data_dir` and `child_webview_builders_use_per_instance_data_dir` panic (missing needle), `webview_udf_module_declares_the_contract` panics (missing file), `lib_helpers_declared_in_webview_args` fails (four needles absent from the then-current webview_args.rs). All four RED on HEAD, GREEN with the fix. ✓
- **Coverage:** all three builder sites asserted (main.rs agent-chat + both child sites) with the wrap-position-before-`add_child` check; both pub helper names (`apply`, `install_default_data_dir`) plus all four lib helpers (`set_webview_data_dir`, `webview_data_dir`, `secondary_webview_data_dir`, `mutex_name_for`). Windows-only named-mutex round-trip test is deterministic in-process (second `CreateMutexW` → ERROR_ALREADY_EXISTS). ✓
- **Frontend:** InstanceConflictDialog.test.tsx (renderToStaticMarkup contract + `?raw` source-contract) **is registered** in frontend/vitest.config.ts include list. ✓
- Reported matrix green: cargo test root 298 passed / exit 0 (4 in tests/webview_udf.rs bin), `npx tsc --noEmit` exit 0, frontend npm test 1,104 passed / 81 files / exit 0, `#![deny(warnings)]` ⇒ warning-free build. ✓

## 3. Security

- Only `pid: u32` + `started_at: u64` ride the existing snapshot — no project path or file content crosses IPC (the marker JSON is parsed into typed serde structs backend-side; the dialog receives and renders numbers only). `{conflict.pid}` and `{launched}` are JSX text (React-escaped; `toLocaleTimeString` of a numeric value) — no HTML injection path. ✓
- CDP gate untouched: the diff to browser_webview.rs is builder-wrapping only; `enable_browser_inspection` / `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` handling is not modified. Opt-in status preserved. ✓
- Per-pid dir component `mnemo-{pid}` is a decimal u32 — path-safe (no separators); base is LOCALAPPDATA + the config identifier (trusted). ✓

## 4. Multi-platform neutrality

- All Windows-only APIs cfg-gated: mutex body + test cfg(windows) (webview_udf.rs), `install_default_data_dir` body cfg(windows) (no-op elsewhere), probe cfg(windows)/cfg(not(windows)). `apply`'s body is platform-neutral (calls the tauri `data_directory` builder API — WKWebsiteDataStore-appropriate on macOS — and passes through when no dir is recorded). ✓
- lib helpers (`webview_args.rs` OnceLock + path + name helpers, `instance_marker.rs`) are pure Rust, no Windows API/path. The dialog is plain React — no Windows assumption. ✓
- Concrete `WebviewBuilder<tauri::Wry>` signature (the plan sketch said `<R: tauri::Runtime>`) — all three call sites use the default Wry runtime in an app that is Wry-only; compiles identically on both platforms. Doc'd deviation, not a defect. macOS compile-time safety confirmed: `install_default_data_dir(_app, _pid)` has underscore-prefixed unused params and an empty non-Windows body — no warnings under deny(warnings). ✓

## 5. Documentation sync

README.md highlight row, docs/FEATURES.md browser bullet, PLAN.md "Multi-instance WebView2 profiles" row, and .coding/browser-debugging.md "Multi-instance notes" section all match the shipped code (primary keeps `%LOCALAPPDATA%\com.mnemo.app\EBWebView`, secondary `...\WebView2\mnemo-<pid>`, mutex decision, same-project warning, `custom-protocol` debug-build gotcha). Module docs in webview_udf.rs / instance_marker.rs / webview_args.rs document the invariants. Cosmetic only: two doc files place the code-span backtick as `` `%LOCALAPPDATA%`\com.mnemo.app `` (backslash outside the span) — content accurate, no finding.

## 6. File-tools-first

No shell-based file mutation in the change set: all source/doc edits are proper file-tool edits; .coding/analysis/* are runtime evidence captures; .coding/knowledge + .coding/plans are the sanctioned memory/plan writers; .coding/backlog.jsonl is backlog_add bookkeeping (explicitly not a finding per the review brief).

## 7. Not-findings respected

(a) First NEW instance white while a LEGACY release host holds the default UDF — by design (documented in the plan + browser-debugging.md), (b) .coding/analysis/* acceptance artifacts, (c) backlog.jsonl bookkeeping — all correctly treated as deliberate.

## 8. Bug-plan closing checks

- Regression test exercises the changed path: source-contract tests assert the exact builder sites + helper contract; mutex round-trip + marker unit tests + frontend dialog test complement. ✓
- Root cause documented: plan context (static analysis + live evidence), BUG knowledge file `.coding/knowledge/bug/2027-01-11-second-mnemo-instance-renders-a-blank-white-wind.md` (symptom → root cause → fix + test names + evidence pointers), module docs. ✓
- BUG memory written: `[semantic] BUG: Second mnemo instance renders a blank white window — shared default WebView2 user data folder` (id efb5c6e0-4e30-510e-81ba-18ddfdc65e7f) — present in the store, matching title/content. ✓
- "Landed design —" context amendment present (architecture, deviations, live evidence numbers, invariants) + `## Landed design: yes` flag. ✓

## Conclusion

The two mechanisms (per-instance UDF via named-mutex arbitration; same-project conflict warning via marker + exact-pid liveness probe) are correctly implemented, all builder sites wrapped, regression tests legitimately RED-without-fix, security surface minimal (numbers only, React-escaped, CDP gate untouched), platform-neutral where required, docs in sync, and the bug-plan closing requirements all met. **Verdict: PASS** — no findings.
