## Verdict: PASS

Verification review of commit `00733eb` on branch `feat/context-menu-guard`
("Suppress webview context menu in Mnemo UI"). This is a focused re-review
confirming that the two LOW findings from
`.coding/reviews/2026-08-24-context-menu-guard-review.md` are correctly fixed in
the committed changes, and that the fixes introduced no new issues.

**Scope reviewed** — `git show HEAD` (commit 00733eb; no uncommitted changes per
the task). The commit bundles the original feature + both fixes + the prior
review report in one commit. Changed source files:
- `frontend/src/lib/contextMenu.ts` (new, 85 lines)
- `frontend/src/lib/contextMenu.test.ts` (new, 41 lines, 12 table-driven cases)
- `frontend/src/main.tsx` (guard installed once at app entry)
- `frontend/vitest.config.ts` (test registered in `include`)
- `README.md` (feature bullet)

---

### LOW 1 — case-insensitive `contenteditable` matching — FIXED ✓
`frontend/src/lib/contextMenu.ts:33-36`

The fix applies exactly the normalization the prior review suggested:
```ts
// The `contenteditable` enumerated attribute matches its keywords
// case-insensitively per the HTML spec, so normalize before comparing.
const a = contentEditableAttr?.toLowerCase() ?? null;
return a === "" || a === "true" || a === "plaintext-only";
```

Verified by tracing every input the signature (`contentEditableAttr: string | null`) admits:

| Input | After `?.toLowerCase() ?? null` | Result | Correct? |
|-------|----------------------------------|--------|----------|
| `"TRUE"` | `"true"` | editable | ✓ (the fix) |
| `"Plaintext-Only"` | `"plaintext-only"` | editable | ✓ (the fix) |
| `"True"` / `"PLAINTEXT-ONLY"` | `"true"` / `"plaintext-only"` | editable | ✓ |
| `""` | `""` | editable | ✓ (preserved) |
| `"true"` / `"plaintext-only"` | unchanged | editable | ✓ (preserved) |
| `"false"` | `"false"` | not editable | ✓ (preserved) |
| `"inherit"` | `"inherit"` | not editable | ✓ (preserved) |
| `null` | `null` (via `null?.…` → `undefined` → `?? null`) | not editable | ✓ (null-safe) |
| `undefined` (graceful) | `null` | not editable | ✓ |

- **Null/undefined safety:** correct. Optional chaining (`?.`) yields `undefined`
  for `null`/`undefined`, and `?? null` collapses that to `null`, which matches
  none of the three editable literals. No `TypeError` path.
- **Preserved semantics:** the three editable values (`""`, `"true"`,
  `"plaintext-only"`) still match after lowercasing; the three non-editable
  values (`"false"`, `"inherit`, `null`) still don't. The fix is purely
  additive — it only widens the match to case variants; it changes no
  previously-correct outcome.
- **Case folding correctness:** the HTML spec matches `contenteditable`
  keywords **ASCII case-insensitively**; the keywords (`true`/`false`/`inherit`/
  `plaintext-only`) are all ASCII, and `String.prototype.toLowerCase()` lowercases
  ASCII A–Z identically to ASCII folding. No non-ASCII character can fold into an
  ASCII keyword, so there is no false-positive risk from Unicode case mapping.

**Regression test rows** — `frontend/src/lib/contextMenu.test.ts:27-29`:
```ts
// Enumerated attribute keywords match case-insensitively (HTML spec).
["DIV", "TRUE", true],
["DIV", "Plaintext-Only", true],
```
Both assertions are correct: `"TRUE".toLowerCase()` → `"true"` → editable; and
`"Plaintext-Only".toLowerCase()` → `"plaintext-only"` → editable. Each row would
fail under the old strict-`===` code (which returned `false` for both) and passes
with the fix — a genuine regression lock. The suite grew 10 → 12 cases; the
implementer's reported total (540 → 542 tests) is consistent with +2 rows.

