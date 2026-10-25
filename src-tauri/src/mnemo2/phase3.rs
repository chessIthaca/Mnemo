// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Durable Phase 3 review and merge records plus local Git primitives.

use std::path::{Component, Path, PathBuf};
use std::process::Output;

use serde::{Deserialize, Serialize};

/// One lead review finding. Resolution evidence survives application restarts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    /// Stable identifier supplied by the reviewer.
    pub id: String,
    /// `high` or `low`.
    pub severity: String,
    /// Short finding title.
    pub title: String,
    /// Why the finding matters.
    pub detail: String,
    /// Repository-relative source path.
    pub file: String,
    /// One-based line, or zero when the finding is file-wide.
    pub line: u32,
    /// `open`, `fixed`, or `wontfix`.
    pub status: String,
    /// Commit that fixed the finding, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_commit: Option<String>,
    /// Verification or reasoning that closed the finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Required explanation for `wontfix`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// The lead's review of one exact worker commit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewRecord {
    /// Human/model-readable review summary.
    pub summary: String,
    /// Worker `HEAD` reviewed by the lead.
    pub tip: String,
    /// Unix epoch milliseconds.
    pub reviewed_at_ms: u128,
}

/// A successful local merge and its optional later revert.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergeRecord {
    /// Resulting merge commit on the lead branch.
    pub commit: String,
    /// Merged worker commit.
    pub worker_tip: String,
    /// Retained worker branch.
    pub branch: String,
    /// Local lead branch (never `main`).
    pub target: String,
    /// Exact-tip check names used by the merge gate.
    pub checks: Vec<String>,
    /// Review summary captured at merge time.
    pub review_summary: String,
    /// Files changed relative to the merge commit's first parent.
    #[serde(default)]
    pub files: u64,
    /// Added lines relative to the merge commit's first parent.
    #[serde(default)]
    pub additions: u64,
    /// Deleted lines relative to the merge commit's first parent.
    #[serde(default)]
    pub deletions: u64,
    /// Actual serving model, or the selected model spec when unavailable.
    #[serde(default)]
    pub worker_model: Option<String>,
    /// Real worker token telemetry captured at merge time.
    #[serde(default)]
    pub worker_tokens: u64,
    /// Estimated cost in millionths of a dollar when pricing is configured.
    #[serde(default)]
    pub cost_micros: Option<u64>,
    /// Unix epoch milliseconds.
    pub merged_at_ms: u128,
    /// Local revert commit, if the merge was backed out before landing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revert_commit: Option<String>,
}

/// Current Unix epoch milliseconds without exposing clocks in the record logic.
pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

/// Run Git without a shell in one repository and preserve diagnostics.
pub async fn git(root: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = tokio::process::Command::new("git");
    command.args(args).current_dir(root);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .output()
        .await
        .map_err(|err| format!("could not run git in {}: {err}", root.display()))
}

/// Render a failed Git invocation with stderr/stdout for the durable error.
pub fn git_error(args: &[&str], output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    format!(
        "git {} failed: {}{}",
        args.join(" "),
        stderr,
        if stdout.is_empty() {
            String::new()
        } else {
            format!("\n{stdout}")
        }
    )
}

