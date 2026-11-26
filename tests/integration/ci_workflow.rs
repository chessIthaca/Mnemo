// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! CI workflow-file hygiene: guards the GitHub Actions context-availability
//! rules the repo's own build pipeline depends on.

/// Step `if:` conditionals must not reference the `secrets` context.
///
/// GitHub's expression validator rejects it at workflow-load time
/// ("Unrecognized named-value: 'secrets'" — the docs context-availability
/// table excludes `secrets` from `jobs.<job_id>.steps.if`), so the workflow
/// never even starts. Secret-dependent gates must be computed as job-level
/// `env` (which MAY reference `secrets`) with the steps checking `env.*`.
/// Regression: the pre-fix build.yml carried three such `if:` lines
/// (L111/L121/L141, the Apple secret-group gates) and the workflow failed
/// to load on every run.
#[test]
fn build_workflow_step_ifs_never_reference_secrets() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("if:") {
            assert!(
                !trimmed.contains("secrets."),
                "line {} references the secrets context in an if: conditional \
                 (rejected at workflow-load time; compute the gate as job-level \
                 env instead): {}",
                idx + 1,
                trimmed
            );
        }
    }
}

/// The workflow-level `CI` env var must be a clap-valid bool ("true"/"false").
///
/// The tauri CLI's `--ci` flag is a clap bool that reads the `CI` env var
/// (`[env: CI=]` in `tauri build --help`) and rejects every value other than
/// true/false — so `CI: "1"` killed `npx tauri build` at startup with
/// `error: invalid value '1' for '--ci'` (exit 1 in ~2s, before the Mnemo2
/// frontend rebuild even started). Regression: the pre-fix build.yml set
/// `CI: "1"` and the windows job's "Tauri build Mnemo2 (unsigned
/// installers)" step never produced installers (run 35514715816, tag v0.1.1).
#[test]
fn build_workflow_ci_env_is_clap_bool() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("CI:") {
            let value = rest
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            assert!(
                value == "true" || value == "false",
                "line {} sets CI to {:?} — the tauri CLI's --ci flag reads the CI \
                 env var via clap and only accepts true/false (\"1\" kills \
                 `npx tauri build` at startup)",
                idx + 1,
                value
            );
        }
    }
}

/// `uses:` pins must not target the deprecated Node.js 20 action majors.
///
/// GitHub deprecated Node 20 on Actions runners (2025-09-19): actions still
/// targeting node20 are forced onto Node 24 with a warning annotation on
/// every job, and a future runner image may drop Node 20 entirely, turning
/// the warning into a hard failure. Regression: the pre-bump build.yml
/// pinned actions/checkout@v4 (x3) and actions/setup-node@v4 (x2),
/// annotating both jobs of every run (e.g. 35514715816, 35521221353).
#[test]
fn build_workflow_avoids_node20_actions() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    for (idx, line) in text.lines().enumerate() {
        let t = line.trim();
        let t = t.strip_prefix("- ").unwrap_or(t);
        if let Some(action) = t.strip_prefix("uses:") {
            let action = action.trim();
            assert!(
                !matches!(action, "actions/checkout@v4" | "actions/setup-node@v4"),
                "line {} pins {} - a node20-targeting action major (GitHub \
                 deprecated Node 20 on Actions runners 2025-09-19); bump to \
                 the Node-24-native v5 major",
                idx + 1,
                action
            );
        }
    }
}

/// The GITHUB_TOKEN must be scoped: the workflow declares a least-privilege
/// `permissions:` block at the workflow level, and the only widening is the
/// release job's explicit per-job override.
///
/// An unscoped token is CodeQL's `actions/missing-workflow-permissions` finding
/// — alerts 1 (windows) and 13 (macos), open since 2026-09-21. The repo-level
/// default is already `read`, but that is a repository *setting*: this test
/// keeps the workflow itself least-privilege, so flipping that setting can
/// never silently hand a build job a write token.
///
/// The workflow-level value must be exactly `read`: an existence-only check
/// would accept `write`/`write-all` and silently defeat least-privilege.
#[test]
fn build_workflow_scopes_token_permissions() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    let lines: Vec<&str> = text.lines().collect();

    let workflow_level = lines.iter().position(|l| l.starts_with("permissions:"));
    let contents = workflow_level.and_then(|idx| {
        lines
            .iter()
            .skip(idx + 1)
            .take_while(|l| l.trim().is_empty() || l.starts_with(' '))
            .find_map(|l| l.trim_start().strip_prefix("contents:"))
    });
    let value = contents.map(|v| v.trim().trim_matches('"').trim_matches('\''));
    assert_eq!(
        value,
        Some("read"),
        "build.yml's workflow-level `permissions:` block must scope the \
         GITHUB_TOKEN to exactly `contents: read` (CodeQL \
         actions/missing-workflow-permissions alerts 1/13) — `write`/`write-all` \
         defeats least-privilege; the release job's per-job override is the only \
         widening"
    );

    assert!(
        text.contains("permissions:\n      contents: write"),
        "the release job's contents: write override went missing"
    );
}

/// The release job must upload the installers to a DRAFT release and publish it
/// afterwards — in that order.
///
/// This repo has GitHub's "immutable releases" setting ON: both existing
/// releases report `immutable:true`. A published release rejects asset uploads
/// with
///
/// ```text
/// ##[error]Cannot upload asset Mnemo_1.2.0_x64_en-US.msi to an immutable
/// release. GitHub only allows asset uploads before a release is published,
/// so upload assets to a draft release before you publish it.
/// ```
///
/// The pre-fix workflow created the release published (action-gh-release's
/// default) and uploaded afterwards, so the v1.2.0 rerun (run 36354666875 —
/// windows + macos green, release red) left that release with zero assets, and
/// immutability makes that permanent. Regression: `draft: true` plus a
/// `gh release edit --draft=false` publish step, in that order.
#[test]
fn build_workflow_uploads_installers_to_a_draft_release_first() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");

    // The release job is the last job in the file; everything after it belongs
    // to it (no job follows the 2-space `release:` key).
    let release_job = text
        .split_once("\n  release:")
        .map(|(_, rest)| rest)
        .expect("build.yml declares a `release:` job");

    let draft_at = release_job.find("draft: true").unwrap_or_else(|| {
        panic!(
            "the release step must set `draft: true`: with immutable releases \
             enabled, uploading assets to an already-published release fails \
             (\"GitHub only allows asset uploads before a release is published\")"
        )
    });
    let publish_at = release_job.find("--draft=false").unwrap_or_else(|| {
        panic!(
            "a step must publish the draft AFTER the installers are attached \
             (gh release edit \"${{ github.ref_name }}\" --draft=false)"
        )
    });
    assert!(
        draft_at < publish_at,
        "the publish step (`--draft=false`) must come after the draft upload \
         (`draft: true`) — publishing first makes the release immutable and \
         the asset upload fails"
    );
}

