## Verdict: PASS

Narrow round-3 verification of the one-case fix for round-2's single LOW, in `C:/Mnemo` @ `wt/mnemo` (uncommitted working tree; plan 92b9d0b7). **The low is verifiably closed and the change introduced no new defect:** the `mailto:` case now snapshot-asserts the tab channel, those assertions fail under the exact regression the case names (statically traced through both store values), the correct path still passes, and the change is confined to that one case body — every round-2-verified source position reads back at the same content and line as round 2 left it, so round 2's verification of the memoized `loadIntoChild`, the generation token, the rect read, the OS-browser fallback and the scheme gate stands.

Answers to the two scoped questions:

1. **Round-2 LOW: CLOSED.** `frontend/src/components/chat/MarkdownLink.test.tsx:187-201` — after `await flushRouter()` the case takes `const s = useAgentStore.getState()` (`:193`) and asserts `s.pendingFileOpen === null` (`:194`), **`s.pendingBrowserUrl === null` (`:198`)** and **`s.rightPanelTab === "plan"` (`:199`)**, plus the retained `expect(vi.mocked(openExternal)).not.toHaveBeenCalled()` (`:200`). Under the regression (a non-openable scheme wrongly accepted) the only reachable wrong channel is the Browser tab: `requestBrowserOpen` (`useAgentStore.ts:894-903`) writes `pendingBrowserUrl` + `rightPanelTab: "browser"` in the SAME atomic `set`, so both new assertions fail; the other two channels remain covered by the retained assertions (Files tab → `pendingFileOpen`; OS browser → `openExternal`). The correct behaviour is a true no-op (`markdownLink.ts:56`'s `SCHEME_RE` rejects `mailto:` before any opener), so the case still passes.
2. **No new defect, no weakened assertion, no leakage.** The case mutates nothing (it only reads the store and inspects the `openExternal` mock; on the correct path it invokes neither mock), and the `beforeEach` (`:110-122`) re-sets `rightPanelTab`/`disabledTabs`/`rightPanelVisible`/`pendingFileOpen`/`pendingBrowserUrl` and re-arms both mocks before every case — so there is no leakage in or out. The two pre-existing assertions are retained in substance (the first now reads the same post-flush snapshot; nothing mutates the store between the snapshot and the reads). The +6-line insertion sits inside `:187-201` and every later line in the file shifted uniformly by +6 (round 2 cited the sibling `data:`/`file:` case at `:197-210`; it is now `:203-216`) — the signature of a single-case body edit, with no other case touched.

---

## Evidence — (1) the new assertions are load-bearing

- **They observe the channel the regression uses.** A widened allow-list can only route a chat `mailto:`/`data:`/`file:` target to the Browser tab: `openMarkdownTarget` → `openChatLink` → `useAgentStore.getState().requestBrowserOpen(parsed.href)` (`openChatLink.ts:83`), which is one `set` writing `disabledTabs` (browser filtered), `rightPanelTab: "browser"`, `rightPanelVisible: true`, `pendingBrowserUrl: url` (`useAgentStore.ts:894-903`) and calls no `openExternal`. The two new assertions read exactly those values off the post-flush snapshot.
- **They read after the write.** The store write happens only after `await browserWebviewSupported()` (mocked `mockResolvedValue(true)` — one microtask hop; the rejection branch is irrelevant when the value resolves). `flushRouter()` is `setTimeout(0)` (`:46-47`), a macrotask, and the entire microtask chain is drained before it fires. No fake timers exist anywhere under `frontend/` — `search` for `useFakeTimers|useRealTimers|setupFiles|fakeTimers|restoreMocks` over 242 files: no matches, and `frontend/vitest.config.ts` configures no `setupFiles` — so the flush really is a macrotask after the router chain.
- **Empirical confirmation in the same file:** the external-href case (`:154-165`) asserts `pendingBrowserUrl === "https://example.com/docs"` behind the identical flush, and the hand-off suite is green (84/84 files) — the flush demonstrably reaches post-await store writes, so the sibling assertion set is not vacuous and neither is this one.
- **Under the regression the new assertions fail, not only the old ones.** Pre-fix (round-2's finding) `pendingFileOpen === null` and `openExternal`-not-called both stayed green while the store already held `pendingBrowserUrl: "mailto:a@b.c"` and `rightPanelTab: "browser"`. The `beforeEach` pins `rightPanelTab: "plan"` (`:112`) and `pendingBrowserUrl: null` (`:116`), so both new expectations discriminate: `expect(s.pendingBrowserUrl).toBeNull()` fails on the written URL, `expect(s.rightPanelTab).toBe("plan")` fails on `"browser"`.
- **Correct path still passes.** `MarkdownLink.tsx:41`'s `/^https?:\/\//i` is not widened, so `mailto:a@b.c` never reaches `openChatLink`; it falls through to `normalizeLocalFileHref`, where `markdownLink.ts:13`/`:56` (`SCHEME_RE = /^[a-zA-Z][a-zA-Z0-9+.-]*:/`, tested on the decoded form) returns `null` → no `openFileInViewer`, no store write, no shell open. The store stays exactly as `beforeEach` left it, so all four assertions pass. (Belt and braces: `openChatLink.ts:59` rejects non-http(s) before any side effect, so even a single-place widening cannot write the store from the chat; and the router-level twin `openChatLink.test.ts:97-116` awaits `openChatLink` per target — `mailto:`, `ftp:`, `data:`, `file:`, `javascript:`, relative, fragment, empty — with `pendingBrowserUrl`/`rightPanelTab`/`openExternal` assertions.)

## Evidence — (2) no production/source file changed since round 2

Every file round 2 cited re-reads at the same content and the same line positions, so round 2's per-fix verification is undisturbed:

| Round-2 citation | Current source |
| --- | --- |
| `BrowserView.tsx` `:58` areaRef, `:61` loadToken | same |
| `loadIntoChild` = `useCallback(async …, [])`, `:111-144` | same |
| token taken after the empty guard `:112-113`; guards after every await `:118`/`:133`/`:136`/`:141` | same |
| rect read + dpr at call time `:122-131` (`window.devicePixelRatio` `:125`) | same |
| `openUrl` routes through it `:147-149`; Enter `:222`, Open `:244` | same |
| consume effect `:159-170`, deps `[pendingBrowserUrl, supported, loadIntoChild]`, `null` early return, `!supported` clear-and-return, `void loadIntoChild(...)` + synchronous clear | same |
| `openChatLink.ts` `:51-59` parse+scheme gate, `:62-65` modifier → `openExternal`, `:71-76` optimistic rejection default, `:77-82` non-Windows fallback, `:83` `requestBrowserOpen` | same |
| `MarkdownLink.tsx` `:41` http(s) gate, `:88` `ctrlKey \|\| metaKey`, `:90-96` middle-click `onAuxClick` | same |
| `useAgentStore.ts` `:251-259` decl, `:668` initial `null`, `:894-903` write, `:904` clear | same |
| `Message.tsx` chip `:619-644`, modifier `:635` | same |
| `openChatLink.test.ts` `:97-116` scheme suite, `:149-150` `areaRef.current`/`getBoundingClientRect` rect pins | same |
| `vitest.config.ts:70` registration; `docs/FEATURES.md:56`, `:69` | same |
| `git status --short` / `git diff HEAD` file set | identical to round 2's scope line; no Rust file, no new production file |

In `MarkdownLink.test.tsx` the diff shape is itself the evidence of confinement: the flush (`:192`) and the `beforeEach` (`:110-122`, `rightPanelVisible: false` at `:114`, `pendingBrowserUrl: null` at `:116`, mock reset/re-arm at `:118-121`) sit exactly where round 2 recorded them; the six added lines are all inside the case body `:187-201`; and every subsequent line shifted uniformly by +6 — round 2 placed the sibling `data:`/`file:` case at `:197-210`, it is now `:203-216`, with no other drift. A uniform shift with the edited region first is what a single-body insert produces; any second edit elsewhere would break the 1:1 line correspondence.

## Evidence — (3) no leakage, no weakened assertion, other cases unaffected

- **No mutation, no leakage.** The case reads the store and inspects a mock; it never calls either mocked function on the correct path, so mock call state for later cases is untouched (and is reset regardless). The `beforeEach` re-sets all four store slices explicitly, so the case's outcome cannot depend on, and cannot influence, the `data:`/`file:` case that follows (which starts from an explicit `"plan"` tab and null pending values).
- **Retained assertions intact.** `expect(s.pendingFileOpen).toBeNull()` and `expect(vi.mocked(openExternal)).not.toHaveBeenCalled()` are unchanged in substance — one snapshot taken after the flush, nothing mutating the store between the snapshot and the reads. No assertion was dropped or softened; the case's failure surface only widened.
- **Other cases unaffected.** The external / ctrl-click / non-Windows-fallback cases (`:154-185`), the rendered-affordance describe (`:219-267`) and the `Message.tsx` source contract (`:269-281`) are at their round-2 content, shifted only by the uniform +6 offset.

## Residual observations (noted, not findings)

- The new expectations are blind to a hypothetical *timer-deferred* store write (the flush is a single macrotask). No shipped path produces one — the only await before the write is the mocked/resolved platform probe — and the macrotask flush is the established, empirically load-bearing seam in this file (the sibling external case depends on it). Noted only.
- As rounds 1–2 established, the chat-level case can only ever exercise a two-place widening; the single-place widening (a relaxed `openChatLink.ts:59`) is covered at the router level (`openChatLink.test.ts:97-116`). With the new assertions, all three opener channels (tab / file / OS browser) are now distinguishable at the chat seam too.
- This reviewer's allow-list has no shell tool, so the verification runs were not re-run here. Hand-off reports `npx tsc --noEmit` exit 0 and `npx vitest run` 84/84 files; root `cargo test` was green at hand-off and no Rust file appears in `git status`/`git diff HEAD`, so the Rust suite is unaffected by this plan. Statically re-confirmed the two conditions the new assertions depend on: real `setTimeout` (no fake timers anywhere under `frontend/`) and `src/components/chat/MarkdownLink.test.tsx` in the vitest include list (`vitest.config.ts:24`).
