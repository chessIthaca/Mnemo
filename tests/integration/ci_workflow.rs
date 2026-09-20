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