/// One job's body from `.github/workflows/build.yml` — the text after its
/// `  <job>:` header up to the next job key.
///
/// A job block runs to the next 2-space job key (job bodies indent 4 spaces
/// and steps 6), or to EOF for the last job.
fn job_block(text: &str, job: &str) -> String {
    let header = format!("\n  {job}:\n");
    let start = text
        .find(&header)
        .unwrap_or_else(|| panic!("build.yml declares a `{job}:` job"))
        + header.len();
    text[start..]
        .lines()
        // Stop at the next 2-space job key: two leading spaces, but not the
        // three or more of a body line.
        .take_while(|line| !line.starts_with("  ") || line.starts_with("   "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Byte offset of the `- name: <name>` step line inside a job block.
fn step_offset(block: &str, name: &str) -> usize {
    let header = format!("\n      - name: {name}");
    block
        .find(&header)
        .unwrap_or_else(|| panic!("the job has no `- name: {name}` step"))
        + 1
}

/// The text of the step named `name` inside a job block — from its `- name:`
/// line to the next step (`      - `) or the end of the block.
fn step_named<'a>(block: &'a str, name: &str) -> &'a str {
    let start = step_offset(block, name);
    let tail = &block[start..];
    match tail.find("\n      - ") {
        Some(end) => &tail[..end],
        None => tail,
    }
}

/// Both platform jobs must build the Mnemo2 installer with `npx tauri
/// build` run from `src-tauri/mnemo2-app`.
///
/// Mnemo2 is its own Cargo package (`mnemo2-app`) with its own
/// `tauri.conf.json` (productName Mnemo2, identifier com.mnemo.mnemo2),
/// bundled from that directory into the shared `<repo>/target/...` bundle
/// directories under `Mnemo2_*` names. Mnemo1 is retired: no step in either
/// job may run `npx tauri build` from any other working directory — a stray
/// bundle step would bundle from a retired tree or a wrong config.
///
/// The frontend dist (target/mnemo2-dist, the nested config's frontendDist)
/// must be built before the custom-protocol bundle that embeds it.
#[test]
fn build_workflow_bundles_mnemo2() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");

    for job in ["windows", "macos"] {
        let block = job_block(&text, job);

        // The Mnemo2 bundle: `npx tauri build` with the nested package as its
        // working directory.
        let step_dir = block
            .find("working-directory: src-tauri/mnemo2-app")
            .unwrap_or_else(|| {
                panic!(
                    "the {job} job must build the Mnemo2 installer: \
                     `npx tauri build` with `working-directory: \
                     src-tauri/mnemo2-app` is missing"
                )
            });
        let step_start = block[..step_dir]
            .rfind("\n      - name:")
            .unwrap_or_else(|| {
                panic!("the {job} job's Mnemo2 working-directory line is not inside a step")
            });
        let step_tail = &block[step_start + 1..];
        let step_end = step_tail.find("\n      - name:").unwrap_or(step_tail.len());
        let step = &step_tail[..step_end];
        assert!(
            step.contains("npx tauri build"),
            "the {job} job's step with `working-directory: \
             src-tauri/mnemo2-app` does not run `npx tauri build`: {step}"
        );
        assert_eq!(
            step.contains("--target aarch64-apple-darwin"),
            job == "macos",
            "the Mnemo2 bundle step must target aarch64-apple-darwin on macOS \
             only"
        );
        // The frontend dist (target/mnemo2-dist, the nested config's
        // frontendDist) must be built before the custom-protocol bundle that
        // embeds it.
        assert!(
            block[..step_start].contains("npm run build:mnemo2"),
            "the {job} job must build the Mnemo2 frontend (`npm run \
             build:mnemo2`) before bundling it"
        );
        // Mnemo1 is retired: `npx tauri build` may only run from
        // src-tauri/mnemo2-app — no step in the job may bundle anywhere else.
        for step in block.split("\n      - ").skip(1) {
            if step.contains("npx tauri build") && step.contains("run:") {
                assert!(
                    step.contains("working-directory: src-tauri/mnemo2-app"),
                    "the {job} job runs `npx tauri build` outside \
                     src-tauri/mnemo2-app — Mnemo1 is retired, only the Mnemo2 \
                     package may be bundled: {step}"
                );
            }
        }

        if job == "windows" {
            // The upload globs must still match the Mnemo2 installers: they
            // land in the shared bundle dirs, distinguished by file name.
            for glob in [
                "target/release/bundle/msi/*.msi",
                "target/release/bundle/nsis/*.exe",
            ] {
                assert!(
                    block.contains(glob),
                    "the windows upload must keep the `{glob}` glob — it \
                     matches the Mnemo2 installers (`Mnemo2_*`)"
                );
            }
        } else {
            // The Mnemo2 zip, staged into dist-macos/ — ditto keeps the
            // exec bit and the symlinks upload-artifact drops.
            assert!(
                block.contains(
                    "--keepParent \
                     target/aarch64-apple-darwin/release/bundle/macos/Mnemo2.app \
                     dist-macos/mnemo2.app.zip"
                ),
                "the macos job must zip Mnemo2.app into \
                 dist-macos/mnemo2.app.zip (ditto keeps the exec bit and the \
                 symlinks upload-artifact drops)"
            );
            let upload = step_named(&block, "Upload macOS bundles");
            assert!(
                upload.lines().any(|line| line.trim() == "path: dist-macos/*"),
                "the macos upload must ship `dist-macos/*` — it holds Mnemo2's \
                 zip and dmg: {upload}"
            );
        }
    }
}

