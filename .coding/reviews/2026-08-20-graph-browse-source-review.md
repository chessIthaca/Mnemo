# Review — Graph browse-to-source: shared SourceEditor + resizable bottom pane

Branch `feat/graph-browse-source` (forked from main). Reviewed ALL uncommitted changes
(`git diff HEAD` + `git status --short`): FileViewer.tsx refactor, new
SourceEditor.tsx + SourceEditor.test.ts, GraphView.tsx + GraphView.test.ts,
vitest.config.ts, plus `.coding/` bookkeeping churn (backlog flip #55, stack.json,
new plan file — expected rider, not reviewed as code).

**Overall**: the extraction is clean — one editor, opt-in session, no dead imports,
Tailwind classes all real, doc comments in house style, GraphView B1 (log in both
branches) preserved, splitter math and localStorage-on-END correct, session
isolation between the two live instances is airtight (only `persistSession`
consumers touch `editorSession`; Graph passes neither `persistSession` nor
`browsed`). However the refactor regressed two of the old FileViewer's deliberate
invariants. Findings by severity:

---

## HIGH

### H1. The B1 unmount-straddle session writes are dead code — stale content restored under the new path, with a file-corruption path via Save
`frontend/src/components/common/SourceEditor.tsx:236-257`

The old FileViewer's `readFile(...).then(...)` had **no** `disposed` guard: after an
unmount the `setContent` calls no-op'd but the direct `editorSession.content =
text` writes still landed — that was the whole point ("if the component unmounted
while the read was in flight … the remount would restore a stale file under the
new path. (B1)").

The new code returns early on `disposed` (SourceEditor.tsx:239 and :252) **before**
those writes, so on an unmount-straddled read the session keeps whatever the
mirror effect last wrote. Repro:

1. File `W` open with unsaved edits (session dirty).
2. Click file `X` in the tree → render with `path=X` → mirror effect writes
   `editorSession.path = X`, `editorSession.content = <W's dirty text>` → load
   effect starts `readFile(X)`.
3. Switch the right-panel tab before the IPC resolves → unmount → cleanup sets
   `disposed = true`.
4. `readFile(X)` resolves → `if (disposed) return;` → session stays
   `{path: X, content: W's dirty text, savedContent: W's saved text}`.
5. Remount the Files tab → `path`/`content`/`savedContent` initialized from the
   session; `restoring` is true (`editorSession.path === path`) → the re-read is
   skipped → **W's text is shown under X's path, dirty flag armed**. Ctrl+S /
   Save then writes W's edited text **into X** (the save guard only checks
   `dirty`/`browsed`, not that the content belongs to the path).

The failure path has the same regression: the `catch` (:251-257) no longer clears
`editorSession.path` on an unmount-straddled failed read, so the remount restores
stale content under the failed path.

The guard itself is a genuine improvement for the *dep-change* race (a late read
for file A must not clobber the just-loaded file B) — it just needs to distinguish
the two cases. Minimal fix: still write the session when disposed **iff** the read
was for the path the session currently holds:

```ts
.then((text) => {
  if (disposed) {
    // Unmount-straddle (B1): the read's path still being the session path means
    // the mirror already recorded it — persist the body for the remount. On a
    // dep-change race the session path has moved on, so skip.
    if (persistSession && editorSession.path === path) {
      editorSession.content = text;
      editorSession.savedContent = text;
    }
    return;
  }
  ...
```

(and the analogous `editorSession.path = ""` in the `catch`).

## MEDIUM

### M1. Same-path loads are silent no-ops — "Discard unsaved changes?" then nothing happens; a fresh Browse pick of the same file is dropped
`frontend/src/components/views/FileViewer.tsx:159-217` +
`SourceEditor.tsx:261-264`

The load effect keys on `[path, browsed, persistSession]` (with `browsedText`
deliberately excluded). The old code re-read the file on **every**
`loadFile`/`openEntry`/`handleBrowse`; the new declarative flow only reacts to
path/browsed changes. Three visible regressions when the target equals the current
target:

- **Tree click on the currently-open file** (FileViewer.tsx:160-166):
  `confirmDiscard()` prompts; the user confirms "discard" — then `setPath(p)` is a
  no-op (same value), the effect never re-runs, and the dirty edits **survive the
  confirmed discard**. Old behavior: content re-read from disk (revert).
- **Browse re-picks the same absolute file** (FileViewer.tsx:197-217):
  `setBrowsedText(result.content)` changes, but `browsedText` is not a dep, and
  `path`/`browsed` are unchanged → the effect never runs → the freshly picked
  content is never applied. Worse, the direct `editorSession.*` writes in
  `handleBrowse` are immediately clobbered by the mirror effect (:184-191), which
  re-writes the session from the (unchanged) live state on the next render — so
  even a remount shows the old text. Old behavior: the picker's content always
  applied (the only way to refresh a preview-only file).
- **Deep link (`pendingFileOpen`) to the already-open file** (FileViewer.tsx:190-194):
  no reload from disk. Old behavior: re-read.

