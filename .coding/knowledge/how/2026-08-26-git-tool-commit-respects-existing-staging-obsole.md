+++
title = "git tool commit respects existing staging (OBSOLETE — fixed, merged into main)"
supersedes = "2026-08-26-git-tool-commit-auto-stages-everything-use-shell"
created = "2026-08-26"
+++

OBSOLETE (2026-08-26): the git tool's commit subcommand now respects existing staging (merged into main at ed38bd9). The old behavior (unconditional git add -A overriding selective staging) is fixed — commit now checks git diff --cached --quiet first and only auto-stages when nothing is already staged. The shell-for-selective-commits workaround documented in the predecessor is NO LONGER NECESSARY. Regression test: commit_respects_existing_staging (src/tool/agent/git.rs).
