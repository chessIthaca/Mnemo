// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `write_review_report` — the read-only reviewer's single output channel.
//!
//! Writes a review report to a path under `.coding/reviews/` only. The path is
//! sandboxed: the canonicalized target must start with the canonicalized
//! reviews dir (path-traversal guard, mirroring `FinishTool`'s report
//! validation). This is the only way a read-only reviewer subagent can persist
//! its findings — it has no `file_write`/`file_edit`/`shell`/`git`. `AutoRun`
//! — it only writes to the project's own `.coding/` bookkeeping dir, never
//! user code.
//!
//! **Constructor-only authorship:** this tool is visible under NO base
//! workflow filter — only a spawned `role: "reviewer"` agent carries it, via
//! the strict `ToolFilter::Reviewer` allow-list granted on the spawn path
//! (`Workflow::set_reviewer_allowlist` in `spawn_agent_shared`). The main
//! agent can never call it (denied at schema + dispatch), and `.coding/reviews/`
//! is protected from the file tools (`Sandbox::is_protected_write_target`) so
//! a report cannot be fabricated through file writes either. A review report
//! can only ever be authored by a spawned reviewer.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Strip a leading date-shaped `YYYY-MM-DD-` prefix from `name`, if any
/// (backlog 5b46674d: reviewers sometimes invent stale dates — live-observed
/// reports named 2026-12-23/24 written 2026-12-29/30).
fn strip_date_prefix(name: &str) -> &str {
    let b = name.as_bytes();
    let date_shaped = b.len() >= 11
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'-';
    if date_shaped {
        &name[11..]
    } else {
        name
    }
}

/// The report filename the tool actually uses: today's UTC date prefix
/// (system clock, via `format_epoch_date`) + the reviewer's name with any
/// date-shaped prefix stripped. The date on a review report is when it was
/// WRITTEN, not when the reviewer thinks it is.
fn normalize_report_filename(name: &str) -> String {
    let today = crate::tool::memory::format_epoch_date(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64,
    );
    format!("{today}-{}", strip_date_prefix(name))
}

/// The first non-blank line of `s` (the verdict-line position), capped at
/// 120 chars for error messages.
fn first_non_blank_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

/// Arguments for `write_review_report`.
#[derive(Debug, Deserialize)]
struct WriteReviewReportArgs {
    /// The filename to write under `.coding/reviews/` (e.g.
    /// `my-feature-review.md` — the `YYYY-MM-DD` date prefix is added
    /// automatically from the system clock; a supplied date-shaped prefix is
    /// replaced with today's). Must be a bare filename — no sub-directories,
    /// no `..`, no absolute paths. Anything else is rejected.
    path: String,
    /// The report body (Markdown) for THIS call. Must be non-empty.
    content: String,
    /// Optional write mode, mirroring `file_write`: `"overwrite"` (default)
    /// replaces the report; `"append"` adds to the end of the existing report
    /// (creating it if absent) — the chunked protocol for long reports:
    /// first call carries the verdict line + summary, follow-up calls append
    /// finding sections in small chunks.
    #[serde(default)]
    mode: Option<String>,
}

/// The `write_review_report` tool.
///
/// Holds the reviews directory (`.coding/reviews/`). The reviewer subagent
/// calls this to persist its findings; the main agent later reads the written
/// path and passes it to `finish` to close out the review.
pub struct WriteReviewReportTool {
    reviews_dir: PathBuf,
}

impl WriteReviewReportTool {
    /// Create the tool bound to the reviews directory (`.coding/reviews/`).
    pub fn new(reviews_dir: impl Into<PathBuf>) -> Self {
        Self {
            reviews_dir: reviews_dir.into(),
        }
    }

