+++
title = "Register new frontend test files in vitest.config.ts include list"
created = "2027-01-07"
+++

frontend/vitest.config.ts uses an EXPLICIT include list (not a glob) — a new *.test.ts(x) file silently does NOT run unless its path is added to `test.include`. GUARDED since 2027-01-07: `src/lib/vitestInclude.test.ts` fails loudly listing any unregistered test file (Vite's import.meta.glob discovery vs the include list, glob→RegExp matching) — a green suite again proves the whole suite ran. Live catches so far: src/lib/tauri.test.ts (noticed manually — the original case), src/App.shellRender.test.ts (caught by the guard's first run).
