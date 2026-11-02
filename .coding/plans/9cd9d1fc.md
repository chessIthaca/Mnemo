# Plan: Delete old workers: instant dialog with progress, faster removal, failures don't stop the rest

## Goal
Pressing "Delete old" opens the dialog at once with per-worker progress until it is ready, can't be triggered twice, deletes noticeably faster, and deletes every worker it can even when some fail, reporting each failure.

## Kind
bug_fixing

## Context
Why: the user: "When i press the delete old button it can take a long time. I can still press the button which opens the dialog twice. Can we instead open the dialog and show progress until it can be interacted with? And also, does it have to be that slow?" and "if one delete fails, the others still could be successful, no?"

Verified facts:
- verified: the button is in `mnemo2/src/components/ControlRoom.svelte` line 114 (`onclick={() => actions.requestDeleteOldWorkers()}`, disabled only when `oldCount === 0`). `requestDeleteOldWorkers` → `requestDeleteWorkers` (`mnemo2/src/lib/store.ts` ~1680-1699) awaits `transport.deletePreview(ids)` BEFORE setting `workerDeletion`, so nothing is shown and nothing blocks a second click while the preview runs. `confirmDeleteWorkers` (~1706) sets `busy` and awaits `transport.deleteWorkers(..)`; the sheet is `mnemo2/src/components/WorkerDeleteSheet.svelte`.
- verified: backend `TeamRuntime::deletion_preview` (`src-tauri/src/mnemo2/team.rs` ~3898) takes `self.merge_lock` and calls `inspect_deletion` (~3967) for each worker SEQUENTIALLY. Per worker it runs `checkout_is_registered` (a full `git worktree list --porcelain -z`, ~4164), `current_branch`, `git status --porcelain -uall`, `branch_exists` and `git rev-list --count HEAD..refs/heads/<branch>`. Measured here: each of those git calls takes about 0.1 s, so the preview is roughly 0.4-0.5 s per worker plus any wait for the merge lock (held during merges).
- verified: `delete_workers` (~3916) takes the merge lock, re-checks every worker, stops live ones, then calls `self.delete_one(&deletion).await?` in a loop (~3940): the `?` returns on the first failure, so later workers are never deleted. `delete_one` removes the checkout with `remove_agent_worktree` (git worktree remove --force) in a retry loop of up to 12 tries with 50 ms sleeps (~4053-4070).
- verified: worker checkouts are large: `du -sh` on one `.worktrees/agent-*` folder did not finish in two minutes (node_modules, build output), so deleting folder contents file by file is the dominant cost.
- verified: related history: `.coding/knowledge` BUG "Finish half-removed worker checkouts on merge and delete" and the DECISION that checkout removal waits for every owner of the worktree code graph (Windows locks `.coding/codegraph.db`); keep those guarantees.

Required behaviour:
1. UI: clicking Delete old (or a single worker's delete) opens the sheet immediately in a "checking" state listing the workers with a spinner each; the button is disabled while a deletion request is open or running; a second click does nothing. Rows fill in as each worker's preview arrives (stream per-worker results with an event such as `mnemo2://worker-deletion` or return them incrementally) and the confirm button enables when all are checked. During deletion each row shows its own progress and result (deleted / failed with the reason); the sheet closes on full success or stays open showing failures with a Retry for the failed ones.
2. Preview speed: run `git worktree list` once per preview (not per worker), check workers concurrently (bounded, e.g. 4 at a time), use `git status --porcelain --untracked-files=normal` (no recursion into untracked directories; we only need "anything uncommitted"), and read ahead/behind for all branches in one `git for-each-ref` call where possible. Do not hold the merge lock for the read-only preview if the deletion re-checks under the lock anyway.
3. Deletion speed: move each checkout folder out of the way with a single rename into a trash folder (e.g. `.worktrees/.trash/<id>-<timestamp>`), prune/deregister the git worktree, delete the branch, and remove the trash folder in the background (retrying later, also on app start), so the user waits for a rename instead of deleting tens of thousands of files. If the rename fails because something holds the folder, fall back to today's path and report the holder (the existing lock-holder message).
4. Partial failure: delete every worker that can be deleted; collect per-worker errors and return them (e.g. `Vec<WorkerDeletionResult>` plus the snapshot) instead of returning on the first error.

Regression test: `delete_workers_continues_past_a_failed_worker` in the `#[cfg(test)]` module of `src-tauri/src/mnemo2/team.rs`: three old workers where the middle one's deletion fails (e.g. its checkout is held open or made undeletable); assert the first and third are deleted, the middle one is reported with its error and still listed. Without the fix the third worker survives because the loop returned on the middle one's error. Add a vitest for the store: the sheet state exists before the preview resolves and a second request while one is open is ignored.

Verification: `cargo test --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib mnemo2::team`, `cargo clippy --manifest-path src-tauri/Cargo.toml --features mnemo2 --lib --tests --bins -- -D warnings`; in `mnemo2/`: `npm run check`, `npm run check:no-mock`, `npx vitest run`. Never start a dev server or any command that does not exit on its own.

## Acceptance scenarios
- Clicking Delete old opens the dialog immediately showing each worker as 'checking', and clicking the button again does nothing.
- Deleting ten old workers with large checkouts finishes in seconds from the user's point of view; the folders disappear in the background.
- If one worker cannot be deleted, all others are deleted, and the dialog shows which one failed and why, with a Retry.

## Steps
- [x] 1. **Instant sheet with progress, faster preview and deletion, per-worker results** — `src-tauri/src/mnemo2/team.rs` (deletion_preview, inspect_deletion, delete_workers, delete_one, trash sweep; regression test `delete_workers_continues_past_a_failed_worker`), `src-tauri/src/mnemo2/ipc.rs` (team_delete_preview / team_delete return shapes, progress event), `mnemo2/src/lib/store.ts` (requestDeleteWorkers / confirmDeleteWorkers region only), `mnemo2/src/components/WorkerDeleteSheet.svelte`, `mnemo2/src/components/ControlRoom.svelte` (button disabled state), tests.

## Bug
In mission control, "Delete old" takes a long time before its dialog appears, so the button can be pressed twice and open the dialog twice; the deletion itself is slow; and when one worker's deletion fails, the remaining workers are not deleted.

## Regression test
delete_workers_continues_past_a_failed_worker

## Progress
- 2026-11-02 step 1 done: step 1 merged: deleteFlow89 (deleteFlow89) on glm-5.3, merge commit d97dea78; review: Delete old: the sheet opens at once and streams per-worker preview rows, the button can't double-open, previews batch git facts with bounded concurrency and no merge lock, checkouts are renamed into .worktrees/.trash and swept in the background (also at startup), and deletion returns per-worker results so one failure no longer stops the rest.
