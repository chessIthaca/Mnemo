+++
title = "give parallel steps distinct anchors in shared list files"
created = "2026-11-02"
+++

When two or more workers add entries to the same list at the same time, their appends collide at merge. Examples are the generate_handler! list and module lines in src-tauri/src/mnemo2/mod.rs, and the scene list in mnemo2/src/test/previewScenes.ts plus its test. Workers cannot merge or rebase, so the lead has them move their insertions instead. Prevent it in the brief: name an exact, distinct anchor per step ("insert directly after `mnemo2_host_ping,`", "put the scene after the `lead-chat` entry"), at least two unchanged lines apart from other steps' anchors. Never say "append at the end". Before merging, dry-run with `git merge-tree --write-tree <lead branch> <worker branch>`. Wrong-position or duplicate handler entries still compile, so check the handler block by eye in review.
