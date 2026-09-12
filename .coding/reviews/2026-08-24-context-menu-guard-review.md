## Verdict: FINDINGS (0 high, 2 low)

Review of all uncommitted changes for the "Suppress webview context menu" feature
(plan: suppress the native right-click menu across the Mnemo UI except inside
editable fields; the Browser tab's separate child WebView2 is intentionally
untouched).

**Scope reviewed** — `git diff HEAD` + untracked files:
- `frontend/src/lib/contextMenu.ts` (NEW, 86 lines)
- `frontend/src/lib/contextMenu.test.ts` (NEW, 38 lines, 10 table-driven cases)
- `frontend/src/main.tsx` (modified — import + one top-level call)
- `frontend/vitest.config.ts` (modified — test registered in `include`)

No other source files changed (the only other untracked item is the plan's own
`.coding/plans/03543ad2-….md` bookkeeping file).

---

### Findings

#### LOW 1 — `contenteditable` attribute values matched case-sensitively (spec gap)
`frontend/src/lib/contextMenu.ts:34-36`

`isEditableContext` compares the attribute value with strict equality:
```ts
contentEditableAttr === "" ||
contentEditableAttr === "true" ||
contentEditableAttr === "plaintext-only"
```
The HTML spec defines `contenteditable` as an **enumerated attribute whose
keywords are matched case-insensitively**, so `contenteditable="TRUE"`,
`"True"`, or `"PLAINTEXT-ONLY"` are valid editable states. With the current
strict-`===` check, such a value falls through to "not editable" and the native
context menu would be suppressed on a host the spec considers editable.

**Impact today: none.** A search of the frontend (`**/*.ts`, `**/*.tsx`)
confirms the app renders **no** `contenteditable` attributes anywhere — the only
non-test hit is a JSDoc *comment* in `ApprovalPrompt.tsx:93` (and that file's
actual code at `:106` uses the `t.isContentEditable` DOM property, not the
attribute). React also serializes `contentEditable` values to lowercase. So this
is a spec-completeness gap, not a live bug.

**Suggested fix (one line):** normalize before comparing —
`const a = contentEditableAttr?.toLowerCase() ?? null;` then compare `a === ""`,
`a === "true"`, `a === "plaintext-only"`. (Alternatively, `isEditableTarget`
could consult `host.isContentEditable`, which already encodes the full spec —
but that would lose the pure/DOM-free testability the plan deliberately chose,
so the lowercase-normalize is the lighter touch.) Add one table row to
`contextMenu.test.ts` (e.g. `["DIV", "TRUE", true]`) to lock it in.

#### LOW 2 — Docs sync: README feature list has no mention of the new behavior
`README.md` (lines 55-73, "Agents & interface" feature list)

