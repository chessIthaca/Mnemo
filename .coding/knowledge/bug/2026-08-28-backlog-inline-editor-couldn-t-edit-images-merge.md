+++
title = "backlog inline editor couldn't edit images — MERGED"
supersedes = "2026-08-28-backlog-inline-editor-couldn-t-edit-images-text"
created = "2026-08-28"
+++

BUG: backlog inline editor couldn't edit images — MERGED into main at a019c80f (2026-09-11, merge_to_main skill), branch wt/agenticcoder deleted, not pushed. Symptom: "For items in the backlog I can't edit the images or add new screenshots" (backlog item cb04ddea). Root cause: per-card inline editor in frontend/src/components/views/BacklogView.tsx was text-only — handleSaveEdit passed item.images unchanged, no thumbnail/remove/paste/drop UI in edit mode. Fix: editImages state + removable strip + paste/drop + save persists editImages; display strip gated on !editing. Regression: 6 source-contract tests in BacklogView.test.ts ("BacklogView inline editor image editing").
