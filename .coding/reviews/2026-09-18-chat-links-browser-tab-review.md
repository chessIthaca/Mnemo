## Verdict: FINDINGS (0 high, 4 low)

The routing change is correct, secure and platform-neutral: chat links reach the app's own Browser tab through the store, the OS browser stays reachable exactly where the child webview cannot exist (non-Windows probe) and through the explicit escape hatch (ctrl/cmd-click, middle-click), and no non-http(s) scheme gains an opener. All four lows are test-strength / effect-hygiene items — no live defect in the shipped path.

Scope reviewed: `git diff HEAD` working tree on `C:/Mnemo` @ `wt/mnemo` — `frontend/src/lib/openChatLink.ts` (+test), `frontend/src/hooks/useAgentStore.ts` (+test), `frontend/src/components/chat/MarkdownLink.tsx` (+test), `frontend/src/components/chat/Message.tsx`, `frontend/src/components/views/BrowserView.tsx`, `frontend/src/lib/toolCardPaths.ts`, `frontend/vitest.config.ts`, `docs/FEATURES.md`, the two knowledge records, `.coding/plans/92b9d0b7.md`.

### LOW 1 — the two scheme-discipline tests cannot fail for the regression they name (test strength)

`frontend/src/components/chat/MarkdownLink.test.tsx:187-191` and `:193-202` are synchronous while the path they guard is not. For a wrongly-accepted scheme the router's store write lands in a **microtask** (`openChatLink.ts:71-83`: the write happens only after `await browserWebviewSupported()`), so a widened allow-list at `openChatLink.ts:59` would leave `pendingBrowserUrl` null at assertion time and both tests would still pass — the exact "data:/file: must stay unreachable" contract the test names is not actually enforced at this seam. (The router-level twin, `openChatLink.test.ts:97-116`, *does* `await openChatLink` per target and therefore does catch a widening — which is why this is low, not high.) Same file, second gap: `:162` asserts `rightPanelVisible === true` but the `beforeEach` (`:114`) already sets it true, so the assertion cannot discriminate.

Fix: make both tests `async`, `await flushRouter()` before the assertions (the helper already exists at `:46-47`), and reset `rightPanelVisible: false` in the `beforeEach` so the reveal assertion at `:162` is load-bearing.

### LOW 2 — the `?raw` BrowserView pin is weaker than its doc comment claims (test strength)

`frontend/src/lib/openChatLink.test.ts:134-147` pins four string presences. All four survive a regression that keeps ensure+navigate but derives the rect from something other than the placeholder (a hard-coded/computed rect, or a 0×0 rect while the view is mounted but not laid out) — i.e. the "**or moved to a wrong rect**" half of the comment at `:138-139` is not covered. Nothing pins that `loadIntoChild` reads the rect at all.

Fix (one line): add `expect(browserViewSource).toContain("areaRef.current")` and `toContain("getBoundingClientRect")` — both occur in `BrowserView.tsx:110-112` only, inside `loadIntoChild` — or drop the wrong-rect claim from the comment. The rest of the pin (`loadIntoChild(pendingBrowserUrl)` + `browserWebviewEnsure` + `browserWebviewNavigate` + `clearPendingBrowserUrl`) is genuinely load-bearing: a fire-and-forget navigate, a missing ensure, or a non-single-shot consume each breaks it.

### LOW 3 — `loadIntoChild` is an unstable effect dependency (latent, no impact today)

`BrowserView.tsx:154` lists `loadIntoChild` in the deps of the consume effect, and that function is re-created on every render (`:101-128`), so the effect re-runs each render. It is harmless **only** because the clear is synchronous in the same effect body (`:152-153`, before any re-render can interleave), so later runs early-return on `pendingBrowserUrl === null`. If the clear ever moves to the natural place for a failure-aware version (`.finally()` inside `loadIntoChild`), every BrowserView render would re-load the still-pending URL — a re-load loop.

Fix: wrap `loadIntoChild` in `useCallback([], …)` (it touches only state setters, `areaRef`, and module imports) or read the value via `useAgentStore.getState().pendingBrowserUrl` inside the effect.

### LOW 4 — two different links clicked inside one IPC round-trip can settle out of order (narrow)

Each click runs an independent async chain. `setUrl` fires synchronously in click order (`:104`), but `ensure`/`navigate`/`setLoadedUrl` run in *resolve* order (`:106-123`), so if normalize for link A resolves after link B's, the webview ends on A while the URL bar shows B. Both targets are user-initiated chat links (no security or state impact), the window is one IPC round-trip, and the URL-bar Open path has the same shape pre-existing — reported because the review asks about replay/ordering explicitly.

Fix (optional, ~2 lines): a per-view generation ref — `const token = ++loadToken.current;` at the top of `loadIntoChild`, `if (token !== loadToken.current) return;` after each await — which also covers the same-URL-twice case with one guard.

---

## Focus-point verification

