## Verdict: FINDINGS (0 high, 4 low)

Review of the uncommitted bug-fix changeset on `wt/agenticcoding` — "Chat markdown file links open through the sanctioned openers" (plan 66a5ee24, user report 2027-01-07). The core defect (CSP-dead raw `<a>` anchors in chat markdown) is correctly fixed with minimal, well-tested wiring; all four findings are rare-input normalizer/component edge cases with graceful failure modes. No high-severity issues; no security regressions.


## Scope reviewed

All uncommitted changes on `wt/agenticcoding` (git diff HEAD + untracked files):
- `frontend/src/lib/markdownLink.ts` (NEW) — `normalizeLocalFileHref`
- `frontend/src/components/chat/MarkdownLink.tsx` (NEW) — `openMarkdownTarget` + `MarkdownLink`
- `frontend/src/components/chat/Message.tsx` — `a: MarkdownLink` wiring beside the existing `code` override
- `frontend/src/components/chat/MarkdownLink.test.tsx` (NEW) + `frontend/vitest.config.ts` registration
- `README.md` (one chat-features line), `.coding/knowledge/spec/…`, `.coding/plans/66a5ee24.md`, `.coding/backlog.jsonl` (bookkeeping)

## What was verified (all green)

- **Root cause + fix correctness.** Message.tsx:304-328 previously passed only a `code` override, so `[report](.coding/reviews/x.md)` rendered as a raw `<a href>` anchor — dead under the production CSP (`default-src 'self'`). The new `a: MarkdownLink` override routes local hrefs through `openFileInViewer` (lib/openFile.ts:27 → `requestFileOpen`, useAgentStore.ts:839-848 — sets `pendingFileOpen {path, line}` + reveals/enables the Files tab) and external http(s) through `openExternal` (lib/openExternal.ts:26 — own URL parse + http/https-only protocol gate before the Tauri shell open, matching the webfetch chip pattern at Message.tsx:592-608). The `code` override is byte-for-byte unchanged.
- **Store + viewer contract.** FileViewer consumes `pendingFileOpen` on mount/change (FileViewer.tsx:220-225) → `loadFile` → SourceEditor; a failed read degrades to `setError(errMsg(err))` (SourceEditor.tsx:312) — every wrong-path edge case below fails as a graceful in-viewer error, never a crash.
- **Security.** No traversal munging in the frontend: `..` segments pass through untouched and the backend sandbox rejects them (files.rs:583 pins `repo_relative_pathspec(&sandbox, "../outside.txt")` → err). Percent-decoding cannot smuggle a scheme past the router: the http(s) test runs on the raw href, and the decoded-form scheme check (`SCHEME_RE`) rejects `%68ttps://…`-style inputs. `openExternal` re-validates protocol itself. React escapes the interpolated `title` and children; react-markdown's default `urlTransform` sanitizes `javascript:`-style hrefs before the component sees them, and a falsy/empty href renders as a plain span. Encoded slashes (`%2F`) and encoded dots (`%2E%2E`) decode to literal path characters that the backend sandbox then gates — correct semantics, no bypass.
- **Regression test quality.** `localFileHrefRoutesThroughRequestFileOpen` (named, code-graph-pointable) drives the REAL `requestFileOpen` store action — asserts `pendingFileOpen {path, line: null}` + `rightPanelTab: "files"`; `openExternal` is factory-mocked so `@tauri-apps/plugin-shell` never loads in node. Static-markup tests pin the affordances (local: `role="button"`, `cursor-pointer`, NO `href`; external: keeps `href`; mailto: plain span, no `role="button"`), and the `?raw` source contract pins `a: MarkdownLink` in Message.tsx. RED phase documented (module-not-found against pre-fix code). The file is registered in vitest.config.ts's include list, satisfying the vitestInclude guard (vitestInclude.test.ts).
- **Docs + platform neutrality.** The README line accurately describes the shipped behavior (including the `./`, leading-`/`, backslash, percent-encoded normalizations and the plain-text fallback). Module doc comments match the code. Backslash normalization is Windows-friendly; on macOS a literal-backslash filename misresolves to a graceful not-found (inherent ambiguity, acceptable). Scheme rejection correctly covers Windows drive letters (`C:`). No platform-specific APIs introduced.

## Findings

### LOW 1 — percent-decode before fragment-strip conflates an encoded literal `#` with a fragment separator

`frontend/src/lib/markdownLink.ts:34-47` — decode runs first, then the FIRST `#` is stripped as a fragment. A file whose name contains `#` must be percent-encoded in markdown (`[x](src/a%23b.md)`); the normalizer decodes it to `src/a#b.md` and then truncates at the `#`, returning `src/a` — the link opens the wrong path (or a not-found). Fix: strip the fragment on the RAW href (before `decodeURIComponent`) — a raw `#` is the separator, and `%23` then survives decoding as a literal hash. Keep the scheme check where it is (decode-then-check is what correctly rejects smuggled schemes); only the fragment strip needs to move. Add a normalizer test for `src/a%23b.md` → `src/a#b.md`.

### LOW 2 — query strings are not stripped

`frontend/src/lib/markdownLink.ts` — `src/foo.ts?x=1` passes through unchanged (no `#`, no scheme, no prefix to strip) and is handed to `openFileInViewer` as a path containing `?x=1` → not-found error in the viewer. Graceful, but the link dead-ends for a form a model could plausibly emit (GitHub-style URLs pasted with a query). Fix: strip at the first raw `?` alongside the fragment strip (same raw-before-decode pass as LOW 1). Add a test.

### LOW 3 — UNC paths normalize to a bogus project-relative path

`frontend/src/lib/markdownLink.ts:43,51-54` — the protocol-relative `//` check runs on the pre-conversion string, so `\\server\share\x.md` (backslashes) passes it, converts to `//server/share/x.md`, then loses its leading slashes to the root-relative strip and returns `server/share/x.md` — a nonexistent relative path → not-found error. Fix: run the `//` rejection AFTER the backslash→forward-slash conversion (or re-check), so UNC forms are rejected as non-local and render as plain text instead of a dead-end open. Windows-only input, graceful failure — low.

### LOW 4 — markdown link `title` attributes are silently dropped

`frontend/src/components/chat/MarkdownLink.tsx:50-56` — the props type destructures only `href`/`children`; react-markdown also passes the author's `title` (`[x](path "the title")`), which is discarded in favor of the fixed "Opens … in the Files tab" / "Opens in your browser" titles. Minor authoring-surface fidelity loss. Fix if desired: accept `title?: string` and merge it into (or suffix) the fixed title. Cosmetic; also fine to close as won't-fix with a doc-comment note.

## Notes (no action required)

- Scope is chat message text only; FileViewer preview / PlanProgress / BacklogView markdown links still render raw anchors — documented as a potential follow-up in the SPEC, consistent with the plan's goal. Not a regression (pre-existing behavior, untouched).
- The BUG: memory is auto-captured by the bug_fixing plan kind at finish — expected absent at review time, per the task note.
- `openMarkdownTarget` re-normalizes the href the component already normalized (harmless duplication, keeps the exported router self-contained for the test seam).
