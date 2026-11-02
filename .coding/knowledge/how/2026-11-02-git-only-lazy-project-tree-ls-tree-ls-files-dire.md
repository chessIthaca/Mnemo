+++
title = "git-only lazy project tree (ls-tree + ls-files --directory) and the canonicalize path-confinement idiom"
created = "2026-11-02"
status = "superseded"
+++

A lazy, .gitignore-aware directory tree in this repo is built from two Git calls per directory instead of walking the disk:
- tracked entries: `git ls-tree -z HEAD` (root) or `git ls-tree -z HEAD:<dir>`, one level only, records shaped `<mode> SP <type> SP <sha> TAB <name> NUL`, type `tree` = directory;
- untracked entries: `git ls-files --others --exclude-standard --directory -z -- <dir>/` (spec `.` at the root), which emits `<path> NUL`, no `./` prefix, trailing `/` on condensed directories.

Merging the two and taking each path's first component under the listed directory yields immediate children with ignore rules honoured and tracked-but-ignored files still shown; it also keeps the work O(dir), not O(repo). If `ls-files` fails (not a Git work tree), fall back to std::fs::read_dir and skip `.git`.

Confinement idiom (used by src-tauri/src/mnemo2/files_view.rs): reject NUL bytes and every non-Normal path component first (catches `..`, `/etc`, `C:\...`, `\\server\share`), then canonicalize root.join(rel) and require starts_with(canonical root) so symlinks/junctions that escape are refused; the lexical pass runs before the filesystem so the error message is about the spelling, and the canonical pass covers links. A red-check that disables both makes the escape tests reach the parent directory.
