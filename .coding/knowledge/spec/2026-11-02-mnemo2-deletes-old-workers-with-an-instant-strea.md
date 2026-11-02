+++
title = "Mnemo2 deletes old workers with an instant streaming sheet, trash renames and per-worker results"
created = "2026-11-02"
+++

Mission control's Delete old (and single-worker delete) opens WorkerDeleteSheet immediately with a "checking" row per worker and ignores a second request while one is open; rows fill in from mnemo2://worker-deletion events. The preview (src-tauri/src/mnemo2/team.rs deletion_preview_streaming) runs without the merge lock, gathers git facts once per batch (one `git worktree list`, one `git for-each-ref --format=%(refname) %(ahead-behind:HEAD)` with a rev-list fallback) and checks checkouts in chunks of DELETION_CHECK_CONCURRENCY with `git status --porcelain --untracked-files=normal`. Deletion (under the merge lock, re-checked) renames each checkout into the gitignored `.worktrees/.trash/` with one rename, prunes the worktree, deletes the branch, and sweeps the trash in the background (also at TeamRuntime open); a refused rename falls back to the foreground removal that names the holding process. It returns TeamDeleteOutcome with a result per worker, so one failure never stops the others; the sheet keeps failed rows with a Retry.
