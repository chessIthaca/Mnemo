## Verdict: FINDINGS (0 high, 1 low)

Frontend-only change (7 files, ~+560/−90) implementing tool-card expanded views: file_edit shows its Rust-computed unified diff, read_files shows a compact per-file line-range list, and file links deep-link the Files viewer to the read line. Parsing contracts verified against the Rust source; line plumbing is 1-indexed end-to-end (no off-by-one); fallbacks, stale-line reset, and `buildPathChips` regression behavior all correct; every new exported function carries a doc comment; multi-platform neutral; no security concerns. One LOW finding on a same-file re-scroll edge case (the "reloadToken bump re-triggers the reveal effect" mechanism described in the review brief does not actually hold).


### LOW 1 — Same-file / same-line deep-link re-click does not re-scroll (reveal effect deps exclude `reloadToken`)

**Location:** `frontend/src/components/common/SourceEditor.tsx:333-344` (reveal effect) vs `:240-324` (load effect); driven by `frontend/src/components/views/FileViewer.tsx:197-200` (`loadFile` → `openTarget` → `setRevealLine` + `setReloadToken`).

**Finding:** The review brief states "the same-file re-open case (reloadToken bump re-triggers SourceEditor's reveal effect)." That mechanism does not hold: the reveal effect's dependency array is `[content, revealLine, path]` (`SourceEditor.tsx:344`) — `reloadToken` is **not** in it. The `reloadToken` bump re-triggers the **load** effect (deps `[path, browsed, persistSession, reloadToken]` at `:324`), not the reveal effect.

Consequence — clicking a read_files deep-link (header chip or expanded row) for a file that is **already open** in the FileViewer, at the **same line** as a prior click, with the file **unchanged on disk**, does NOT re-scroll to that line:
- `setRevealLine(40)` is a no-op (React bails — value unchanged from the prior click).
- `setReloadToken(t+1)` changes → load effect re-reads the file → `setContent(text)` with a byte-identical string → React bails (`Object.is`-equal) → `content` doesn't change → reveal effect deps unchanged → never re-runs.

The common cases all work correctly: first open (`revealLine` null→N triggers the effect), a **different** line (`revealLine` changes), or a file that **changed on disk** (`content` changes → effect re-runs). This is **not a regression** — pre-change there was no `revealLine` at all, so already-open files never scrolled on chip click. It is a narrow gap in the new feature: a user who scrolls away from line 40 and re-clicks the same "line 40" link sees no scroll.

**Fix (optional, low-risk):** add `reloadToken` to the reveal effect's dependency array:
```tsx
}, [content, revealLine, path, reloadToken]);
```
GraphView (the other `SourceEditor` consumer) never changes `reloadToken` (default `0`), so this only affects FileViewer; the existing `if (revealLine === null || revealLine < 1) return;` guard (`:334`) keeps manual opens (which reset `revealLine` to `null`) as no-ops. For the same-file/same-line case, the effect re-runs on the `reloadToken` bump and re-scrolls using the already-loaded (correct) content. Alternatively, accept the limitation and drop the "reloadToken bump re-triggers the reveal effect" claim from any comments/docs.

---

## Verified correct

**1. Parsing contracts vs actual Rust output** (checklist item 1)
- `parseReadFilesSections` (`toolCardPaths.ts:725-744+`) regexes match the real `read_files.rs` output exactly:
  - Range header `=== {path} (lines {first}-{last} of {total}) ===` — Rust at `read_files.rs:369-372`; regex `/^=== (.+) \(lines (\d+)-(\d+) of (\d+)\) ===$/`. ✓
  - `(error)` variant — Rust at `read_files.rs:298/308/318` (`format!("=== {} (error) ===\n...", spec.path)`); regex `/^=== (.+) \((error|empty range)\) ===$/`. ✓
  - `(empty range)` variant — Rust at `read_files.rs:367`; same note regex. ✓
  - `SYMBOL NUDGE:` prefix (`read_files.rs:265`) and `... (truncated ...)` tails (`:273` outer cap, `:360` per-file cap) are correctly ignored — they don't start with `=== `, so no header match. ✓
  - **False-positive safety:** every content line is numbered `{:>4}: {line}` (`read_files.rs` numbering), so no content line can start with `=== ` — a file whose source contains a header-shaped line can't be misparsed. ✓
