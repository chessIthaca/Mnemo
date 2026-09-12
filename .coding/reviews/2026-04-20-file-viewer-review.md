# Review: Unified File viewer + clickable file links + syntax highlighting

**Branch:** `feat/file-viewer` · **Date:** 2026-04-20
**Scope reviewed:** ALL uncommitted changes (`git diff HEAD` + untracked files): `frontend/src/lib/language.ts`, `language.test.ts`, `toolCardPaths.ts`, `toolCardPaths.test.ts`, `openFile.ts`, `components/chat/CodeBlock.tsx`, `components/views/FileViewer.tsx`, `components/chat/Message.tsx`, `components/chat/Markdown.tsx`, `hooks/rightPanelViews.tsx`, `hooks/agentState.ts`, `hooks/useAgentStore.ts`, `components/settings/sections/AdvancedSection.tsx`, `vitest.config.ts`, deletions of `MdViewer.tsx`/`FileBrowser.tsx`, plan bookkeeping.

---

## Findings by severity

### HIGH — 1. `openFileInViewer` event races the FileViewer mount: the file-open event is LOST whenever the Files tab is not already mounted

**Files:** `frontend/src/lib/openFile.ts:10-18`, `frontend/src/components/views/FileViewer.tsx:162-168`, `frontend/src/components/layout/RightPanel.tsx:114-120`, `frontend/src/App.tsx:566`

`openFileInViewer` does:

```ts
useAgentStore.getState().revealRightPanelTab("files");
window.dispatchEvent(new CustomEvent("myharness:open-file", { detail: path }));
```

The comment in `openFile.ts:11-15` claims zustand `set` is synchronous, "so by the time dispatchEvent runs the Files tab is active and the FileViewer is mounted/listening". That claim is wrong for React 18:

- zustand's `set` updates the store synchronously, but the notification reaches components via `useSyncExternalStore`, which **schedules** a re-render — it does not render synchronously.
- The `FileViewer` component only mounts (and only then attaches its `myharness:open-file` window listener in a `useEffect`, FileViewer.tsx:162-168) after React processes that scheduled render and commits `App.tsx:566` → `RightPanel` → `FileViewer`.
- `window.dispatchEvent` on the very next line runs **before** any of that: the listener is not attached yet, so the event is dropped on the floor.

Concrete failure case (the default state): on startup `disabledTabs` is `ALL_RIGHT_PANEL_TABS.filter((t) => t !== "plan")` (useAgentStore.ts:507), so `"files"` is **disabled by default** and `FileViewer` is **not mounted**. A user clicking a tool-card file link in this default state gets: the right panel opens and switches to the Files tab (the store update works), but the file never loads — the viewer shows "Click a file in the tree above…". The feature's primary user flow (goal #1: "clicking opens the file, auto-opening the panel + Files tab") is broken exactly in the auto-open scenario it was built for. It only works when the Files tab already happened to be enabled, visible, and mounted.

This race existed in the old FileBrowser→MdViewer flow too, but the old dispatch only fired from within the already-mounted FileBrowser and was best-effort; here the deep-link is the headline feature, so it must be reliable.

**Fix options (any one):**
1. Have `FileViewer` read a pending path from the store instead of a fire-and-forget event: `openFileInViewer` sets e.g. `pendingFileOpen: path` in the store (in the same `set` as the tab reveal); `FileViewer` on mount (and on change) consumes it via `useEffect` and clears it. This is race-free by construction.
2. Defer the dispatch: `setTimeout(() => window.dispatchEvent(...), 0)` — works in practice (React flushes the scheduled render on the next task boundary before a timeout fires in almost all cases) but is a heuristic, not a guarantee.
3. `flushSync(() => useAgentStore.getState().revealRightPanelTab("files"))` from `react-dom` before dispatching — forces the synchronous commit the comment assumes. Works but pulls `flushSync` into app code for a marginal reason.

Option 1 (store-held pending path) is recommended — it also survives the viewer being unmounted/remounted for other reasons (e.g. tab toggled off/on).