/// The macOS job must stage Mnemo2's `.dmg` and `.app.zip` into
/// `dist-macos/` right after its `npx tauri build`, and upload `dist-macos/*`.
///
/// The staging step is also the only place `dist-macos/` is created (`mkdir -p
/// dist-macos`) — Mnemo1's staging step used to do that, and it retired with
/// the app. The zip must use `ditto` so the exec bit and the symlinks survive
/// what upload-artifact drops (a raw `.app` would not launch after download).
#[test]
fn build_workflow_stages_each_macos_dmg() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    let job = job_block(&text, "macos");

    // The dmg and zip are staged out of target/ right after the build, then
    // uploaded from the staging dir.
    let order = [
        "Tauri build Mnemo2 (.app + .dmg, aarch64)",
        "Stage the Mnemo2 bundles",
        "Upload macOS bundles",
    ];
    let offsets: Vec<(&str, usize)> = order
        .iter()
        .map(|name| (*name, step_offset(&job, name)))
        .collect();
    for pair in offsets.windows(2) {
        assert!(
            pair[0].1 < pair[1].1,
            "the macos job must run `{}` BEFORE `{}` — the upload ships the \
             staging dir, so nothing may upload before it is staged",
            pair[0].0,
            pair[1].0
        );
    }

    // The staging step creates dist-macos (Mnemo1's staging step used to),
    // copies the Mnemo2 dmg out of the target dir and zips the bundle next
    // to it. Mnemo1's own staging step retired with the app.
    assert!(
        !job.contains("\n      - name: Stage the Mnemo bundles"),
        "the macos job still stages Mnemo's bundles — Mnemo1 is retired"
    );
    let mnemo2 = step_named(&job, "Stage the Mnemo2 bundles");
    assert!(
        mnemo2.contains("mkdir -p dist-macos"),
        "the Mnemo2 staging step must create dist-macos (`mkdir -p \
         dist-macos`) — Mnemo1's staging step used to: {mnemo2}"
    );
    assert!(
        mnemo2.contains("cp target/aarch64-apple-darwin/release/bundle/dmg/Mnemo2_*.dmg dist-macos/"),
        "the Mnemo2 staging step must copy `Mnemo2_*.dmg` into dist-macos/: {mnemo2}"
    );
    assert!(
        mnemo2.contains(
            "ditto -c -k --keepParent \
             target/aarch64-apple-darwin/release/bundle/macos/Mnemo2.app \
             dist-macos/mnemo2.app.zip"
        ),
        "the Mnemo2 staging step must zip Mnemo2.app into dist-macos/: {mnemo2}"
    );

    // The upload ships the staging dir, not target/ itself.
    let upload = step_named(&job, "Upload macOS bundles");
    assert!(
        upload.lines().any(|line| line.trim() == "path: dist-macos/*"),
        "the macos upload must ship `dist-macos/*` (Mnemo2's dmg and \
         .app.zip): {upload}"
    );
    assert!(
        !upload.contains("release/bundle/dmg"),
        "the macos upload must not read the dmg straight out of target/: \
         {upload}"
    );
    assert!(
        upload.contains("if-no-files-found: error"),
        "the macos upload must keep failing the job on an empty staging dir: \
         {upload}"
    );
}

/// Every release publishes Tauri's auto-update files: both platform builds
/// sign their installers (`--features auto-update` plus the
/// `tauri.updater.conf.json` overlay turn on `createUpdaterArtifacts`), and the
/// release job runs `scripts/updater-manifest.mjs` over the downloaded
/// artifacts to write `latest.json` before the release step attaches it.
///
/// A v* tag build without the `TAURI_SIGNING_PRIVATE_KEY` secret must fail: it
/// can produce no `.sig` files, and a release without `latest.json` would leave
/// every installed Mnemo2 unable to update itself. Branch builds without the
/// secret only skip the updater artifacts.
#[test]
fn build_workflow_publishes_the_auto_update_manifest() {
    let root = env!("CARGO_MANIFEST_DIR");
    let workflow = std::fs::read_to_string(format!("{root}/.github/workflows/build.yml"))
        .expect("build.yml readable");

    // a. Both platform builds sign the installers: the `auto-update` feature
    // and the createUpdaterArtifacts overlay.
    let builds = [
        ("windows", "Tauri build Mnemo2 (unsigned installers)"),
        ("macos", "Tauri build Mnemo2 (.app + .dmg, aarch64)"),
    ];
    for (job, step_name) in builds {
        let block = job_block(&workflow, job);
        let step = step_named(&block, step_name);
        for needle in [
            "--features auto-update",
            "--config tauri.updater.conf.json",
        ] {
            assert!(
                step.contains(needle),
                "the {job} job's `{step_name}` step does not pass {needle:?} — \
                 tauri then writes no signed updater artifacts and the release \
                 ships no auto-update files: {step}"
            );
        }
    }

    // b. The signing key reaches the builds, and a tag without it is an error.
    for needle in [
        "TAURI_SIGNING_PRIVATE_KEY",
        "Release tags need the TAURI_SIGNING_PRIVATE_KEY secret",
    ] {
        assert!(
            workflow.contains(needle),
            "build.yml no longer mentions {needle:?} — the update signing key \
             is what makes the published updater artifacts verifiable, and a \
             release without it must fail instead of shipping no latest.json"
        );
    }

    // c. The release job writes latest.json BEFORE the release step attaches it
    // (the repo's releases are immutable once published, so every asset — the
    // manifest included — goes up while the release is still a draft).
    let release = job_block(&workflow, "release");
    let manifest_step = step_named(&release, "Write the auto-update manifest");
    assert!(
        manifest_step.contains("scripts/updater-manifest.mjs"),
        "the release job's `Write the auto-update manifest` step must run \
         `scripts/updater-manifest.mjs`: {manifest_step}"
    );
    assert!(
        step_offset(&release, "Write the auto-update manifest")
            < step_offset(&release, "Create GitHub release with installers"),
        "the release job must write latest.json BEFORE `Create GitHub release \
         with installers` attaches it — a published release is immutable"
    );
    let upload = step_named(&release, "Create GitHub release with installers");
    assert!(
        upload.lines().any(|line| line.trim() == "dist/latest.json"),
        "the release step's files: list must upload `dist/latest.json`, so \
         releases/latest/download/latest.json resolves for installed apps: \
         {upload}"
    );

    // d. The overlay is what makes tauri write the artifacts and their .sig
    // files; the installed app only accepts updates signed with the private
    // key whose public half sits in tauri.conf.json `plugins.updater`.
    let overlay_rel = "src-tauri/mnemo2-app/tauri.updater.conf.json";
    let overlay_text = std::fs::read_to_string(format!("{root}/{overlay_rel}"))
        .unwrap_or_else(|e| panic!("{overlay_rel} readable: {e}"));
    let overlay: serde_json::Value = serde_json::from_str(&overlay_text)
        .unwrap_or_else(|e| panic!("{overlay_rel} is valid JSON: {e}"));
    assert_eq!(
        overlay["bundle"]["createUpdaterArtifacts"],
        serde_json::Value::Bool(true),
        "{overlay_rel} must set `bundle.createUpdaterArtifacts` to true — \
         without it tauri writes no updater artifacts and no .sig files"
    );

    // e. The manifest writer the release job calls.
    let script_rel = "scripts/updater-manifest.mjs";
    assert!(
        std::path::Path::new(root).join(script_rel).exists(),
        "{script_rel} is missing — the release job's `Write the auto-update \
         manifest` step then fails on every tag"
    );
}

