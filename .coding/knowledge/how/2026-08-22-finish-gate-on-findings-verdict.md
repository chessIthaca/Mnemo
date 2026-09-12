+++
title = "finish gate on FINDINGS verdict"
created = "2026-08-22"
+++

Re-review after fixing findings must review the FIX COMMIT, not the working tree: if you commit the fixes before re-review, git_diff shows nothing. Spawn the pass-2 reviewer with the commit sha (git show <sha>) and the original findings list.
