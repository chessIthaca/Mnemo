# Review: FileViewer Markdown editor upgrade (feat/md-editor)

Reviewed ALL uncommitted changes (`git status --short` + `git diff HEAD`): `src-tauri/src/ipc/files.rs`, `src-tauri/src/main.rs`, `frontend/src/lib/mdEdit.ts` (new), `frontend/src/lib/mdEdit.test.ts` (new), `frontend/src/lib/tauri.ts`, `frontend/src/components/views/FileViewer.tsx`, `frontend/vitest.config.ts`, plus bookkeeping (`.coding/backlog.json`, `.coding/plans/stack.json`, `.coding/plans/97514766-*.md`). Read-only review; tests were verified green by the implementer and not re-run.

**Verdict: APPROVE WITH FINDINGS.** The backend `write_file` IPC is secure and idiomatic (full parity with the agent `file_edit` protections). One High frontend correctness finding (silent data loss through the Done toggle) must be fixed before commit.

---

## Correctness

### C1 (HIGH) — `dirty` is gated on `editing`, so "Done" silently disarms the unsaved-changes guard → data loss

`FileViewer.tsx:126`:
```ts
const dirty = editing && hasUnsavedChanges(savedContent, content);
```

Repro:
1. Edit an .md file in edit mode → `dirty = true`, amber dot shows.
2. Click **Done** (`FileViewer.tsx:466`, `setEditing(!editing)`) — no save, no prompt.
3. Re-render: `editing = false` → `dirty = false` → the amber dot disappears (`:424`), the Save button disappears (`:451` requires `editing || dirty`), and `dirtyRef.current = false` (`:128`).
4. View mode still renders the *edited* `content` (`:548-549`) — it looks saved.
5. Click any other file in the tree (`loadFile:260`), a binary (`openEntry:280`), or Browse (`handleBrowse:306`): `confirmDiscard()` reads `dirtyRef.current === false` → **no prompt** → the read overwrites `content`/`savedContent` → edits permanently lost.

The plan text itself said "when dirty and editing", so this faithfully implements the plan — but it defeats the feature's stated goal (unsaved-changes guard + real persistence). The old UI at least implied Save on Done (it showed a Save icon); the new Check icon implies "finished", inviting exactly this loss path.

**Fix:** decouple dirtiness from mode:
```ts
const dirty = hasUnsavedChanges(savedContent, content);
```
The dot then correctly shows in view mode, the Save button stays available (its condition already includes `(editing || dirty)`), `confirmDiscard` covers the Done-then-switch path, and Ctrl+S-in-textarea is unaffected. (Optionally also prompt or save on Done when dirty.)

### C2 (LOW) — CRLF documents get mixed line endings inside an `insertBlock` block until save

`mdEdit.ts:100-107`: the *gaps* use the document-dominant `nl` (`"\r\n"` in a CRLF file), but the block body itself (`"```\n\n```"`, `FileViewer.tsx:92`) always contains LF. A code-fence inserted into a CRLF doc has LF fences + CRLF gaps in the editor. Healed on save by `normalizeForSave` (collapse-then-expand, `mdEdit.ts:122-128`), so the file on disk is consistent — cosmetic/in-editor only. A one-line `block.replaceAll("\n", nl)` would make it fully consistent.

## Bugs

### B1 (MEDIUM) — Unsaved edits are silently lost when the right-panel tab switches (unmount)

Switching the right panel to Plan/Diff/Output/etc. unmounts FileViewer (registry-rendered active component, `frontend/src/hooks/rightPanelViews.tsx:50-61`; the Files tab starts unmounted by design — `FileViewer.tsx:292-296`). All editor state (`content`/`savedContent`/`editing`) lives in the component (`:112-128`), so a dirty editor is discarded with **no prompt** on any tab switch (and on app close). The new guard covers file switches *within* the viewer but not this path. Not a regression (the old editor never persisted), but it is a coverage gap in the feature's core promise. Suggest lifting `content`/`savedContent`/`editing` into the agent store (the pendingFileOpen mechanism already shows the pattern) or blocking tab switches while dirty — or explicitly deferring with a note. (If C1 is fixed, at least the dot is visible before switching; today both indicators vanish in view mode.)

