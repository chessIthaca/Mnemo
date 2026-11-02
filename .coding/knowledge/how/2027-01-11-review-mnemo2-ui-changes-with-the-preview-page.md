+++
title = "Review Mnemo2 UI changes with the preview page"
created = "2027-01-11"
+++

To look at a Mnemo2 UI change without the Tauri app, run `npx vite` in mnemo2/ (or `npx --prefix <worktree>/mnemo2 vite <worktree>/mnemo2 --port <free port> --strictPort` for a worker's worktree) and open http://127.0.0.1:<port>/preview.html?scene=<name>. Use 127.0.0.1, not localhost. Scenes: mission-control (selected worker with a Markdown thread, tool rows, inbox), worker-thread, lead-chat, lead-idle (idle Claude Code badge), delete-sheet; an unknown scene shows an index. Sample data lives in mnemo2/src/test/previewScenes.ts; add a scene there rather than writing a throwaway harness. preview.html is not a build input and check:no-mock ignores it. Run the server through a subshell or launch config so no shell keeps the worktree folder open, and stop it before merging.
