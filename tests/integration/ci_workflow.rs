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
/// `error: invalid value '1' for '--ci'` (exit 1 in ~2s, before the frontend
/// rebuild even started). Regression: the pre-fix build.yml set `CI: "1"` and
/// the windows job's "Tauri build (unsigned installers)" step never produced
/// installers (run 35514715816, tag v0.1.1).
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

/// Both platform jobs must build the separate Mnemo2 installer with
/// `npx tauri build` run from `src-tauri/mnemo2-app`.
///
/// Mnemo2 is its own Cargo package (`mnemo2-app`) with its own
/// `tauri.conf.json` (productName Mnemo2, identifier com.mnemo.mnemo2). The
/// Mnemo bundle (`npx tauri build` at the repo root) packages every `[[bin]]`
/// of `mnemo-app` — the v2.0.0 tag build died on that ("cannot find
/// target/release/mnemo2_mcp.exe") — so each app is bundled from its own
/// package, both landing in the shared `<repo>/target/...` bundle directories
/// under distinct `Mnemo_*` / `Mnemo2_*` names. Without the Mnemo2 step the
/// release would ship only the Mnemo installer.
///
/// The macOS zip step must zip each bundle separately: the pre-split `*.app`
/// glob matches two bundles now and `ditto` refuses several sources, so both
/// apps get their own zip and both zips are uploaded.
#[test]
fn build_workflow_bundles_both_apps() {
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
                    "the {job} job must build the separate Mnemo2 installer: \
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
             only, mirroring the Mnemo bundle step"
        );
        // The frontend dist (target/mnemo2-dist, the nested config's
        // frontendDist) must be built before the custom-protocol bundle that
        // embeds it.
        assert!(
            block[..step_start].contains("npm run build:mnemo2"),
            "the {job} job must build the Mnemo2 frontend (`npm run \
             build:mnemo2`) before bundling it"
        );
        // The Mnemo installer must still be built first on the same runner —
        // only text BEFORE the Mnemo2 step counts (the Mnemo2 step's own run
        // line must not satisfy the Mnemo check).
        assert!(
            block[..step_start].contains("npx tauri build"),
            "the {job} job runs no Mnemo `npx tauri build` before the Mnemo2 \
             one — the Mnemo installer is missing"
        );

        if job == "windows" {
            // The upload globs must still match both apps' installers: they
            // share the bundle dirs, distinguished only by the file names.
            for glob in [
                "target/release/bundle/msi/*.msi",
                "target/release/bundle/nsis/*.exe",
            ] {
                assert!(
                    block.contains(glob),
                    "the windows upload must keep the `{glob}` glob — it \
                     matches the Mnemo2 installers too (`Mnemo2_*` next to \
                     `Mnemo_*`)"
                );
            }
        } else {
            // One zip per .app, each staged into dist-macos/ — ditto keeps the
            // exec bit and the symlinks upload-artifact drops.
            for (bundle, zip) in [
                ("Mnemo.app", "mnemo.app.zip"),
                ("Mnemo2.app", "mnemo2.app.zip"),
            ] {
                assert!(
                    block.contains(&format!(
                        "--keepParent \
                         target/aarch64-apple-darwin/release/bundle/macos/{bundle} \
                         dist-macos/{zip}"
                    )),
                    "the macos job must zip `{bundle}` separately into \
                     dist-macos/{zip} (ditto with the pre-split `*.app` glob \
                     now matches two bundles and refuses several sources)"
                );
            }
            let upload = step_named(&block, "Upload macOS bundles");
            assert!(
                upload.lines().any(|line| line.trim() == "path: dist-macos/*"),
                "the macos upload must ship `dist-macos/*` — it holds both \
                 apps' zips and dmgs: {upload}"
            );
        }
    }
}

/// The macOS job must stage each app's `.dmg` into `dist-macos/` right after
/// that app's own `npx tauri build`, and upload `dist-macos/*`.
///
/// `tauri build` clears `target/aarch64-apple-darwin/release/bundle/dmg/`
/// while bundling, so the second build (Mnemo2) deletes the first build's
/// (Mnemo's) dmg. Run 42071163294 went green while the
/// `macos-bundles-aarch64` artifact held `mnemo.app.zip`, `mnemo2.app.zip`
/// and ONLY `Mnemo2_2.0.0_aarch64.dmg` — the release would ship no Mnemo dmg.
/// Staging each app's outputs at the point of creation makes their survival
/// independent of any later build.
#[test]
fn build_workflow_stages_each_macos_dmg() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/build.yml"
    ))
    .expect("build.yml readable");
    let job = job_block(&text, "macos");

    // Each dmg is copied out before the next build can clear the bundle dir.
    let order = [
        "Tauri build (.app + .dmg, aarch64)",
        "Stage the Mnemo bundles",
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
            "the macos job must run `{}` BEFORE `{}` — a dmg staged only after \
             the next tauri build has already been overwritten by it",
            pair[0].0,
            pair[1].0
        );
    }

    // Each staging step copies ITS app's dmg out of the shared target dir and
    // zips that app's bundle next to it.
    let mnemo = step_named(&job, "Stage the Mnemo bundles");
    assert!(
        mnemo.contains("cp target/aarch64-apple-darwin/release/bundle/dmg/*.dmg dist-macos/"),
        "the Mnemo staging step must copy the Mnemo dmg (`*.dmg` — it is the \
         only dmg in target/ at that point) into dist-macos/: {mnemo}"
    );
    assert!(
        mnemo.contains(
            "ditto -c -k --keepParent \
             target/aarch64-apple-darwin/release/bundle/macos/Mnemo.app \
             dist-macos/mnemo.app.zip"
        ),
        "the Mnemo staging step must zip Mnemo.app into dist-macos/: {mnemo}"
    );
    let mnemo2 = step_named(&job, "Stage the Mnemo2 bundles");
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

    // The upload ships the staging dir — and no longer reads the dmg out of
    // target/, where the later build's cleanup makes the file list a race.
    let upload = step_named(&job, "Upload macOS bundles");
    assert!(
        upload.lines().any(|line| line.trim() == "path: dist-macos/*"),
        "the macos upload must ship `dist-macos/*` (both apps' dmg and \
         .app.zip): {upload}"
    );
    assert!(
        !upload.contains("release/bundle/dmg"),
        "the macos upload must not read the dmg straight out of target/ — the \
         Mnemo2 build clears that directory: {upload}"
    );
    assert!(
        upload.contains("if-no-files-found: error"),
        "the macos upload must keep failing the job on an empty staging dir: \
         {upload}"
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

/// The PR gate (`.github/workflows/ci.yml`) must keep building, linting and
/// testing every pull request to main. Before it existed only CodeQL ran on
/// pull requests, so v2.2.0 shipped with clippy red and Dependabot PRs looked
/// green while breaking the build or the tests.
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
        "npm run build",
        "npm run build:mnemo2",
        "npm test --workspace frontend",
        "npm test --workspace mnemo2",
        "cargo clippy --workspace --all-targets",
        "cargo clippy -p mnemo-app --all-targets --features mnemo2",
        "cargo test --workspace --no-fail-fast",
        "cargo test -p mnemo-app --features mnemo2 --lib",
    ] {
        assert!(
            text.contains(&format!("run: {command}\n")),
            "ci.yml lost the step `{command}`"
        );
    }
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
