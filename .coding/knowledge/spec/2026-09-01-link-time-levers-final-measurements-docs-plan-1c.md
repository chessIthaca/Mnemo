+++
title = "link-time levers final measurements + docs (plan 1c712c30)"
created = "2026-09-01"
+++

SPEC (plan 1c712c30 step 8, 2026-12-19): link-time levers FINAL measured results (all warm mtime-bump probes, Windows/MSVC/1.95, post all levers):
- cargo check -p mnemo: 2.32s · cargo test --lib: 13.22s · cargo test --workspace: 21.0s warm (67.2s cold-ish after clean rebuilds integration/app bins) · cargo build -p mnemo-app: 8.76s · from-clean cargo build --workspace: 91.4s (93.2s pre-gates — gates don't change the app's cold build since src-tauri selects browser+embeddings).
- Baseline comparison: test --lib 13.7→13.2s, app rebuild 8.6→8.8s (link unchanged — deps don't recompile in warm loop; lever pays on dep rebuilds + PDB sizes), workspace 19.4→21.0s (suite grew; 4 test bins merged into 1 so fewer links per run).
- CGU=1: rejected (3.2s vs 3.21s, noise).
- Docs updated: README.md Building section (feature flags + dep-debuginfo note), PLAN.md technical-decisions table (link-time levers row). Full detail in .coding/knowledge/link-time-research.md + memory records for levers 1-3.
