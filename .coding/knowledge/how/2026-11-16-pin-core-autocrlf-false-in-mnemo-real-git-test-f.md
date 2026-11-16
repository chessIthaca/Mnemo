+++
title = "pin core.autocrlf=false in Mnemo real-git test fixtures"
created = "2026-11-16"
evidence = "src/app/worktrees.rs tests discard_and_release_drops_uncommitted_work and rollback_agent_checkout_frees_an_untouched_pool_and_deletes_the_branch failed with exactly this pair before the fixture set core.autocrlf=false, passed after (cargo test -p mnemo --lib worktrees 15/15)."
+++

Real-git tests in the mnemo crate (tempdir repos) that assert exact file bytes FAIL on this machine when they hand-write LF content over files git checked out or restores: the machine's global git config has core.autocrlf=true, so every checkout and `git reset --hard` writes CRLF, and a hand-written LF "restore" makes `git status --porcelain` report the file modified even though the blob matches (size/eol round-trip check). Fix at the fixture root, not in the assertions: `git config core.autocrlf false` in the test-repo init helper, making checkouts byte-faithful and byte-exact assertions deterministic on every machine (macOS and Windows alike). Symptom pair: assert_eq left "one\r\n" vs "one\n" after a reset, and a clean-restore still reporting "uncommitted work".