The app now suppresses the native right-click menu across its UI (a
user-visible behavior change — users can no longer right-click for the
browser's native menu outside editable fields). The README's granular feature
list has no bullet for it. The plan explicitly asks the reviewer to report a
missing feature bullet. A one-line entry under "Agents & interface" would be
appropriate, e.g.:

> - Native right-click (context) menu is suppressed across the UI except inside editable fields (input/textarea/contenteditable), where Copy/Cut/Paste stays available; the Windows-only Browser tab keeps its full menu

`PLAN.md` (technical decisions / provider strategy) does not need an update —
this is a UI-polish feature, not an architecture/provider decision. Module-level
doc comments in `contextMenu.ts` are thorough and already synced.

---

### Clean (no findings)

**Correctness**
- Capture-phase listener (`contextMenu.ts:82`, third arg `true`) calls
  `preventDefault()` unless the target is editable (`:78-81`). Capture phase
  runs before React's root-level listeners, so the guard reliably sees every
  `contextmenu` event. `preventDefault()` only suppresses the *native* menu; it
  does **not** call `stopPropagation`, so any future component-level
  `onContextMenu` custom menu would still fire — correct, non-breaking behavior.
- `isEditableContext` semantics are correct for the lowercase/empty cases per
  the HTML spec: `""`, `"true"`, `"plaintext-only"` → editable; `"false"`,
  `"inherit"`, `null` → not (`:32-37`). `<input>`/`<textarea>` always editable
  (`:32`).
- Text-node-inside-contenteditable resolution is correct: `asElement`
  (`:47-51`) returns `target.parentElement` for a `Text` node, and
  `isEditableTarget` (`:58-66`) walks `el.closest("[contenteditable]")` to the
  nearest host — so a right-click on selected text inside a contenteditable host
  still resolves as editable. Nearest-host semantics also correctly handles a
  `contenteditable="false"` subtree nested in a `"true"` host.
- Guard installed **exactly once** at app entry: `main.tsx:18` is a top-level
  module statement, not inside a component/effect, so React StrictMode (which
  only double-invokes component renders/effects) does **not** double-install it.
  `main.tsx` is the Vite entry and is not re-executed on HMR of imported modules,
  so no accumulation in dev either.
- The returned cleanup function is intentionally unused (`main.tsx:18` discards
  it). Confirmed **not a problem**: the listener is meant to live for the page
  lifetime, there is a single add with no re-add path, and one listener on
  `document` is not a leak (document persists for the page). `tsc --noEmit`
  accepts the discarded return (no `@mustUse`/unused-binding error); implementer
  reports exit 0.

**Bugs**
- No listener leak: single `addEventListener` with a matching `removeEventListener`
  (same `true` capture flag, `:82`/`:84`) in the returned cleanup.
- No regression to existing `preventDefault` handlers: a frontend-wide search
  for `contextmenu`/`onContextMenu`/`ContextMenu` returns matches **only** in the
  new `contextMenu.ts` and its `main.tsx` import — there was **no pre-existing
  `contextmenu` listener** to shadow or break. Existing keyboard/drag guards
  (e.g. `ApprovalPrompt.tsx:97-109` keydown handler) are unrelated event types.

**Security**
- No change to CSP, IPC, the Browser tab's CDP gating, or any network surface.
  Suppressing a client-side context menu does not widen the attack surface; no
  new IPC commands, no new Tauri capabilities, no new origins. The Browser tab
  (`BrowserView.tsx`) is not among the changed files and keeps its independent
  native context menu.

**Constitution / project rules**
- Doc comments present on **all** exported functions: `isEditableContext`
  (`:15-27`), `isEditableTarget` (`:53-57`), `installContextMenuGuard`
  (`:68-76`); plus a module-level doc comment (`:5-13`). The non-exported
  `asElement` is documented too.
- `tsc --noEmit` reported clean (exit 0) by the implementer; the code is
  type-clean on inspection (correct `MouseEvent`/`Element`/`Text` typing,
  capture-flag-matched remove).
- Test registered in `frontend/vitest.config.ts:38` (`"src/lib/contextMenu.test.ts"`
  in the `include` array) — colocated-test registration rule satisfied. The 10
  `it.each` rows match the plan's "10 new cases" claim and cover INPUT/TEXTAREA,
  the three editable attr values, the three non-editable values, a nested-tag
  case, and the no-tag/no-host case.
- Multi-platform neutrality: pure TypeScript running in the agent-chat webview on
  **all** platforms (macOS/Windows/Linux); no platform-specific APIs, paths, or
  shell. The Browser tab remains behind its existing Windows-only gate and is
  untouched.
- No `#[allow]`/`eslint-disable` added. Line-ending style preserved (LF; the diff
  shows no CRLF conversion warnings and existing files are LF).
- No Rust files changed, so `cargo test` was not required for this frontend-only
  change.

**Verification status (as reported by implementer, not re-run by reviewer —
reviewer is read-only):** `npx tsc --noEmit` → exit 0; `npx vitest run` → 45
files / 540 tests pass (incl. the 10 new cases). Both are consistent with the
code as read.