Suggested fix: pass a monotonically increasing `loadKey` (or `reloadToken`) prop
from FileViewer — bumped by `loadFile`, `openEntry`'s binary branch, and
`handleBrowse` — and include it in the effect's deps. That restores "every
explicit open re-reads" without re-introducing the stale-`browsedText` re-trigger
the eslint-disable guards against. Please add a regression note/test for this
(per the constitution's defect-test rule, a source-contract assertion that the
token is in the dep array is the practical ceiling without DOM infra — call that
out in a comment).

## LOW

### L1. Unavailable-branch memory log lost its height cap and scroll
`frontend/src/components/views/GraphView.tsx:544-546` (+ :214-242)

`MemoryAccessSection` lost its `max-h-32 overflow-y-auto` (now `min-h-0 flex-1
overflow-y-auto`, which only scrolls when the *parent* has a bounded height). In
the explorer layout the fixed-height pane (`:761`) bounds it — fine. But the
codegraph-unavailable branch wraps it in `<div className="border-t
border-slate-800">` with **no height**: `h-full` against an auto-height parent
computes to auto, so the log renders at full content height (up to 100 entries ≈
1400px+) with no scrollbar, overflowing the tab (clipped by an ancestor). The B1
invariant (log visible in both branches) holds, but the branch regressed from
"capped + scrollable" to "unbounded". Fix: give that wrapper a bounded height —
easiest is `style={{ height: bottomHeight }}` + `overflow-hidden` to match the
explorer pane (the state already exists), or restore a `max-h-*` cap for that
branch only.

### L2. A restored mid-edit session is kicked out of edit mode on remount
`frontend/src/components/common/SourceEditor.tsx:209-214`

`setEditing(false)` runs *before* the `if (restoring) return;` early-exit, so on a
remount that restores a session with `editing: true` the component immediately
flips to view mode. Old behavior preserved `editing` across the tab-switch
unmount (lazy `useState(editorSession.editing)` with nothing resetting it on
mount). The B1 core (dirty content + dot + discard guard) survives, but the user
must click Edit again after every tab switch made mid-edit. Fix: only reset when
actually switching targets — move `setEditing(false)` (and the two `setError`
calls, harmlessly) below the `restoring` early-return.

## NITS (fix if convenient)

- **N1.** `SourceEditor.tsx:275-282` — the `editing` branch of the revealLine
  effect is unreachable: the textarea only renders when `editing && isMd`, but
  `isMarkdownPath(path)` returns at :272, so inside the branch `taRef.current` is
  always null. Dead code (house rule: remove dead code rather than keep it).
- **N2.** `GraphView.tsx:106-109` — `storedBottomHeight()` enforces only the min;
  a height persisted on a large window (say 600px) renders unclamped on a later,
  smaller window until the user drags (the 60% clamp only runs during a drag).
  Consider clamping on first measure against `containerRef` height.
- **N3.** The Files tab now shows the path twice (outer header
  `FileViewer.tsx:313-319` + SourceEditor's toolbar :361). Cosmetic / arguably
  intended since both surfaces share chrome — flagging only so it's a conscious
  choice.

## Constitution / cross-cutting checks (all pass)

- Frontend-only; no Rust touched. No `#[allow]`; doc comments present on all new
  public functions (`clampRevealScroll`, `clampBottomHeight`, `editorSession`,
  component file headers). No unused imports left in FileViewer (checked each).
- Session isolation verified: only `persistSession` consumers read/write
  `editorSession` (mirror effect :184-191 early-returns; initializers gated on
  `persistSession`; `restoring` requires `persistSession`) — the Graph peek cannot
  clobber the Files session. `onDirtyChange` is `useCallback`-stable in FileViewer
  and SourceEditor's effect deps `[dirty, onDirtyChange]` are correct; React 18
  flushes passive effects before discrete events, so `dirtyRef` is fresh when
  `confirmDiscard` reads it.
- Drag math: up = grow (`startHeight - Δ`), clamp table pinned by tests,
  `localStorage.setItem` only in `endDrag` (pointerup *and* pointercancel —
  acceptable). Mid-drag unmount cleanup wired. Unavailable branch correctly has no
  splitter. Static-source test assertions all match the shipped source verbatim
  (checked each `toContain` string, incl. the 2-occurrence MemoryAccessSection
  count); `vitest.config.ts` include updated.
- The `revealLine` measurement (`pre`/`code` `getBoundingClientRect` ÷ line count,
  `Math.max(1, lines)` guards) has no div-by-zero; the markdown guard makes the
  `pane.querySelector("pre")` pick the code fence's own `pre` (a fence-wrapped
  body renders exactly one); re-runs on `content` so it scrolls after the load
  lands. Off-by-one from the fence's trailing newline is documented as
  approximate — fine.
- Test gap note: the suite is necessarily static-source (no DOM infra), which is
  why H1/M1 (both behavioral effect-race regressions) escaped. If a fix extracts
  a pure decision helper (e.g. `shouldPersistStraddledRead(sessionPath, readPath,
  disposed, persistSession)`), pin it with a table test per the defect-test rule.
- Author reports `npm test --prefix frontend` 312 passed / 29 files,
  `npm run build --prefix frontend` clean, `cargo test` 1127 passed warning-free;
  reviewer is read-only and did not re-run them.

## Verdict

**Changes requested** — fix H1 and M1 before merge (both are regressions of
deliberate, comment-documented invariants in the code being refactored); L1 and L2
are small targeted fixes; nits optional.