    /// Resolve `path` against `reviews_dir` and confirm the canonicalized
    /// target stays inside `reviews_dir` (path-traversal guard). Returns the
    /// resolved target, or an error message.
    fn resolve_under_reviews(&self, path: &str) -> Result<PathBuf, String> {
        // Reject absolute paths and any component that escapes the reviews dir
        // before touching the filesystem (defense in depth — the canonicalize
        // check below is the real guard, but this gives a clear early error).
        let p = Path::new(path);
        if p.is_absolute() {
            return Err(format!(
                "path '{path}' must be a relative filename under .coding/reviews/, not an absolute path"
            ));
        }
        // Bare filenames only — no sub-paths. A sub-path like "2026/04/x.md"
        // would require creating intermediate dirs before canonicalize can
        // resolve the parent, which means a side effect before the containment
        // check (and a directory created outside .coding/reviews/ for a
        // traversal like "../evil/x.md"). Every report is a bare filename, so
        // reject any path with a separator or a `..` component outright.
        if path.contains(std::path::MAIN_SEPARATOR)
            || path.contains('/')
            || path.contains('\\')
            || path == ".."
            || path.contains("..")
        {
            return Err(format!(
                "path '{path}' must be a bare filename under .coding/reviews/ (no sub-directories or `..`)"
            ));
        }
        let target = self.reviews_dir.join(path);

        // Ensure the reviews dir exists so canonicalize can resolve it.
        std::fs::create_dir_all(&self.reviews_dir).map_err(|e| {
            format!(
                "failed to create reviews dir {}: {e}",
                self.reviews_dir.display()
            )
        })?;

        let canon_target = target.canonicalize().or_else(|_| {
            // The target may not exist yet (we're about to write it). Canonicalize
            // its parent (the reviews dir, which exists) and join the file name,
            // so the starts_with check still works for a not-yet-written file.
            let parent = target.parent().unwrap_or_else(|| Path::new("."));
            let canon_parent = parent.canonicalize().map_err(|e| {
                format!(
                    "failed to resolve reviews dir parent {}: {e}",
                    parent.display()
                )
            })?;
            let file_name = target
                .file_name()
                .ok_or_else(|| "path has no file name".to_string())?;
            Ok::<PathBuf, String>(canon_parent.join(file_name))
        })?;
        let canon_reviews = self
            .reviews_dir
            .canonicalize()
            .map_err(|e| format!("failed to canonicalize reviews dir: {e}"))?;
        if !canon_target.starts_with(&canon_reviews) {
            return Err(format!(
                "path '{path}' resolves outside .coding/reviews/ — refusing to write (path traversal)"
            ));
        }
        Ok(canon_target)
    }
}

