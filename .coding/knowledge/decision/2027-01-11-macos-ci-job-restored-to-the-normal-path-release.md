+++
title = "macOS CI job RESTORED to the normal path — releases dual-platform again"
supersedes = "2027-01-11-macos-ci-job-temporarily-disabled-releases-windo-2"
created = "2027-01-11"
+++

RESOLVED 2026-09-21: the macOS CI job is RESTORED to the normal path — the temporary disable (2026-09-20) is over. The macOS leg is back on the normal triggers (workflow_dispatch + v* tag pushes; it never ran on branch pushes) with no `if:` gate, and the release job is dual-platform again: `needs: [windows, macos]`, downloads both windows-installers and macos-bundles-aarch64, and the files list carries dist/windows/**/*.msi|*.exe AND dist/macos/**/*.dmg|*.zip. The `run_macos` dispatch input and every DIAGNOSTIC-ONLY / TEMPORARILY DISABLED note are gone.

Why the disable happened: the macOS leg failed `cargo test --workspace` on runs 35521221353 / 35531680467 / 35514715816 and the failure could not be read back (the Actions job-logs API is admin-only, 403). The real cause — the SIXTH failure, distinct from the five fixed by plan 5cff52cf — was found by adding a diagnostic capture step (tee + always-run artifact upload) and reading it via `gh run download`: `is_adoptable_path` in src-tauri/src/main.rs accepted a whitespace-only captured PATH value, failing the test `adoptable_path_rejects_empty_and_control_characters`. FIX: `!value.trim().is_empty() && !value.chars().any(|c| c.is_control())`. VERIFIED GREEN: diagnostic dispatch run 35578123470 — macos: completed success (plus windows success).

Plan 263a9e31, branch wt/macos-fix, PR #3. The diagnostic capture step INTENTIONALLY REMAINS in the macOS job as the evidence path for any future macOS-only failure (the job-logs API stays admin-only). NOTE: `main` is now protected by the active ruleset "Protect from direct pushing." (id 23755694, required_approving_review_count 1) — direct pushes to main are rejected (GH013); changes must land via pull request, so the merge_to_main skill's direct merge+push flow no longer works on this repo.
