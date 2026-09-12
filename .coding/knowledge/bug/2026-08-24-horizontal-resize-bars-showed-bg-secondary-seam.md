+++
title = "horizontal resize bars showed bg-secondary seam (transparent-strip drift)"
created = "2026-08-24"
status = "superseded"
+++

BUG: "horizontal resize bar has the wrong background color vs vertical ones" (user, 2026-09-05). Symptom: every horizontal resize bar rendered a lighter #1e293b strip between its hairlines while the vertical App.tsx handle showed the dark app-bg #0f172a. Root cause: all five handles are transparent 6px strips — the seam color was an accident of the surface behind each strip: the vertical one floats on the app root (bg-bg-primary), while InflightBar's handle sits inside its own bg-bg-secondary container and the FileViewer/GraphView/LlmTraceView splitters sit inside RightPanel (bg-bg-secondary). Fix: every handle strip now explicitly paints bg-bg-primary between its border hairlines (App.tsx:805 border-x; InflightBar.tsx:206, FileViewer.tsx:371, GraphView.tsx:776, LlmTraceView.tsx:976 border-y) — the seam is the token, not the backdrop; InflightBar also gained transition-colors for motif parity. Regression test: frontend/src/components/resizeHandleMotif.test.ts (source-contract via ?raw imports, 10 tests, registered in frontend/vitest.config.ts) — fails if any handle drops bg-bg-primary or the shared slate-500 grip pill.
