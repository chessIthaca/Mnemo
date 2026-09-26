+++
title = "merge_to_main wrote its landing records after the branch was frozen — they dangled uncommitted"
created = "2027-01-11"
+++

BUG: merge_to_main's bookkeeping ran AFTER the branch was frozen → its records could never land (8 successor SPECs + 9 superseded markers + backlog.jsonl sat uncommitted on main after PR #7; PR #7's landing had needed closeout plan fc41c5aa for the same reason). Root cause: `.coding/skills/merge_to_main.toml` step 5 superseded the branch's status records after step 3/4's commit+push+merge (direct path: after `git branch -d`); protected main rejects the push and the carrying branch is gone. A transported record cannot name its own merge sha (a commit cannot contain its own hash), and the PR number exists only after `gh pr create`. Fix: write the records BEFORE the commit, phrased on pre-landing facts (`— MERGED into main (branch <name>, <date>)` + work tip), plus a no-write-after-landing rule and a clean-tree check. Regression: merge_to_main_and_new_release_write_their_records_before_the_landing_commit (src/skill/mod.rs).
