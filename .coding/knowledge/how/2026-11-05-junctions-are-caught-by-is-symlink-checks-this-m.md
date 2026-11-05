+++
title = "junctions are caught by is_symlink checks; this machine refuses true symlinks"
created = "2026-11-05"
+++

Windows environment + Rust facts for symlink-related security work (verified 2026-11 by a rustc probe on this machine):

1. On this toolchain/Windows 11, `std::fs::symlink_metadata(path).file_type().is_symlink()` and `DirEntry::file_type().is_symlink()` return TRUE for directory junctions (mklink /J reparse points), with file_type().is_dir()=false while `Path::is_dir()`/`Path::is_file()` through the link follow it. So any "refuse symlinks" gate written with symlink_metadata/entry.file_type + is_symlink automatically refuses junctions too — the one link kind any Windows user can create WITHOUT privilege (no developer mode, no elevation). No separate junction handling is needed; just do NOT use Path::is_dir()/fs::copy alone, which follow both junctions and symlinks.

2. This machine CANNOT create true symlinks: std::os::windows::fs::symlink_file/symlink_dir fail with os error 1314 ("A required privilege is not held by the client"; no developer mode). Tests needing symlinks must try the link and skip with a printed message on that error. For a regression test that actually runs red/green on this machine, use a junction via `cmd /c mklink /J <link> <target>` in a temporary, never-committed test.