---

### LOW — 2. `read_files` chips imply multiple links but only the first path is clickable

**File:** `frontend/src/components/chat/Message.tsx:344-356` (chips builder), `:393-408` (render)

For a `read_files` call with paths `[a.rs, b.rs, c.rs]`, the chip text is `"a.rs, b.rs, c.rs"` (basename, capped at 3) and the single chip's click opens only `paths[0]`. The chip styling (cyan link) suggests each listed name is a link, but b.rs/c.rs are dead text inside the link for a.rs. Functionally acceptable, but slightly misleading UI. Consider rendering one link chip per path (cap the *chips* at 3 with a `+N` plain-text overflow), so every visible name is the link it appears to be.

### LOW — 3. Plan file contains a stale, unimplemented migration step

**File:** `.coding/plans/5539a493-bdd3-4aad-bc9a-c7d63847fa6f.md` (step 3)

Step 3 says: *"Add a normalize-on-load in the store init so a persisted `rightPanelTab:"md"` in localStorage maps to 'files'."* Verified against the code: `rightPanelTab` and `disabledTabs` are **session-only** — `useAgentStore.ts:506-507` hardcodes `rightPanelTab: "plan"` / `disabledTabs: ALL_RIGHT_PANEL_TABS.filter(...)` with no `readLs` for either (only `LS_RIGHT_PANEL_WIDTH` is persisted). So the migration was correctly unnecessary and correctly omitted — but the checked-off plan step still asserts it was done. Harmless to the code, misleading to future readers of the plan. Amend the step text (or its checkmark annotation) to record that the normalize was dropped because tab state is not persisted.

### LOW — 4. `listMarkdownFiles` is now dead frontend code

**File:** `frontend/src/lib/tauri.ts:966`

After deleting `MdViewer.tsx`, nothing imports `listMarkdownFiles` (searched `frontend/**/*.ts*` — only the definition matches). The exported wrapper (and possibly the backend `list_markdown_files` command it invokes) is now unreachable from the UI. Not a runtime problem, but it is dead code in a repo whose constitution requires rooting out dead code under `#![deny(warnings)]` discipline. Either remove the wrapper (+ backend command if nothing else uses it) or note it as intentionally retained.

### LOW — 5. Hidden/dot-directory behavior changed silently with the merged tree

**Files:** `frontend/src/components/views/FileViewer.tsx:98-102, 325`, `frontend/src/components/settings/sections/AdvancedSection.tsx:419-424`

The old `MdViewer` dropdown hid dot-directories unless "Show hidden" was checked; the merged `FileViewer` tree shows *all* entries (including `.git`, `.coding`, …) unless their name appears in `skip_dirs`. The settings copy was updated accordingly (good), but on a default config the tree may now show dot-directories the user never saw before. Behavior is coherent and arguably better (the full tree should show everything); flagged only so the change is a conscious decision, and so a sensible default `skip_dirs` (e.g. `.git, node_modules, target, dist`) is confirmed in shipped config.

### LOW — 6. No test for the new `revealRightPanelTab` store action

**File:** `frontend/src/hooks/useAgentStore.ts:658-665`

`revealRightPanelTab` is the one new piece of store logic (enable-if-disabled + select + reveal, never-disable). The repo has a table-driven `useAgentStore.test.ts` suite covering neighboring actions; a case asserting (a) disabled→enabled+selected+visible, (b) already-enabled+already-selected→no disable (the regression risk vs. `toggleTabAndReveal`) would be consistent with the project's test discipline. Not blocking.

---

## Verified correct (focus-point checklist)

