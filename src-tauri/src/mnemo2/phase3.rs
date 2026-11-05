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

/// Structured lead-review evidence: the lead's own unanchored read of the
/// change plus the substantive assessments it must author itself, beyond the
/// worker's summary and test counts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewEvidence {
    /// The lead read the change itself first, unanchored to worker claims.
    pub unanchored_pass: bool,
    /// Why the work exists and what it was meant to achieve.
    pub intent: String,
    /// How the implementation actually reaches that goal.
    pub execution: String,
    /// How the change fits the surrounding contracts, modules, and callers.
    pub integration: String,
    /// How well the tests genuinely exercise the change.
    pub test_quality: String,
    /// Residual risk the lead explicitly accepts.
    pub residual_risk: String,
    /// The independent audit worker linked to the same reviewed tip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub independent_reviewer_id: Option<String>,
    /// The lead's written adjudication of that audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_adjudication: Option<String>,
}

/// Require complete, substantive evidence for a lead review or merge gate.
///
/// `require_audit` additionally demands the independent-audit linkage fields;
/// high-risk reviews pass it once `team_audit_dispatch` exists (plan step 4).
pub fn validate_review_evidence(
    evidence: &ReviewEvidence,
    require_audit: bool,
) -> Result<(), String> {
    if !evidence.unanchored_pass {
        return Err(
            "review evidence must record an unanchored diff pass (unanchored_pass=true); read the change itself, not only the worker summary"
                .to_string(),
        );
    }
    for (field, value) in [
        ("intent", &evidence.intent),
        ("execution", &evidence.execution),
        ("integration", &evidence.integration),
        ("test_quality", &evidence.test_quality),
        ("residual_risk", &evidence.residual_risk),
    ] {
        if value.trim().is_empty() {
            return Err(format!(
                "review evidence '{field}' is empty; record a substantive assessment"
            ));
        }
    }
    if require_audit {
        if evidence
            .independent_reviewer_id
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            return Err(
                "high-risk reviews require an independent reviewer; dispatch one and link its audit"
                    .to_string(),
            );
        }
        if evidence
            .audit_adjudication
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            return Err(
                "high-risk reviews require a written adjudication of the independent audit"
                    .to_string(),
            );
        }
    }
    Ok(())
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
    /// Structured review evidence; legacy records decode without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ReviewEvidence>,
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
    /// Review risk captured at merge time; absent on legacy records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_risk: Option<String>,
    /// Structured review evidence captured at merge time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_evidence: Option<ReviewEvidence>,
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

/// The bookkeeping the lead keeps in the project, shared with Mnemo1: plan
/// files (`.coding/plans/`), the closing team review reports
/// (`.coding/reviews/`), memory knowledge records (`.coding/knowledge/`), and
/// OpenSpec current-state specs and archives (`.coding/openspec/`), all
/// committed with the lead's own work.
const LEAD_BOOKKEEPING_PREFIXES: [&str; 4] = [
    ".coding/plans/",
    ".coding/reviews/",
    ".coding/knowledge/",
    ".coding/openspec/",
];

