+++
title = "read the running app's webview console (and diagnose React loops) over CDP"
created = "2026-09-29"
+++

The app exposes the WebView2 Chrome DevTools Protocol on `localhost:9222` whenever
agent browser inspection is enabled, and the dev build always does. That is enough
to see what the UI is *actually* logging — the webview's `console.error` is never
forwarded to the Rust-side log, which is why a React error can spam the console for
days while `.coding/logs` looks clean.

Recipe (Windows PowerShell):

1. `$ws = (Invoke-RestMethod http://localhost:9222/json | Where-Object type -eq page | Select-Object -First 1).webSocketDebuggerUrl`
2. `.coding/tmp/cdp-errs.mjs $ws` — attaches with Node's built-in `WebSocket`,
   installs a `console.error`/`window.onerror` interceptor through
   `Page.addScriptToEvaluateOnNewDocument`, reloads, and prints what was collected.
   `cdp-console.mjs` is the live-stream variant (no interceptor) and `cdp-fiber.mjs`
   walks React's fiber tree to test store snapshots for stability.

Run the app in **dev mode** (`start.bat dev`, i.e. `tauri dev`) when diagnosing: the
dev build is unminified, so messages carry component names and dev-only React
warnings that the prod bundle hides. On 2026-09-29 that is exactly how "Minified
React error #185" (Maximum update depth exceeded, surfaced as the ErrorBoundary's
"UI render error") was traced: the dev console printed `The result of getSnapshot
should be cached to avoid an infinite loop` at `updateSyncExternalStore`, i.e. a
zustand selector returning a fresh object/array on every call. zustand 5 dropped the
`useSyncExternalStoreWithSelector` wrapper v4 used for `useStore`, so
`createWithEqualityFn` from `zustand/traditional` (with the
`use-sync-external-store` shim) is what restores the v4 memoization for every
consumer at once.

Gotchas:

- Vite's pre-bundled dep cache (`node_modules/.vite`) survives a dependency install.
  After adding a package, restart the dev server, or the page dies with
  `Could not resolve "<pkg>" imported by "zustand"`.
- A detached background process started from an agent tool call can be killed when
  that call times out — give long builds their own call and keep the rest short.

## Adding a warning detector when the repo has no DOM test infra

Two warnings fixed on 2026-09-29 (a `<button>` nested in a `<button>`, and Radix's
"Tabs is changing from uncontrolled to controlled") are invisible to the existing
test style:

- React's DOM-nesting check lives in the **client** renderer's host config, so
  `renderToStaticMarkup` stays silent about nested `<button>` — confirmed by
  rendering a deliberately nested one and capturing zero console output.
- Radix's controlled-value warning only fires on a *transition*, so it needs a
  re-render, which server rendering cannot do either.

So both are pinned as source contracts (the documented style, since vitest runs in
`node`) **and** were proven behaviorally with a throwaway harness:

1. `npm install --no-save --no-package-lock jsdom` — resolves at the repo root
   (vitest is hoisted there), and leaves package.json + package-lock.json clean.
2. A temp `scratch.vitest.config.ts` with `environment: "jsdom"` and an `include`
   naming just the scratch file — the committed config is an explicit allowlist
   (guarded by `src/lib/vitestInclude.test.ts`), so extra files are never picked up.
3. `createRoot` + `act` (from `react` in 19) with
   `globalThis.IS_REACT_ACT_ENVIRONMENT = true`, and a `capture()` helper that
   records `console.error`, `console.warn` **and** `process.stderr.write` —
   Radix's warning arrives on the warn/stderr side, which a console.error-only spy
   misses.
4. Every check ships a CONTROL case that reproduces the old shape and asserts the
   detector fires. Without it a "no warning" result proves nothing.
5. Delete the harness and run `npm ci` to restore the lockfile-exact tree.