/// Every `uses:` in this repo's workflows must be pinned to a full 40-hex
/// commit SHA, with the human version in a trailing `# vN` comment so
/// Dependabot can keep it current.
///
/// A tag ref is mutable: whoever controls the action repository can repoint
/// `@v7` at new code that then runs with this repo's `GITHUB_TOKEN` — the
/// release job's token is `contents: write`. SHA pins make the executed commit
/// immutable.
///
/// There are no exceptions: Rust is installed by `rustup toolchain install`
/// from rust-toolchain.toml, not by an action, so the repo-level "require SHA
/// pinning" Actions setting can be on.
///
/// The guard enumerates `.github/workflows/` itself — every `*.yml`/`*.yaml`
/// file found there is checked, so a workflow file added later is covered
/// without editing the test; a hardcoded file list would silently leave it
/// unguarded. A missing or empty workflows directory fails the test outright
/// instead of passing vacuously.
#[test]
fn workflows_pin_actions_to_full_shas() {
    const ALLOWED_UNPINNED: [&str; 0] = [];
    let root = env!("CARGO_MANIFEST_DIR");
    let workflows_dir = format!("{root}/.github/workflows");
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&workflows_dir)
        .unwrap_or_else(|e| panic!("{workflows_dir} readable: {e}"))
        .map(|entry| entry.expect(".github/workflows entry readable").path())
        .filter(|path| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
            ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml")
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        ".github/workflows holds no *.yml/*.yaml workflow files — the SHA-pin \
         guard would pass vacuously; the enumeration must see the workflows it \
         guards"
    );
    for path in &paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-UTF-8 workflow file name>");
        let rel = format!(".github/workflows/{name}");
        let text =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{rel} readable: {e}"));
        for (idx, line) in text.lines().enumerate() {
            let t = line.trim();
            let t = t.strip_prefix("- ").unwrap_or(t);
            let Some(action) = t.strip_prefix("uses:") else {
                continue;
            };
            let action = action.trim();
            if ALLOWED_UNPINNED.contains(&action) {
                continue;
            }
            assert!(
                action.contains('@'),
                "{rel}:{} uses {action:?} with no @ref",
                idx + 1
            );
            let rev = action
                .rsplit('@')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or_default();
            assert!(
                rev.len() == 40 && rev.chars().all(|c| c.is_ascii_hexdigit()),
                "{rel}:{} pins {action:?} — use a full 40-hex commit SHA with \
                 the version in a trailing `# vN` comment (or add the action to \
                 ALLOWED_UNPINNED with a reason)",
                idx + 1
            );
        }
    }
}

/// Mnemo1 is retired: no workflow may reference its frontend workspace or
/// build the retired `mnemo-app` binary. The guard enumerates
/// `.github/workflows/` itself — every `*.yml`/`*.yaml` file found there is
/// checked, so a workflow added later is covered without editing the test.
#[test]
fn workflows_never_mention_mnemo1_frontend() {
    let root = env!("CARGO_MANIFEST_DIR");
    let workflows_dir = format!("{root}/.github/workflows");
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&workflows_dir)
        .unwrap_or_else(|e| panic!("{workflows_dir} readable: {e}"))
        .map(|entry| entry.expect(".github/workflows entry readable").path())
        .filter(|path| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
            ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml")
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        ".github/workflows holds no *.yml/*.yaml workflow files — the Mnemo1 \
         guard would pass vacuously"
    );
    for path in &paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-UTF-8 workflow file name>");
        let rel = format!(".github/workflows/{name}");
        let text =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{rel} readable: {e}"));
        for needle in [
            "working-directory: frontend",
            "--workspace frontend",
            "frontend/",
            "--bin mnemo-app",
        ] {
            assert!(
                !text.contains(needle),
                "{rel} references Mnemo1 ({needle:?}) — Mnemo1 is retired; \
                 Mnemo2 is the only app"
            );
        }
    }
}

