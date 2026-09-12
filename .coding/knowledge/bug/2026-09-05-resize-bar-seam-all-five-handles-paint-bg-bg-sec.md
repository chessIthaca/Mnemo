+++
title = "resize-bar seam — all five handles paint bg-bg-secondary (direction reversed per user)"
supersedes = "2026-08-24-horizontal-resize-bars-showed-bg-secondary-seam"
created = "2026-09-05"
+++

BUG: resize-bar seam direction — all five handles must paint bg-bg-secondary (user follow-up reversed the 2026-09-05 fix). Original symptom: "horizontal resize bar has the wrong background color vs vertical ones" — all five handles are transparent 6px strips, so the seam was an accident of the backdrop (vertical on bg-bg-primary #0f172a, horizontals on bg-bg-secondary #1e293b surfaces). Commit 75e4c82 painted every strip bg-bg-primary so the horizontals matched the vertical. USER FOLLOW-UP (2026-09-05): they wanted the OPPOSITE — the vertical handle should match the horizontal bars' lighter seam. Final fix: every handle paints bg-bg-secondary between its border hairlines (App.tsx:805 border-x; InflightBar.tsx:212, FileViewer.tsx:373, GraphView.tsx:779, LlmTraceView.tsx:979 border-y). Regression test: frontend/src/components/resizeHandleMotif.test.ts (source-contract via ?raw imports, fails if any handle drops bg-bg-secondary or the shared slate-500 grip pill).
