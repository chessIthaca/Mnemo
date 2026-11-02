+++
title = "index-based lazy project tree (git ls-files --cached --others --directory) and the canonicalize path-confinement idiom"
supersedes = "2026-11-02-git-only-lazy-project-tree-ls-tree-ls-files-dire"
created = "2026-11-02"
+++

A lazy, .gitignore-aware directory tree in this repo is built from ONE Git call per directory instead of walking the disk:

`git ls-files --cached --others --exclude-standard --directory -z -- <dir>/` (pathspec `.` at the root).

- `--cached` is what makes staged-but-uncommitted files visible: a `git add`-ed path is in neither `HEAD` (so `git ls-tree` misses it) nor `--others` (which lists only unstaged files), so a HEAD-based listing silently hides the file the user just staged, and a staged deletion lingers. Read the index, not HEAD.
- Output is `<path> NUL`, no `./` prefix, trailing `/` on condensed untracked directories. Derive each path's first component under the listed directory to get immediate children (a remaining `/` means a directory).
- Cost is O(files under the listed directory), not O(repo walk), and a non-Git work tree falls back to std::fs::read_dir (skip `.git`).

Confinement idiom (src-tauri/src/mnemo2/files_view.rs): reject NUL bytes and every non-Normal path component first (catches `..`, `/etc`, `C:\...`, `\\server\share`), then canonicalize root.join(rel) with dunce and require starts_with(canonical root) so symlinks/junctions that escape are refused. Lexical pass first so the error names the spelling; canonical pass covers links. A red check that disables both makes the escape tests reach the parent directory.

Persisted-tab note: the accepted Workbench tabs live in one Rust constant, `WORKBENCH_TABS` in src-tauri/src/mnemo2/project.rs, checked by ProjectPreferences::validate (the persisted width bound is 320..=8192, window-relative). Adding a tab means appending there AND to the frontend's WorkbenchTab union and ProjectPreferences.workbench_tab in mnemo2/src/lib/projectEngine.ts.