/// Every `npx tauri` call in build.yml needs the Tauri CLI installed by
/// `npm ci`: some package.json in the npm workspace must declare
/// `@tauri-apps/cli`, and package-lock.json must lock it with its `tauri`
/// bin, or npx fails with "could not determine executable to run".
/// Regression: retiring Mnemo1 (674702df) deleted frontend/package.json,
/// the only package that declared the CLI, and the v2.3.0 tag build
/// (run 43233239658) died at "Tauri build Mnemo2 (.app + .dmg, aarch64)"
/// with no installers. PR CI never runs the Tauri CLI, so only this guard
/// catches it before a tag.
#[test]
fn build_workflow_tauri_cli_is_declared_and_locked() {
    let root = env!("CARGO_MANIFEST_DIR");

    // a. Every `npx tauri` invocation in the release workflow.
    let workflow = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    let npx_calls: Vec<String> = workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("npx tauri"))
        .map(str::to_owned)
        .collect();
    assert!(
        !npx_calls.is_empty(),
        "build.yml no longer calls `npx tauri` — whatever builds the \
         installers now has to be covered by this guard instead"
    );

    // b. The root package.json plus every workspace package it lists.
    let root_manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/package.json"
    ))
    .expect("root package.json readable");
    let root_json: serde_json::Value =
        serde_json::from_str(&root_manifest).expect("root package.json is valid JSON");
    let mut manifests = vec![String::from("package.json")];
    for dir in root_json["workspaces"]
        .as_array()
        .expect("root package.json declares a workspaces array")
    {
        let dir = dir
            .as_str()
            .expect("every workspaces entry is a string");
        manifests.push(format!("{dir}/package.json"));
    }

    // c. At least one of them must declare the CLI.
    let mut declaring = Vec::new();
    let mut inspected = Vec::new();
    for rel in &manifests {
        let text = std::fs::read_to_string(format!("{root}/{rel}"))
            .unwrap_or_else(|e| panic!("{rel} readable: {e}"));
        let pkg: serde_json::Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel} is valid JSON: {e}"));
        inspected.push(rel.clone());
        let declared = pkg["devDependencies"]["@tauri-apps/cli"].as_str().is_some()
            || pkg["dependencies"]["@tauri-apps/cli"].as_str().is_some();
        if declared {
            declaring.push(rel.clone());
        }
    }
    assert!(
        !declaring.is_empty(),
        "no package.json in the npm workspace declares @tauri-apps/cli, so \
         `npm ci` installs no `tauri` executable and build.yml fails at \
         {npx_calls:?} with \"could not determine executable to run\" — add it \
         as a devDependency of mnemo2: `npm install -D \
         @tauri-apps/cli@^2.12.4 -w mnemo2` (inspected: {inspected:?})"
    );

    // d. package-lock.json must lock it together with its `tauri` bin.
    let lock_text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/package-lock.json"
    ))
    .expect("package-lock.json readable");
    let lock: serde_json::Value =
        serde_json::from_str(&lock_text).expect("package-lock.json is valid JSON");
    let entry = &lock["packages"]["node_modules/@tauri-apps/cli"];
    assert!(
        entry["bin"]["tauri"].as_str().is_some(),
        "package-lock.json does not lock @tauri-apps/cli with its `tauri` bin, \
         so `npm ci` would install no `tauri` executable — re-run `npm install \
         -D @tauri-apps/cli@^2.12.4 -w mnemo2`"
    );
}

/// The release installers must carry the Claude Code helper, the shared
/// models host and the worker host daemon: the `mnemo` plugin runs
/// `bin/mnemo2-mcp`, which only a source build used to create
/// (`scripts/install-mnemo2-mcp.mjs`), so an installed Mnemo2 could not lead
/// with Claude Code; `mnemo-models` is the machine-wide embedding host every
/// instance embeds through (plan d3336a8a step 2 — review finding
/// ciMissesModelsBin: an installer that silently drops it disables the shared
/// host and no instance notices); `mnemo2-node` is the worker host daemon a
/// paired Windows or Mac machine runs (plan e8cb5112 step 9), and an
/// installer that drops it breaks pairing on every worker host.
/// Tauri's bundler ships every `[[bin]]` of the app package on its own
/// (`src-tauri/mnemo2-app` declares `mnemo2-mcp`, `mnemo-models` and
/// `mnemo2-node`), so each platform job bundles normally and then checks that
/// the produced installer really carries ALL of those binaries — no
/// `bundle.externalBin` sidecar anywhere.
///
/// Regression: the released installers shipped without `mnemo2-mcp`, so the
/// bundled plugin's MCP server could not start on a user's machine. A second
/// attempt declared the helper as an explicit `bundle.externalBin` sidecar
/// instead; that duplicated the auto-bundled bin in the Windows MSI, where
/// WiX `light.exe` failed with ICE30 — the same `mnemo2-mcp.exe` installed
/// by two different components (CI run 43652392587) — so the sidecar
/// machinery is gone for good.
#[test]
fn build_workflow_bundles_the_mnemo2_mcp_helper() {
    let root = env!("CARGO_MANIFEST_DIR");
    let workflow = std::fs::read_to_string(format!("{root}/.github/workflows/build.yml"))
        .expect("build.yml readable");

    // a. Each job keeps its post-build check that the produced installer
    // really carries the helper: the installer sources on Windows, the built
    // app bundle on macOS.
    let checks = [
        (
            "windows",
            "Check the installers carry mnemo2-mcp, mnemo-models and mnemo2-node",
            [
                "installer.nsi",
                "main.wxs",
                "mnemo2-mcp.exe",
                "mnemo-models.exe",
                "mnemo2-node.exe",
            ]
            .as_slice(),
        ),
        (
            "macos",
            "Check the app bundle carries mnemo2-mcp, mnemo-models and mnemo2-node",
            [
                "Contents/MacOS/mnemo2-mcp",
                "Contents/MacOS/mnemo-models",
                "Contents/MacOS/mnemo2-node",
            ]
            .as_slice(),
        ),
    ];
    for (job, check, needles) in checks {
        let block = job_block(&workflow, job);
        assert!(
            block.contains(&format!("- name: {check}")),
            "the {job} job has no `{check}` step — nothing then proves the \
             built installers carry the bundled binaries"
        );
        let step = step_named(&block, check);
        for needle in needles {
            assert!(
                step.contains(needle),
                "the {job} job's `{check}` step no longer looks for {needle:?} \
                 — nothing then proves the built installers carry the \
                 mnemo2-mcp, mnemo-models and mnemo2-node binaries: {step}"
            );
        }

        // b. The check runs after the job's own bundle, never before it.
        // Commands, not prose: a comment naming `npx tauri build` is no
        // bundle call.
        let mut bundle_at: Option<usize> = None;
        let mut offset = 0;
        for line in block.split_inclusive('\n') {
            let trimmed = line.trim();
            if !trimmed.starts_with('#') && trimmed.contains("npx tauri build") {
                bundle_at = Some(offset + (line.len() - line.trim_start().len()));
                break;
            }
            offset += line.len();
        }
        let bundle_at =
            bundle_at.unwrap_or_else(|| panic!("the {job} job runs no `npx tauri build`"));
        assert!(
            bundle_at < step_offset(&block, check),
            "the {job} job's `{check}` step runs before `npx tauri build`, so \
             it inspects no installer at all"
        );
    }

    // c. The helper reaches the installers because tauri bundles every
    // `[[bin]]` of the app package by itself.
    let manifest_rel = "src-tauri/mnemo2-app/Cargo.toml";
    let manifest = std::fs::read_to_string(format!("{root}/{manifest_rel}"))
        .unwrap_or_else(|e| panic!("{manifest_rel} readable: {e}"));
    let declares_helper_bin = manifest.split("[[bin]]").skip(1).any(|bin| {
        bin.lines()
            .take_while(|line| !line.trim_start().starts_with('['))
            .any(|line| line.trim() == "name = \"mnemo2-mcp\"")
    });
    assert!(
        declares_helper_bin,
        "{manifest_rel} declares no `[[bin]]` named `mnemo2-mcp` — tauri then \
         bundles no helper and the installers ship without the Claude Code one"
    );

    // d. No config may declare `bundle.externalBin`: tauri already bundles
    // the bin (a sidecar duplicated it in the installers, MSI ICE30 above),
    // and tauri's build script refuses to compile while a configured sidecar
    // file is missing — breaking every plain `cargo build`/`cargo test`.
    for conf_rel in [
        "src-tauri/mnemo2-app/tauri.conf.json",
        "src-tauri/tauri.conf.json",
    ] {
        let conf_text = std::fs::read_to_string(format!("{root}/{conf_rel}"))
            .unwrap_or_else(|e| panic!("{conf_rel} readable: {e}"));
        let conf: serde_json::Value = serde_json::from_str(&conf_text)
            .unwrap_or_else(|e| panic!("{conf_rel} is valid JSON: {e}"));
        assert!(
            conf["bundle"].get("externalBin").is_none(),
            "{conf_rel} declares `bundle.externalBin` — tauri already bundles \
             the mnemo2-mcp `[[bin]]`, so a sidecar duplicates it in the \
             installers (MSI ICE30, CI run 43652392587)"
        );
    }
    let bundle_conf_rel = "src-tauri/mnemo2-app/tauri.bundle.conf.json";
    assert!(
        !std::path::Path::new(root).join(bundle_conf_rel).exists(),
        "{bundle_conf_rel} exists again — the externalBin sidecar duplicates \
         the auto-bundled mnemo2-mcp bin (MSI ICE30, CI run 43652392587)"
    );

    // e. The workflow carries none of the sidecar machinery either.
    for needle in ["--config tauri.bundle.conf.json", "stage-mnemo2-mcp-sidecar"] {
        assert!(
            !workflow.contains(needle),
            "build.yml mentions {needle:?} again — that externalBin sidecar \
             path duplicates the mnemo2-mcp bin tauri already bundles (MSI \
             ICE30, CI run 43652392587)"
        );
    }
}

