+++
title = "Mnemo version -> 1.2.0 across the six manifests (user choice)"
created = "2027-01-11"
status = "superseded"
+++

User decision (2027-01-11): asked to "set the version of mnemo to 1.0", the user chose 1.2.0 instead — a forward bump from the current 1.1.0, not a downgrade. Scope: the six files carrying the version — Cargo.toml:3, src-tauri/Cargo.toml:3, src-tauri/tauri.conf.json:4, package.json:5, frontend/package.json:5, plus package-lock.json (10 occurrences; keep in sync with npm). Not a release: no tag, no GitHub release, no new_release skill run. Landing path: wt/mnemo -> merge_to_main (main is ruleset-protected: the skill pushes the branch and opens a PR; the human merges). Sequenced AFTER plan 49f53bf5's review closes so the bump is not swept into the reviewed commit.

Amended 2027-01-11: Scope correction — the bump is FIVE manifests plus the About-dialog fallback literal: src-tauri/tauri.conf.json:4, package.json:5, frontend/package.json:5, Cargo.toml:3, src-tauri/Cargo.toml:3, and frontend/src/components/about/AboutDialog.tsx (the useState fallback string; runtime reads the manifest version via getVersion(), the literal is only the error fallback). Refresh the locks afterwards — `npm install --package-lock-only` for package-lock.json and `cargo check` for Cargo.lock — then commit everything on the working branch. Same mechanical scope as .coding/skills/new_release.toml step 1, but NO release: the user asked only for the version change plus landing (no tag, no GitHub release, no new_release skill run).

Amended 2027-01-11: Landed on wt/mnemo as commit 3ca47f7 (plan dd928931, 2027-01-11): all six sites plus both generated locks read 1.2.0 — Cargo.toml / src-tauri/Cargo.toml / src-tauri/tauri.conf.json / package.json / frontend/package.json / AboutDialog.tsx:42, with Cargo.lock's mnemo + mnemo-app entries and package-lock's root/workspace fields refreshed by cargo check + npm install --package-lock-only (no dependency moved, no resolved/integrity touched). Review PASS: .coding/reviews/2026-09-27-mnemo-1.2.0-version-bump-review.md. Landing via merge_to_main follows — PR path expected under ruleset 23755694, human merges.
