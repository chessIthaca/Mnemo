+++
title = "tool-card expanded views — file_edit diff, read_files line ranges, line-aware links — MERGED into main (e11b76b)"
supersedes = "2026-12-28-tool-card-expanded-views-file-edit-diff-read-fil"
created = "2026-12-28"
+++

MERGED into main at e11b76b2a07b1cb9a9dbe801e5e57c24e165400e on 2026-09-04 (merge_to_main skill), branch wt/agenticcoding deleted (pre-merge tip 49e8110). Tool-card expanded views (commits 4a31d1c + 27535b7): expanding a file_edit card renders the Rust-computed unified diff (result.data.diff via UnifiedDiffView); expanding a read_files card renders a clickable per-file line-range list parsed from section headers; file links deep-link the Files viewer to the read line (openFileInViewer(path, line?) → pendingFileOpen → SourceEditor revealLine). Helpers in frontend/src/lib/toolCardPaths.ts + vitest contracts. Full detail: .coding/knowledge/spec/2026-12-28-tool-card-expanded-views-file-edit-diff-read-fil.md. Plan 9cf99d5e, backlog cb3461fe.