// Failure text for the vendored-patch lock guard below.
const UNUSED_PATCH_MSG: &str = "Cargo.lock carries a [[patch.unused]] entry: a [patch.crates-io] override (the wry path patch in Cargo.toml) no longer applies to the resolved version, so the registry crate would ship instead of the vendored build — losing the WebView2 OS-account SSO + hard-reload patches. Re-pin tauri / tauri-runtime-wry by hand together with vendor/wry (see vendor/wry/PATCHES.md and the .github/dependabot.yml ignore list), then re-run this guard";

/// The lock must keep the vendored `[patch.crates-io]` override in effect:
/// the `wry` package must stay PATH-sourced in `Cargo.lock` (no `source = `
/// line in its `[[package]]` entry, so the resolved crate is the `vendor/`
/// copy) and the lock must carry no `[[patch.unused]]`
/// entries. This is the machine check behind the `.github/dependabot.yml`
/// review note: bumping `tauri` past 2.12.0 (or `tauri-runtime-wry` past
/// 2.12.0) moves wry off `^0.57`, turning the override into a
/// `[[patch.unused]]` — the registry crate would then ship instead of the
/// vendored build, losing the WebView2 OS-account SSO + hard-reload
/// patches (vendor/wry/PATCHES.md) while CI stays green. (tao needs no
/// check: tauri 2.12 resolves tao 0.37.x, which carries the PR #1215
/// keyboard/IME deadlock fix natively — vendor/tao was dropped with it.)
/// Review H1 of
/// `.coding/reviews/2026-09-29-rust-ci-dependency-wave-review.md`.
#[test]
fn cargo_lock_keeps_the_vendored_patches() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/Cargo.lock"
    ))
    .expect("root Cargo.lock readable");
    assert!(!text.contains("[[patch.unused]]"), "{}", UNUSED_PATCH_MSG);

    // A path-sourced entry has NO `source = ` line; any source (registry or
    // git) means the vendored build is not what resolves. Exact-name match:
    // only a `name = "wry"` line arms the check.
    let mut seen_wry = false;
    let mut target_wry = false;
    for (idx, line) in text.lines().enumerate() {
        if line.starts_with("[[") {
            target_wry = false;
        } else if let Some(name) = line
            .strip_prefix("name = \"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            target_wry = name == "wry";
            if target_wry {
                seen_wry = true;
            }
        } else if line.starts_with("source = ") && target_wry {
            panic!(
                "Cargo.lock's `wry` entry (line {}) is not path-sourced \
                 (`{line}`): the vendored [patch.crates-io] override is not \
                 in effect, so a registry/git build ships instead of \
                 vendor/wry — see vendor/wry/PATCHES.md",
                idx + 1
            );
        }
    }
    assert!(
        seen_wry,
        "Cargo.lock resolves no wry package (saw wry={seen_wry}): the \
         vendored crate must resolve"
    );
}

