# Review: resize-bar highlight hairlines (fix/resize-handle-hairlines)

**Scope:** all uncommitted changes vs HEAD — `frontend/src/components/chat/InflightBar.tsx`, `frontend/src/components/views/FileViewer.tsx`, `.coding/backlog.json`, `.coding/plans/stack.json`, untracked `.coding/plans/408e0de6-….md`.

**Plan intent:** extend the app's standard resize-affordance motif (1px flanking hairline in the `border-border` token + centered grip pill) to the two remaining drag strips — the InflightBar top handle and the FileViewer tree|content splitter. The scrollbar-thumb-inset half of the backlog item was already satisfied by `globals.css` + `scrollbar.test.ts` and correctly left untouched.

## Verdict

**No correctness, bug, security, or constitution findings.** The diff is clean and does exactly what the plan intended. Details verified below; two informational notes and one non-blocking suggestion follow.

## Correctness — verified

- **`border-y` is a real utility here.** `tailwindcss ^3.4.15` (`frontend/package.json:35`); `border-y` (1px top+bottom width) is a core borderWidth utility since v3.0, and its exact sibling `border-x` already ships in `App.tsx:675` (`border-x border-border … hover:bg-cyan-500/20`) — same generator, same build. Tailwind preflight (`box-sizing: border-box`, `border-width: 0; border-style: solid` on all elements) means `border-y border-border` renders as two 1px solid lines with no companion classes needed.
- **`border-border` resolves and is theme-aware.** `frontend/tailwind.config.ts:14-15` defines `colors.border.DEFAULT = var(--border-color)`; `globals.css:14`/`:41` theme it (#334155 dark / #cbd5e1 light). The grip pill's `bg-border` rides the same token.
- **InflightBar geometry — no doubled or lost hairlines.** `InflightBar.tsx:124` container keeps `bg-bg-secondary`, loses `border-t`; the handle (`:135`, first child) now draws `border-y`. Under border-box, `h-1.5` (6px) = 1px line + 1px gap + 2px pill + 1px gap + 1px line — matching the updated comment (`:125-132`). The top hairline now paints at the same pixel the old container `border-t` did (handle is the first child), so no separation is lost above the bar; the new bottom hairline faces the compact toggle button (`:144-148`, borderless) — no doubling. When the reasoning panel expands (`:344-365`) it renders below the compact bar, unaffected by the handle's borders. Net effect: the strip is 1px shorter overall (7px → 6px, border moved inside the fixed height) — imperceptible, not a defect.
- **FileViewer geometry — no doubled hairlines.** Handle `FileViewer.tsx:569` gained `border-y border-border`; its neighbors are the tree pane (`:549`, borderless) and the content pane (`:575`, borderless). The header's `border-b` (`:490`) sits ≥ `TREE_MIN` 60px above the handle — never adjacent. Same 6px = 1/1/2/1/1 motif.
- **Hover wash interaction is safe.** Backgrounds paint under borders (default `background-clip: border-box`) and borders render atop the background, so the `hover:bg-cyan-500/20` wash tints the strip while both hairlines stay visible — the identical combo already proven on the App.tsx ResizeHandle. `FileViewer`'s `transition-colors` is retained; `InflightBar` never had it (unchanged).
- **className ordering** (layout → borders → hover/transition states) matches the App.tsx ResizeHandle and the surrounding codebase style in both files.
- **Scrollbar half untouched and still guarded.** `globals.css:104-112` still has the 1px transparent border + `background-clip: padding-box` + `background-color` longhand on base and `:hover`, and `scrollbar.test.ts` still asserts it. Correct to leave alone.
- **No stale references.** `border-t border-border` remains legitimately used in 21 other places (StatusBar, InputBar, dialogs, …) — nothing depended on InflightBar's container border specifically. No test or selector references the old container combo.

## Bugs — none

- **a11y:** `FileViewer.tsx:564-568` keeps `role="separator"`, `aria-orientation="horizontal"`, `aria-label`, `title` — untouched. The InflightBar drag handle has no `role="separator"` (only `title`), but that is **pre-existing** and unchanged by this diff — not a regression introduced here. The a11y-relevant toggle button's `aria-expanded`/`aria-label` are untouched and still covered by `InflightBar.test.ts:55-58`.
- **Existing contract tests unaffected:** `InflightBar.test.ts` asserts source substrings (`pb-1 group-hover:block`, `compact(agentId)`, …) — none reference the changed classNames, so the reported 295 passing vitest suites is consistent with the diff (no test edits were needed or made).
- No logic, event handlers, IPC, or state touched anywhere — className + comment changes only.

## Security — none

Display classNames and comments only; no user input, no IPC surface, no secrets. The `note` field added in `backlog.json` is a hex plan id.

## Constitution compliance — pass

- Frontend-only change; no Rust touched, so doc-comment and `#![deny(warnings)]` concerns are moot; no `#[allow(...)]` anywhere in the diff.
- Tests were run (cargo 1112 passed, vitest 295 passed, `tsc && vite build` clean per the task statement); the change introduces no warnings surface.
- Comments updated in the codebase's explanatory style and are accurate (including the border-box 6px arithmetic).

## Informational notes (no action required)

1. **`backlog.json` carries one more edit than the task described.** Beyond the status flip (`pending` → `in_flight`) + note stamp on the reviewed item, item 54's *text* was also edited (typos fixed: "falls on" → "fails on", "whetehr" → "whether"). Almost certainly a user edit via the Backlog UI, and harmless (no test references that text — verified), but the committer should know it rides along in this commit.
2. **Git CRLF warning on `FileViewer.tsx`** ("CRLF will be replaced by LF the next time Git touches it"): standard Windows autocrlf normalization — the repo stores LF, so no line-ending style change enters the commit. Benign.
3. **Non-blocking suggestion:** the codebase has a precedent of static source-contract tests pinning visual CSS details (`scrollbar.test.ts`, `InflightBar.test.ts`). If the hairline motif is considered contract-worthy, a two-line test asserting `border-y border-border` on both handles would match that precedent. Not required — this is a styling enhancement, not a defect fix, so the constitution's regression-test rule does not apply.

**Conclusion: ship as-is.** Include the `.coding/` bookkeeping (backlog.json, stack.json, the new plan file) in the commit as expected.
