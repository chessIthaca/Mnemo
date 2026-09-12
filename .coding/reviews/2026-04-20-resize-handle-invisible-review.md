# Review — ResizeHandle invisible-divider simplification

**Plan goal:** Remove the thin vertical highlight line on the divider between the left
tools sidebar and the right main panel (Graph tab). The user explicitly asked to drop
the visible handle and keep only the `w-1` spacer + resize cursor as affordance.

**Scope reviewed:** All uncommitted changes via `git diff HEAD` —
- `frontend/src/App.tsx` (substantive change)
- `.coding/backlog.json`, `.coding/plans/5539a493-….md`, `.coding/plans/stack.json`,
  `.coding/plans/50af2cd3-….md` (bookkeeping, no code impact)

---

## Correctness — ✅ PASS

Verified `frontend/src/App.tsx:560-678`:

1. **Hit area intact.** The outer div still carries
   `relative flex w-1 shrink-0 cursor-col-resize items-center justify-center` (line 670),
   and the wider invisible hit-area child `absolute inset-y-0 -left-1 -right-1` (line 675)
   is unchanged. The grabbable zone is still ~9 px wide centered on a 4 px column.
2. **Drag logic unchanged.** `onPointerDown` (lines 616-661), pointer capture
   (`setPointerCapture` / `releasePointerCapture`), clamping
   (`Math.max(300, Math.min(window.innerWidth * 0.8, px))`), live width updates
   (`onWidthChangeLive`), localStorage commit on drag-end (`onWidthCommit`), and the
   unmount-mid-drag `useEffect` cleanup (lines 608-614) are byte-for-byte identical
   to the previous revision — confirmed via the diff, which touches only the
   doc comment, the outer `className`, and deletes the grip-pill `<div>`.
3. **Removed classes are safe to remove.**
   - `group` — no descendant of `ResizeHandle` uses `group-hover:` (grep confirms the
     only remaining `group-hover:` consumers in the tree are `CodeBlock`, `InflightBar`,
     `InputBar`, `MainPanel`, `RightPanel`, `BacklogView` — all in their own
     `group` scopes, none inside `ResizeHandle`).
   - `transition-colors` — only useful when a transition target exists; with
     `hover:bg-cyan-500/20` gone, it was dead.
   - `hover:bg-cyan-500/20` — that *was* the visible wash the user asked to remove.
4. **Deleted grip pill is safe.** The pill was
   `<div className="pointer-events-none h-8 w-0.5 rounded-full bg-border" />`. No CSS
   selector, test, or sibling component references it. `bg-border` remains defined and
   used elsewhere (`Sidebar.tsx:43`, `ProjectPicker.tsx:175`, `InflightBar.tsx:132`,
   `FileViewer.tsx:342`), so no orphaned theme token.
5. **Accessibility preserved.** `role="separator"`, `aria-orientation="vertical"`,
   `aria-label="Resize tools panel"`, and `title="Drag to resize the tools panel"`
   are all still on the outer div, so screen-reader and tooltip affordances are
   unchanged.

## Bugs — ✅ None found

- No dangling references: searched for `hover:bg-cyan-500/20`, `ResizeHandle`,
  `group-hover`, `bg-border` across the repo. All remaining matches are either
  unrelated components with their own `group` scopes, or historical plan/review
  documents under `.coding/` (not code).
- No test depends on the removed styling. The only handle-adjacent frontend test is
  `frontend/src/components/chat/InflightBar.test.ts`, which asserts the InflightBar's
  own popup styling — untouched by this diff.
- No accidental removal of required classes: the outer div keeps `relative` (needed
  for the absolutely positioned hit area), `flex` + `items-center` + `justify-center`
  (needed to center the hit area vertically — although the pill is gone, the classes
  are harmless and future-proof), `w-1 shrink-0 cursor-col-resize` (the actual
  affordance), and the `onPointerDown` prop.
- **Historical context (informational, not a finding).** Plan `e1c63669` (2026-04-20)
  records an earlier user clarification that the grip pill should be KEPT and only the
  RightPanel's `border-l` was unwanted. The current plan `50af2cd3` documents a new,
  later user request explicitly targeting the handle itself ("the space alone is
  enough"). The two are consistent — user preference evolved — so no action needed.

## Security — ✅ N/A

Pure presentational change. No new props, no state, no IPC, no event surface, no
dependencies, no HTML injection vector.

## Constitution compliance — ✅ PASS

- **Doc comment updated.** The component's doc comment (lines 580-591) now carries a
  new paragraph explicitly stating the handle is intentionally invisible and why the
  `w-1` column + `cursor-col-resize` are sufficient. This satisfies the "public
  functions must have doc comments" rule (treating this exported-internal component
  as public-ish).
- **No `#[allow(...)]` introduced.**
- **No warnings silenced.** Frontend checks are reported green
  (`npx vitest run` 192/192, `npx tsc --noEmit` exit 0); I cannot re-execute them
  (read-only reviewer), but the diff itself introduces no new code that could
  warn — it only deletes JSX and removes class tokens.

---

## Verdict

**No findings.** The diff is a minimal, surgical removal of the two visual affordances
the user asked to drop. Everything else — hit area, pointer capture, clamping, live
updates, commit-on-drag-end, unmount cleanup, ARIA — is unchanged. Safe to commit.
