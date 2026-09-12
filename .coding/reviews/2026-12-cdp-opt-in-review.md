# Review: Opt-in browser inspection (CDP) for release builds

**Scope:** All uncommitted changes on the current branch (10 files, +175/-22).
**Verdict:** The change is correct, secure, and well-tested. The core logic
(env-var-before-WebView2 ordering, fail-closed config read, debug-always-on,
flag-defaults-off, round-trip persistence, frontend dirty/snap/save) is sound.
Findings below are all **low-severity documentation-accuracy** issues — no
correctness, security, or build bugs were found.

## What was verified (no findings)

- **Env-var ordering** — `main()` (src-tauri/src/main.rs:60-65) sets
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS` before `tauri::Builder::default()`
  runs, so WebView2 reads it at creation time. Correct.
- **Fail-closed** — `browser_inspection_enabled()` (main.rs:41-46) uses
  `.unwrap_or(false)`, so any read/parse error returns false. Correct.
- **Flag/env consistency** — debug: env set + `new()` defaults
  `webview_enabled=true` (both on); release+on: env set + `build_brain` calls
  `set_webview_enabled(true)` (both on); release+off: env unset + flag stays
  default false (both off). All three states consistent.
- **Flag defaults OFF** — `GeneralSection::default()` (general.rs:199) +
  `#[serde(default)]` on the struct (general.rs:52) → absent key = false.
  Correct.
- **`normalize_url` choke point** — `webview_navigate` (mod.rs:868-869) still
  routes through `normalize_url`; no bypass introduced. Correct.
- **Unauthenticated-port risk communicated** — UI warning
  (AdvancedSection.tsx:362-368), docs (browser-debugging.md:107-110), and
  config doc comment (general.rs:97-105) all clearly state the risk. Good.
- **All constructors/Default impls updated** — `BrowserManager::new()`
  (mod.rs:161), `new_with_webview_url()` (mod.rs:200), `Default` (mod.rs:1167)
  all set `webview_enabled`. `GeneralSection` full literals at general.rs:644,
  settings.rs:1304, settings.rs:1380, contract_fixtures.rs:207 all include
  the field. Literals using `..GeneralSection::default()` (general.rs:537,
  client_factory.rs:315, client_factory.rs:343) are unaffected. No missed
  constructor.
- **Deserialization edge case** — absent key → `#[serde(default)]` → false;
  explicit `false` → false. Both correct.
- **Frontend dirty/snap/save** — load (line 57-58), dirty (line 73), save
  patch (line 96-98), snap update (line 100) all correct. `bumpConfigVersion()`
  (line 101) triggers reload. Correct.
- **Regression tests** — `enable_browser_inspection_defaults_false` +
  `enable_browser_inspection_round_trips` (general.rs:574-598) mirror the
  existing `run_all_strict_success` pattern. Good.
- **Doc comments** — present on all new public fields/functions. Good.
- **Warning-free** — `AtomicBool`/`Ordering` import added (mod.rs:33);
  `Ordering::Relaxed` used consistently; no obvious dead code or unused
  imports.

## Findings

### Low — Documentation accuracy

**L1. `browser_inspection_enabled()` doc comment contradicts the implementation**
- **File:** `src-tauri/src/main.rs:35-46`
- **Issue:** The doc comment states the function reads the flag "without
  loading the full [`Config`] (which reads four files)". The implementation
  calls `Config::load(&config_dir)`, which **does** load all four files
  (config.toml, endpoints.toml, keys.toml, projects.toml — see
  `src/config/mod.rs:55-70`). The plan (step 3) specified
  `GeneralConfig::load_or_default(&path)` to read only config.toml, but the
  implementation deviated to `Config::load`.
- **Impact:** Minor — the function still works correctly and fails closed.
  The only real cost is three unnecessary file reads (endpoints/keys/projects)
  at the very top of `main()`, before WebView2 creation. The misleading doc
  comment is the actionable issue.
- **Fix (either):**
  - Align with the plan + doc comment — read only config.toml:
    ```rust
    let path = config_dir.join("config.toml");
    GeneralConfig::load_or_default(&path)
        .map(|c| c.general.enable_browser_inspection)
        .unwrap_or(false)
    ```
  - Or fix the doc comment to match the implementation (remove the "without
    loading the full Config" claim).

**L2. `ensure_webview` doc comment is stale**
- **File:** `src/browser/mod.rs:661-662`
- **Issue:** The doc comment still reads "Release builds short-circuit: the
  CDP port is never exposed there, so probing localhost:9222 for 30s would
  only waste time (Review R1)." With this change, release builds **can**
  expose the port when opted in. The inline comment at lines 679-682 was
  updated correctly, but the doc comment above the function was not.
- **Fix:** Update the doc comment to reflect the runtime flag, e.g.
  "Short-circuits when the CDP port is not exposed (debug builds always
  expose it; release builds only when opted in via
  `enable_browser_inspection`) — probing localhost:9222 for 30s would only
  waste time (Review R1)."

**L3. `webview_click` / `webview_type` doc comments are stale**
- **File:** `src/browser/mod.rs:880-881` and `src/browser/mod.rs:893`
- **Issue:** Both say "Requires a debug build (the CDP port is dev-only)."
  This is no longer accurate — the tools now work in release builds when the
  user opts in. The actual gate is the runtime `webview_enabled` flag.
- **Fix:** Change to "Requires the CDP port to be exposed (debug builds, or
  release builds with `enable_browser_inspection` enabled)."

## Informational (no action required)

- **Fallback paths create `BrowserManager::new()` without calling
  `set_webview_enabled(true)`** (main.rs:288 NeedsProject, main.rs:354 Err).
  In a release build with the flag on, the env var IS set (in `main()`), but
  `webview_enabled` stays false in these fallback managers. This is harmless:
  no agent runs on these paths (project picker / startup-error screen), so
  `ensure_webview` is never called. Not a bug — noting for completeness.
- **Config is read twice in release** — once in `main()` via
  `browser_inspection_enabled()`, again in `build_brain()` via `Config::load`
  (main.rs:583). Unavoidable given the env-var-before-WebView2 constraint;
  `build_brain` needs the full config anyway. If L1 is fixed to use
  `GeneralConfig::load_or_default`, the first read becomes a single-file read.
