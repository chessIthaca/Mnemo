# Review: file_edit tool-card chip opens the Diff tab (branch fix/file-edit-open-diff)

Scope reviewed: ALL uncommitted changes (`git status` / `git diff HEAD`) — `frontend/src/lib/openFile.ts`, `frontend/src/components/chat/Message.tsx`, `frontend/src/lib/openFile.test.ts` (new), `frontend/vitest.config.ts`, `README.md`, plus `.coding/backlog.json` / `.coding/plans/stack.json` bookkeeping.

## Findings

### Medium

1. **DiffViewer dropdown label disagrees with the displayed diff when the deep-linked path has no `planDiffs` entry.**
   - `frontend/src/components/views/DiffViewer.tsx:229-231` — `selectedEntry = entries.find((e) => e.path === selectedDiffPath) ?? null`.
   - `frontend/src/components/views/DiffViewer.tsx:249-251` — in git mode (the default), the *body* scope uses the raw store value: `gitScopePath = selectedDiffPath ?? fallbackEntry?.path ?? null`.
   - `frontend/src/components/views/DiffViewer.tsx:391-396` — the *dropdown value* instead falls back to `fallbackEntry?.path` (or `"__all__"` "all changed files" when entries is empty), because it derives from `selectedEntry`, not `selectedDiffPath`.

   Before this change the mismatch state was unreachable: `selectedDiffPath` was only ever set from the dropdown's own options (`DiffViewer.tsx:404,407` — an entry path or `null`), so `selectedDiffPath` non-null always implied `selectedEntry` non-null. The new `openDiffInViewer` (`frontend/src/lib/openFile.ts:33-37`) sets an arbitrary path via `selectDiffPath`, and a clicked `file_edit` path can legitimately be absent from `planDiffs` — the entry list resets per top-level plan and is capped (`MAX_PLAN_DIFFS`), so an older card's file may have no entry. In that case the Diff tab opens with the body correctly showing the clicked file's vs-Git diff while the dropdown displays a *different* file's name (or "all changed files"), i.e. the same label/body mismatch class this component otherwise guards against (`scopedResult`, `DiffViewer.tsx:208-211`). The plan's stated intent — "fall back to the first entry when the path isn't in the current entries" — also only holds in edit mode (`shownEntry = selectedEntry ?? fallbackEntry`, line 235); git mode never falls back. Suggested fix: derive the dropdown value from `selectedDiffPath` when set (adding a transient `<option>` for a selected path not among `entries`), so the control reflects what the body renders.

### Low / informational

2. **Unrelated backlog status flips ride along in this commit.** `.coding/backlog.json` marks items 66, 68, 70, 71 `done` (item 72 → `in_flight` with a note, which *is* this work). The extra flips look like housekeeping from prior completed work rather than part of backlog item 72. No action if intentional; flagging only so the commit contents are a conscious choice.

## Clean areas (no findings)

- **Branch scoping (file_edit-only) — verified.** `Message.tsx:511` (`const opensDiff = name === "file_edit"`) reads the per-card `name` prop (`Message.tsx:410-416`, rendered at `Message.tsx:230`). Cards only ever group same-name calls (`agentEventReducer.ts:388-392`: merge requires `last.name === event.name`), so a click can never fire the diff branch for a non-`file_edit` call sharing a grouped card. Every other tool takes the untouched `openFileInViewer(chip.path as string)` else-branch (`Message.tsx:519-521`) — behavior byte-identical to before, including `e.stopPropagation()`.
- **Store invariants — verified.** `revealRightPanelTab("diff")` (`useAgentStore.ts:694-701`) unconditionally enables the tab (filters it out of `disabledTabs`), selects it, and reveals the panel — idempotent, never disables, so clicking when the Diff tab was disabled behaves, and repeated clicks re-assert the tab. `selectDiffPath` (`useAgentStore.ts:654`) updates the selection each click, so repeated clicks on different `file_edit` chips retarget correctly.
- **Stale-path degradation in edit mode — verified** (`shownEntry = selectedEntry ?? fallbackEntry`, `DiffViewer.tsx:235`; see the Medium finding for the git-mode/dropdown residue).
- **Title swap — scoped correctly.** Only `file_edit` chips get `"Open the diff of <path> in the Diff tab"`; all others keep `"Open <path> in the Files tab"` (`Message.tsx:523-527`). Plain attribute text — React escapes it.
- **`argPaths` unchanged — verified.** `file_edit` is not in the label-only exclusions and falls through to the common `path`/`file` branch, returning `[parsed.path]` (`toolCardPaths.ts:64-69`); the file is untouched by this diff, so chips still receive their path.
- **Regression test — genuine.** `frontend/src/lib/openFile.test.ts` fails on old code (no `openDiffInViewer` export existed → import error) and asserts the three post-conditions (`rightPanelTab === "diff"`, `rightPanelVisible === true`, `selectedDiffPath === "src/x.rs"`). Initial `selectedDiffPath` is `null` (`useAgentStore.ts:565`), so the third assertion is non-vacuous; direct store import matches the established test pattern (`useAgentStore.preview.test.ts`). Registered in `frontend/vitest.config.ts:39`.
- **No unused imports / strictness.** Both `openDiffInViewer` and `openFileInViewer` remain used in `Message.tsx` (lines 518/520); no new warnings surface.
- **Documentation sync — accurate.** README bullet (`README.md:56`) matches the implemented behavior; the `openFile.ts` module doc (lines 16-18) correctly removes `file_edit` from the Files-tab caller list and points to `openDiffInViewer`, consistent with the chip comment (`Message.tsx:425-430`, `509-510`); the exported function carries a doc comment (`openFile.ts:27-32`).
- **Security — nothing new.** No new unescaped rendering (React escapes chip text; `title` is a plain attribute); the path is display/deep-link only — DiffViewer resolves it against `planDiffs` entries or passes it as a git-diff scope at the same trust level as the pre-existing `openFileInViewer` flow.
- **Multi-platform neutrality — clean.** Pure TS/React; no platform-specific APIs, paths, or shell syntax.
