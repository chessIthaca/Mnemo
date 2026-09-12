# Markdown Viewer: Hide Dot-Dirs, Show-Hidden Toggle, Configurable Skip List, Browse Button

**Date:** 2026-04
**Reviewer:** read-only subagent (spawned review)
**Scope:** ALL uncommitted changes in the working tree (`git diff HEAD`), focused on the Markdown-viewer feature plan.
**Branch:** feature branch (uncommitted)

## Files reviewed

- `src/config/general.rs` — `MarkdownConfig` struct + `markdown` field on `GeneralConfig`; tests.
- `src/config/mod.rs` — re-export `MarkdownConfig`.
- `src-tauri/src/ipc/files.rs` — `collect_markdown` helper (dot-dir + skip-list pruning), `list_markdown_files(show_hidden)`, `browse_markdown_file` command; tests.
- `src-tauri/src/ipc/settings.rs` — `MarkdownWire`, `markdown` on `GetSettingsResponse`, `skip_dirs` on `SettingsSaveDto` + apply logic; test fixtures.
- `src-tauri/src/ipc/contract_fixtures.rs` — `markdown` in fixture struct literal.
- `src-tauri/src/main.rs` — `tauri_plugin_dialog::init()` + `browse_markdown_file` registration.
- `src-tauri/Cargo.toml` / `Cargo.lock` — `tauri-plugin-dialog = "2"` + transitive deps.
- `src-tauri/capabilities/default.json` — `dialog:default` permission.
- `frontend/src/lib/tauri.ts` — `listMarkdownFiles(showHidden)`, `BrowsedFile`, `browseMarkdownFile()`, `AppSettings.markdown`, `SettingsSavePatch.skip_dirs`.
- `frontend/src/components/views/MdViewer.tsx` — Show-hidden checkbox, Browse button, browsed-option rendering.
- `frontend/src/components/settings/sections/AdvancedSection.tsx` — skip-dirs input + dirty/save.
- `frontend/src/lib/ipc-fixtures/dto-get-settings.json` + `ipc-contract.test.ts` — `markdown` key + assertion.

---

## Findings

### Bugs

#### B1 (medium) — Re-selecting a browsed external file from the dropdown fails with a sandbox error
**File:** `frontend/src/components/views/MdViewer.tsx:86-98`

The `<select onChange>` handler unconditionally calls `loadFile(e.target.value)` → `readFile(path)` (line 24), which invokes the **sandboxed** `read_file` command. `read_file` routes the path through `sandbox.validate` (`src-tauri/src/ipc/files.rs:26-28`), and `Sandbox::validate` → `check_inside` (`src/tool/agent/sandbox.rs:137-143`) rejects any canonical path outside the project root with `PathOutsideRoot`.

The Browse feature's entire purpose is to load `.md` files from **outside** the project (the command is explicitly non-sandboxed — `files.rs:247-249`). But once a browsed external file is loaded:

1. `handleBrowse` sets `path` to the absolute (forward-slash) path and `browsed` to the same (lines 67-68).
2. `showBrowsedOption` is true (the absolute path is not in the relative `files` list), so the option is rendered (lines 77, 94-98).
3. If the user selects a different file and then clicks back on the browsed option, `onChange` fires `loadFile(<absolute external path>)` → `readFile` → `sandbox.validate` → **`Err(PathOutsideRoot)`** → the UI shows `"path rejected: …"` and the file does not reload.

The browsed file's content was loaded once (directly into state by `handleBrowse`), but it cannot be re-loaded from the dropdown. This is a functional dead-end for the feature's primary use case.

**Suggested fix (any one):**
- In the `onChange` handler, no-op (or re-use cached content) when `e.target.value === browsed` — the content is already displayed.
- Or clear `browsed` (set to `null`) when the user selects any non-browsed option, so the browsed option disappears from the list once navigated away from.
- Or store browsed content keyed by path and serve it from cache on re-selection instead of calling the sandboxed `readFile`.

---

### Correctness — no findings

