# Review — CDP attach spike (interactive browser architecture)

**Scope:** All uncommitted changes in the working tree.
**Files changed:** `src/browser/mod.rs`, `src-tauri/tauri.conf.json`, `.coding/safety.toml`, `.coding/plans/stack.json` (+ 2 untracked plan `.md` files — bookkeeping, not reviewed as source).

**Plan goal:** De-risk the "interactive browser" architecture — prove `chromiumoxide::Browser::connect` attaches to a running Chromium over its HTTP CDP endpoint (enumerate targets, eval, screenshot), and verify Tauri 2 WebView2 honors `--remote-debugging-port` via `additionalBrowserArgs`.

---

## Findings

### SECURITY — HIGH: `--remote-debugging-port=9222` is always-on in the shipped app config

**File:** `src-tauri/tauri.conf.json:23`

```json
"additionalBrowserArgs": "--disable-extensions --remote-debugging-port=9222"
```

`tauri.conf.json` is the **production** config — `additionalBrowserArgs` applies to every build, including the bundled release app (`bundle.active = true`, `targets = "all"`). Enabling the Chrome DevTools Protocol on `localhost:9222` exposes the app's WebView2 to **any local process** on the machine:

- Full DOM inspection and arbitrary JS execution inside the app's webview (the same origin/trust as the app UI).
- Screenshot capture, network interception, and IPC observation of the agent harness.
- This is effectively local arbitrary code execution within the app context for any local user/process — including low-privilege malware or another user's process on a shared machine.

Chromium binds the debug port to loopback by default (mitigating remote attack), but loopback does **not** protect against other local processes, which is the threat model that matters for a desktop app handling prompts, code, and credentials. Chromium's own docs flag `--remote-debugging-port` as not for production use.

**This is a spike** — landing the arg unconditionally is fine for *verifying* WebView2 honors it, but it must not ship. Recommended fix before this leaves the feature branch:

- Gate the arg behind a debug/feature flag. Prefer setting it from Rust via `WebviewWindowBuilder::additional_browser_args(...)` conditioned on `cfg!(debug_assertions)` (or a `cdp-debug` cargo feature), so release builds never expose CDP.
- Or, at minimum, revert `tauri.conf.json` to `--disable-extensions` after manual verification and document the dev-only mechanism.

Leaving it always-on in `tauri.conf.json` is a security regression for production.

---

### CORRECTNESS — LOW: Hardcoded debug port 9333 in the test is a flakiness risk

**File:** `src/browser/mod.rs:972` (`.port(9333)`)

The test pins the launched Chromium to port 9333 with no fallback. If that port is already bound — e.g. a leftover Chromium from a previous failed/crashed run of this very test (the child is killed on Drop, but a crashed/killed process can briefly hold the socket), or any other local process — `Browser::launch` fails immediately at the `.expect("launch should succeed")` with a non-obvious bind error. There is no retry and no ephemeral fallback.

The rest of the suite uses `BrowserManager`, which does not pin a port, so this is the only test with a fixed port. Acceptable for a one-off spike, but if the test is kept long-term, consider `.port(0)` (ephemeral) and reading back the assigned port, or wrapping launch in a short retry. Not blocking for the spike.

---

### BUGS — INFO (no actual leak): Cleanup is success-path-only, but RAII covers the failure path

**File:** `src/browser/mod.rs:1046-1051`

The explicit cleanup (`drop(connected); connected_task.abort(); drop(launched); launched_task.abort();`) only executes when every prior `.expect()`/`assert` succeeded. On a panic mid-test, that block is skipped. **However, this is not a real resource leak**, because Rust's unwind-drop covers it:

- `launched` (`Browser`) owns the child process; its `Drop` kills Chromium (confirmed by the existing comment at `src/browser/mod.rs:482` "Dropping the browser kills the Chromium child process"). On unwind, `launched` drops → child killed.
- `profile` (`tempfile::TempDir`) drops on unwind → temp dir removed (subject to the same async file-lock release the `profile_dir_is_removed_on_close` test already polls for).
- `launched_task`/`connected_task` are `JoinHandle`s; dropping them *detaches* (does not abort) the task, but each task body is `while handler.next().await.is_some() {}`, which returns `None` once the owning browser is dropped and the CDP socket closes — so the tasks self-terminate shortly after the browser drops. No permanent task leak.

The explicit `abort()` calls are belt-and-suspenders for the success path and are harmless. No fix required; noted only to confirm the failure path was analyzed and is sound. (If hardening is desired, a `Drop` guard or `scopeguard`/`defer` could make the intent explicit, but it is not necessary for correctness.)

---

### CONSTITUTION COMPLIANCE — PASS (no findings)

- **Warning-free build:** No `#[allow(...)]` suppressions added anywhere. The test's local `use` statements (`src/browser/mod.rs:958-960`) shadow the module-level imports of `Browser`, `BrowserConfig`, `EvaluateParams` (`src/browser/mod.rs:31,33-34`) — shadowing is legal and produces no warning; `HeadlessMode` and `ScreenshotParams` are newly imported locally and both used. No unused imports. The test lives inside the existing `#[cfg(test)] mod tests`, so it adds zero code to non-test builds. `#![deny(warnings)]` at `src/lib.rs:1` is satisfied.
- **Public doc comments:** The test function is private (`#[tokio::test] async fn`); no doc comment required. It has an explanatory `///` doc comment anyway. No public API was changed.
- **Code style:** Matches the existing test style (`#[tokio::test]`, `.expect("…")` messages, inline comments). Consistent with `respawn_after_close` / `profile_dir_is_removed_on_close` neighbors.

---

### STYLE — INFO (not constitution-enforced): One line exceeds typical rustfmt width

**File:** `src/browser/mod.rs:994`

```rust
            tokio::time::timeout(std::time::Duration::from_secs(15), Browser::connect("http://localhost:9333"))
```

This line is ~107 columns, beyond rustfmt's default 100. `#![deny(warnings)]` does **not** enforce `cargo fmt`, so this does not break the build, but it would produce a `cargo fmt` diff. Minor; consider wrapping for tidiness (e.g. bind the duration to a `const` or split the `Browser::connect` arg onto its own line).

---

### BOOKKEEPING — INFO

- `.coding/safety.toml:138-141` adds a `cd;cargo build` `command_class` auto-approve rule. This is consistent with the existing `cargo build` (line 28) and `cd;cargo test` (line 39) rules — the classifier treats `cd;<cmd>` as a distinct class from `<cmd>`, so the new rule is needed and matches repo convention. No new risk beyond the already-broad `cargo build` auto-approval.
- `.coding/plans/stack.json` and the two untracked `.coding/plans/*.md` are plan bookkeeping, not source. Not reviewed for correctness.
- Git reports `LF will be replaced by CRLF` for `.coding/safety.toml`; this is a `.coding/` config file (not source code) and is git's autocrlf normalization notice, not a line-ending violation in source.

---

## Summary

| Severity | Count | Blocking? |
|---|---|---|
| Security (HIGH) | 1 | Yes — gate/revert `--remote-debugging-port` before release |
| Correctness (LOW) | 1 | No (spike) |
| Bugs (INFO) | 1 | No (RAII covers it) |
| Constitution | 0 | — |
| Style (INFO) | 1 | No |

The spike's *technical* goal is met and the test is sound. The one actionable item is **security**: `--remote-debugging-port=9222` must not ship unconditionally in `tauri.conf.json` — gate it behind `cfg!(debug_assertions)` / a feature flag, or revert it after verification.
