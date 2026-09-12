+++
title = "open-existing-project silently re-shows the switch dialog when the target lacks .coding/ (fixed)"
created = "2026-12-23"
+++

BUG (backlog 16e4a7f8, plan 7f5903c6): "Opening an existing project shows the switch project dialog instead of the agent after pressing Open."

Root cause (empirically pinned 2026-12-23, plan 7f5903c6): `switch_project` (src-tauri/src/ipc/projects.rs) validated only `is_dir()`. A registered project whose `.coding/` directory is missing (deleted/moved/never scaffolded) passed validation, the app restarted, and the reload's marker arm in `build_brain_inner` (src-tauri/src/main.rs) fell through silently to `BrainOutcome::NeedsProject` — `needs_project=true` → the startup ProjectPicker (what the user calls "the switch project dialog") reappeared with no explanation. Probes: (1) marker pointing at a valid `.coding` scaffold → project opens (codegraph.db/memory.db created, process alive, Mnemo UI) — the happy path was never broken; (2) marker pointing at a `.coding`-less dir → marker consumed, project NOT opened, stderr `build_brain took 1ms ... (needs-project)`, picker shown, process alive — the reported symptom reproduced exactly.

Fix: (a) `validate_switch_target` in projects.rs — `switch_project` now rejects a directory without `.coding/` with a clear error surfaced in the dialog the user just pressed Open in (safe: `create_project` scaffolds `.coding/` before `switchProject`, per tauri.ts createProject doc); (b) the marker arm now eprintlns the stale path ("mnemo: pending-project marker pointed at '...' which has no .coding/ — showing the picker") so hand-planted/stale markers are diagnosable (verified live on a debug exe, probe 4); (c) doc comments updated in projects.rs (switch_project + helper), main.rs marker arm, frontend/src/lib/tauri.ts switchProject.

Regression test: `switch_target_requires_coding_dir` (src-tauri/src/ipc/projects.rs tests) — fails without the fix (the old is_dir-only check accepted any directory), passes with it. Full suites green: cargo 1872+16 passed / 0 failed; vitest 51 files / 709 passed.

Note: the frontend invoke-before-manage race (startup effect fires while build_brain still runs) was analyzed and is NOT this bug — its outcome is an empty agent list (every catch falls through; the render condition is needsProject-only), and on Windows WebView2 initialization is gated on the message pump, so it doesn't fire in practice.
