# Review — Multi-platform build (macOS support + GitHub Actions + game_* gating)

Branch `feat/multiplatform-build`, all uncommitted changes vs HEAD (9 modified files +
untracked `.github/workflows/build.yml`). Reviewed: `git diff HEAD` + full reads of
`src-tauri/src/ipc/browser_webview.rs`, `src-tauri/src/main.rs`, `src/agent/factory.rs`,
`src-tauri/tauri.conf.json`, `.github/workflows/build.yml`,
`frontend/src/components/views/BrowserView.tsx`, `frontend/src/hooks/useBrowserRect.ts`,
`frontend/src/lib/tauri.ts`, plus the workspace/build-layout files (`Cargo.toml`,
`src-tauri/Cargo.toml`, `package.json`, `frontend/package.json`, `.gitignore`,
`src/browser/mod.rs`, `src/lib.rs`) that determine where build artifacts actually land.

---

## HIGH

### F1 — CI: artifact upload paths point at the wrong target dir — both jobs will fail at the last step [bugs / correctness]
`.github/workflows/build.yml:67-70` (windows) and `:135-138` (macos) glob
`src-tauri/target/release/bundle/...` and `src-tauri/target/aarch64-apple-darwin/release/bundle/...`.

But the repo root `Cargo.toml:90-92` declares `[workspace] members = ["src-tauri"]` — the
workspace root is the **repo root**, and no `.cargo/config.toml` exists (checked root and
`src-tauri/`; neither directory exists). Cargo therefore writes *all* artifacts — including
the myharness-app bundles — to `<repo-root>/target/`, and the Tauri CLI reads that same
`target_directory` from `cargo metadata`. Corroborated by `src-tauri/tauri.conf.json:7`
(`"frontendDist": "../target/frontend-dist"` → root `target/`) and `.gitignore:2` (`/target/`).

Actual output locations:
- Windows: `target/release/bundle/msi/*.msi`, `target/release/bundle/nsis/*.exe`
- macOS: `target/aarch64-apple-darwin/release/bundle/macos/*.app`, `target/aarch64-apple-darwin/release/bundle/dmg/*.dmg`

With `if-no-files-found: error`, **both jobs fail at the upload step after the entire
(expensive, signed) build succeeds**. First workflow run will be red on both platforms.

Fix: change the four globs to the root-`target` layout above. Optional follow-up nit:
`.gitignore:29` (`src-tauri/target/`) is stale under this workspace layout and can be dropped.

## MEDIUM

### F2 — CI: raw `.app` directory upload via upload-artifact@v4 is not launchable [bugs]
`.github/workflows/build.yml:136` uploads `*.app` as a directory. `upload-artifact@v4` does
not preserve file permissions (the executable bit on the inner Mach-O binaries) or symlinks,
so a `.app` restored from the artifact will not launch. The `.dmg` (line 137) is a single
file and is the real distributable — it's fine. Fix: drop the `*.app` line, or
`ditto -c -k --keepParent <App>.app <App>.app.zip` and upload the zip.

### F3 — CI: partial Apple-secret configs fail late with confusing errors [bugs / robustness]
The guards are per-secret but the signing/notarization ceremony is coupled:
- `APPLE_CERTIFICATE` set but `APPLE_SIGNING_IDENTITY` empty (guard at `:99`, env at `:127`):
  the import step runs, then tauri build receives `APPLE_SIGNING_IDENTITY` **set-but-empty** —
  the bundler treats a set var as the identity to use → `codesign --sign ""` fails at the end
  of the build.
- `APPLE_API_KEY_CONTENT` set but `APPLE_API_KEY` (the key id) empty (`:115`, `:121`): writes
  `~/private_keys/AuthKey_.p8` → notarytool can never resolve it → notarization fails at the end.
- `APPLE_SIGNING_IDENTITY` set without `APPLE_CERTIFICATE`: codesign fails (no identity in keychain).

