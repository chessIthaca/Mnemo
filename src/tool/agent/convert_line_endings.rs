// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `convert_line_endings` — convert a file's line endings to LF or CRLF in place.
//!
//! This exists so the agent can fix line-ending style **without a shell
//! invocation** (e.g. a PowerShell `(Get-Content …) -join` dance or
//! `unix2dos`): shell commands always require approval and are easy to get
//! wrong on quoting/encoding. The tool reuses the shared
//! [`normalize_line_endings`] helper (which also collapses lone `\r`), so a
//! mixed-ending file comes out uniformly LF or CRLF.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::line_endings::normalize_line_endings;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Arguments for `convert_line_endings`.
#[derive(Debug, Deserialize)]
struct ConvertLineEndingsArgs {
    path: String,
    to: String,
}

/// The `convert_line_endings` tool.
pub struct ConvertLineEndingsTool {
    sandbox: Sandbox,
}

impl ConvertLineEndingsTool {
    /// Create the tool, confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Parse the `to` argument into the target line-ending string.
///
/// Case-insensitive (`"lf"`, `"LF"`, `"crlf"`, `"CRLF"`) so a sloppy model
/// call still works; anything else is an error naming the valid values.
fn parse_target(to: &str) -> std::result::Result<&'static str, String> {
    match to.to_ascii_lowercase().as_str() {
        "lf" => Ok("\n"),
        "crlf" => Ok("\r\n"),
        other => Err(format!(
            "invalid `to` value '{other}': expected \"lf\" or \"crlf\""
        )),
    }
}

