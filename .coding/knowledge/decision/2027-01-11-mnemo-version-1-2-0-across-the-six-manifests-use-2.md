+++
title = "Mnemo version -> 1.2.0 across the six manifests (user choice) — MERGED into main (branch wt/mnemo, 2026-09-27)"
supersedes = "2027-01-11-mnemo-version-1-2-0-across-the-six-manifests-use"
created = "2027-01-11"
+++

User decision (2027-01-11): asked to "set the version of mnemo to 1.0", the user chose 1.2.0 instead — a forward bump from 1.1.0, not a downgrade. Scope: the five manifests (Cargo.toml:3, src-tauri/Cargo.toml:3, src-tauri/tauri.conf.json:4, package.json:5, frontend/package.json:5) plus the AboutDialog.tsx:42 fallback literal (runtime reads the manifest version via getVersion(); the literal is only the error fallback); both generated locks refreshed (cargo check; npm install --package-lock-only) — never hand-edited, no dependency moved. Not a release: no tag, no GitHub release (CI owns release creation on v-tags). Landed as 3ca47f7 (plan dd928931). Review: PASS — .coding/reviews/2026-09-27-mnemo-1.2.0-version-bump-review.md. Branch wt/mnemo @ 3ca47f7952fed4d51a32a01dc394766c5d57eb87 (work tip, 2026-09-27) — a PR against main is opened from this branch, awaiting human review; the landing becomes part of main when the human merges.
