# Review: Startup project picker + runtime project switching

**Scope:** All uncommitted changes (`git diff HEAD` + untracked files) for the
project-picker feature.

**Files reviewed:** `src/config/mod.rs`, `src/project/mod.rs`,
`src-tauri/src/main.rs`, `src-tauri/src/ipc/state.rs`,
`src-tauri/src/ipc/projects.rs` (new), `src-tauri/src/ipc/mod.rs`,
`frontend/src/lib/tauri.ts`, `frontend/src/components/projects/ProjectPicker.tsx`
(new), `frontend/src/App.tsx`, `frontend/src/components/layout/Sidebar.tsx`.

**Verdict:** No blocking correctness, security, or constitution-compliance
findings. The marker consume-on-read semantics, `build_brain` resolution order,
switch/create flows, and NeedsProject IpcState are all correct. The findings
below are minor bugs / nits worth cleaning up.

---

## Correctness — no findings

- **Marker consume-on-read:** `consume_marker_at` (config/mod.rs:411-425) reads
  then deletes the file before returning, so the marker is always consumed on
  read — even when `build_brain` (main.rs:461-467) finds the pending path is not
  a project and falls through to the picker. No stale re-trigger across
  restarts. Pinned by the four new unit tests. ✓
- **Resolution order** (main.rs:447-491): pending marker → `--project` flag →
  auto-detect ancestor → NeedsProject. Correct: the marker (most recent UI
  action) wins over the CLI flag; the flag initializes a non-project (preserving
  the CLI workflow); auto-detect uses `find_coding_dir_ancestor`; the registry
  is deliberately not auto-picked. ✓
- **`switch_project`** (projects.rs:114-129): validates `is_dir()`, writes the
  marker via `write_pending_project`, then `tauri::process::restart(&app.env())`.
  The marker is written before restart. ✓
- **`create_project`** (projects.rs:60-105): validates `is_dir()`, scaffolds via
  `Project::init`, registers via `config.projects.add`, saves `projects.toml`
  directly (correct — `Config::save_all` deliberately does not touch
  `projects.toml`). The in-memory config is mutated in place under the lock, so
  `list_projects` sees the new entry immediately without a full reload — this is
  consistent and avoids a save/reload race. (The task brief says "reloads
  config"; the code mutates in place, which is equivalent and safer.) ✓
- **NeedsProject IpcState** (main.rs:204-256): `factory: None`, but the project
  commands only use `state.project.config` (populated with the real config) and
  `state.runtime.needs_project`. Factory-dependent commands (`send_prompt`,
  `spawn_agent`, `set_model`, `enter_skill`) all guard on
  `state.runtime.factory` and return `"agent factory unavailable (brain failed
  to start)"` — clean errors. ✓
- **`pick_directory`** (projects.rs:155-171): blocking dialog correctly run on
  `spawn_blocking`; `FilePath::into_path()` error handled. ✓

---

## Bugs (minor)

### B1. Redundant `config.clone()` in the NeedsProject arm
**File:** `src-tauri/src/main.rs:242`
```rust
config: Arc::new(tokio::sync::Mutex::new(config.clone())),
```
`config` is owned (moved out of `BrainOutcome::NeedsProject(config)` by the
match) and used only here. The `.clone()` is redundant — `config` can be moved
directly (`Mutex::new(config)`). Harmless (Config is cheap to clone) but
unnecessary; also trips `clippy::redundant_clone` if clippy runs in CI.
**Fix:** drop `.clone()`.

### B2. `handleRemove` doesn't gate with `busy`
**File:** `frontend/src/components/projects/ProjectPicker.tsx:68-76`
`handleRemove` never sets `busy=true`, so the Open/Create buttons (and the
remove buttons on other rows) stay enabled during the remove + refresh round
trip. A user could click "Open" on another project mid-remove, racing the
`removeProject` IPC against `switchProject`'s restart. No data corruption (the
remove only touches the registry), but it's a confusing UX race.
**Fix:** `setBusy(true)` at the start of `handleRemove` and `setBusy(false)` in
a `finally` (mirroring `handleOpen`/`handleCreate`).

### B3. Missing `refresh` in `useEffect` deps (lint consistency)
**File:** `frontend/src/components/projects/ProjectPicker.tsx:51-53`
```tsx
useEffect(() => {
  void refresh();
}, []);
```
`refresh` is an inline closure, so `react-hooks/exhaustive-deps` flags it as a
missing dependency. This doesn't fail the `tsc + vite` build, but `App.tsx`
disables the same lint explicitly (`// eslint-disable-next-line
react-hooks/exhaustive-deps`). For consistency, either add the disable comment
or wrap `refresh` in `useCallback` (preferred — it also lets `handleRemove`
call a stable reference).
**Fix:** add the eslint-disable comment (matching App.tsx) or `useCallback`.

### B4. Duplicate comment line in tests
**File:** `src/config/mod.rs:810-811`
```rust
    // ── pending-project marker ──────────────────────────────────────────────
    // ── pending-project marker ──────────────────────────────────────────────
```
Copy-paste artifact — the section header is duplicated.
**Fix:** delete one line.

---

## Security — no findings

- `create_project` and `switch_project` both validate `dir.is_dir()` before any
  filesystem operation. ✓
- No path traversal: paths originate from the native folder picker
  (`pick_directory`) or from registered entries; `Project::init` only creates
  `.coding/` + `agent.md` inside the chosen directory. ✓
- The marker file lives in the global config dir
  (`~/.myharness/.pending_project`) and holds a plain path string read back via
  `PathBuf::from` — no injection surface. ✓
- `remove_project` only calls `config.projects.remove` + `save` — no file
  deletion on disk. ✓

---

## Constitution compliance — no findings

- **Warning-free under `#![deny(warnings)]`:** no `#[allow(...)]` suppressions
  added. All new public functions (`pending_project_path`,
  `write_pending_project`, `take_pending_project`, `find_coding_dir_ancestor`,
  and the six `#[tauri::command]` fns) have doc comments. Private helpers
  (`write_marker_at`, `consume_marker_at`) are exercised by tests (no dead-code
  warning). The `BrainOutcome` enum and its variants are all used. ✓
- **Windows/PowerShell:** no shell commands in the diff; all paths use
  cross-platform `Path`/`PathBuf`. ✓
- **No commit to main:** this is a review of uncommitted changes; no commits in
  the diff. ✓

---

## Observations (not bugs — design/accessibility notes)

### O1. Missing `DialogDescription` in ProjectPicker Dialog mode
**File:** `frontend/src/components/projects/ProjectPicker.tsx:241-260`
The switch-mode Dialog uses `DialogTitle` but not `DialogDescription`. Radix
Dialog logs a console warning when a dialog has a Title but no Description
(missing `aria-describedby`). The codebase's other dialogs
(`MergeToMainDialog`, `SafetyToggleDialog`) include `DialogDescription`.
**Suggestion:** add a visually-hidden `DialogDescription` for accessibility
consistency.

### O2. One-shot marker → re-launch may re-show the picker
By design, the marker is consumed on the first read. After a successful switch
+ restart, the project opens via the marker. But on a *subsequent* launch (user
closes and reopens the app) with a cwd that is not inside the chosen project,
there is no marker, auto-detect fails, and the registry is not auto-picked — so
the picker shows again. This is the documented/intended behavior (the marker is
one-shot; the registry is never auto-opened), but it may surprise users who
expect the app to "remember" the last-opened project across sessions. Not a
code defect — noting for product awareness.
