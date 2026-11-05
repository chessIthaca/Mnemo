+++
title = "Copy untrusted folders without following links, and test it with junctions on Windows"
created = "2026-11-05"
+++

Any feature that copies a user-supplied or cloned folder into the project (for example Settings > Specs "Add schema" in src-tauri/src/mnemo2/specs_settings.rs) must not follow links. Otherwise a crafted source can pull an arbitrary local file into the repo, such as a private key posing as a template. Walk the whole source first with `entry.file_type()` / `symlink_metadata` and refuse symlinks before writing anything. Copy only regular files and real directories. Clone with `git clone -c core.symlinks=false --depth 1 -- <url>` (the `--` blocks option injection from a URL starting with '-'). On Windows, creating a true symlink usually fails without developer mode (os error 1314), so symlink tests skip there. Directory junctions (`mklink /J`) need no privilege and report `is_symlink()`, so a `#[cfg(windows)]` junction test gives real coverage on every Windows machine.