### B2 (LOW) — `handleSave` is re-entrant: Ctrl+S can fire concurrent saves

The Save button is disabled while saving (`FileViewer.tsx:454`), but `onEditorKeyDown` (`:175-186`) calls `void handleSave()` on every Ctrl+S with no `saving`/`dirty` check. A slow write + a second Ctrl+S → two concurrent `write_file` invocations; the first `finally` clears `saving` while the second is in flight (button re-enables mid-save). Content is identical so the outcome is benign, but the UI state lies. **Fix:** first line of `handleSave`: `if (saving || !dirty || path === "") return;`.

## Security — no findings (verified sound)

- **Sandbox + protected parity confirmed.** `write_sandboxed` (`files.rs:51-60`) runs `Sandbox::validate` (canonicalize + `starts_with(root)`; rejects `..` traversal, absolute-outside, symlink escapes) then `refuse_if_protected` — the *same* function the agent `file_edit` tool uses (`file_edit.rs:544,586` → `sandbox.rs:257`), producing the ONE shared refusal message (`sandbox.rs:269-275`). Protected set covers memory/codegraph DBs + `-wal`/`-shm` sidecars, `.coding/safety.toml`, `.coding/backlog.json`, `.coding/plans/*`, with case-insensitive matching and the NTFS ADS `:` guard (`sandbox.rs:170-204`). Protected check precedes `fs::write` (same ordering as file_edit); the regression test asserts nothing is created on refusal (`files.rs:156`).
- **Async hygiene correct.** `sandbox` (cheap `Clone` of a `PathBuf` root) + owned `path`/`content` moved into `spawn_blocking`; no `State` across the await; blocking `fs::write` never parks a tokio worker (N2 convention, matches `read_file`/`save_conversation`).
- **Error mapping sound.** `JoinFailure → String → IpcError::from` and inner `Result<String,String> → IpcError::from` (`From<String>` at `error.rs:43-47`); frontend `errMsg` handles the `{kind,message}` DTO (`tauri.ts:104-112`), so `saveError` renders properly.
- **No HTML injection.** The live preview renders through the shared `Markdown` stack (remark-gfm + rehype-highlight only; no `rehype-raw` anywhere in `frontend/src` — verified by search). ReactMarkdown escapes raw HTML by default.
- **No new dependencies.** `package.json`/`Cargo.toml` untouched (git status); all new lucide icons (`Braces`, `Bold`, `Italic`, `Code`, `Link2`, `List`, `ListOrdered`, `Heading1-3`) are existing exports of the already-used `lucide-react`.
- **Informational (no action):** `Sandbox::validate` accepts a nonexistent target with an existing parent (`sandbox.rs:92-102`), so `write_file` can *create* files — unlike `file_edit`, which then fails at read. The UI only passes loaded (existing) paths and it's the user's own project root, so this is acceptable create-capability; worth knowing the IPC is not strictly edit-only. Browsed absolute paths outside the root would be rejected by `validate` anyway (frontend block + backend rejection = defense in depth).

## mdEdit.ts correctness — no findings

- **Multi-byte safe.** All slicing/index arithmetic uses UTF-16 code units, exactly matching `textarea.selectionStart/End` semantics; `before.length` additions are unit-consistent. No real bug — no path splits a surrogate pair beyond what platform selection semantics already define.
- **CRLF handling verified against the tricky cases.** `setLinePrefix`: collapsed cursor at a line start doesn't grab the previous line (`lastIndexOf("\n", max(0, selStart-1)) + 1`); `endsAtNewline` + the `\r` backtrack (`mdEdit.ts:62-71`) keeps a selection ending on `\r\n` from pulling the next line and keeps `\r` with its `\n`; a selection ending *between* `\r` and `\n` still prefixes correctly because `split(/(\r?\n)/)` replays separators verbatim (and real textareas normalize `\r\n`→`\n` in `.value`, so that boundary is unreachable from the UI). `insertBlock`: dominant-ending gaps, blank-line collapse before/after, caret placement all correct per tests.
- **`normalizeForSave` style-oracle choice is right.** The ORIGINAL (`savedContent`, which retains CRLF after a save) decides the style, so the textarea's spec-mandated CRLF→LF `.value` normalization can never flip the file's style — `applyTransform` reading the LF-normalized `ta.value` makes `content` LF, and save heals it back to CRLF.
- **`hasUnsavedChanges`** trailing-(CR?)LF-run-insensitive compare matches its spec and tests (`mdEdit.test.ts:108-122`).