/// Read a successful Git stdout value.
pub async fn git_value(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = git(root, args).await?;
    if !output.status.success() {
        return Err(git_error(args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Read the exact current commit of a checkout.
pub async fn tip(root: &Path) -> Result<String, String> {
    git_value(root, &["rev-parse", "HEAD"]).await
}

/// Return `(files, additions, deletions)` introduced by a merge commit
/// relative to its first (lead-branch) parent.
pub async fn merge_diff_stats(root: &Path, commit: &str) -> Result<(u64, u64, u64), String> {
    let first_parent = format!("{commit}^1");
    let numstat = git_value(root, &["diff", "--numstat", &first_parent, commit]).await?;
    Ok(parse_numstat(&numstat))
}

fn parse_numstat(numstat: &str) -> (u64, u64, u64) {
    let mut stats = (0, 0, 0);
    for line in numstat.lines().filter(|line| !line.trim().is_empty()) {
        let mut columns = line.split('\t');
        let (Some(additions), Some(deletions), Some(_path)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        stats.0 += 1;
        stats.1 += additions.parse::<u64>().unwrap_or(0);
        stats.2 += deletions.parse::<u64>().unwrap_or(0);
    }
    stats
}

/// Require a clean checkout. Ignored files do not block integration.
pub async fn require_clean(root: &Path, label: &str) -> Result<(), String> {
    let status = git_value(root, &["status", "--porcelain"]).await?;
    if status.is_empty() {
        Ok(())
    } else {
        Err(format!("the {label} working tree must be clean"))
    }
}

/// Resolve an existing, canonical, symlink-free review-edit source file inside
/// the worker worktree.
pub fn review_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    let blocked_dir = path.components().any(|part| {
        let Component::Normal(part) = part else {
            return false;
        };
        matches!(
            part.to_string_lossy().to_ascii_lowercase().as_str(),
            ".git"
                | ".coding"
                | ".github"
                | "test"
                | "tests"
                | "benches"
                | "node_modules"
                | "target"
                | "dist"
        )
    });
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let test_file = file_name.contains(".test.")
        || file_name.contains(".spec.")
        || path
            .file_stem()
            .map(|stem| {
                let stem = stem.to_string_lossy().to_ascii_lowercase();
                stem.ends_with("_test") || stem.ends_with("_tests")
            })
            .unwrap_or(false);
    let blocked_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "json"
                    | "jsonl"
                    | "toml"
                    | "yaml"
                    | "yml"
                    | "lock"
                    | "sql"
                    | "graphql"
                    | "gql"
                    | "proto"
            )
        })
        .unwrap_or(true);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || blocked_dir
        || test_file
        || blocked_extension
    {
        return Err(
            "review_fix requires an existing repository-relative non-test source file".to_string(),
        );
    }
    let candidate = root.join(path);
    let canonical_root = root
        .canonicalize()
        .map_err(|err| format!("could not resolve the worker worktree: {err}"))?;
    let canonical = candidate
        .canonicalize()
        .map_err(|err| format!("review_fix target must be an existing source file: {err}"))?;
    if canonical != canonical_root.join(path) {
        return Err("review_fix must not follow symlinks".to_string());
    }
    Ok(canonical)
}

/// Validate finding identifiers and terminal-state explanations.
pub fn validate_findings(findings: &[Finding]) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for finding in findings {
        if finding.id.trim().is_empty() || finding.title.trim().is_empty() {
            return Err("every finding needs an id and title".to_string());
        }
        if finding.detail.trim().is_empty() || finding.file.trim().is_empty() {
            return Err(format!("finding '{}' needs detail and a file", finding.id));
        }
        if !ids.insert(finding.id.as_str()) {
            return Err(format!("duplicate finding id '{}'", finding.id));
        }
        if !matches!(finding.severity.as_str(), "high" | "low") {
            return Err(format!("finding '{}' has an invalid severity", finding.id));
        }
        match finding.status.as_str() {
            "open" => {}
            "fixed" if finding.evidence.as_deref().unwrap_or("").trim().is_empty() => {
                return Err(format!("finding '{}' needs fixed evidence", finding.id));
            }
            "wontfix"
                if finding
                    .justification
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty() =>
            {
                return Err(format!(
                    "finding '{}' needs a wontfix justification",
                    finding.id
                ));
            }
            "fixed" | "wontfix" => {}
            _ => return Err(format!("finding '{}' has an invalid status", finding.id)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(status: &str) -> Finding {
        Finding {
            id: "F-1".into(),
            severity: "low".into(),
            title: "Example".into(),
            detail: "Detail".into(),
            file: "src/lib.rs".into(),
            line: 1,
            status: status.into(),
            fix_commit: None,
            evidence: None,
            justification: None,
        }
    }

    #[test]
    fn review_paths_cannot_escape_or_change_project_metadata_or_tests() {
        let root = tempfile::tempdir().expect("worktree");
        std::fs::create_dir_all(root.path().join("src")).expect("source dir");
        std::fs::write(root.path().join("src/lib.rs"), "pub fn source() {}\n").expect("source");
        assert!(review_file(root.path(), "../lead.rs").is_err());
        assert!(review_file(root.path(), "package.json").is_err());
        assert!(review_file(root.path(), "tests/regression.rs").is_err());
        assert!(review_file(root.path(), "src/component.test.ts").is_err());
        assert!(review_file(root.path(), ".coding/plans/example.rs").is_err());
        assert!(review_file(root.path(), "src/missing.rs").is_err());
        assert_eq!(
            review_file(root.path(), "src/lib.rs").expect("source"),
            root.path()
                .join("src/lib.rs")
                .canonicalize()
                .expect("canonical source")
        );
    }

    #[test]
    fn review_paths_reject_symlink_targets() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink as symlink_file;
        #[cfg(windows)]
        use std::os::windows::fs::symlink_file;

        let root = tempfile::tempdir().expect("worktree");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir_all(root.path().join("src")).expect("source dir");
        std::fs::write(outside.path().join("lead.rs"), "pub fn lead() {}\n")
            .expect("outside source");
        if symlink_file(
            outside.path().join("lead.rs"),
            root.path().join("src/lib.rs"),
        )
        .is_ok()
        {
            assert!(review_file(root.path(), "src/lib.rs").is_err());
        }
    }

    #[test]
    fn wontfix_requires_written_justification() {
        let finding = finding("wontfix");
        assert!(validate_findings(&[finding])
            .expect_err("justification")
            .contains("justification"));
    }

    #[test]
    fn fixed_findings_need_valid_severity_and_evidence() {
        let mut fixed = finding("fixed");
        fixed.severity = "critical".into();
        assert!(validate_findings(&[fixed.clone()])
            .expect_err("invalid severity")
            .contains("severity"));
        fixed.severity = "high".into();
        assert!(validate_findings(&[fixed.clone()])
            .expect_err("fixed evidence")
            .contains("evidence"));
        fixed.evidence = Some("regression test passes on the fix commit".into());
        validate_findings(&[fixed]).expect("fully evidenced fixed finding");
    }
}