/// The root `overrides.jsdom` pin and mnemo2's `devDependencies.jsdom` must
/// name the same exact version, and package-lock.json must lock that version
/// as the single hoisted copy. `npm ci` on CI's npm 10 rejects the whole
/// lockfile when the override disagrees with a workspace requirement
/// ("Invalid: lock file's jsdom@31.0.0 does not satisfy jsdom@31.0.1"), so
/// build.yml's "Install npm deps" step dies before anything installs. The
/// override itself must stay: it forces one hoisted jsdom so vitest resolves
/// it on a clean `npm ci` (c4e34d80).
///
/// Regression: Dependabot folded jsdom 31.0.1 into mnemo2/package.json while
/// the root override stayed at 31.0.0, and build.yml run 45289698182 failed
/// at "Install npm deps" on both runners.
#[test]
fn jsdom_override_matches_the_mnemo2_dev_dependency() {
    let root = env!("CARGO_MANIFEST_DIR");
    let read_json = |rel: &str| -> serde_json::Value {
        let text = std::fs::read_to_string(format!("{root}/{rel}"))
            .unwrap_or_else(|e| panic!("{rel} readable: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{rel} is valid JSON: {e}"))
    };
    let root_manifest = read_json("package.json");
    let mnemo2_manifest = read_json("mnemo2/package.json");
    let lock = read_json("package-lock.json");

    let override_pin = root_manifest["overrides"]["jsdom"].as_str();
    let dev_pin = mnemo2_manifest["devDependencies"]["jsdom"].as_str();
    if let (Some(override_pin), Some(dev_pin)) = (override_pin, dev_pin) {
        assert_eq!(
            override_pin, dev_pin,
            "root package.json overrides.jsdom pins jsdom {override_pin} but \
             mnemo2/package.json devDependencies.jsdom requires jsdom {dev_pin}: \
             `npm ci` on CI's npm 10 rejects the lockfile when they differ \
             (\"Invalid: lock file's jsdom@{override_pin} does not satisfy \
             jsdom@{dev_pin}\"), failing build.yml's Install npm deps step"
        );
    }

    // Only an exact override pin can be compared with the resolved entry; a
    // floating range (^ ~ > < * x) is not a lockfile version.
    let Some(override_pin) = override_pin else {
        return;
    };
    if override_pin
        .chars()
        .any(|c| matches!(c, '^' | '~' | '>' | '<' | '*' | 'x' | 'X' | '='))
    {
        return;
    }
    let locked = lock["packages"]["node_modules/jsdom"]["version"].as_str();
    assert_eq!(
        locked,
        Some(override_pin),
        "package-lock.json locks node_modules/jsdom at {locked:?} but the root \
         override pins jsdom {override_pin}: `npm ci` installs exactly the \
         lockfile and rejects it as \"Invalid: lock file's jsdom@{} does not \
         satisfy jsdom@{override_pin}\" — regenerate it with `npm install \
         --package-lock-only`",
        locked.unwrap_or("<missing>")
    );
    assert!(
        lock["packages"].get("mnemo2/node_modules/jsdom").is_none(),
        "package-lock.json nests a second jsdom under mnemo2/node_modules, but \
         the root override must leave exactly one hoisted copy for vitest to \
         resolve on a clean `npm ci` (c4e34d80) — regenerate the lockfile with \
         `npm install --package-lock-only`"
    );
}

/// The PR gate (`.github/workflows/ci.yml`) must keep building, linting and
/// testing every pull request to main. Before it existed only CodeQL ran on
/// pull requests, so v2.2.0 shipped with clippy red and Dependabot PRs looked
/// green while breaking the build or the tests. The separate `-p mnemo-app`
/// clippy/tests steps were dropped because `--workspace` already covers the
/// Mnemo2 host (see agent.md).
#[test]
fn ci_workflow_gates_pull_requests_with_clippy_and_tests() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/ci.yml"
    ))
    .expect("ci.yml readable");
    assert!(
        text.contains("\n  pull_request:\n"),
        "ci.yml must trigger on pull_request"
    );
    for command in [
        "npm run build:mnemo2",
        "npm test --workspace mnemo2",
        "cargo clippy --workspace --all-targets",
        "cargo test --workspace --no-fail-fast",
    ] {
        assert!(
            text.contains(&format!("run: {command}\n")),
            "ci.yml lost the step `{command}`"
        );
    }
    assert!(
        !text.contains("-p mnemo-app"),
        "ci.yml still contains `-p mnemo-app`: the workspace runs already \
         cover the Mnemo2 host; see agent.md"
    );
    assert!(
        text.contains("\npermissions:\n  contents: read\n"),
        "ci.yml must scope the GITHUB_TOKEN to exactly contents: read"
    );
}

