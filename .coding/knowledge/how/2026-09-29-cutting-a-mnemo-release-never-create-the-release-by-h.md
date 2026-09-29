+++
title = "cutting a Mnemo release (1.3.0 onward): never create the release by hand"
created = "2026-09-29"
+++

CI owns release creation (`build.yml`, `on: push: tags: ["v*"]`). The order is:
bump the version in the six manifests (see the 1.2.0 decision record), add the
CHANGELOG section, merge the prep PR, then push the tag — `git tag -a v1.3.0 -m …`
and `git push origin v1.3.0` (use an **annotated** tag: the local lightweight
v1.2.0 tag vs the remote annotated object is what made the 2026-09-28 retarget
look like a moved tag).

Then: windows job (~25 min, runs `cargo test --workspace` + `tauri build`) and
macos (~10 min) run in parallel, and the `release` job uploads the installers and
publishes. Verify with `gh release view v1.3.0 --json isDraft,isImmutable,assets`
— expect four assets: `Mnemo_<v>_x64_en-US.msi`, `Mnemo_<v>_x64-setup.exe`,
`Mnemo_<v>_aarch64.dmg`, `mnemo.app.zip`.

**Do not create the release for a tag by hand.** This repo has GitHub's
immutable-releases setting ON (`isImmutable: true` on every release): a published
release rejects asset uploads, so a hand-made release blocks the workflow's
upload and the release can never gain installers — that is exactly what happened
to v1.2.0 (run 36354666875: windows + macos green, release red with "Cannot
upload asset … to an immutable release. GitHub only allows asset uploads before a
release is published"). The workflow now creates the release as a **draft**
(`draft: true`), uploads, and only then publishes (`gh release edit
--draft=false --latest`); pinned by
`tests/integration/ci_workflow.rs::build_workflow_uploads_installers_to_a_draft_release_first`.

Probed end-to-end on 2026-09-29 with a throwaway tag + release: draft + asset
upload OK, publish OK (the asset survives), then a later upload to the published
release returns HTTP 422 "Cannot upload assets to an immutable release". Deleting
an immutable release works (`gh release delete … --yes --cleanup-tag`), so a
scratch release cannot get stuck — but do not test that on a real version tag.

Two more traps seen while cutting 1.3.0: a `git commit -F` with a wrong message
path fails *and* a follow-up `git push`/`gh pr merge` still succeeds, which
merged a PR without its commit — always confirm the commit landed before merging.
And this repo's `main` is PR-only (ruleset `main-protection`), so the version
bump and any workflow change must land through a PR, never a direct push.
