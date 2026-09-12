## Verdict: FINDINGS (0 high, 1 low)

Review of all uncommitted changes for plan 89daef6f "Safeguard the WebView2 CDP port against multiple app instances" (kind: bug_fixing, backlog 55ba23b1). The fix is correct, minimal, and faithful to the plan; every design decision called out for scrutiny holds up. One low documentation-sync finding: five in-code comment/doc sites still describe the CDP port as a fixed 9222.

**Changed files reviewed:** src/webview_args.rs, src-tauri/src/main.rs, src/browser/mod.rs, .coding/browser-debugging.md, frontend/src/components/settings/sections/AdvancedSection.tsx, .coding/backlog.jsonl (bookkeeping only: status pending→in_flight + plan_id/note — expected for an in-flight item), plus the untracked plan .coding/plans/89daef6f.md.

---

## Finding L1 (low) — stale "9222" wording in five in-code comment/doc sites

The user-facing docs were updated correctly (.coding/browser-debugging.md:110-112, AdvancedSection.tsx:529 — security warning text preserved verbatim, only the port number became dynamic). But these in-code sites still state the port is fixed 9222:

1. **src/config/general.rs:110-118** — doc comment on the user-facing `enable_browser_inspection` config field: "exposes the WebView2 Chrome DevTools Protocol on `localhost:9222`". Now 9222-or-next-free. Most user-facing of the five (this documents a config field users read).
2. **src-tauri/src/main.rs:167** — block comment directly above the changed code: "Expose the WebView2 Chrome DevTools Protocol on port 9222 so the agent can attach".
3. **src-tauri/src/main.rs:233** — setup-closure comment: "its WebView2 env binds the CDP port (9222)".
4. **src/browser/mod.rs:1224** — `is_app_url` comment: "Hardcoding 5179 mirrors the hardcoded 9222 CDP port". The mirror-constant rationale still holds (5179 ↔ `DEFAULT_CDP_PORT`), but 9222 is no longer hardcoded.
5. **src/browser/mod.rs:320** — `new_with_webview_url` doc: "ephemeral port instead of the real port 9222" ("the default port 9222" is what is now accurate).

No behavior impact — comment/doc text only. Suggested fix: one-line rewording each, e.g. "port 9222" → "the CDP port (9222 by default; the next free port when several instances run)". README.md, PLAN.md, tauri.conf.json, and docs/ contain no other stale 9222 references (verified by search).

---

## Design decisions — all verified

- **`pick_cdp_port` semantics — correct.** `DEFAULT_CDP_PORT..DEFAULT_CDP_PORT + 10` is an exclusive-end range = 9222..=9231 (10 ports, matching the plan). Probe binds `127.0.0.1:<port>` and drops — correct host (Chromium's remote-debugging bind is loopback-only) and std sets no SO_REUSEADDR, so the probe is a true exclusivity test. Ephemeral fallback via `bind(("127.0.0.1", 0))` → `local_addr().port()`; last resort `DEFAULT_CDP_PORT` only if even bind(:0) fails. Port 0 is never returned (the OS assigns a real port on bind(:0); the test asserts it). An ephemeral assigned port landing inside 9222..9231 would still be fine — it was just proven bindable.
- **main.rs gate — correct.** `Some(port)` exactly when `cfg!(debug_assertions) || browser_inspection_enabled()` (gate text unchanged); `set_cdp_port(port)` is called before `build_webview2_args(cdp_port, …)`; the env var is set (line 213) before `tauri::Builder::default()` (line 217). The block stays `#[cfg(windows)]` — the sanctioned WebView2 exception.
- **OnceLock over explicit threading — reasoning holds.** Set-before-read is guaranteed: main's env block runs before the builder → before setup → before `build_brain` → before any `BrowserManager::new()`/`Default` (the startup `sweep_orphan_profiles` is a static fn, constructs nothing). Skipping the round-trip test is justified and documented in the accessor docs: a test setting the process-global `OnceLock` would poison parallel tests constructing managers that expect the default. Tests never set it → `cdp_port()` = 9222 → zero test-behavior change; `new_with_webview_url` (test ctor) is untouched, so launched-Chromium stand-in tests are unaffected.
- **`is_app_url` needs no change — claim verified.** src/browser/mod.rs:1219-1230 matches only `tauri://`, `http(s)://tauri.localhost`, `http://localhost:5179`, `about:blank`, `chrome://` — it never special-cased 9222. The existing test at :1992 (`!is_app_url("http://localhost:9222")`) remains valid for any port. The backlog-text correction in the plan is accurate.
- **Non-Windows / CDP-off — unchanged.** When the gate is off, no port is picked, `cdp_port()` returns 9222, and `webview_url` is never used because `ensure_webview` fails fast on `webview_enabled == false` before probing. On non-Windows the cfg(windows) block doesn't run — same as before the fix (pre-existing behavior, not a regression).
- **Probe-then-bind TOCTOU — acceptable.** Documented in `pick_cdp_port`'s doc comment; the residual window is milliseconds and the failure mode degrades to exactly the pre-fix behavior (second env's bind fails silently). Strictly better than the status quo ante, which collided 100% of the time.
- **Multi-platform neutrality — clean.** `TcpListener`/`OnceLock`/`format!` are std and cross-platform; all lib-side changes compile everywhere; the only cfg(windows) code is the pre-existing env-var block.
- **Security — no new exposure.** The endpoint remains an unauthenticated localhost port behind the identical opt-in gate; only the number becomes dynamic. tauri.conf.json carries no hardcoded `--remote-debugging-port` (verified). Warning text preserved in both doc and UI.

## Bug-plan checks

- **Regression tests exercise the changed paths.** `cdp_port_avoids_an_occupied_default` (holds 9222, picks, builds args, parses the port back, asserts ≠ 9222 and verbatim carry-through) and `pick_cdp_port_returns_bindable_port` (re-bindable, ≠ 0, with a documented retry loop for the sibling-probe transient) both target the new code; `args_all_combinations` keeps the exact legacy strings for `Some(9222)` and adds a `Some(9227)` dynamic case. Red→green claim is consistent with the old code always emitting 9222.
- **Root cause documented** in the plan (§Context + §Bug), including the is_app_url correction.
- **Caller surface complete.** `build_webview2_args` has exactly one production caller (src-tauri/src/main.rs:209, via the code graph) plus the in-file tests — the `Option<u16>` signature change is fully covered; no other call sites exist.
- **Constitution.** Doc comments on every new public item (`DEFAULT_CDP_PORT`, `pick_cdp_port`, `set_cdp_port`, `cdp_port`) and the private static; no `#[allow]` suppressions; no shell-based file mutation in the diff (file-tools-first respected).

## Verification notes

Reviewer is read-only — test suites were not re-executed. The implementer's evidence (cargo test full workspace green, 2274+16 passed, 0 failed, warning-free under `#![deny(warnings)]`; frontend vitest 1087 tests green) is consistent with the diff: every call site of the changed signature is updated, and the new tests' concurrency interactions (shared 9222/9223 between the two port tests) are handled by the `.ok()` hold and the retry loop. The two-instance acceptance (each instance attaches only to its own WebView2) remains an inherently two-process manual check; the unit tests prove the pieces (distinct port under occupation, env-var carry-through, single source of truth for BrowserManager).