/// One Rust version for local builds and CI, pinned in `rust-toolchain.toml`.
///
/// CI used to install whatever stable Rust was current
/// (`dtolnay/rust-toolchain@stable`) while a developer machine could sit on an
/// older release, so a new stable could turn the PR gate red on clippy lints
/// alone (build run 42067076616, the 1.95 lint storm, PR #118's 1.99.1
/// lints). The committed channel makes a bump a deliberate one-line change,
/// and a bare `rustup toolchain install` installs its channel and components
/// on any dev machine and on every runner (rustup >= 1.28, which the GitHub
/// runners ship).
///
/// The guard fails when the file goes missing, pins a floating channel
/// ("stable"/"beta"/"nightly") or an incomplete `major.minor`, drops clippy,
/// declares `targets` (that would fetch the Apple std libraries on every
/// Windows machine and runner), or when a workflow reintroduces
/// `dtolnay/rust-toolchain` or runs cargo without installing the pinned
/// toolchain first. The macOS job must add its cross-build target itself.
#[test]
fn rust_toolchain_is_pinned_for_local_and_ci() {
    let root = env!("CARGO_MANIFEST_DIR");
    let rel = "rust-toolchain.toml";
    let text = std::fs::read_to_string(format!("{root}/{rel}")).unwrap_or_else(|e| {
        panic!(
            "{rel} is missing at the repo root ({e}): local cargo commands and \
             every CI job take their Rust version from it"
        )
    });

    // An exact version, not a floating channel: "stable"/"beta"/"nightly" and
    // an incomplete "1.99" all let a new release in without a commit.
    let channel = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("channel = "))
        .unwrap_or_else(|| panic!("{rel} declares no `channel = \"...\"` line"));
    let channel = channel.trim().trim_matches('"');
    let parts: Vec<&str> = channel.split('.').collect();
    let exact_version = parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    assert!(
        exact_version,
        "{rel} pins channel {channel:?}: use an exact three-part version such \
         as \"1.99.1\" — \"stable\", \"beta\", \"nightly\" and \"1.99\" all \
         let a new Rust release in without a commit"
    );

    let components = text
        .lines()
        .find(|line| line.trim_start().starts_with("components"))
        .unwrap_or_else(|| panic!("{rel} declares no `components` line"));
    assert!(
        components.contains("\"clippy\""),
        "{rel} must list clippy in `components` — every CI job runs `cargo \
         clippy` under #![deny(warnings)]: {components}"
    );

    // A `targets` KEY, not the substring: the bump comment above names the
    // `--all-targets` clippy runs, and only a key makes rustup fetch std
    // libraries (the macOS job adds aarch64-apple-darwin itself).
    assert!(
        !text
            .lines()
            .map(str::trim)
            .any(|line| line.starts_with("targets")),
        "{rel} must not declare a `targets` key: rustup would then download \
         those std libraries on every machine and runner that reads the file. \
         The macOS job adds aarch64-apple-darwin itself"
    );

    // Every workflow that runs cargo must install the pinned toolchain first —
    // a runner's preinstalled Rust is whatever its image tag happens to carry.
    let workflows_dir = format!("{root}/.github/workflows");
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&workflows_dir)
        .unwrap_or_else(|e| panic!("{workflows_dir} readable: {e}"))
        .map(|entry| entry.expect(".github/workflows entry readable").path())
        .filter(|path| {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
            ext.eq_ignore_ascii_case("yml") || ext.eq_ignore_ascii_case("yaml")
        })
        .collect();
    paths.sort();
    assert!(
        !paths.is_empty(),
        ".github/workflows holds no *.yml/*.yaml workflow files — the \
         toolchain guard would pass vacuously"
    );
    for path in &paths {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<non-UTF-8 workflow file name>");
        let workflow_rel = format!(".github/workflows/{name}");
        let workflow =
            std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{workflow_rel} readable: {e}"));
        assert!(
            !workflow.contains("dtolnay/rust-toolchain"),
            "{workflow_rel} still installs Rust with dtolnay/rust-toolchain — \
             replace the action with `run: rustup toolchain install`, which \
             reads {rel}"
        );
        // Byte offset of the first cargo command: a `run:` one-liner, or a line
        // inside a `run: |` block (those carry no `run:` prefix).
        let mut offset = 0;
        let mut first_cargo: Option<(usize, String)> = None;
        for line in workflow.split_inclusive('\n') {
            let trimmed = line.trim();
            if first_cargo.is_none()
                && (trimmed.starts_with("run: cargo ") || trimmed.starts_with("cargo "))
            {
                first_cargo = Some((
                    offset + line.len() - line.trim_start().len(),
                    trimmed.to_string(),
                ));
            }
            offset += line.len();
        }
        if let Some((cargo_at, cargo_line)) = first_cargo {
            let install_at = workflow.find("rustup toolchain install").unwrap_or_else(|| {
                panic!(
                    "{workflow_rel} runs `{cargo_line}` without installing the \
                     pinned toolchain — add a step `run: rustup toolchain \
                     install` before it, so the job uses {rel} and not the \
                     runner's preinstalled Rust"
                )
            });
            assert!(
                install_at < cargo_at,
                "{workflow_rel}: `rustup toolchain install` comes after the \
                 first cargo command (`{cargo_line}`) — the pinned toolchain \
                 must be installed first"
            );
        }
    }

    // The Apple cross-build target belongs to the macOS job only: rustup would
    // otherwise download those std libraries on every Windows dev machine.
    let build_rel = ".github/workflows/build.yml";
    let build = std::fs::read_to_string(format!("{root}/{build_rel}"))
        .unwrap_or_else(|e| panic!("{build_rel} readable: {e}"));
    let macos = job_block(&build, "macos");
    let target_at = macos
        .find("rustup target add aarch64-apple-darwin")
        .unwrap_or_else(|| {
            panic!(
                "{build_rel}: the macos job must run `rustup target add \
                 aarch64-apple-darwin` — without that std the pinned toolchain \
                 cannot run `npx tauri build --target aarch64-apple-darwin`"
            )
        });
    let build_at = macos
        .find("npx tauri build --target aarch64-apple-darwin")
        .unwrap_or_else(|| {
            panic!(
                "{build_rel}: the macos job must build with `npx tauri build \
                 --target aarch64-apple-darwin`"
            )
        });
    assert!(
        target_at < build_at,
        "{build_rel}: `rustup target add aarch64-apple-darwin` comes after the \
         first `npx tauri build --target aarch64-apple-darwin` — add the target \
         before the build"
    );
}

/// Temp folders stay inside the checkout: `.cargo/config.toml` `[env]` points
/// TMP, TEMP and TMPDIR at this checkout's `target` folder.
///
/// The test suite creates thousands of temp folders through `TempDir::new()` /
/// `tempdir()`, and each one asks `std::env::temp_dir()` where to live. Without
/// the redirect that is the user's system temp folder — outside the dev tree
/// and on another drive — where a single `cargo test --lib` run left a dozen
/// `.tmpXXXXXX` folders behind (31,000 in two days): tempfile ignores a failing
/// delete, and on Windows a live SQLite handle keeps the folder open. The
/// `[env]` entries make every cargo-launched process — the tests, the build
/// scripts and the children they spawn — use this checkout's `target` folder.
///
/// The value is `target`, not `target/tmp`: cargo creates `target/tmp`
/// (CARGO_TARGET_TMPDIR) only while it builds or runs an integration-test or
/// bench target, so a unit-test binary would get a temp folder that does not
/// exist yet and every `tempdir()` would fail with NotFound.
///
/// Regression: without the `[env]` table in `.cargo/config.toml`,
/// `temp_dir()` is the system temp folder and this test fails.
#[test]
fn temp_dir_is_inside_the_checkouts_target_dir() {
    let expected = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    let temp = std::env::temp_dir();
    let canonical = |path: &std::path::Path| {
        std::fs::canonicalize(path).unwrap_or_else(|error| {
            panic!(
                "{path:?} does not resolve ({error}): `.cargo/config.toml` must \
                 carry an `[env]` table with TMP, TEMP and TMPDIR = \
                 {{ value = \"target\", relative = true, force = true }}, so \
                 every cargo-run process keeps its temp folders inside this \
                 checkout's target folder"
            )
        })
    };
    let actual = canonical(&temp);
    let wanted = canonical(&expected);
    // Both paths are canonical, so they name one folder; only ASCII case can
    // differ, on a case-insensitive filesystem. Compare case-insensitively, no
    // Windows-only code needed.
    assert!(
        actual
            .to_string_lossy()
            .eq_ignore_ascii_case(&wanted.to_string_lossy()),
        "std::env::temp_dir() is {temp:?} (canonical {actual:?}), but a \
         cargo-run process must write its temp folders to this checkout's \
         {wanted:?}. Add the `[env]` table with TMP, TEMP and TMPDIR = \
         {{ value = \"target\", relative = true, force = true }} to \
         .cargo/config.toml: without it every test temp folder lands in the \
         user's system temp folder and leaks there (tempfile ignores the delete \
         error when a handle is still open on Windows)"
    );
}