Fix: gate the pairs/triples together — import step
`if: secrets.APPLE_CERTIFICATE != '' && secrets.APPLE_SIGNING_IDENTITY != ''` (and export the
identity under the same condition); key-write step
`if: secrets.APPLE_API_KEY != '' && secrets.APPLE_API_KEY_CONTENT != ''` — or add a fail-fast
consistency check step with a clear message.

## LOW

### F4 — Frontend: mount-time supported fetch has no `.catch` [bugs]
`frontend/src/components/views/BrowserView.tsx:51-53` — `browserWebviewSupported().then(...)`
with no rejection handler. If the invoke ever rejects (stale frontend vs older backend, IPC
failure) this is an unhandled promise rejection and `supported` silently stays `true`
(optimistic), leaving the interactive tab whose actions then all error. Add an explicit
`.catch(() => {})` — the optimistic fallback behavior is fine, but the rejection should be
deliberately swallowed. Low because the command is registered unconditionally and returns a
plain bool — it cannot reject in normal operation.

### F5 — `inherit_shell_path` rejects any PATH containing whitespace [bugs]
`src-tauri/src/main.rs:72` — `!path.chars().any(char::is_whitespace)` rejects a *legal* PATH
that contains a spaced entry (e.g. `/Applications/Some App/bin`). On such machines the app
keeps the bare GUI PATH — exactly the failure the function exists to fix — with no diagnostic.
Consider rejecting only newlines/control chars, or at least eprintln why the value was
rejected. Also worth a doc-comment note (accepted trade-off): any dotfile that echoes to
stdout pollutes the captured PATH, since login-shell rc output shares the same stdout as the
`printf`.

### F6 — Indentation glitch in the handler list [constitution / style]
`src-tauri/src/main.rs:602` — `ipc::browser_webview::browser_webview_ensure,` is indented one
level shallower than every sibling (601/603 use 12 spaces, 602 uses 8). Inconsistent with the
list style; rustfmt would reformat. Cosmetic only.

---

## Verified clean (no findings)

- **React hooks-rules / early return** — `BrowserView.tsx`: every hook (4×useState, useRef,
  2×useEffect, `useBrowserRect`) runs unconditionally above the `if (!supported)` early return
  (line 105); hook count/order is identical on both sides of the flip. Compliant.
- **`useBrowserRect` enabled semantics** — `useBrowserRect.ts:59-110`: early return precedes
  all observer setup; `enabled` is in the dep array; on a true→false flip the prior cleanup
  runs (ResizeObserver disconnect, resize listener removed, rAF cancelled) and the new run
  registers nothing; final unmount is clean. The single pre-flip report on macOS lands in
  `set_rect`, which harmlessly records the rect (no webview exists). Correct.
- **Tab-visible effect** (`BrowserView.tsx:65-71`) — re-runs on `supported`; on the flip the
  old cleanup sends `setTabVisible(false)` (backend no-op with no webview) and the new run
  early-returns without registering a cleanup. Correct.
- **Alive-guard race** (`BrowserView.tsx:49-57`) — `alive` flag cleared in cleanup; no
  setState-after-unmount; StrictMode double-mount safe (modulo F4).
- **factory.rs gating + test** — registration block (`factory.rs:663-671`) and the test's
  `#[cfg(windows)] expected.extend` (`:1191-1195`) list the identical six tool names;
  `use crate::tool::browser as bt;` (`:638`) is used unconditionally by the ten headless
  `browser_*` registrations (no unused-import on macOS); the `Game*Tool` types sit behind
  `pub` in the browser tool module, so unreferenced-on-macOS cannot trip dead_code;
  `for &name in &expected` over `Vec<&str>` is correct; the exact-count assert
  (`:1203-1207`) holds on both platforms. Warning-free on both targets as far as static
  analysis shows; the macOS job's `cargo test --workspace` (`build.yml:90-91`) is the
  compile proof and runs before bundling — good design.
- **browser_webview.rs cfg-safety** — every new symbol (`UNSUPPORTED_BROWSER_TAB_MSG`,
  `child_webview_supported`, `ensure_platform_gate`, `browser_webview_supported`) is reached
  unconditionally on both platforms (commands are registered in `main.rs` on all targets);
  nothing becomes dead on macOS. Gate placement is correct: first line of
  `browser_webview_ensure` (`:261`), before normalize/state-lock/create.
