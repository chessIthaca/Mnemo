+++
title = "read_files ToolCard duplicate/missing filenames in header chips"
created = "2026-08-26"
+++

BUG: read_files/file-tool ToolCard headers show duplicate filenames + sometimes none (backlog 2a03710d, 2026-09-08). Root cause: Message.tsx ToolCard chip loop (lines ~498-516) renders one chip per path PER CALL with NO dedup — same file read in two grouped calls → duplicate basename chips. "Not shown" = edge case: pathless/malformed args (empty files array) → argPaths returns [] + argLabel null → bare header; also >3-files cap hides names beyond 3rd. Rust read_files result body emits `=== <path> (lines X-Y of Z) ===` per file (read_files.rs:286-290) so expanded cards also show the name in the body. Fix: dedup header path-chips by normalized path (case-insensitive, slash-normalized), keep first occurrence's link; extract pure dedupeChips in toolCardPaths.ts + vitest tests. Frontend test: `npm test` (vitest run) in frontend/.
