+++
title = "full test coverage = root cargo test + cargo test -p mnemo-app + tsc + vitest"
created = "2027-01-07"
+++

Symptom: a plan's "cargo test green" claim was false for the committed state — a broken test module in src-tauri (E0425, gate used before binding in run_all.rs tests) shipped in commit 06aca97 and was only caught by the round-2 review. Root cause: `cargo test` from the repo root does NOT compile the src-tauri app crate's unit tests — the root run covers only the mnemo lib (2022 unit + 19 integration + doc tests). The app crate's 223 unit tests compile only under `cargo test -p mnemo-app`. The README's "cargo test from the repo root (workspace: library + app)" phrasing is misleading — the root invocation alone is NOT sufficient coverage. Rule: after ANY src-tauri change, run BOTH `cargo test` (root, lib) AND `cargo test -p mnemo-app` (app unit tests) — plus `npx tsc --noEmit` and `npm test` in frontend/ for any frontend change (vitest does not type-check; TS2741-class errors surface only under tsc). Discovered 2027-01-07 during plan 7369d7f3 (auto-compact between run-all items), round-2 review HIGH-1.