- **`collect_markdown` skip logic** (`files.rs:197-232`): correct. Dot-directories (`name_str.starts_with('.')`) are pruned unless `show_hidden`; configurable skip names are matched by **name** (`s.as_str() == name_str`), so nested `target`/`node_modules` are skipped too — a deliberate, documented change from the old path-based matching. The three unit tests (`hides_dot_dirs_and_skip_names_by_default`, `show_hidden_reveals_dot_dirs_but_not_skip_names`, `custom_skip_list_is_respected`) cover the matrix. The old always-hidden `.coding/plans` and `.claude` are now hidden via the dot-dir rule (preserved by default; revealed only when the user explicitly opts into "Show hidden", which matches the feature intent and the doc comment's `.coding/skills` example).
- **`browse_markdown_file`** (`files.rs:251-281`): `spawn_blocking` is the correct, documented pattern for `blocking_pick_file` (it pumps the OS message loop). `AppHandle` is `Clone + Send` and moved into the closure. Cancel (`None`) → `Ok(None)`. `into_path()` returns `Result<PathBuf, tauri_plugin_dialog::Error>`; `Error` implements `Display` (thiserror), so the `map_err(|e| format!(…))` compiles. `set_directory(&root)` initializes the picker at the project root as required.
- **Settings save path** (`settings.rs:1167-1175`): `general` is cloned from `current.general` at the top of the block (`settings.rs:1094`), so applying `skip_dirs` preserves all other sections. Trim + drop-empty is correct. The frontend (`AdvancedSection.tsx:81-90`) builds the patch conditionally — only sends `skip_dirs` when `skipDirs !== skipDirsSnap` and `summarize_at_fill_rate` when the float differs — so unrelated sections are never clobbered. Snap is updated after save (`setSkipDirsSnap(skipDirs)`).
- **Frontend show-hidden re-fetch** (`MdViewer.tsx:33-46`): `refreshFiles` depends on `[showHidden]`; the `useEffect` depends on `[refreshFiles]`, so toggling the checkbox recreates `refreshFiles` and re-runs the fetch. Correct.
- **Browsed-option rendering** (`MdViewer.tsx:77, 94-98`): `showBrowsedOption = browsed !== null && !files.includes(browsed)` correctly shows the extra option only when the browsed absolute path isn't already represented in the relative `files` list. (Subject to B1 above for re-selection.)

### Security — no findings

- The `browse_markdown_file` command reads arbitrary files outside the sandbox. This is **by design** (documented in the doc comment, `files.rs:247-249`) and is the explicit purpose of the Browse button. The path originates from the OS native file picker (user-initiated, not agent-supplied text), so there is no path-traversal vector — the user can only pick files the OS dialog allows. No secrets are leaked: the command returns only the picked file's path + content to the renderer that requested it. `dialog:default` is the minimal capability. Acceptable.
- `read_file` / `list_files` / `list_markdown_files` remain sandboxed; no sandbox bypass introduced.

### Constitution compliance — no findings

- **Doc comments:** all new public items have doc comments — `MarkdownConfig` + `skip_dirs` field (`general.rs:191-206`), `MarkdownWire` + `skip_dirs` field (`settings.rs:671-679`), `BrowsedFile` (`files.rs:234-242`), `browse_markdown_file` (`files.rs:244-249`), `list_markdown_files` (`files.rs:161-175`), `GetSettingsResponse::markdown` (`settings.rs:627-628`), `SettingsSaveDto::skip_dirs` (`settings.rs:981-984`), `AppSettings.markdown` / `SettingsSavePatch.skip_dirs` / `BrowsedFile` / `browseMarkdownFile` / `listMarkdownFiles` (`tauri.ts`). Private `collect_markdown` is also documented. Consistent with existing style (e.g. `FileEntry` fields have no per-field docs).
- **Line endings:** the `git diff` CRLF warnings are git's autocrlf normalization on pre-existing files, not mixed endings introduced by the edits. No mixed `\r\n`/`\n` introduced.
- **No commit to main:** all changes are uncommitted in the working tree; no commit has been made. The plan commits to a feature branch.

---

## Observations (non-blocking)

- **`package-lock.json`** changes (removal of `"dev": true` from `@types/prop-types`, `@types/react`, `csstype`) are unrelated to this feature — likely spurious drift from an `npm install`. Harmless but adds noise to the commit; consider excluding if not intended.
- **Portability note (out of scope):** on macOS, `rfd`-backed blocking dialogs ideally run on the main thread. This is a Windows 11 project, and `spawn_blocking` is the documented pattern for `blocking_pick_file`, so no action needed here.
- The `Cargo.lock` additions (`tauri-plugin-dialog`, `tauri-plugin-fs`, `rfd`, `libc`) are the expected transitive resolution of the new dependency.

---

## Summary

One medium bug (B1): re-selecting a browsed external file from the dropdown hits the sandboxed `read_file` and errors with `PathOutsideRoot`. Everything else — the skip logic, dialog/spawn_blocking, cancel handling, settings patch preservation, doc comments, line endings, security posture — is correct and constitution-compliant. Fix B1 before committing.
