## Verdict: FINDINGS (1 high, 1 low)

Review of all uncommitted changes on `wt/mnemo` for plan 47735e3c (backlog 3f838ea1) — chat URL clicks + agent-steered browser URL-box sync.

**Summary:** Defect 1's fix (default `a: MarkdownLink` in MarkdownImpl) is correct and complete — verified at the single choke point with a clean raw-anchor audit. Defect 2's fix (browser://url-changed → store.browserUrl → URL box) has a **payload-shape mismatch between the Rust emit and the TS listener**: the backend emits a bare JSON string, the frontend reads `payload.url` from an object interface — at runtime `payload.url` is `undefined`, the store guard no-ops, and **the URL box never syncs**. All tests pass because nothing exercises the actual event payload extraction.

---

## HIGH-1: `browser://url-changed` payload mismatch — the URL box never syncs at runtime (defect 2 unfixed)

**Evidence — backend emits a bare string** (`src-tauri/src/ipc/browser_webview.rs:394-397`):

```rust
.on_page_load(move |url, _event| {
    // Best-effort: a failed emit must not fail the page load.
    let _ = app_for_loads.emit(BROWSER_URL_CHANNEL, url.to_string());
})
```

`Emitter::emit` serializes the payload with serde: a `String` becomes a JSON string. The channel's own Rust doc comment even says "Payload: the URL string." (browser_webview.rs:370).

**Evidence — frontend reads an object field** (`frontend/src/lib/tauri.ts:1850-1863` + `frontend/src/hooks/useAgentEvents.ts`):

```ts
export interface BrowserUrlPayload { url: string; }
export function onBrowserUrlChanged(handler: (payload: BrowserUrlPayload) => void) {
  return listen<BrowserUrlPayload>(BROWSER_URL_CHANNEL, (event) => handler(event.payload));
}
// useAgentEvents.ts:
onBrowserUrlChanged((payload) => { handleBrowserUrlChanged(payload.url); })
```

**Runtime trace:** `event.payload` arrives as the string `"https://…"` → `payload.url` is `undefined` → `handleBrowserUrlChanged(undefined)` → `setBrowserUrl(undefined)` (silently violating the `browserUrl: string` type) → BrowserView's `if (!browserUrl) return;` no-ops. The event fires on every navigation, but the URL box never updates — the exact symptom backlog 3f838ea1(2) describes remains.

**Why the suite misses it:** `browserUrl.test.ts` calls the handler directly with strings; `useAgentStore.test.ts` drives the action directly; the BrowserView/MarkdownImpl tests are source-contract greps. No test extracts a payload from an event — the one seam where the bug lives.

**Root of the trap:** the pattern was copied from `browser://reveal`, which carries the *same latent mismatch* (backend emits `normalized.clone()` — a bare string, browser_webview.rs:473 — while tauri.ts:1837 declares `BrowserRevealPayload { url }`). It is harmless there only because `handleBrowserReveal()` takes no payload and never reads it. The new code copied the interface and then actually read the field.

**Fix (either side, one seam):**
- Backend (matches the codebase convention — `ConsoleEntry`, `AgentEventPayload` are structs whose fields the frontend reads): emit an object, e.g. a `#[derive(Serialize)] struct BrowserUrlPayload { url: String }` or `serde_json::json!({ "url": url.to_string() })`, and sync the channel doc comment; or
- Frontend: type the payload as `string` (`listen<string>` + `handleBrowserUrlChanged(payload)`), dropping the `BrowserUrlPayload` interface.

While touching this, align the reveal channel's latent mismatch too (fix its interface or its emit) so the next copy of the pattern doesn't reintroduce this.

**Regression test:** node-env tests can't do the IPC round-trip, so pin the seam contractually: e.g. a source-contract test asserting the Rust emit carries a `url`-keyed object (or the TS listener passes the payload through unchanged), or a unit test on a payload-constructing helper. Without one, this exact mismatch can silently reappear.

---

## LOW-1: `setBrowserUrl` test placed in an unrelated describe block

`frontend/src/hooks/useAgentStore.test.ts` — the new test ("setBrowserUrl records the child webview's current URL") is appended inside `describe("revealRightPanelTab + requestFileOpen")`, whose subject is the pending-open request plumbing. A `browserUrl` store-action test belongs in its own describe (e.g. `describe("browserUrl (child webview current URL)")`) or alongside the pendingBrowserUrl coverage it conceptually pairs with. Harmless but misleading organization; cheap move.

---

## Verified correct (the plan's stated invariants)

- **Every markdown surface routes links through MarkdownLink.** `components={{ a: MarkdownLink, ...components }}` (MarkdownImpl.tsx:52) is the single choke point: `react-markdown` is imported ONLY by MarkdownImpl (search-verified), and all four surfaces (Message.tsx, PlanProgress.tsx, BacklogView.tsx, SourceEditor.tsx) render via the lazy `Markdown` wrapper → MarkdownImpl. No `rehype-raw` anywhere, so raw HTML in markdown input is escaped — the `a` override is the only anchor path. Message.tsx:310's explicit `a: MarkdownLink` is unchanged (same component — no behavior change). Caller's spread wins (`{...undefined}` is a no-op).
- **Raw-anchor audit clean.** The only JSX `<a` anchors in frontend/src are MarkdownLink's own two (both `preventDefault`'d — MarkdownLink.tsx:84-96, 113-122); AdvancedSection's blob-download anchor is programmatic (pre-existing, out of scope); JsonView explicitly avoids `dangerouslySetInnerHTML`.
- **Hook ordering.** The sync effect (BrowserView.tsx:181-186) sits BEFORE the `!supported` early return (line 211); on unsupported platforms `browserUrl` stays `""` and the effect no-ops. The comment at 208-210 documents the invariant.
- **No mount-timing gap.** `ensureBrowserUrlListenerStarted` is module-scope + idempotent, started from `useAgentEvents()` (mounted at App.tsx:95 — app root, always alive), mirroring the reveal listener. The store tracks the URL while the Browser tab is hidden; BrowserView's effect catches up on mount.
- **URL box stays an input.** The effect deps are `[browserUrl]` only — user typing never triggers it.
- **Both creation sites share the helper.** `child_webview_builder` (browser_webview.rs:382-398) is called by `browser_webview_ensure` (:333) and `ensure_for_agent_impl` (:446); the duplicated builder construction is gone. The emit is best-effort (`let _ =`), so an emit failure never fails the load. wry's page-load hook (vendor/wry/src/webview2/mod.rs:650-673) wires `ContentLoading`/`NavigationCompleted` — top-level-document events reading the webview's current URL, so no subframe clobbering, and CDP/script-initiated navigations fire them.
- **Platform neutrality.** browser_webview.rs is Windows-gated by design (the sanctioned WebView2 Browser-tab exception, documented in the module doc); the frontend changes are platform-neutral (listener registers everywhere, fires only where a child webview exists).
- **Docs.** MarkdownImpl/BrowserView/store/tauri.ts doc comments match the landed behavior; README's Browser-tab mention (line 44, feature table) is generic and not stale. No shell-based file mutation in the diff. `browserUrl.test.ts` is registered in vitest's include allow-list. Test coverage (source contracts + store + handler) is meaningful and follows the established patterns.

## Notes (not findings)

- **Same-URL re-navigation while mid-edit:** if the user is typing in the URL box and the agent re-navigates the child to the *identical* last-emitted URL, the zustand selector returns an unchanged value and the effect doesn't re-fire (the box keeps the user's typed text). This is the plan's explicitly designed trade-off ("the effect only fires on browserUrl changes, not on user typing") — noted for the record, not a defect.
- **Commit contents:** the untracked `.coding/plans/47735e3c.md` must be included in the commit (plan file travels with the change), alongside the fixed files and this report.
- **Fix ripple:** whichever side of HIGH-1 is fixed, re-run `cargo test` + `npm --prefix frontend test` (the source-contract tests pin `a: MarkdownLink, ...components`, `s.browserUrl`, `setUrl(browserUrl)`, `setLoadedUrl(browserUrl)` — a frontend-side fix must keep those strings intact).
