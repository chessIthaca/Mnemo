## Verdict: FINDINGS (0 high, 1 low)

Plan f4512a03 (disable macOS CI job — publish v0.1.1 Windows-only): the workflow change is correct and complete. The release job publishes cleanly from the windows artifact alone (needs/artifact-download/file-globs all consistent under `fail_on_unmatched_files: true`), `if: false` is the right skip mechanism with no dangling references to the skipped job, the YAML is valid, all three ci_workflow.rs regression tests stay green, and docs/memory are synced. One low finding: the in-file re-enable checklist omits the three doc-revert steps (header STATUS note, release-job pointer comment, new_release skill-prompt note), so a checklist-only re-enable would leave stale "temporarily disabled" claims in the repo.


## Scope

All uncommitted changes on wt/mnemo (HEAD = 21bf39d = main tip, so `git diff HEAD` is the full change set vs main):

- `.github/workflows/build.yml` — STATUS header note (3-6); dated TEMPORARILY DISABLED comment + `if: false` first key on the macos job (85-94); release job `needs: [windows, macos]` → `needs: [windows]` + pointer comment (209-211); macos-bundles-aarch64 download step removed; `dist/macos/**/*.dmg` + `*.zip` globs removed from the gh-release files list.
- `.coding/skills/new_release.toml` — prompt now notes Windows-only (.msi/.exe) assets while the macOS job is temporarily disabled.
- `.coding/knowledge/` — new DECISION record (macOS CI job temporarily disabled) + amendments to the CI-owns-release-creation DECISION and the five-macOS-tests BUG record (exact `memory_amend` "Amended 2027-01-11:" heading format, content accurate against the diff).
- Untracked: the plan file `.coding/plans/f4512a03.md` + the new decision knowledge file (bookkeeping, travels with the commit).

## Correctness — release job publishes with only the windows artifact

- **`needs: [windows]` (build.yml:211)** — the release job runs after windows succeeds. This is not just cosmetic: a *skipped* `needs` entry would propagate the skip to the release job (skipped needs cause dependents to skip unless `if: always()`-style guards are added), so removing macos from `needs` is exactly the right move alongside `if: false`.
- **Artifact/glob consistency under `fail_on_unmatched_files: true`** — windows uploads `windows-installers` (`target/release/bundle/msi/*.msi` + `nsis/*.exe`, `if-no-files-found: error`, 76-83) → release downloads `windows-installers` → `dist/windows` (218-221). upload-artifact@v4 roots the multi-path artifact at the LCA `target/release/bundle`, so `msi/*.msi` + `nsis/*.exe` land under `dist/windows/`; both remaining globs (`dist/windows/**/*.msi`, `dist/windows/**/*.exe`, 226-227) match, with `**` covering zero-or-more segments (same rooting analysis as the landed release-infra review 2026-09-20). No glob can dangle. This is byte-for-byte the windows half of the previously-verified release path; the windows job succeeded in run 35531680467, so the artifact will exist with both file kinds.
- **No dangling references to the skipped job** — repo-wide searches for `macos-bundles-aarch64` / `dist/macos` hit only the intentional checklist comment in build.yml, immutable history (old plans/reviews), and the accurate knowledge records. Nothing functional references the macos job.

## `if: false` vs commenting out

`if: false` (build.yml:94) is the standard GitHub Actions skip: the job stays defined (one-line re-enable), shows as skipped in the run UI, and — because the job never runs — its `secrets`-referencing job-level `env` (105-107) is never evaluated. Commenting the job out would also work but is strictly worse: no visible skipped marker, an error-prone ~100-line restore, and a fuzzier checklist. Bare `false` is a valid `if:` expression (no context needed); placement as the first key mirrors the release job's if-first style. Workflow-load validity is unchanged — the file is still fully parsed/validated at load time, and every `secrets` use was already in an allowed position (job env / step env) before this change.

## YAML validity

Read the full post-edit file: indentation consistent, comment blocks at job level, `if: false` a plain boolean key. The executor additionally ran the `yaml.safe_load` sanity check (plan step 4) and `cargo test --workspace` (exit=0).

## Regression tests (tests/integration/ci_workflow.rs)

All three stay green, verified line-by-line against the edited file:

- `build_workflow_step_ifs_never_reference_secrets` — every `if:` line checked: `if: false` (94, no secrets), step ifs (139/149/169, `env.*` only), release `if` (208, `github.ref`). Clean.
- `build_workflow_ci_env_is_clap_bool` — `CI: "true"` (44) untouched.
- `build_workflow_avoids_node20_actions` — no `uses:` pin changed (checkout@v5, setup-node@v5, rust-toolchain@stable, rust-cache@v2, artifact@v4, gh-release@v2).

The executor's `cargo test --workspace` (all green, exit=0) is consistent with this static verification.

## Re-enable checklist

Functionally complete: the macos job body is fully intact (all steps, env gates, signing/notarization logic unchanged), so deleting `if: false` + restoring the three release-job pieces (needs entry, macos-bundles-aarch64 download, dist/macos globs) fully re-enables macOS releases. See LOW 1 for the doc-revert gap.

## Constitution checks

- **Documentation sync** — new_release.toml updated accurately (Windows-only assets, return on re-enable). README.md verified: its macOS mentions (platform badge :8, prerequisites :109-113, `npm start bundle` row :129) are local-build claims that remain true; it never claims GitHub releases ship macOS assets. PLAN.md carries no release-job claims (searched). Knowledge records (new DECISION + two amendments) are accurate and match the diff. No stale doc beyond LOW 1.
- **Multi-platform neutrality** — CI-only change: no library/app code touched, no Windows-only assumptions added, macOS codebase support untouched (no source files in the diff).
- **File-tools-first** — edits are structured file edits; the knowledge amendments carry the exact `memory_amend` heading format; no shell-mutation evidence in the diff.
- **Security** — no secrets exposure (the disabled job's secret gates never evaluate; nothing new reads secrets); the release job keeps its job-scoped `permissions: contents: write`; no unsafe patterns.

## Follow-through verification (beyond the diff)

Verified via the public API (`GET /repos/chessIthaca/Mnemo/releases`): the only existing release is the old manual `WindowsRelease` tag (2026-09-13, no assets) — **no release exists for v0.1.1**, so the plan's force-move of the v0.1.1 tag is safe: nothing immutable blocks the CI release job from creating it once the windows job goes green.

## Findings

### LOW 1 — Re-enable checklist omits the doc-revert steps (build.yml:85-92)

The in-file checklist covers the four functional pieces but not the three documentation notes that also encode the disabled state: (a) the file-header STATUS note (build.yml:3-6), (b) the release job's pointer comment (build.yml:209-210), (c) the new_release skill-prompt Windows-only note (.coding/skills/new_release.toml:38). The DECISION record mentions (c) but not (a)/(b), and it designates the in-file checklist as the canonical procedure ("Re-enable checklist lives in build.yml's macos job comment"). A re-enable following only the in-file checklist leaves stale "TEMPORARILY DISABLED" claims in the workflow header, the release-job comment, and the release skill prompt — misleading future readers about the repo's actual state.

**Fix:** extend the checklist comment with a third item — "(3) drop the TEMPORARILY DISABLED notes: the header STATUS note, the release job's pointer comment, and the new_release skill's Windows-only note" (one `file_edit` to build.yml; optionally mirror it in the decision record). No functional impact — macOS releases re-enable fully either way.