+++
title = "Check a worker branch's merged result before team_merge when parallel steps shared files"
created = "2026-11-02"
+++

`git merge-tree` only proves the text merges; to check the merged code, add a detached scratch worktree at the worker's branch, merge the lead branch into it (with a throwaway user.name/email), link dependencies with Windows junctions (`cmd //c "mklink /J <scratch>\node_modules C:\Mnemo\node_modules"`, plus mnemo2\node_modules, which holds only svelte-check, typescript and highlight.js because the workspace hoists to the root), and run `npx svelte-check` and `npx vitest run` in the scratch mnemo2 folder. Clean up by removing the junctions first with `cmd //c rmdir <junction>` (a forced delete could follow the link into the real node_modules), then `git worktree remove --force` and `git worktree prune`. Two branches that both ADD the same new file merge cleanly only when the bytes are identical; when one worker must reuse another's new file, copy it unchanged and defer edits to a later step.