fn is_lead_bookkeeping(path: &str) -> bool {
    LEAD_BOOKKEEPING_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// Require a clean lead checkout, ignoring its plan and knowledge bookkeeping: the lead
/// edits its plan while it works and commits it with its own changes, so a
/// dirty plan must not block integration. Worker checks stay strict
/// ([`require_clean`]).
pub async fn require_clean_lead(root: &Path, label: &str) -> Result<(), String> {
    let args = ["status", "--porcelain", "-z", "-uall"];
    let output = git(root, &args).await?;
    if !output.status.success() {
        return Err(git_error(&args, &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = stdout.split('\0').filter(|entry| !entry.is_empty());
    while let Some(entry) = entries.next() {
        let (status, path) = entry.split_at(entry.len().min(3));
        // A rename or copy is followed by its source path as its own entry.
        let source = if status.contains(['R', 'C']) {
            entries.next()
        } else {
            None
        };
        if is_lead_bookkeeping(path) && source.is_none_or(is_lead_bookkeeping) {
            continue;
        }
        return Err(format!("the {label} working tree must be clean"));
    }
    Ok(())
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
            return Err(format!(
                "finding '{}' has an invalid severity \"{}\" (allowed: {})",
                finding.id,
                finding.severity,
                FINDING_SEVERITIES.join(", ")
            ));
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
            _ => {
                return Err(format!(
                    "finding '{}' has an invalid status \"{}\" (allowed: {})",
                    finding.id,
                    finding.status,
                    FINDING_STATUSES.join(", ")
                ))
            }
        }
    }
    Ok(())
}

/// Allowed finding severities, in the order the tool schema lists them.
pub const FINDING_SEVERITIES: [&str; 2] = ["high", "low"];
/// Allowed finding statuses.
pub const FINDING_STATUSES: [&str; 3] = ["open", "fixed", "wontfix"];

/// Parse a reviewer's `findings` array leniently: common field aliases map to
/// the canonical names (`path` → `file`, `description`/`message` → `detail`),
/// `status` defaults to `open`, `line` to 0, a missing `id` derives from the
/// file and title (stable across re-reviews), and well-known severity words
/// map to `high`/`low`. Every remaining problem is reported in one error, one
/// `findings[i].field` line each, with the allowed values for enums.
pub fn parse_findings(value: &serde_json::Value) -> Result<Vec<Finding>, String> {
    use serde_json::Value;
    let Some(items) = value.as_array() else {
        return Err("findings: must be an array of finding objects".to_string());
    };
    let mut errors = Vec::new();
    let mut findings = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            errors.push(format!("findings[{i}]: must be an object"));
            continue;
        };
        let text = |names: &[&str]| -> Option<String> {
            names.iter().find_map(|name| {
                obj.get(*name)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
        };
        let title = text(&["title", "summary", "name"]);
        let detail = text(&["detail", "description", "message", "body", "details"]);
        let file = text(&["file", "path", "file_path", "filename"]);
        for (present, field) in [
            (title.is_some(), "title"),
            (detail.is_some(), "detail (why it matters)"),
            (file.is_some(), "file (repository-relative path)"),
        ] {
            if !present {
                let (name, hint) = field.split_once(' ').unwrap_or((field, ""));
                let hint = if hint.is_empty() {
                    String::new()
                } else {
                    format!(" {hint}")
                };
                errors.push(format!("findings[{i}].{name}: required{hint}"));
            }
        }
        let severity = match text(&["severity", "priority"]) {
            None => {
                errors.push(format!(
                    "findings[{i}].severity: required (allowed: {})",
                    FINDING_SEVERITIES.join(", ")
                ));
                None
            }
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "high" | "blocking" | "blocker" | "critical" | "major" | "error" | "must" => {
                    Some("high")
                }
                "low" | "minor" | "nit" | "info" | "warning" | "suggestion" | "should" => {
                    Some("low")
                }
                _ => {
                    errors.push(format!(
                        "findings[{i}].severity: invalid \"{raw}\" (allowed: {})",
                        FINDING_SEVERITIES.join(", ")
                    ));
                    None
                }
            },
        };
        let status = text(&["status"])
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "open".to_string());
        if !FINDING_STATUSES.contains(&status.as_str()) {
            errors.push(format!(
                "findings[{i}].status: invalid \"{status}\" (allowed: {})",
                FINDING_STATUSES.join(", ")
            ));
        }
        let line = match obj.get("line") {
            None | Some(Value::Null) => 0,
            Some(v) => match v
                .as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            {
                Some(n) => u32::try_from(n).unwrap_or(0),
                None => {
                    errors.push(format!(
                        "findings[{i}].line: must be a non-negative integer"
                    ));
                    0
                }
            },
        };
        let (Some(title), Some(detail), Some(file), Some(severity)) =
            (title, detail, file, severity)
        else {
            continue;
        };
        let id = text(&["id"]).unwrap_or_else(|| derived_finding_id(&file, &title));
        findings.push(Finding {
            id,
            severity: severity.to_string(),
            title,
            detail,
            file,
            line,
            status,
            fix_commit: text(&["fix_commit"]),
            evidence: text(&["evidence"]),
            justification: text(&["justification"]),
        });
    }
    if errors.is_empty() {
        Ok(findings)
    } else {
        Err(format!("invalid findings:\n{}", errors.join("\n")))
    }
}