- **main.rs gating** — `browser_inspection_enabled` (`:44-50`) is `#[cfg(windows)]` with its
  sole caller inside the `#[cfg(windows)]` block (`:132-141`); `inherit_shell_path` has
  unix + not(unix) twins with an unconditional call site (`:104`); `webview_args` is
  `pub mod` (`src/lib.rs:24`) so it cannot dead-code-warn on macOS. The ungated
  `browser.set_webview_enabled(true)` at `main.rs:1048` is inert on macOS — the flag is read
  only by `BrowserManager::ensure_webview` (`src/browser/mod.rs:685`), reachable only from
  the (unregistered) game_* tools.
- **IPC surface** — `browser_webview_supported` is a read-only, argument-free bool; it
  reveals only the compile target. No new attack surface.
- **tauri.conf.json** — valid JSON; `bundle.macOS.minimumSystemVersion: "11.0"` matches the
  Tauri 2 schema; deliberate no-entitlements/no-signingIdentity is consistent with the
  CI env-injection design.
- **build.yml general semantics** — YAML valid; triggers (workflow_dispatch + `v*` tags),
  per-ref concurrency with cancel-in-progress, action pins (checkout@v4, setup-node@v4,
  rust-toolchain@stable, rust-cache@v2, upload-artifact@v4) all correct;
  `working-directory: frontend` scopes tsc/vitest correctly while root `npm ci` covers both
  workspaces via the existing root `package-lock.json` (v3; `frontend/package-lock.json` is
  deliberately gitignored — `.gitignore:12-17`); `npx` resolves hoisted workspace binaries;
  node 22 fine; dtolnay toolchain + `targets: aarch64-apple-darwin` on an arm64
  `macos-latest` runner is coherent (host tests + same-triple bundle build); the Windows
  msi/nsis glob shape matches Tauri v2's layout (modulo F1's target-dir prefix).
- **Secrets never printed** — no `set -x`; the p12 is piped through base64 into a file that
  is `rm`'d; the `.p8` is written via `printf` to a file; `if: ${{ secrets.X != '' }}`
  conditions don't output values; GitHub auto-masks secret values in logs. Clean.
- **`inherit_shell_path` trust boundary** — running `$SHELL` from the environment is an
  acceptable local-user boundary: anyone able to set the user's `SHELL` already executes code
  as that user (launch agents, rc files are equivalent vectors); the function adopts only a
  string (no eval) and fails closed to the inherited PATH. Documented in its doc comment.
- **Constitution compliance** — doc comments on every new pub fn/const (including the
  private-but-documented `ensure_platform_gate` and the frontend
  `browserWebviewSupported`); no `#[allow(...)]` anywhere in the diff; the new gate has
  both-branch unit tests plus a cfg-tracking test (`browser_webview.rs:588-606`), and the
  factory test covers the platform-conditional registry; `cargo test --workspace` was green
  pre-review.
- **`.coding/` churn** — `backlog.json` (items 66-67, `next_id` 68) and `plans/stack.json`
  are expected bookkeeping, no corruption (item 67's embedded base64 image matches the
  backlog schema).

## Trade-offs noted (no action required)

- Optimistic `supported=true` gives unsupported platforms a one-IPC-round-trip flash of the
  URL bar before the red panel — deliberate, Windows-first, documented at
  `BrowserView.tsx:41-44`. Fine.
- The explicit frontend build step duplicates tauri's `beforeBuildCommand` (`npm run build`)
  once more inside `npx tauri build` — defensible fail-fast duplication (~a minute per run).
- The keychain created by the cert step is never deleted post-build — irrelevant on an
  ephemeral runner.

**Summary: fix F1 (both jobs' upload globs) before the first tag push — it fails both
pipelines at the last step; F2/F3 harden the macOS artifact + secrets UX; F4-F6 are small.**
