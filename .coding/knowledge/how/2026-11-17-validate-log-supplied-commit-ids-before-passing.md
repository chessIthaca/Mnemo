+++
title = "validate log-supplied commit ids before passing them to git"
created = "2026-11-17"
evidence = "src-tauri/src/mnemo2/quality.rs tests changed_files_rejects_option_like_commit / changed_files_accepts_a_real_commit; manual repro showed git diff --output=<tmp>/leak.txt creates the file."
+++

Never hand a string from a committed, union-merged log (e.g. `.coding/laya/dispatch_routing.jsonl` merge_commit) to git as a revision: git parses it as an option (`--output=<path>` makes `git diff` write a file wherever it names). Validate revisions as 7–64 ASCII hex digits (is_ascii_hexdigit) and append `--` to the git argument list. Implemented in `changed_files_commits` in src-tauri/src/mnemo2/quality.rs via `is_plain_commit_id`; regression test `changed_files_rejects_option_like_commit` is red without it (git creates leak.txt).