- `fileEditDiff` (`toolCardPaths.ts:696-702`) reads `result.data.diff` — matches `file_edit.rs:905` `data: Some(json!({"diff": prepared.diff}))`. Failed edits use `ToolResult::error` (`mod.rs:77-83` → `success: false, data: None`), and `fileEditDiff`'s `!result.success` guard returns `null` for them. ✓

**2. Line plumbing end-to-end** (checklist item 2) — no off-by-one.
- Rust treats `start_line` as **1-indexed**: `start = spec.start_line.unwrap_or(1).saturating_sub(1)` (`read_files.rs:325`), `first_line = start + 1` (`:364`). So `start_line: 40` → header `first = 40`.
- Frontend `argPathLines`/`lineOf` (`toolCardPaths.ts:143-146`) uses `start_line` directly (≥1, default 1) — equals the header's `first`. The chip line (from args' `start_line`) and the expanded-row line (parsed `first` from the result) are therefore consistent. ✓
- `openFileInViewer(path, line?)` → `requestFileOpen(path, line ?? null)` (`openFile.ts:27-28`) → `pendingFileOpen: {path, line}` (`useAgentStore.ts:753`) → FileViewer effect (`FileViewer.tsx:221-225`) → `loadFile(path, line)` → `openTarget(p, null, null, line ?? null)` → `setRevealLine(line)` → `SourceEditor` `revealLine` prop. ✓
- **Stale-line reset on manual opens:** tree click (`openEntry` → `loadFile(path)`, `:206`), binary open (`:211`), and Browse (`:244`) all call `openTarget` with the default `line = null` → `setRevealLine(null)`, so a stale line never re-scrolls a later file. ✓
- **No missed callers:** only two `openFileInViewer` call sites (`Message.tsx:780` chip, `:955` expanded row), both pass the line; the three label-chip construction sites pass `line: null`. ✓

**3. Fallbacks** (checklist item 3) — failed file_edit (no diff → `fileEditDiff` returns null), unparseable read_files output (no header match → `parseReadFilesSections` returns `[]`), and still-running calls (`result === null` → both helpers return null/`[]`) all fall through to the generic pretty-args + raw-output rendering. The `editDiff !== null || readSections.length > 0` guard suppresses the args JSON only when a specialized view exists. ✓

**4. `buildPathChips` regression** (checklist item 4) — the rewrite (`:274-305`) collects `PathLine[]` via `argPathLines` (which delegates to the unchanged `argPaths` for non-read tools, returning `line: null`), then `dedupePaths(entries.map(e => e.path))` preserves the exact prior dedupe. First-occurrence-line-wins via `lineByPath` (first-wins, mirroring `dedupePaths`' first-wins). Overflow chip gets `line: null`. Disambiguate/overflow logic unchanged. Non-read tools are byte-identical (chips now carry `line: null`). ✓

**5. Documentation sync** (checklist item 5) — all new exported items carry doc comments: `fileEditDiff` (`:681-695`), `parseReadFilesSections` (`:710-724`), `ReadFilesSection` (`:704-705`), `argPathLines` (`:124-131`), `PathLine` (`:117-118`), `ToolCardChip.line` (`:171-172`). README.md has no tool-card expanded-view documentation (no matches for tool-card/expanded patterns; prior plan 2035cb82 noted the same), so no README update is required — module doc comments carry the contract. ✓

**6. Multi-platform neutrality** (checklist item 6) — pure TypeScript/React; `normalizePath` (`:179`) handles both `/` and `\` separators; no platform-specific APIs, paths, or shell syntax. ✓

**7. Security** (checklist item 7) — diff/line data comes from tool results the UI already displays; all rendered text is React-escaped (no `dangerouslySetInnerHTML`); paths are display/deep-link only at the existing trust level. Nothing sensitive. ✓

**Test status:** frontend `npm test` (740 tests, incl. 12 new) + `npx tsc --noEmit` + root `cargo check --tests` all green per the brief; the new tests pin the parsing contracts and the `pendingFileOpen` line-carrying shape.