## FileViewer other checks — no findings beyond C1/B1/B2

- Guard fires on every content-switch path: `loadFile:260`, `openEntry` binary arm `:280`, `handleBrowse:306`; the tool-card `pendingFileOpen` path routes through `loadFile` (guarded) and clears the pending request even when the user declines (acceptable).
- `dirtyRef` render-time mirror (`:127-128`) is only read inside `confirmDiscard` (event handlers), never during render — safe "latest value" pattern.
- Ctrl+S `preventDefault` runs before save (`:176-177`); the handler exists only on the editor textarea, so the browser/webview save can't trigger from the editor.
- `applyTransform` reads `ta.value`/selection from the DOM (always current, immune to stale state) and restores selection via `requestAnimationFrame` — React flushes discrete (click) events synchronously, so the DOM is committed before the rAF callback. Sound.
- Save disabled while `saving || !dirty` (`:454`); browsed files: Save hidden (`browsed === null` at `:451`), `handleSave` and Ctrl+S both refuse with an explicit message, and the "local preview only" hint shows in edit mode (`:514-518`).

## Constitution compliance — no findings

- Doc comments present on all new public items: `write_file` command + `write_sandboxed` (private but documented), all 5 exported mdEdit helpers + `TextSel` and its fields, the `writeFile` wrapper, `ToolbarButton`/`TOOLBAR_BUTTONS`, and the new tests.
- No `#[allow(...)]` added anywhere; `#![deny(warnings)]` build passes per the implementer's verified test matrix.
- Regression tests present and meaningful: 3 backend tests (`write_sandboxed_round_trips`, `write_sandboxed_rejects_escape`, `write_sandboxed_refuses_protected_plan_file` — each asserts non-creation on failure) + 17 frontend cases; `mdEdit.test.ts` registered in `frontend/vitest.config.ts:29`. C1 is UI wiring (no DOM test infra exists — the plan explicitly waived a FileViewer test), so no test-gap finding.

## Bookkeeping (verify-only) — one Low

- `.coding/plans/stack.json` correctly points at the active plan `97514766` ✓; the plan file's step statuses are accurate (5/7 done, review in flight) ✓.
- **K1 (LOW):** `.coding/backlog.json:39-41` marks item 49 `done` with `note: null` while plan steps 6–7 (review, fix, commit) are open and nothing is committed — the prior convention kept `in_flight` + a commit sha in `note` until completion. Suggest reverting to `in_flight` until step 7 commits (then set `done` with the new sha), or filling `note` at commit time.

---

## Summary of required actions before commit

| # | Sev | File:line | Action |
|---|-----|-----------|--------|
| C1 | HIGH | FileViewer.tsx:126 | Drop the `editing &&` from `dirty` so Done keeps the dot/Save/guard armed |
| B1 | MED | FileViewer.tsx:112-128 (state lift) | Guard unsaved edits against tab-switch unmount (store-lift or block) — or record an explicit deferral decision |
| B2 | LOW | FileViewer.tsx:137 | `if (saving || !dirty) return;` at top of `handleSave` |
| C2 | LOW | mdEdit.ts:110 | (Optional) `block.replaceAll("\n", nl)` for consistent CRLF blocks |
| K1 | LOW | .coding/backlog.json:39 | Defer `done`/note until the commit lands |
