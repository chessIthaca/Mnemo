+++
title = "macOS CI job temporarily disabled — releases Windows-only"
created = "2027-01-11"
+++

User decision 2026-09-20: the macOS job in .github/workflows/build.yml is TEMPORARILY disabled (`if: false` + dated comment on the job) after it failed `cargo test --workspace` again on run 35531680467 (tag v0.1.1) even after the five-test fixes — the release job is Windows-only (`needs: [windows]`, no macos-bundles-aarch64 download, no dist/macos globs) so v0.1.1 can publish. macOS support everywhere else is unchanged (codebase, local builds, README platform claims). Re-enable checklist lives in the macos job's comment in build.yml: delete `if: false`, restore the release job's macos references (needs entry, macos-bundles-aarch64 download, dist/macos globs), and drop the Windows-only note from the new_release skill prompt. Landed via plan f4512a03 on wt/mnemo.

Amended 2027-01-11: The re-enable checklist in build.yml's macos job comment is canonical and complete (review LOW 1 remediated): (1) delete `if: false`; (2) restore the release job's macos references (needs entry, macos-bundles-aarch64 download, dist/macos globs); (3) drop the TEMPORARILY DISABLED notes — the file-header STATUS note, the release job's pointer comment, and the new_release skill's Windows-only note.
