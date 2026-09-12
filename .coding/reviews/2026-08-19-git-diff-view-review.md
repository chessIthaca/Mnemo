# Review: Comprehensive git-HEAD diff mode for the Diff tab (uncommitted diff on feat/memory-access-log)

Scope: ALL uncommitted changes (`git diff HEAD` + untracked). The already-committed memory-access-log work (70d2cc6) was not re-reviewed. Bookkeeping (`.coding/plans/cb832482-….md` step checkbox, `.coding/plans/stack.json`, untracked plan `985d7ff3-….md`) noted, not deep-reviewed. Untracked list contains ONLY `frontend/src/components/views/DiffViewer.test.ts` (new, must be committed) + the plan md — no stray debris (no a.txt/b.txt/file.txt). ✓

## Findings (severity-ordered)

### CORRECTNESS — High: per-file scoped diff passes a Windows verbatim (`\\?\`) pathspec to git; the production input shape is untested

Evidence chain (all verified in-session):

1. `planDiffs[].path` = `previewPath ?? parsedArgs.path` (agentEventReducer.ts:430). With approvals on, `previewPath` = `ApprovalPreview.path` = the sandbox-`validated` PathBuf (file_edit.rs:636-638, file_write.rs:156-158) — i.e. `Sandbox::validate`'s `candidate.canonicalize()` result (sandbox.rs:88).
2. On Windows, `std::fs::canonicalize` returns a verbatim path (`\\?\C:\…`). So `gitScopePath` (DiffViewer.tsx:227-229) is — in the common case — a `\\?\C:\…` string; even when the model typed a relative/normal path, the backend re-validates in `git_diff_head` (files.rs:462-465) and canonicalizes it again, so the path handed to git is ALWAYS verbatim-prefixed when the file exists.
3. `git_diff_head_at` passes it verbatim after `--` (files.rs:410-411). Git-for-Windows pathspec handling of `\\?\`-prefixed absolutes is unverified/fragile: git normalizes `\`→`/`, yielding `//?/C:/…`, which does not prefix-match the worktree (`C:/…`), so the scoped diff risks `fatal: '<path>' is outside repository at '<root>'` (version-dependent; this is the canonicalization gotcha the `dunce` crate exists for).
4. The unit test does NOT cover this: `git_diff_head_reports_changes_and_scope` passes `root.join("a.txt").display()` (files.rs:161, 178) — a plain non-canonicalized `C:\…` path, which git accepts. So the tests are green while the production-shaped input (canonicalized `\\?\` path) is never exercised.

Impact: the DEFAULT git view is scoped (`gitScopePath` = selected ?? newest planDiffs entry) whenever any file was edited this plan — i.e. the headline path of the feature goes through the verbatim pathspec on Windows.

Fix (both):
- Send a REPO-RELATIVE pathspec instead of an absolute one: strip the sandbox root from the validated path and join with forward slashes (`git diff HEAD -- src/main.rs` with `current_dir(root)`). This is immune to verbatim prefixes, drive-letter case, and symlink/canonicalization mismatches.
- Add a regression test that routes the scoped path through `Sandbox::validate` (canonicalized, verbatim on Windows) into `git_diff_head_at`, so the production input shape is pinned.

### PERF / CORRECTNESS — Medium: unbounded diff size into UnifiedDiffView (F3-class freeze vector)

`git_diff_head_at` returns the full stdout uncapped (files.rs:424-425); `UnifiedDiffView`→`parseUnifiedDiff` splits and renders EVERY line as a DOM node with no truncation (DiffView.tsx:31-62, 156-192). The existing approval-preview path is capped at `PREVIEW_DIFF_CHAR_BUDGET = 120_000` chars (file_edit.rs:610-635); the new git path has NO equivalent guard. "All changed files (vs Git)" + Refresh on a large change set can ship tens of MB over IPC and paint 100k+ DOM nodes — exactly the unbounded-frontend freeze class of the 2026-04-19 F3 diagnosis.

Fix: cap the output in `git_diff_head_at` (truncate stdout at a char budget and append a `… [diff truncated for size] …` marker, mirroring the preview path) — this bounds both the IPC payload and the render. (A frontend-side truncation in `UnifiedDiffView` would be defense-in-depth but is optional once the backend caps.)

### BUG — Low: stale diff/error rendered under the NEW scope's label while the refetch is in flight

On a `gitScopePath` change the effect does not clear `gitDiff`/`gitError` at start (DiffViewer.tsx:237-257). While the new fetch is in flight, the body branch `gitDiff !== null && gitDiff !== ""` (DiffViewer.tsx:314-315) renders the PREVIOUS file's diff under the NEW file's header label (`gitScopeLabel`, DiffViewer.tsx:294); the spinning Refresh icon is the only in-body hint. Same for a stale `gitError` (DiffViewer.tsx:310-313) after the scope changes. Self-corrects when the fetch resolves, but misleads during it (a subprocess round-trip, longer on big repos). Fix: at effect start clear `gitDiff`+`gitError` (or let the `gitLoading` body take precedence); if "keep the last good diff on error" is desired, retain it only for the same-scope error case, not across scope changes.

### CONSTITUTION — Low: CRLF line endings in the working copies of two existing files

`git diff HEAD` warns `CRLF will be replaced by LF` for exactly `frontend/src/components/views/DiffViewer.tsx` and `src-tauri/src/ipc/files.rs` — and NOT for the other four modified files (main.rs, tauri.ts, vitest.config.ts are LF in the working copy), so the repo convention is LF and these two files were saved with CRLF. The committed content will normalize to LF, but the working copies violate "preserve the line-ending style of existing files." Fix: re-save both files with LF. (The untracked `DiffViewer.test.ts` produces no git signal; confirm it is LF when staging.)

### UX NIT — Low: `allScope` survives into edit mode, so the dropdown mislabels the edit view

Clicking the "last edit" toggle while "all changed files (vs Git)" is selected leaves `allScope === true`; the select then displays `__all__` (DiffViewer.tsx:363-368) while the body shows a captured per-file edit (shownEntry branch, DiffViewer.tsx:331+). Not a crash — the `__all__` option always exists — but the dropdown label no longer matches the content. Fix: clear `allScope` in the "last edit" toggle handler (or hide the `__all__` selection display in edit mode).

## Explicitly verified clean (the plan's review questions)

- **`--` separator placement**: `git diff HEAD --no-color [-- <path>]` (files.rs:403-411) — the path follows `--` as a single argv element, no shell; it can never be parsed as a flag. ✓
- **Root lock not held across the spawn**: `state.project.root` is `Arc<tokio::sync::Mutex<Project>>` (state.rs:16, 140); `let root = state.project.root.lock().await.root.clone();` (files.rs:458) drops the temporary guard at statement end, before `spawn_blocking` (files.rs:469). Sandbox validation is sync, before the spawn, no await between validate and spawn. Mirrors `get_git_branch` (files.rs:379-385). ✓
- **Effect cleanup / stale-response races**: React runs the previous effect's cleanup (`disposed = true`) before re-running, so an older in-flight `gitDiffHead` response's `.then`/`.catch`/`.finally` all no-op — last-write-wins is GUARDED; also covers setState-after-unmount. ✓
- **showPending flip → refetch**: `showPending` is in the effect deps and `shouldFetchGitDiff` gates on it (DiffViewer.tsx:150-152, 257) — when the approval resolves, the effect re-runs and refetches. Pending approvals render first in the body chain (DiffViewer.tsx:262) — precedence preserved, untouched. ✓
- **Select value ↔ option matching in every reachable state**: `""` is produced only when `showPending` (which renders the `""` option) — the edit-mode/zero-entries/no-pending state that would yield `""` with no option is unreachable because `showDropdown` is false there (DiffViewer.tsx:222). `__all__` and entry paths always have options. allScope + selectedDiffPath interplay: picking `__all__` clears the selection and flips to git mode; picking a file clears `allScope`. ✓ (Cap-eviction edge: git mode uses the RAW `selectedDiffPath` while edit mode falls back when the path leaves `planDiffs` — harmless, the path is still a valid on-disk file.)
- **Fallback path derivation matches edit mode** (selected ?? newest entry ?? whole tree). ✓
- **Security**: sandbox-validated path confined to the project; `GIT_DIR`/`GIT_WORK_TREE`/`GIT_INDEX_FILE` scrubbed (review-L5 parity); CREATE_NO_WINDOW on Windows; React text rendering escapes diff content (no `dangerouslySetInnerHTML`); diff content exposure is the same sandboxed class as the existing `read_file`/Files tab — no new exposure; untracked-file limitation is documented in doc comments and the UI empty state. ✓
- **Constitution (other)**: doc comments on every new pub item (`git_diff_head_at`, `git_diff_head`, `gitDiffHead`, `shouldFetchGitDiff`, `DiffViewMode`) ✓; no `#[allow(...)]` ✓; regression tests exist for the defect class (backend: clean/whole-vs-scoped/staged/no-ANSI/non-repo; frontend: fetch predicate) ✓; command registered in main.rs ✓; `errMsg` exists (tauri.ts:104) ✓.