/// A short id from a finding's file and title (FNV-1a), so re-reviewing the
/// same finding without an explicit id keeps its identity.
fn derived_finding_id(file: &str, title: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in file
        .bytes()
        .chain([0])
        .chain(title.to_ascii_lowercase().bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("f-{:08x}", hash as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_findings_normalizes_aliases_and_defaults() {
        let parsed = parse_findings(&serde_json::json!([{
            "title": "default import",
            "description": "game.js exports a named Game",
            "path": "src/main.js",
            "severity": "blocking"
        }]))
        .expect("aliases and defaults");
        let f = &parsed[0];
        assert_eq!(
            (f.file.as_str(), f.detail.as_str()),
            ("src/main.js", "game.js exports a named Game")
        );
        assert_eq!(
            (f.severity.as_str(), f.status.as_str(), f.line),
            ("high", "open", 0)
        );
        assert!(f.id.starts_with("f-"), "{}", f.id);
        let again = parse_findings(&serde_json::json!([{
            "title": "Default import", "detail": "x", "file": "src/main.js", "severity": "low"
        }]))
        .unwrap();
        assert_eq!(again[0].id, f.id, "derived id is stable across re-reviews");
    }

    #[test]
    fn parse_findings_reports_every_problem_at_once() {
        let err = parse_findings(&serde_json::json!([
            {"title": "a", "severity": "urgent", "status": "done"},
            "not an object"
        ]))
        .expect_err("invalid");
        for expected in [
            "findings[0].detail: required",
            "findings[0].file: required",
            "findings[0].severity: invalid \"urgent\" (allowed: high, low)",
            "findings[0].status: invalid \"done\" (allowed: open, fixed, wontfix)",
            "findings[1]: must be an object",
        ] {
            assert!(err.contains(expected), "missing {expected:?} in {err}");
        }
    }

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

    /// Complete evidence: an unanchored pass plus five substantive assessments.
    fn evidence() -> ReviewEvidence {
        ReviewEvidence {
            unanchored_pass: true,
            intent: "the worker implements the scoped brief".into(),
            execution: "the change flows through the documented path".into(),
            integration: "the module fits the surrounding runtime".into(),
            test_quality: "the regression reproduces the defect before the fix".into(),
            residual_risk: "no residual risk beyond the reviewed diff".into(),
            independent_reviewer_id: None,
            audit_adjudication: None,
        }
    }

    #[test]
    fn legacy_reviews_decode_without_evidence() {
        let review: ReviewRecord = serde_json::from_value(serde_json::json!({
            "summary": "old-style review",
            "tip": "abc123",
            "reviewed_at_ms": 7
        }))
        .expect("legacy review decodes");
        assert_eq!(review.summary, "old-style review");
        assert!(review.evidence.is_none());
    }

    #[test]
    fn review_evidence_round_trips() {
        let review = ReviewRecord {
            summary: "review".into(),
            tip: "abc123".into(),
            reviewed_at_ms: 7,
            evidence: Some(evidence()),
        };
        let value = serde_json::to_value(&review).expect("review JSON");
        assert!(value.get("evidence").is_some());
        let restored: ReviewRecord =
            serde_json::from_value(value).expect("evidence round-trips");
        assert_eq!(restored, review);
    }

    #[test]
    fn merge_records_decode_without_legacy_evidence() {
        let merged: MergeRecord = serde_json::from_value(serde_json::json!({
            "commit": "def456",
            "worker_tip": "abc123",
            "branch": "wt/agent-00000001",
            "target": "wt/lead",
            "checks": ["cargo test --workspace"],
            "review_summary": "reviewed",
            "merged_at_ms": 9
        }))
        .expect("legacy merge record decodes");
        assert!(merged.review_risk.is_none());
        assert!(merged.review_evidence.is_none());
    }

    #[test]
    fn unanchored_or_empty_evidence_is_rejected() {
        let mut missing_pass = evidence();
        missing_pass.unanchored_pass = false;
        assert!(validate_review_evidence(&missing_pass, false)
            .expect_err("an anchored-only review is not substantive")
            .contains("unanchored"));

        let mut empty_intent = evidence();
        empty_intent.intent = "   ".into();
        assert!(validate_review_evidence(&empty_intent, false)
            .expect_err("empty intent is not substantive")
            .contains("intent"));

        let mut empty_risk = evidence();
        empty_risk.residual_risk = String::new();
        assert!(validate_review_evidence(&empty_risk, false)
            .expect_err("empty residual risk is not substantive")
            .contains("residual_risk"));

        validate_review_evidence(&evidence(), false).expect("complete evidence passes");
    }

    #[test]
    fn high_risk_evidence_requires_an_audit_linkage_when_demanded() {
        assert!(validate_review_evidence(&evidence(), true)
            .expect_err("an unlinked review is incomplete")
            .contains("independent reviewer"));
        let mut linked = evidence();
        linked.independent_reviewer_id = Some("audit0001".into());
        assert!(validate_review_evidence(&linked, true)
            .expect_err("an unadjudicated audit is incomplete")
            .contains("adjudication"));
        linked.audit_adjudication = Some("the audit confirmed the reviewed tip".into());
        validate_review_evidence(&linked, true).expect("audited evidence passes");
        // The base contract never demands the audit fields.
        validate_review_evidence(&evidence(), false).expect("standard evidence passes");
    }
}