---

### LOW 2 — README docs sync — FIXED ✓
`README.md:67`

The feature bullet was added under the **Agents & interface** list (header at
`:55`), placed between the "React + Tailwind UI" bullet (`:66`) and "Dark +
light themes" (`:68`):

> - Native right-click (context) menu is suppressed across the UI except inside editable fields (input/textarea/contenteditable), where Copy/Cut/Paste stays available; the Windows-only Browser tab keeps its full menu

The text matches the prior review's suggested wording verbatim and accurately
describes the shipped behavior:
- "suppressed across the UI except inside editable fields (input/textarea/
  contenteditable)" — matches `isEditableContext` (`INPUT`/`TEXTAREA` always
  editable; `contenteditable` `""`/`"true"`/`"plaintext-only"` editable).
- "where Copy/Cut/Paste stays available" — correct: `preventDefault()` is only
  called when the target is **not** editable (`contextMenu.ts:78-79`), so editable
  fields keep the native menu (and its Copy/Cut/Paste entries).
- "the Windows-only Browser tab keeps its full menu" — correct: the listener is
  attached to `document` of the agent-chat webview only; the Browser tab is a
  separate native child WebView2 with its own document, unaffected by this guard
  (module doc comment `:5-13` documents this).

`PLAN.md` was correctly left untouched (UI-polish feature, not an
architecture/provider decision), as the prior review noted.

---

### No new issues introduced

- **No regressions:** the only source change to `contextMenu.ts` is the
  one-line normalization + its explanatory comment (`:33-36`). It is
  behavior-preserving for all 10 previously-tested cases and only adds
  case-insensitive matching. `isEditableTarget`, `asElement`, and
  `installContextMenuGuard` are byte-identical to the already-reviewed versions.
- **No type errors:** `contentEditableAttr?.toLowerCase()` is `string | undefined`;
  `?? null` narrows to `string | null`; comparing that to string literals via
  `===` is valid TypeScript. Implementer reports `npx tsc --noEmit` → exit 0,
  consistent with the code as read.
- **No dead code:** the local `a` is consumed by the `return`; the comment is
  accurate and useful. No unused imports/bindings added.
- **Test registration:** `"src/lib/contextMenu.test.ts"` is present in the
  `include` array of `frontend/vitest.config.ts` (added after
  `src/lib/answerSelect.test.ts`, alphabetical ordering preserved).
- **Line endings / lint:** no `#[allow]`/`eslint-disable` added; the diff shows
  no CRLF conversion; existing files are LF.

### Overall feature still correct

- Capture-phase `contextmenu` listener on `document` calls `preventDefault()`
  unless the target is editable (`contextMenu.ts:76-85`; third arg `true` =
  capture phase at `:81`; `isEditableTarget(e.target)` guard at `:78`).
- Installed **exactly once** at app entry: `installContextMenuGuard()` is a
  top-level statement in `main.tsx` (before `ReactDOM.createRoot`), not inside a
  component/effect, so React StrictMode does not double-install it and HMR of
  imported modules does not re-execute the entry.
- The Browser tab (separate child WebView2) is unaffected — its context menu
  lives on its own document; this guard only touches the agent-chat webview.
- `preventDefault()` suppresses only the native menu; it does **not** call
  `stopPropagation`, so any future component-level `onContextMenu` custom menu
  would still fire (non-breaking).

### Verification status (as reported by the implementer; reviewer is read-only)

- `cd frontend; npx tsc --noEmit` → exit 0 (clean).
- `cd frontend; npx vitest run` → 45 files, 542 tests pass (incl. the 12
  `contextMenu` cases).

Both are consistent with the committed code as read. No Rust files changed, so
`cargo test` was not required for this frontend-only change.

---

Both prior LOW findings are correctly fixed, regression tests lock the fix, docs
are synced, and no new issues were introduced.
