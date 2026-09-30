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
        if trimmed.starts_with("CI:") {
            let value = trimmed["CI:".len()..]
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

/// Every `uses:` in this repo's workflows must be pinned to a full 40-hex
/// commit SHA, with the human version in a trailing `# vN` comment so
/// Dependabot can keep it current.
///
/// A tag ref is mutable: whoever controls the action repository can repoint
/// `@v7` at new code that then runs with this repo's `GITHUB_TOKEN` — the
/// release job's token is `contents: write`. SHA pins make the executed commit
/// immutable.
///
/// The documented exception is `dtolnay/rust-toolchain@stable`: that action
/// reads its own ref to choose the toolchain (`@stable`, `@nightly`,
/// `@1.75.0`, …) and publishes no version tags, so a SHA pin would break it.
/// Because of that exception the repo-level "require SHA pinning" Actions
/// setting stays off (it would reject the rust-toolchain step).
///
/// The guard enumerates `.github/workflows/` itself — every `*.yml`/`*.yaml`
/// file found there is checked, so a workflow file added later is covered
/// without editing the test; a hardcoded file list would silently leave it
/// unguarded. A missing or empty workflows directory fails the test outright
/// instead of passing vacuously.
#[test]
fn workflows_pin_actions_to_full_shas() {
    const ALLOWED_UNPINNED: [&str; 1] = ["dtolnay/rust-toolchain@stable"];
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
const UNUSED_PATCH_MSG: &str = "Cargo.lock carries a [[patch.unused]] entry: a [patch.crates-io] override (the tao/wry path patches in Cargo.toml) no longer applies to the resolved version, so the registry crate would ship instead of the vendored build — losing the Windows keyboard/IME deadlock backport and the WebView2 SSO + hard-reload patches. Re-pin tauri / tauri-runtime-wry by hand together with vendor/ (see vendor/tao/PATCHES.md, vendor/wry/PATCHES.md and the .github/dependabot.yml ignore list), then re-run this guard";

/// The lock must keep the vendored `[patch.crates-io]` overrides in effect:
/// the `tao` and `wry` packages must stay PATH-sourced in `Cargo.lock` (no
/// `source = ` line in their `[[package]]` entries, so the resolved crates
/// are the `vendor/` copies) and the lock must carry no `[[patch.unused]]`
/// entries. This is the machine check behind the `.github/dependabot.yml`
/// review note: bumping `tauri` past 2.11.6 (or `tauri-runtime-wry` past
/// 2.11.4) moves wry to 0.57 / tao to 0.37, turning both overrides into
/// `[[patch.unused]]` — the registry crates would then ship instead of the
/// vendored builds, losing the Windows keyboard/IME deadlock backport
/// (vendor/tao/PATCHES.md) and the WebView2 OS-account SSO + hard-reload
/// patches (vendor/wry/PATCHES.md) while CI stays green. Review H1 of
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
    // `tao-macros` must never count as `tao`.
    let mut seen_tao = false;
    let mut seen_wry = false;
    let mut target: Option<&str> = None;
    for (idx, line) in text.lines().enumerate() {
        if line.starts_with("[[") {
            target = None;
        } else if let Some(name) = line
            .strip_prefix("name = \"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            match name {
                "tao" => {
                    seen_tao = true;
                    target = Some("tao");
                }
                "wry" => {
                    seen_wry = true;
                    target = Some("wry");
                }
                _ => target = None,
            }
        } else if line.starts_with("source = ") {
            if let Some(name) = target {
                panic!(
                    "Cargo.lock's `{name}` entry (line {}) is not path-sourced \
                     (`{line}`): the vendored [patch.crates-io] override is not \
                     in effect, so a registry/git build ships instead of \
                     vendor/{name} — see vendor/{name}/PATCHES.md",
                    idx + 1
                );
            }
        }
    }
    assert!(
        seen_tao && seen_wry,
        "Cargo.lock resolves no tao/wry package (saw tao={seen_tao}, \
         wry={seen_wry}): both vendored crates must resolve"
    );
}
