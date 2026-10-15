+++
title = "root-only cargo test never compiles src-tauri — battery must be --workspace"
created = "2027-01-11"
+++

Symptom: job-4 D7–D8 commits (698ddff, 1bb38b6) shipped with mnemo-app (src-tauri) UNCOMPILABLE while every "battery green" claim passed — reviewer round 1 caught it (report .coding/reviews/2026-10-13-job-4-standalone-setup-review.md, finding 1).

Root cause: plain `cargo test` at a virtual-workspace ROOT with a root package builds ONLY the root package (mnemo) — src-tauri (mnemo-app) is a separate workspace member that root-only `cargo test` never compiles. Four consecutive green batteries (D7, D8, ×2) masked a hard app-crate break: pub(crate) field read cross-crate, a console twin on a renamed field, wrong sink arg shapes, a dropped State import, unused imports under deny(warnings), and G9 `.engine.` nesting fallout — all accumulated invisibly.

Fix: the closing battery is `cargo test --workspace` (compiles + runs the app crate's tests), plus `cargo test --features embeddings` for the lib's gated tests (src-tauri's dep enables the feature paths, but the lib's own gated unit tests need the flag). Landed in commit 3373096 (fix(4.9), branch wt/mnemo) together with the compile fixes; the compile itself is the red check (red at 1bb38b6, green after).

Rule going forward (this repo): any battery claim must be `cargo test --workspace` — a root-only `cargo test` green proves nothing about src-tauri. Same trap exists for any workspace where the app/shell crate is a member, not the root.
