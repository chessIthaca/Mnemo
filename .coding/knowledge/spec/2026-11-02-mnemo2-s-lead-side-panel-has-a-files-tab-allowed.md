+++
title = "Mnemo2's lead side panel has a Files tab; allowed workbench tabs live in WORKBENCH_TABS"
created = "2026-11-02"
+++

The lead's Workbench has Browser, Diff, Plan and Files. Files (FilesTab.svelte, lib/files.ts, src-tauri/src/mnemo2/files_view.rs: files_list, files_read, files_find) shows a lazy git-aware tree (index entries including staged files, plus untracked, with a disk fallback) and a read-only highlighted viewer; every path is confined to the project (NUL, '..' and absolute paths refused, canonicalized and checked under the root), reads cap at 256 KB with binary detection. Saved project preferences validate the open tab against WORKBENCH_TABS in src-tauri/src/mnemo2/project.rs (also the TS WorkbenchTab and ProjectPreferences.workbench_tab unions) and the width against 320..=8192; a new Workbench tab must be added to all three or every preference save fails.
