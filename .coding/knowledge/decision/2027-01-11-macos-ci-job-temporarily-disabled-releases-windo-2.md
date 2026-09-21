+++
title = "macOS CI job temporarily disabled — releases Windows-only — MERGED into main (53534b6)"
supersedes = "2027-01-11-macos-ci-job-temporarily-disabled-releases-windo"
created = "2027-01-11"
+++

User decision 2026-09-20: the macOS job in .github/workflows/build.yml is TEMPORARILY disabled (`if: false` + dated comment) after it failed `cargo test --workspace` again on run 35531680467 (tag v0.1.1) even after the five-test fixes — the release job is Windows-only (`needs: [windows]`, no macos-bundles-aarch64 download, no dist/macos globs) so v0.1.1 can publish. MERGED into main at 53534b6 on 2026-09-21 via the merge_to_main skill (plan f4512a03; branch wt/mnemo deleted, pre-merge tip 9af6428). macOS support everywhere else is unchanged (codebase, local builds, README platform claims). The re-enable checklist in build.yml's macos job comment is canonical and complete: (1) delete `if: false`; (2) restore the release job's macos references (needs entry, macos-bundles-aarch64 download, dist/macos globs); (3) drop the TEMPORARILY DISABLED notes — the file-header STATUS note, the release-job pointer comment, and the new_release skill's Windows-only note.
