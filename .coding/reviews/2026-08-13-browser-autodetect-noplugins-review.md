# Browser URL Autodetect + No-Plugins Hardening — Review

**Date:** 2026-08-13
**Reviewer:** read-only subagent
**Scope:** all uncommitted changes (`git diff HEAD`): `src/browser/mod.rs`, `src/tool/browser/mod.rs`, `src-tauri/tauri.conf.json`, `frontend/src/components/views/BrowserView.tsx`, `.coding/plans/stack.json`, `.coding/plans/b30a28b4-…md` (bookkeeping).

---

## Verdict: no blocking findings. Two low-priority, non-blocking observations.

### Correctness of `normalize_url` / `autodetect_scheme` — verified by tracing every branch

Traced each acceptance/rejection case in the table-driven tests against the code:

- **Passthrough** (`src/browser/mod.rs:679-680`): `http`/`https`/`data` prefix (case-insensitive via `to_ascii_lowercase` on the prefix only) returns `trimmed` unchanged. `HTTPS://EXAMPLE.COM` → candidate `"https"` matches → returned verbatim. `data:text/html,<h1>hi</h1>` → prefix `data` → verbatim. ✓
- **Scheme-like rejection** (`:685-690`): `file:`, `javascript:`, `about:`, `mailto:`, `C:\` all hit `is_scheme_like(candidate) && !rest.chars().all(is_ascii_digit)`. For each of these `rest` is non-digit → exact `"unsupported URL scheme '{other}' in '{url}' — only http, https, data are allowed"` preserved. The pre-existing `navigate_rejects_disallowed_schemes` asserts `contains("unsupported URL scheme")` — still satisfied. ✓
- **host:port autodetect** (`:727-731`): `example.com:8080` → candidate `example.com` is scheme-like but `rest="8080"` all digits → guard false → falls through. `rsplit_once(':')` splits port `8080` (digits) → host `example.com` → contains `.` → `https`. ✓ `example.com:8080abc` → port `8080abc` not all digits → `Some(_) => return None` → rejected. ✓ `localhost:abc` → `Some(_) => None` → rejected. ✓
- **IPv6** (`:722-725`): `[::1]:3000` → `split_once(':')` gives candidate `"["` — not allowed-scheme, `is_scheme_like("[")` is false (first char not alphabetic) → falls to autodetect; `authority` starts with `[` → `http`. ✓
- **IPv4** (`:739-742`): `127.0.0.1:8080`, `10.0.0.1` → all digits+dots containing a dot → `http`. ✓
- **localhost** (`:736-738`): `eq_ignore_ascii_case("localhost")` → `http`; `LOCALHOST:5173` works. ✓
- **Edge cases:** empty/whitespace-only → `"empty URL"` error (`:671-674`). Interior whitespace (`hello world`) → `autodetect_scheme` whitespace guard → `None` → rejected (`:714-717`). `example.com.` → `trim_end_matches('.')` yields `example.com` → `https`; output preserves the original trailing dot (test expects `https://example.com.`) because only the *scheme decision* uses the trimmed host, not the output string. ✓

### Security — allow-list is still the single choke point

Both entry points funnel through `BrowserManager::navigate` → `normalize_url`:
- Agent tool `browser_navigate`: `src/tool/browser/mod.rs:87` → `self.0.manager.navigate(&args.url)`.
- UI IPC `browser_open`: `src-tauri/src/ipc/browser.rs:96-103` → `state.runtime.browser.navigate(&url)`.

No caller passes a pre-validated URL around the check. `file:`/`javascript:`/`about:` are rejected before any CDP call, so they cannot reach Chromium. The `http`-for-IP-literals/localhost choice mirrors Chrome/Edge/Firefox omnibox behavior (these are loopback/intranet contexts where TLS is unusual); it does not widen the allow-list (still only `http`/`https`/`data` reach the browser) and introduces no SSRF channel that wasn't already reachable via an explicit `http://` URL. ✓

### Hardening knobs

- `src-tauri/tauri.conf.json:23` `"additionalBrowserArgs": "--disable-extensions"` — a valid Tauri v2 window key; `tauri.conf.json` is deserialized by `generate_context!` at compile time in the workspace member `src-tauri` (tauri 2.11.5, tauri-utils 2.9.3 per Cargo.lock). The plan records `cargo test` passed across the workspace, which is itself the schema validation — an unknown field would fail the build. ✓
- `src/browser/mod.rs:181` `.args(vec!["--disable-extensions".to_string()])` on the chromiumoxide 0.7.0 `BrowserConfig` builder. The comment correctly notes the fresh temp profile already prevents extension loading; this makes the policy explicit. ✓

### Constitution compliance

- `#![deny(warnings)]` at both crate roots (`src/lib.rs:1`, `src-tauri/src/main.rs:7`) unchanged; the plan's recorded green `cargo test` proves a warning-free build.
- **No `#[allow(...)]` suppressions added** — the only two in the tree (`src/agent/loop_impl.rs:191,230`, `clippy::too_many_arguments`) are pre-existing and untouched by this diff.
- All three new private fns have doc comments (`normalize_url` `:661`, `is_scheme_like` `:701`, `autodetect_scheme` `:711`); `BrowserManager::navigate`'s public doc updated (`:222-227`); module `//!` security paragraph updated (`:16-21`).
- Line endings: the diff introduces no `\r\n`; the `LF will be replaced by CRLF` git warnings are the standard autocrlf notice on checkout-touch, identical to what the surrounding files already produce, not a mixed-ending regression.
- Frontend change is placeholder text only (`BrowserView.tsx:146`); the plan records the typecheck ran (step 4).
- Bookkeeping (`.coding/plans/stack.json` + plan `.md`) is expected plan state, not a hidden source change.

### No unrelated changes hiding in the diff — confirmed. Five files, all in scope.

---

## Low-priority, non-blocking observations (no fix required)

**O1 — `chromiumoxide` `.args()` semantics could not be re-verified from this sandbox.**
The crate source (`C:\Users\carst\.cargo\registry\src\...\chromiumoxide-0.7.0\src\browser.rs`) is outside the project sandbox my read tools are confined to, so I could not re-confirm whether `BrowserConfig::args` *replaces* chromiumoxide's default arg set or *appends* to it. If it replaces, and if the defaults carried something load-bearing, passing only `--disable-extensions` could drop a default. Mitigating evidence: the implementer opened that exact file during the session, and the plan's recorded `cargo test` ran the Chromium-spawning browser tests to green — so the launched browser demonstrably works with this arg set. The risk is residual and low (the feature's tests exercise the real launch path), but it's the one claim in the change I could not independently re-derive from first principles. Flagging for transparency only.

**O2 — cosmetic: `normalize_url` error for scheme-less junk echoes untrimmed input.**
`:695` builds `"'{url}' is not a valid URL…"` from the original `url`, while the empty-URL error at `:673` also uses raw `url`. For input like `"  hello world  "` the message shows the padded form. Harmless (the test only asserts `is_err()`), purely cosmetic, and consistent with the pre-existing `validate_url` style that also interpolated raw `url`. Not a defect.