#[async_trait]
impl Tool for WriteReviewReportTool {
    fn name(&self) -> &str {
        "write_review_report"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "write_review_report",
            "Write your review report under .coding/reviews/. Your ONLY way to persist \
             findings — you have no file_write/file_edit/shell/git. Exists ONLY for \
             role:\"reviewer\" agents; the main agent can never call it. The main agent \
             reads the report, fixes every finding, then calls `finish` with the path \
             returned here.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Bare filename under .coding/reviews/ (e.g. \"my-feature-review.md\") — the YYYY-MM-DD date prefix is added automatically from the system clock (any date prefix you supply is replaced with today's); absolute paths, sub-directories and `..` are rejected. Reuse the SAME path for follow-up appends."
                    },
                    "content": {
                        "type": "string",
                        "description": "The report body for THIS call (Markdown), non-empty. The FIRST call's content MUST open with \"## Verdict: PASS\" or \"## Verdict: FINDINGS (n high, n low)\" — the write is rejected without it and an unparseable verdict counts as FINDINGS (fail closed). For a long report, keep the first call short (verdict + one-line summary) and append the finding sections in chunks with mode=\"append\"."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["overwrite", "append"],
                        "description": "Default \"overwrite\". \"append\" adds content to the end of the existing report (creates it if absent) — like file_write's append mode, so a long report can be written in several small calls."
                    }
                },
                "required": ["path", "content"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Only writes to the project's own .coding/reviews/ bookkeeping dir —
        // never user code, never prompts.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: WriteReviewReportArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        if args.path.trim().is_empty() {
            return ToolResult::error("write_review_report requires a non-empty 'path'");
        }
        if args.content.trim().is_empty() {
            return ToolResult::error("write_review_report requires non-empty 'content'");
        }
        let append = match args.mode.as_deref() {
            None | Some("overwrite") => false,
            Some("append") => true,
            Some(other) => {
                return ToolResult::error(format!(
                    "write_review_report mode must be \"overwrite\" or \"append\" (got {other})"
                ))
            }
        };

        // Validate the reviewer-supplied path FIRST (backlog 5b46674d): the
        // rejection error must show the path the reviewer actually sent — an
        // absolute path must hit the absolute-path branch, not be
        // date-mangled into a sub-path-looking name first.
        if let Err(e) = self.resolve_under_reviews(&args.path) {
            return ToolResult::error(e);
        }
        // Date normalization (backlog 5b46674d): the tool date-prefixes from
        // the system clock — a supplied date-shaped prefix is replaced with
        // today's. Appends resolve the AS-GIVEN path first (the chunked
        // protocol's "reuse the SAME path" contract, plus files seeded before
        // normalization), then the normalized name.
        let normalized = normalize_report_filename(&args.path);
        let mut requested = if append && self.reviews_dir.join(&args.path).is_file() {
            args.path.clone()
        } else {
            normalized
        };
        // Cross-midnight chunked append (review LOW 1, backlog 5b46674d): a
        // bare-named report written before a UTC-midnight rollover lives at
        // <old-date>-name; a follow-up append with the same bare name
        // resolves to <today>-name, which does not exist — the create path
        // would then reject a legitimate verdict-less append chunk. Look
        // for the unique <any-date>-<stripped-name> variant before falling
        // to the create path (ambiguous matches stay fail-closed).
        if append && !self.reviews_dir.join(&requested).is_file() {
            let stripped = strip_date_prefix(&args.path).to_string();
            let mut variants: Vec<String> = std::fs::read_dir(&self.reviews_dir)
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|name| {
                    name != &requested && name != &stripped && strip_date_prefix(name) == stripped
                })
                .collect();
            if variants.len() == 1 {
                requested = variants.pop().unwrap_or_default();
            }
        }
        let target = match self.resolve_under_reviews(&requested) {
            Ok(t) => t,
            Err(e) => return ToolResult::error(e),
        };
        // The project-relative display path (the reviews dir is
        // <project>/.coding/reviews by construction — factory and FinishTool
        // derive it identically), so the notification and the parent's finish
        // call carry ".coding/reviews/<file>" instead of the canonicalized
        // \\?\ absolute alias (backlog 5b46674d).
        let display_path = format!(".coding/reviews/{requested}");

        // The reviews dir (the target's parent, since `path` is a bare
        // filename) was created by resolve_under_reviews; no intermediate
        // dirs to create here.
        let existing = std::fs::read_to_string(&target).ok();

        // The verdict line is the report's contract: the FIRST non-blank line
        // of the report FILE must be "## Verdict: PASS" or
        // "## Verdict: FINDINGS (n high, n low)". An unparseable verdict
        // counts as FINDINGS (fail closed). The contract is enforced whenever
        // this call CREATES the report (overwrite, or append to a missing /
        // empty file) so a report file can never exist without a verdict; an
        // append to an existing report skips the check on the incoming chunk.
        let starts_with_verdict = |s: &str| {
            s.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .is_some_and(|l| {
                    l.starts_with("## Verdict: PASS") || l.starts_with("## Verdict: FINDINGS")
                })
        };
        let verdict_error = "review report must open with \"## Verdict: PASS\" or \
             \"## Verdict: FINDINGS (n high, n low)\" — an unparseable verdict \
             counts as FINDINGS (fail closed)";
        let existing_body = match (append, existing) {
            (true, Some(existing)) if !existing.trim().is_empty() => {
                // Defense in depth: never grow a report file that doesn't
                // open with a verdict — it could never reach `finish` as a
                // PASS, and the file may predate this tool's contract.
                if !starts_with_verdict(&existing) {
                    return ToolResult::error(format!(
                        "cannot append: existing report {} does not open with a \
                         \"## Verdict:\" line. Existing first line: \"{}\"",
                        display_path,
                        first_non_blank_line(&existing)
                    ));
                }
                Some(existing)
            }
            _ => None,
        };
        let mut appended = false;
        let body = match existing_body {
            Some(mut existing) => {
                if !existing.ends_with('\n') {
                    existing.push('\n');
                }
                existing.push_str(&args.content);
                appended = true;
                existing
            }
            None => {
                if !starts_with_verdict(&args.content) {
                    return ToolResult::error(format!(
                        "{verdict_error}. Received first line: \"{}\"",
                        first_non_blank_line(&args.content)
                    ));
                }
                args.content.clone()
            }
        };
        if let Err(e) = std::fs::write(&target, &body) {
            return ToolResult::error(format!(
                "failed to write review report {}: {e}",
                target.display()
            ));
        }
        let wrote = if appended {
            format!(
                "Appended {} bytes to review report {} (now {} bytes).",
                args.content.len(),
                display_path,
                body.len()
            )
        } else {
            format!(
                "Wrote review report to {} ({} bytes).",
                display_path,
                body.len()
            )
        };
        ToolResult::success(format!(
            "{wrote} Pass this path to `finish` after the main agent fixes any findings."
        ))
        .with_data(json!({ "path": display_path }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_in_temp() -> (WriteReviewReportTool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reviews = dir.path().join("reviews");
        std::fs::create_dir_all(&reviews).unwrap();
        (WriteReviewReportTool::new(&reviews), dir)
    }

    /// Today's UTC date prefix, computed the same way the tool does —
    /// tests assert date normalization against it.
    fn today_prefix() -> String {
        crate::tool::memory::format_epoch_date(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        )
    }

    #[tokio::test]
    async fn writes_valid_report_and_returns_path() {
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "2026-04-04-review.md",
                "content": "## Verdict: PASS\n\nno findings"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("Wrote review report"));
        // The file exists with the right content — at TODAY's date (the
        // supplied 2026-04-04 prefix is normalized away, backlog 5b46674d).
        let expected = format!("{}-review.md", today_prefix());
        let written = tool.reviews_dir.join(&expected);
        let content = std::fs::read_to_string(&written).unwrap();
        assert_eq!(content, "## Verdict: PASS\n\nno findings");
        // The returned path is project-relative (".coding/reviews/<file>"),
        // not the canonicalized \\?\ absolute alias.
        assert!(
            result.output.contains(&format!(".coding/reviews/{expected}")),
            "the success message must carry the project-relative path: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn stale_date_prefix_normalized_to_today() {
        // Backlog 5b46674d: the reviewer invents the date prefix and sometimes
        // uses a stale one (live-observed: reports named 2026-12-23/24 written
        // 2026-12-29/30). The tool must date-prefix from the system clock —
        // a supplied date-shaped prefix is replaced with today's date — and
        // return the PROJECT-RELATIVE ".coding/reviews/<file>" path (not the
        // canonicalized \\?\ absolute alias the notification used to display).
        let (tool, _dir) = tool_in_temp();
        let today = today_prefix();
        let result = tool
            .execute(serde_json::json!({
                "path": "2026-12-23-security-review.md",
                "content": "## Verdict: PASS\n\nAll good."
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        let expected = format!("{today}-security-review.md");
        assert!(
            tool.reviews_dir.join(&expected).is_file(),
            "the report must land at today's date ({expected})"
        );
        assert!(
            !tool.reviews_dir
                .join("2026-12-23-security-review.md")
                .exists(),
            "the stale-dated name must not be used"
        );
        let path = result
            .data
            .as_ref()
            .and_then(|d| d.get("path"))
            .and_then(|v| v.as_str())
            .expect("with_data carries the path");
        assert_eq!(
            path,
            format!(".coding/reviews/{expected}"),
            "the returned path must be project-relative"
        );
        assert!(
            !result.output.contains("\\\\?\\"),
            "the success message must not show the verbatim alias: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn accepts_findings_verdict_line() {
        // A FINDINGS verdict (with the severity counts) is a valid report —
        // the main agent must fix the findings before finish.
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "2026-04-04-findings.md",
                "content": "## Verdict: FINDINGS (2 high, 1 low)\n\n- high: crash in x"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
    }

    #[tokio::test]
    async fn rejects_report_without_verdict_line() {
        // A report that doesn't open with the verdict line is rejected — an
        // unparseable verdict counts as FINDINGS (fail closed), and the write
        // must not land so `finish` can never mistake it for a PASS.
        let (tool, _dir) = tool_in_temp();
        for content in [
            "# Review\n\nno findings",
            "no findings",
            "## Verdict: maybe\n\nunclear",
        ] {
            let result = tool
                .execute(serde_json::json!({
                    "path": "bad.md",
                    "content": content
                }))
                .await;
            assert!(!result.success, "content {content:?} must be rejected");
            assert!(
                result.output.contains("## Verdict"),
                "error names the verdict requirement: {}",
                result.output
            );
        }
        // Nothing was written.
        assert!(!tool.reviews_dir.join("bad.md").exists());
    }

    #[tokio::test]
    async fn verdict_rejection_names_received_first_line() {
        // Backlog 5b46674d: the verdict-line rejection must show the expected
        // format verbatim AND quote the received first line, so a reviewer can
        // fix its report in ONE retry instead of guessing (live-observed
        // 2026-12-30: two reviewers died report-less after repeated verdict
        // rejections — the static error gave them nothing to correct against).
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "my-review.md",
                "content": "# Review\n\nAll good."
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("## Verdict: PASS"),
            "the rejection must show the expected PASS format verbatim: {}",
            result.output
        );
        assert!(
            result.output.contains("## Verdict: FINDINGS"),
            "the rejection must show the FINDINGS format verbatim: {}",
            result.output
        );
        assert!(
            result.output.contains("Received first line: \"# Review\""),
            "the rejection must quote the received first line: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn rejects_path_traversal() {
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "../evil.md",
                "content": "## Verdict: PASS\nmalicious"
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("bare filename") || result.output.contains("traversal"),
            "expected traversal/bare-filename rejection, got: {}",
            result.output
        );
        // Nothing was written outside the reviews dir.
        assert!(!tool.reviews_dir.parent().unwrap().join("evil.md").exists());
    }

    #[tokio::test]
    async fn rejects_sub_path() {
        // A sub-path like "2026/04/report.md" is rejected — only bare
        // filenames are accepted (sub-paths would require creating
        // intermediate dirs before the containment check, a side effect
        // outside .coding/reviews/ for a traversal input).
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "2026/04/report.md",
                "content": "## Verdict: PASS\nx"
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("bare filename"),
            "got: {}",
            result.output
        );
        // No intermediate dir was created.
        assert!(!tool.reviews_dir.join("2026").exists());
    }

    #[tokio::test]
    async fn rejects_absolute_path() {
        let (tool, _dir) = tool_in_temp();
        let abs = if cfg!(windows) {
            "C:\\Windows\\evil.md"
        } else {
            "/etc/evil.md"
        };
        let result = tool
            .execute(serde_json::json!({
                "path": abs,
                "content": "## Verdict: PASS\nx"
            }))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("relative"), "got: {}", result.output);
    }

    #[tokio::test]
    async fn rejects_empty_content() {
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "x.md",
                "content": "   "
            }))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("non-empty"));
    }

    #[tokio::test]
    async fn append_creates_missing_file_and_enforces_verdict() {
        // Append to a MISSING file is the create path: the verdict contract
        // must be enforced (fail closed) exactly like overwrite.
        let (tool, _dir) = tool_in_temp();
        let bad = tool
            .execute(serde_json::json!({
                "path": "chunked.md",
                "content": "no verdict here",
                "mode": "append"
            }))
            .await;
        assert!(
            !bad.success,
            "append-create without verdict must be rejected"
        );
        assert!(bad.output.contains("## Verdict"), "got: {}", bad.output);
        assert!(!tool.reviews_dir.join("chunked.md").exists());

        // With a verdict, append-create lands and the file holds the content —
        // at the normalized (today-prefixed) name, backlog 5b46674d.
        let ok = tool
            .execute(serde_json::json!({
                "path": "chunked.md",
                "content": "## Verdict: PASS\n\nsummary line",
                "mode": "append"
            }))
            .await;
        assert!(ok.success, "output: {}", ok.output);
        let expected = format!("{}-chunked.md", today_prefix());
        let content = std::fs::read_to_string(tool.reviews_dir.join(&expected)).unwrap();
        assert_eq!(content, "## Verdict: PASS\n\nsummary line");
    }

    #[tokio::test]
    async fn append_to_existing_concatenates_without_verdict() {
        // The chunked protocol: first call writes verdict + summary, follow-up
        // calls append finding sections WITHOUT repeating the verdict line.
        let (tool, _dir) = tool_in_temp();
        let first = tool
            .execute(serde_json::json!({
                "path": "chunked.md",
                "content": "## Verdict: FINDINGS (1 high)\n\nsummary"
            }))
            .await;
        assert!(first.success, "output: {}", first.output);
        // The overwrite created the file at the normalized (today-prefixed)
        // name, backlog 5b46674d; the appends below reuse the bare name and
        // must resolve onto that same file.
        let expected = format!("{}-chunked.md", today_prefix());
        let result = tool
            .execute(serde_json::json!({
                "path": "chunked.md",
                "content": "\n### Finding 1\n\ndetail body",
                "mode": "append"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("Appended"));
        let content = std::fs::read_to_string(tool.reviews_dir.join(&expected)).unwrap();
        assert_eq!(
            content,
            "## Verdict: FINDINGS (1 high)\n\nsummary\n\n### Finding 1\n\ndetail body"
        );

        // A chunk that DOES carry a verdict line is harmless — it's just body
        // text mid-file; the first line of the FILE is still the verdict.
        let again = tool
            .execute(serde_json::json!({
                "path": "chunked.md",
                "content": "\n## Verdict: PASS\ntext",
                "mode": "append"
            }))
            .await;
        assert!(again.success, "output: {}", again.output);
    }

    #[tokio::test]
    async fn append_with_stale_name_lands_on_normalized_file() {
        // Backlog 5b46674d: the chunked protocol must survive date
        // normalization — a reviewer that reuses its ORIGINAL (stale-dated)
        // path for follow-up appends must still land on the normalized file,
        // never on a second file.
        let (tool, _dir) = tool_in_temp();
        let today = today_prefix();
        let first = tool
            .execute(serde_json::json!({
                "path": "2026-12-23-security-review.md",
                "content": "## Verdict: PASS\n\nPart one."
            }))
            .await;
        assert!(first.success, "output: {}", first.output);
        let append = tool
            .execute(serde_json::json!({
                "path": "2026-12-23-security-review.md",
                "content": "\n### Detail\n\nPart two.",
                "mode": "append"
            }))
            .await;
        assert!(append.success, "output: {}", append.output);
        let expected = format!("{today}-security-review.md");
        let content =
            std::fs::read_to_string(tool.reviews_dir.join(&expected)).unwrap_or_default();
        assert!(
            content.contains("Part one.") && content.contains("Part two."),
            "the append must land on the normalized file: {content:?}"
        );
        let files: Vec<_> = std::fs::read_dir(&tool.reviews_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            files,
            vec![expected],
            "exactly one report file may exist: {files:?}"
        );
    }

    #[tokio::test]
    async fn append_after_date_rollover_finds_dated_variant() {
        // Review LOW 1 (backlog 5b46674d): a bare-named report written before
        // a UTC-midnight rollover lives at <old-date>-name; a follow-up
        // append with the same bare name must find that dated variant (the
        // unique <any-date>-<stripped-name> file) instead of falling to the
        // create path and rejecting a legitimate verdict-less chunk.
        // Simulated by seeding a differently-dated variant directly.
        let (tool, _dir) = tool_in_temp();
        std::fs::write(
            tool.reviews_dir.join("2020-01-01-my-review.md"),
            "## Verdict: PASS\n\nPart one.",
        )
        .unwrap();
        let result = tool
            .execute(serde_json::json!({
                "path": "my-review.md",
                "content": "\n### Detail\n\nPart two.",
                "mode": "append"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        let content =
            std::fs::read_to_string(tool.reviews_dir.join("2020-01-01-my-review.md")).unwrap();
        assert!(
            content.contains("Part one.") && content.contains("Part two."),
            "the append must land on the dated variant: {content:?}"
        );
        // Exactly one file — no second report created.
        let files: Vec<_> = std::fs::read_dir(&tool.reviews_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            files,
            vec!["2020-01-01-my-review.md".to_string()],
            "exactly one report file may exist: {files:?}"
        );
    }

    #[tokio::test]
    async fn append_to_empty_existing_file_takes_the_create_path() {
        // A pre-existing 0-byte / whitespace-only file (e.g. from an
        // interrupted earlier attempt) is the create seam: the verdict
        // contract must be enforced on the chunk, and the file ends up
        // holding exactly that chunk.
        let (tool, _dir) = tool_in_temp();
        for (name, seed) in [("empty.md", ""), ("blank.md", "\n\n")] {
            std::fs::write(tool.reviews_dir.join(name), seed).unwrap();

            // Verdict-less chunk → rejected, the file keeps its (empty) body.
            let bad = tool
                .execute(serde_json::json!({
                    "path": name,
                    "content": "no verdict here",
                    "mode": "append"
                }))
                .await;
            assert!(!bad.success, "{name}: verdict-less chunk must be rejected");
            assert_eq!(
                std::fs::read_to_string(tool.reviews_dir.join(name)).unwrap(),
                seed,
                "{name}: file untouched after rejection"
            );

            // Verdict chunk lands verbatim (the whitespace seed is replaced).
            let ok = tool
                .execute(serde_json::json!({
                    "path": name,
                    "content": "## Verdict: PASS\n\nchunk body",
                    "mode": "append"
                }))
                .await;
            assert!(ok.success, "{name}: output: {}", ok.output);
            assert_eq!(
                std::fs::read_to_string(tool.reviews_dir.join(name)).unwrap(),
                "## Verdict: PASS\n\nchunk body",
                "{name}: create path replaces the empty seed"
            );
        }
    }

    #[tokio::test]
    async fn append_rejects_missing_verdict_on_existing_file_without_one() {
        // Defense in depth: a report file that somehow lacks a verdict (older
        // tool version, manual edit) must never be grown — it could never
        // reach `finish` as a PASS.
        let (tool, _dir) = tool_in_temp();
        std::fs::write(tool.reviews_dir.join("orphan.md"), "# Review\n\nno verdict").unwrap();
        let result = tool
            .execute(serde_json::json!({
                "path": "orphan.md",
                "content": "## Verdict: PASS\nchunk",
                "mode": "append"
            }))
            .await;
        assert!(!result.success, "got: {}", result.output);
        assert!(
            result.output.contains("does not open with a"),
            "got: {}",
            result.output
        );
        // The file is untouched.
        let content = std::fs::read_to_string(tool.reviews_dir.join("orphan.md")).unwrap();
        assert_eq!(content, "# Review\n\nno verdict");
    }

    #[tokio::test]
    async fn append_mode_still_rejects_traversal_and_sub_path() {
        // The sandbox guards hold identically in append mode.
        let (tool, _dir) = tool_in_temp();
        for path in ["../evil.md", "2026/04/report.md", "C:\\Windows\\evil.md"] {
            let result = tool
                .execute(serde_json::json!({
                    "path": path,
                    "content": "## Verdict: PASS\nx",
                    "mode": "append"
                }))
                .await;
            assert!(
                !result.success,
                "path {path} must be rejected in append mode"
            );
        }
        // Nothing was written outside the reviews dir / no subdirs created.
        assert!(!tool.reviews_dir.parent().unwrap().join("evil.md").exists());
        assert!(!tool.reviews_dir.join("2026").exists());
    }

    #[tokio::test]
    async fn rejects_invalid_mode_value() {
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "x.md",
                "content": "## Verdict: PASS\nx",
                "mode": "truncate"
            }))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("overwrite"),
            "got: {}",
            result.output
        );
        assert!(!tool.reviews_dir.join("x.md").exists());
    }

    #[tokio::test]
    async fn rejects_empty_path() {
        let (tool, _dir) = tool_in_temp();
        let result = tool
            .execute(serde_json::json!({
                "path": "",
                "content": "x"
            }))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("non-empty 'path'"));
    }

    #[test]
    fn name_and_category() {
        let (tool, _dir) = tool_in_temp();
        assert_eq!(tool.name(), "write_review_report");
        assert_eq!(tool.category(), ToolCategory::Agent);
        assert_eq!(tool.safety(), SafetyLevel::AutoRun);
    }
}