- **No dangling `"md"` references** — searched `frontend/**/*.ts` and `**/*.tsx` for `"md"`, `MdViewer`, `FileBrowser`: only doc-comment mentions in `FileViewer.tsx` and `Markdown.tsx` (updated). `RightPanelTab` union + `ALL_RIGHT_PANEL_TABS` (agentState.ts:196-215) dropped `"md"`; registry (rightPanelViews.tsx:50-61) has exactly one `"files"` → `FileViewer` entry; the registry↔union sync test (rightPanelViews.test.ts:36-38 asserts equal lengths) stays green. Sidebar/RightPanel consume the registry only. `FileText` import removed.
- **No import cycle** — `openFile.ts` → `useAgentStore`; `Message.tsx` → `openFile.ts`; nothing in `useAgentStore` imports either. Acyclic.
- **Splitter drag leaks no listeners** — `FileViewer.tsx:186-219`: pointer capture on the handle; `pointermove`/`pointerup`/`pointercancel` added on window; `endDrag` removes all three + nulls `dragCleanup.current`; the unmount effect (`:71-76`) invokes `dragCleanup.current?.()`. `releasePointerCapture` wrapped in try/catch for the pointercancel-already-released case. All paths clean up.
- **Nested-button validity** — ToolCard header is now `span[role=button][tabIndex=0]` with Enter/Space `onKeyDown` + `preventDefault` (Message.tsx:378-392); file chips are real `<button>`s with `stopPropagation` (`:393-408`). No `<button>` inside `<button>`; keyboard a11y retained.
- **No HTML-injection path** — `MarkdownImpl.tsx` uses only `remarkGfm` + `rehypeHighlight`; no `rehype-raw`, so ReactMarkdown escapes raw HTML in file content. `wrapCodeFence` (language.ts:63-70) computes the longest backtick run and emits a fence of `max(3, longest+1)` backticks, so embedded ``` fences cannot break out (covered by a test, language.test.ts:76-82). The info string comes only from the fixed `EXT_TO_LANG` map — no user-controlled text on the fence line.
- **`skip_dirs` filter** — applied to root entries (`FileViewer.tsx:325`) AND directory children (`:266`); `getSettings()` failure → `catch(() => setSkipDirs(new Set()))` (`:92-96`), tree still renders. Note: `s.markdown?.skip_dirs` — `markdown` is non-optional in the `Settings` type (tauri.ts:363), so `?.` is just belt-and-braces.
- **`argPaths` correctness** — `read_files` → per-spec paths; `write_review_report` bare name → `.coding/reviews/<name>` (paths already containing a slash left alone); label-only tools (shell/git/search/search_read/spawn_agent/skill_start) → `[]`; malformed JSON → `[]`. Matches the test file exactly (toolCardPaths.test.ts).
- **`revealRightPanelTab`** — only touches `disabledTabs`/`rightPanelTab`/`rightPanelVisible`; never disables; unrelated state untouched (useAgentStore.ts:658-665).
- **Session-only tab state confirmed** — `rightPanelTab: "plan"` / `disabledTabs: …` hardcoded initializers (useAgentStore.ts:506-507); no localStorage reads for either; no migration needed. Claim verified.
- **No `highlight.js` dependency** — `frontend/package.json` has no `highlight.js` entry; highlighting flows through the existing `rehype-highlight`/lowlight stack via the shared `<Markdown>` component. `CodeBlock` extraction is faithful to the original (hljs-classed `<pre>`, `language-*` on `<code>`, copy via `textContent`).
- **Constitution** — no Rust changes, so no `#[allow(...)]` concern; no new npm deps; all new public functions/components carry doc comments (`languageForPath`, `isTextPath`, `isMarkdownPath`, `wrapCodeFence`, `basename`, `argPaths`, `openFileInViewer`, `CodeBlock`, `FileViewer`, `revealRightPanelTab`).
- **`Message.tsx` refactor hygiene** — `Check`/`Copy`/`useRef` imports removed with the extracted CodeBlock; remaining imports all used; `argLabel`'s read_files display cap (3, `+N`) mirrored in chip text.

## Verdict

One HIGH finding (#1, the event/mount race) must be fixed before commit — it defeats the plan's headline feature in the default state. The LOWs are small follow-ups; #2 and #6 are worth doing in the same pass, #3/#4/#5 are bookkeeping/UX notes.