### (a) RECT HAZARD — clean

- The extraction is behaviour-preserving. `openUrl` (`BrowserView.tsx:130-133`) now awaits `loadIntoChild(url.trim())`; the body (`:105-127`) is the previous `openUrl` body verbatim, with one addition: `setUrl(target)` at `:104`. For the Open/Enter path `target === url.trim()`, so the only delta is that the URL bar text is trimmed — benign, and it is what makes a chat-link load echo its URL into the bar. `setLoadedUrl`/`setError(null)`/`setError(errMsg)` ordering and the error panel are unchanged.
- The rect travels with the call: `:110-121` reads `areaRef.current` and multiplies by `devicePixelRatio` at call time, and the child is created via `browser_webview_ensure` (create-on-first-call) followed by `browser_webview_navigate` — matching the backend contract (`src-tauri/src/ipc/browser_webview.rs:297-318`: create with the URL, or move/resize only when it already exists; `:567-589`: navigate is a no-op without a child).
- The new path cannot move an existing child to a wrong rect: it calls the same function with the same rect, and `requestBrowserOpen` (`useAgentStore.ts:894-904`) sets `rightPanelTab`/`rightPanelVisible` in the *same* store write that sets the URL, so by the time the effect runs the panel is already laid out (effects run post-commit). `useBrowserRect`'s continuous coalesced reports remain the correction path for any transient layout.
- No reveal without a load: `RightPanel.tsx:118-124` renders only the active tab's body, so `BrowserView` is mounted exactly when the browser tab is shown; the effect is the only consumer and it runs on mount and on value change. If ensure throws, the view's own error panel renders inside the revealed tab (`:125-127`) rather than failing silently. The only drop-without-load is the deliberate `!supported` guard (`:145-151`), which on a real non-Windows box is unreachable via the router (see (b)).

### (b) NON-WINDOWS FALLBACK — clean, rationale discoverable

- `browser_webview_supported` returns `cfg!(windows)` (`browser_webview.rs:292-295`), so on macOS/Linux `openChatLink` takes the `!supported` branch and awaits `openExternal(parsed.href)` (`openChatLink.ts:77-82`) — the pre-existing shell open, same call shape as before this change.
- The rejection default is safe and deliberate: `openChatLink.ts:71-76` treats a *rejected* probe as supported, mirroring `BrowserView`'s optimistic mount probe. The trade-off is stated in the code ("the tab path degrades visibly (its own error panel), while falling back to an OS-browser launch on a transient failure would surprise the user") and echoed at `BrowserView.tsx:145-151`. Worst case is a red unsupported/error panel in the revealed tab — visible, not silent, and no wider capability than before (the URL would have been shell-opened anyway).
- Rationale lives in four places a maintainer will hit: the router's module + function doc comments (`openChatLink.ts:5-45`), the consume-effect comment (`BrowserView.tsx:135-151`), `docs/FEATURES.md:56`, and the two amended knowledge records.

### (c) SCHEME DISCIPLINE — clean, no widening

