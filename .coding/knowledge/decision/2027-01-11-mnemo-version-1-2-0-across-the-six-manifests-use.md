+++
title = "Mnemo version -> 1.2.0 across the six manifests (user choice)"
created = "2027-01-11"
+++

User decision (2027-01-11): asked to "set the version of mnemo to 1.0", the user chose 1.2.0 instead — a forward bump from the current 1.1.0, not a downgrade. Scope: the six files carrying the version — Cargo.toml:3, src-tauri/Cargo.toml:3, src-tauri/tauri.conf.json:4, package.json:5, frontend/package.json:5, plus package-lock.json (10 occurrences; keep in sync with npm). Not a release: no tag, no GitHub release, no new_release skill run. Landing path: wt/mnemo -> merge_to_main (main is ruleset-protected: the skill pushes the branch and opens a PR; the human merges). Sequenced AFTER plan 49f53bf5's review closes so the bump is not swept into the reviewed commit.

Amended 2027-01-11: Scope correction — the bump is FIVE manifests plus the About-dialog fallback literal: src-tauri/tauri.conf.json:4, package.json:5, frontend/package.json:5, Cargo.toml:3, src-tauri/Cargo.toml:3, and frontend/src/components/about/AboutDialog.tsx (the useState fallback string; runtime reads the manifest version via getVersion(), the literal is only the error fallback). Refresh the locks afterwards — `npm install --package-lock-only` for package-lock.json and `cargo check` for Cargo.lock — then commit everything on the working branch. Same mechanical scope as .coding/skills/new_release.toml step 1, but NO release: the user asked only for the version change plus landing (no tag, no GitHub release, no new_release skill run).