#[async_trait]
impl Tool for ConvertLineEndingsTool {
    fn name(&self) -> &str {
        "convert_line_endings"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "convert_line_endings",
            "Convert a file's line endings to LF or CRLF in place. Use this instead of a shell \
             command to fix line-ending style (e.g. before committing, or to satisfy a repo's \
             .gitattributes). Mixed endings (including lone CR) are normalized to the target. \
             No-op when the file already uses the target style. UTF-8 text files only.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the project root."
                    },
                    "to": {
                        "type": "string",
                        "enum": ["lf", "crlf"],
                        "description": "Target line-ending style: \"lf\" (Unix, \\n) or \"crlf\" (Windows, \\r\\n)."
                    }
                },
                "required": ["path", "to"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    // No `approval_preview` override (consistent with file_append): a
    // whole-file line-ending churn diff is pure noise — every line "changes" —
    // and the tool's result already reports the exact before/after counts.

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: ConvertLineEndingsArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let target = match parse_target(&args.to) {
            Ok(t) => t,
            Err(e) => return ToolResult::error(e),
        };

        // Offload the blocking validate + read + convert + write onto the
        // blocking pool (Perf H1), mirroring file_write/file_append. The whole
        // body runs in one closure so the error ordering (validate → exists →
        // read → write) is preserved.
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || {
            let path = Path::new(&args.path);

            // Validate through the sandbox, then refuse protected targets —
            // the `file_edit` pattern, NOT the creation ladder
            // (`validate_for_write`): this tool only rewrites EXISTING files,
            // so the ladder's mkdir-parents step would create directories for
            // a path the tool then rejects as missing (review L3 filesystem
            // litter on an error path).
            let validated = match sandbox.validate(path) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("path validation failed: {e}")),
            };
            if let Err(e) = sandbox.refuse_if_protected(&validated) {
                return ToolResult::error(format!("{e}"));
            }

            // The tool rewrites an existing file in place; there is nothing to
            // convert on a missing path (unlike file_write/file_append, which
            // create files).
            if !validated.is_file() {
                return ToolResult::error(format!(
                    "'{}' does not exist or is not a file",
                    args.path
                ));
            }

            // UTF-8 only, consistent with the other file tools: converting a
            // binary file's 0x0D 0x0A byte pairs would corrupt it, so refuse
            // with a clear error instead.
            let content = match std::fs::read_to_string(&validated) {
                Ok(c) => c,
                Err(e) => {
                    return ToolResult::error(format!(
                        "failed to read '{}' as UTF-8 (binary files are not supported): {e}",
                        args.path
                    ))
                }
            };

            // Count the current endings. Lone \r is counted separately: it is
            // not the target style for either conversion and
            // normalize_line_endings collapses it too.
            let crlf = content.matches("\r\n").count();
            let bare_lf = content.matches('\n').count() - crlf;
            let lone_cr = content.matches('\r').count() - crlf;
            let total = crlf + bare_lf + lone_cr;

            let target_name = if target == "\r\n" { "CRLF" } else { "LF" };
            let already = if target == "\r\n" {
                bare_lf == 0 && lone_cr == 0
            } else {
                crlf == 0 && lone_cr == 0
            };

            // No-op fast paths: nothing to convert, or everything already in
            // the target style. Report WITHOUT rewriting so the file's mtime
            // (and any watchers) are untouched.
            if total == 0 {
                return ToolResult::success(format!(
                    "'{}' has no line endings — nothing to convert",
                    args.path
                ));
            }
            if already {
                return ToolResult::success(format!(
                    "'{}' is already {target_name} ({total} line endings) — no changes made",
                    args.path
                ));
            }

            let converted = normalize_line_endings(&content, target);
            if let Err(e) = std::fs::write(&validated, &converted) {
                return ToolResult::error(format!("failed to write '{}': {e}", args.path));
            }

            ToolResult::success(format!(
                "converted '{}' to {target_name} ({crlf} CRLF + {bare_lf} bare LF + {lone_cr} lone CR → {total} {target_name} line endings)",
                args.path
            ))
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("convert line endings task failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &Path) -> ConvertLineEndingsTool {
        ConvertLineEndingsTool::new(Sandbox::new(dir).unwrap())
    }

    #[tokio::test]
    async fn converts_crlf_to_lf() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line one\r\nline two\r\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "lf"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("converted"));
        assert!(result.output.contains("2 CRLF"));
        let content = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(content, "line one\nline two\n");
        assert!(!content.contains('\r'));
    }

    #[tokio::test]
    async fn converts_lf_to_crlf() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line one\nline two\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "crlf"})).await;
        assert!(result.success, "{}", result.output);
        let content = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(content, "line one\r\nline two\r\n");
        // No bare LF should remain.
        assert_eq!(
            content.matches("\r\n").count(),
            content.matches('\n').count()
        );
    }

    #[tokio::test]
    async fn already_target_is_noop() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "lf"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("already LF"));
        assert!(result.output.contains("no changes"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\nb\n"
        );
    }

    #[tokio::test]
    async fn mixed_endings_normalize_to_target() {
        // Mixed CRLF + bare LF + lone CR must all collapse to the target.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\r\nb\nc\rd\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "lf"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("1 lone CR"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\nb\nc\nd\n"
        );
    }

    #[tokio::test]
    async fn mixed_endings_normalize_to_crlf() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\r\nb\nc\rd\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "crlf"})).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\r\nb\r\nc\r\nd\r\n"
        );
    }

    #[tokio::test]
    async fn missing_file_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "nope.txt", "to": "lf"})).await;
        assert!(!result.success);
        assert!(result.output.contains("does not exist"));
    }

    #[tokio::test]
    async fn invalid_to_value_errors_with_valid_values() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "unix"})).await;
        assert!(!result.success);
        assert!(result.output.contains("lf"));
        assert!(result.output.contains("crlf"));
        // The file must be untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\n"
        );
    }

    #[tokio::test]
    async fn to_value_is_case_insensitive() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "CRLF"})).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\r\n"
        );
    }

    #[tokio::test]
    async fn missing_nested_path_does_not_create_directories() {
        // Review L3 regression: routing through the creation ladder
        // (validate_for_write) mkdir'd the parent directories before the tool
        // rejected the missing file — filesystem litter on an error path. The
        // tool now uses the file_edit-style validate + refuse_if_protected
        // (no mkdir), so a missing nested path errors without side effects.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "no/such/dir/f.txt", "to": "lf"}))
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(
            !dir.path().join("no").exists(),
            "no directories may be created for a missing file"
        );
    }

    #[tokio::test]
    async fn refuses_protected_coding_state() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/memory.db"), "data\r\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": ".coding/memory.db", "to": "lf"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("protected"));
        // Untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".coding/memory.db")).unwrap(),
            "data\r\n"
        );
    }

    #[tokio::test]
    async fn non_utf8_file_errors_cleanly_and_is_untouched() {
        let dir = tempdir().unwrap();
        let bytes = [0xFF, 0xFE, 0x0D, 0x0A, 0x80];
        std::fs::write(dir.path().join("bin.dat"), bytes).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "bin.dat", "to": "lf"})).await;
        assert!(!result.success);
        assert!(result.output.contains("UTF-8"));
        // The binary content must be byte-identical afterwards.
        assert_eq!(std::fs::read(dir.path().join("bin.dat")).unwrap(), bytes);
    }

    #[tokio::test]
    async fn empty_file_noops() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "crlf"})).await;
        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("nothing to convert"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn conversion_is_idempotent() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
        let tool = make_tool(dir.path());
        let first = tool.execute(json!({"path": "a.txt", "to": "crlf"})).await;
        assert!(first.success, "{}", first.output);
        let second = tool.execute(json!({"path": "a.txt", "to": "crlf"})).await;
        assert!(second.success, "{}", second.output);
        assert!(second.output.contains("already CRLF"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\r\nb\r\n"
        );
    }

    #[tokio::test]
    async fn preserves_content_beyond_line_endings() {
        // Trailing content without a final newline and multi-byte chars must
        // survive the round trip unchanged apart from the endings.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "日本語\r\ntail-no-newline").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt", "to": "lf"})).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "日本語\ntail-no-newline"
        );
    }
}
