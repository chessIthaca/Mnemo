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