- The router parses and rejects before *any* side effect: `new URL` + `protocol !== "http:" && protocol !== "https:" → return` (`openChatLink.ts:51-59`). Everything downstream — the probe, `openExternal`, `requestBrowserOpen` — sits behind that gate, so `data:`/`file:` (which the Browser tab's `normalize_url` *would* accept, `src/browser/mod.rs:1622-1666`) and `mailto:`/fragments `javascript:` never reach the store.
- Both call sites honour the same rule: `MarkdownLink`'s external branch is the only one reaching the router, and the chip's `chip.url` comes from `webFetchUrl` (`toolCardPaths.ts`), which is http(s)-only. `Message.tsx:627-636` passes `chip.url` straight through, with no scheme handling of its own (correct — the router owns the gate).
- The store action is not reachable from anywhere else: `requestBrowserOpen` has exactly one caller (`openChatLink.ts:83`). No new opener was added for non-http(s) targets, and no existing one was widened.
- Escape hatch is a pure caller-side flag (`opts.osBrowser`), so it cannot be reached by a URL itself — no way for a page/href to talk its way into `openExternal` other than the http(s) it already had.

### (d) CONSUME-EFFECT RACE — sound (see LOW 3/LOW 4 for the two residual notes)

- **Click before mount**: this is the case the store design exists for, and it works — `requestBrowserOpen` writes tab + visibility + URL in one `set`, so the view mounts in the next commit and the mount effect consumes the URL. Nothing is lost.
- **Same URL twice**: not replayable and not lost. The effect clears synchronously after starting the load, so the second click changes `null → url` again and re-fires the effect; a stale value cannot re-trigger because the store value is identical only while the first request is still unconsumed (which the synchronous clear precludes).
- **Re-renders**: the effect body is `if (pendingBrowserUrl === null) return;` first, and the clear happens in the same synchronous block as the read (`:152-153`); React cannot interleave a render between them, so a re-render can neither double-load nor replay. Effect flush ordering also closes the "unmount before consume" window: React flushes pending passive effects before performing the next render, so a tab switch cannot unmount the view before the mount-time consume has run.
- **Disabled at click time**: handled explicitly — `disabledTabs` is filtered in the same write (`useAgentStore.ts:899`), so `RightPanel`'s "first enabled tab" fallback (`RightPanel.tsx:21-25`) cannot reroute the reveal.
- Residuals, both minor and both reported: the unstable `loadIntoChild` dep (LOW 3) and out-of-order settling under two clicks inside one IPC round-trip (LOW 4).

---

### (e) TEST MEANINGFULNESS — sound, with the two gaps in LOW 1/LOW 2

- **Registration confirmed**: `frontend/vitest.config.ts:70` lists `src/lib/openChatLink.test.ts`, and the guard (`frontend/src/lib/vitestInclude.test.ts`) walks `src/**/*.test.ts(x)` and fails the suite for any unlisted file — so the include list and the suite agree.
- **The external-href case is asserted correctly** (`MarkdownLink.test.tsx:154-165`): `pendingBrowserUrl === "https://example.com/docs"`, `rightPanelTab === "browser"`, `rightPanelVisible === true`, `pendingFileOpen === null`, **and** `openExternal` not called. That is the "Browser-tab request with no OS-browser launch" contract, and it fails under the old `openExternal` routing on all four assertions.
- Non-vacuous elsewhere: the router suite runs the REAL store, so the side effects are observed, not mocked (`openChatLink.test.ts:55-62`); the disabled-tab/hidden-panel case (`:64-77`) starts from `rightPanelVisible: false` + `disabledTabs: ["browser"]`, so both the enable and the reveal assertions discriminate; `:89-95` asserts the modifier short-circuits the probe (`not.toHaveBeenCalled`) — a real routing property; `:118-126` pins the rejection default; `:128-131` pins the normalized href (case-insensitive scheme/host) which is what a browser would actually load. `MarkdownLink.test.tsx:130-152` drives the unchanged local-file path through the real store, and `:233-252` pins the updated external title hint. No tautological or self-fulfilling assertions in the new code.
- The two weaknesses are reported as LOW 1 (the two sync scheme tests, which cannot fail for a widened allow-list) and LOW 2 (the `?raw` pin does not cover the "wrong rect" half of its own claim). Both are one-to-two-line tightening fixes; neither weakens the regression protection that already exists at the router level.

## Standard checks

- **Correctness / bugs**: no defect found in the shipped path (details in (a)–(d)); the only behavioural delta on the pre-existing Open/Enter path is the new `setUrl(target)` trim/no-op.
- **Security**: no capability widened. The openable set is exactly the previous http(s) set; the OS-browser escape hatch is caller-controlled, not URL-controlled; the chip's title uses React interpolation of a URL that already passed `webFetchUrl`/router validation (escaped, no XSS); the middle-click branch prevents the default anchor action before routing; no `dangerouslySetInnerHTML`, no new IPC commands, no new Tauri capability, no Rust change.
- **Constitution**: doc comments present on the new exported `openChatLink` (with `@param`s), on both new store actions (`useAgentStore.ts:475-481`) and on `loadIntoChild`; no new dependencies; no `#[allow]`; no Rust touched, so the warning-free/`#![deny(warnings)]` gate is unaffected (the hand-off `cargo test` green run stands).
- **Multi-platform neutrality**: the frontend change contains no Windows-only API, path or shell syntax; the platform decision is a runtime probe (`browser_webview_supported` = `cfg!(windows)`) and the Windows-only child webview stays behind its existing gate in Rust plus the view's unsupported-platform panel. The fallback path is exactly the code macOS/Linux had before.
- **File-tools-first**: no shell-based file mutation in this change — the plan's commands were `cargo test` / `tsc` / `vitest` only.
- **Documentation sync**: `docs/FEATURES.md:56` (chat markdown links) and `:69` (web_fetch chip) are both accurate for the new behaviour, including the ctrl/middle-click nuance and the non-Windows fallback; `toolCardPaths.ts`'s `ToolCardChip.url` doc comment no longer says "default browser"; the two knowledge SPECs are amended in place. **README.md needs no change** — it carries no statement about the chip or chat external links (checked for `chip`, `fetched`, `system browser`, `in your browser`), and its only related claim (`:44`, "embedded WebView2 browser tab (Windows)") remains true. **PLAN.md needs no entry** — no dependency, provider or architecture decision was added; the router's rationale lives in the module doc comments and the amended knowledge records.
- **Verification runs**: reported green at hand-off (root `cargo test` 2410/0/5, `tsc --noEmit` exit 0, `vitest run` 84/84 files). Not re-run here (read-only reviewer); the Rust suite is unaffected by this plan, and the frontend suites cover the changed seams as assessed above.
