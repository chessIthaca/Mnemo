// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `file_read` — read a file (auto-run, no approval).
//!
//! Routes the path through the sandbox before any I/O.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::read_files::invalid_args_error;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Default maximum number of lines returned when the caller doesn't specify
/// `max_lines`. Prevents unbounded conversation bloat from reading huge files.
const DEFAULT_MAX_LINES: usize = 500;

/// Default maximum output size in bytes (~100 KB). If the numbered output
/// exceeds this, it is truncated to fit and a note is appended. This is a
/// safety net on top of the line cap — a file with a few very long lines
/// could still blow up the conversation.
const DEFAULT_MAX_BYTES: usize = 102_400;

/// Arguments for `file_read`.
#[derive(Debug, Deserialize)]
struct FileReadArgs {
    path: String,
    /// Optional 1-indexed start line.
    #[serde(default)]
    start_line: Option<usize>,
    /// Optional number of lines to read.
    #[serde(default)]
    max_lines: Option<usize>,
}

/// The `file_read` tool.
pub struct FileReadTool {
    sandbox: Sandbox,
}

impl FileReadTool {
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for FileReadTool {
    fn name(&self) -> &str {
        "file_read"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "file_read",
            "Read one file as text with line numbers. Must be a file, not a directory \
             (use search to list). Capped at 500 lines / 100 KB by default, with a \
             truncation note when the file is larger.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the project root."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "1-indexed start line (optional)."
                    },
                    "max_lines": {
                        "type": "integer",
                        "description": "Max lines to read (optional, default 500)."
                    }
                },
                "required": ["path"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: FileReadArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            Err(e) => {
                // A `files` key means the model reached for `read_files`'s
                // shape while calling the singular tool — point it at the
                // sibling so it can self-correct instead of guessing.
                let hint = if args.get("files").is_some() {
                    "If you meant to read several files in one call, use \
                     read_files (plural): it takes a \"files\" array of \
                     {path, start_line?, max_lines?} specs."
                } else {
                    ""
                };
                return ToolResult::error(invalid_args_error(&e, &args, hint));
            }
        };

        // Offload the blocking path validation + file read + line/byte-cap
        // formatting onto tokio's blocking pool (Perf H1). The sandbox is
        // cheaply `Clone` (one `PathBuf`); the clone + the owned args move
        // into the closure, which returns the fully-built `ToolResult`. This
        // preserves the exact validation/error/cap/ordering behavior — only
        // the *thread* the work runs on changes.
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || {
            let validated = match sandbox.validate(Path::new(&args.path)) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("path validation failed: {e}")),
            };

            // A directory must not fall through to read_to_string: on Windows
            // opening a dir with ReadFile yields ERROR_ACCESS_DENIED —
            // "Access is denied. (os error 5)" — which reads like a sandbox/
            // ACL problem instead of a wrong path KIND (backlog #89).
            if validated.is_dir() {
                return ToolResult::error(format!(
                    "'{}' is a directory, not a file — read_files on '{}' returns its listing",
                    args.path, args.path
                ));
            }

            let content = match std::fs::read_to_string(&validated) {
                Ok(c) => c,
                Err(e) => return ToolResult::error(format!("failed to read '{}': {e}", args.path)),
            };

            // Apply line range if specified, and prefix with line numbers so the
            // model can reference exact lines in file_edit.
            let total_lines = content.lines().count();
            let start = args.start_line.unwrap_or(1).saturating_sub(1);
            // Cap the number of lines at DEFAULT_MAX_LINES unless the caller
            // explicitly requested fewer. This prevents unbounded output from
            // reading a huge file without a range.
            let max = args
                .max_lines
                .unwrap_or(DEFAULT_MAX_LINES)
                .min(DEFAULT_MAX_LINES);

            let numbered: Vec<String> = content
                .lines()
                .skip(start)
                .take(max)
                .enumerate()
                .map(|(i, line)| format!("{:>4}: {}", start + i + 1, line))
                .collect();

            let showed = numbered.len();
            let mut output = numbered.join("\n");

            // Byte-cap safety net: if the output is still huge (e.g. a few very
            // long lines), truncate to DEFAULT_MAX_BYTES and append a note.
            let truncated_by_bytes = if output.len() > DEFAULT_MAX_BYTES {
                output.truncate(DEFAULT_MAX_BYTES);
                true
            } else {
                false
            };

            // Append a truncation note only when the *default* line cap was the
            // limiting factor — not when the caller explicitly requested a range
            // (they got exactly what they asked for). The byte cap always notes.
            let available_from_start = total_lines.saturating_sub(start);
            let default_capped = args.max_lines.is_none() && showed < available_from_start;
            if truncated_by_bytes || default_capped {
                output.push_str(&format!(
                    "\n... (truncated: {total_lines} lines total, showed {showed})"
                ));
            }

            ToolResult::success(output)
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("file read task failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &Path) -> FileReadTool {
        FileReadTool::new(Sandbox::new(dir).unwrap())
    }

    #[tokio::test]
    async fn reads_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "a.txt"})).await;
        assert!(result.success);
        // Output now includes line numbers.
        assert_eq!(result.output, "   1: hello\n   2: world");
    }

    #[tokio::test]
    async fn reads_with_line_range() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line1\nline2\nline3\nline4").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "start_line": 2, "max_lines": 2}))
            .await;
        assert!(result.success);
        // Line numbers reflect the original file positions.
        assert_eq!(result.output, "   2: line2\n   3: line3");
    }

    #[tokio::test]
    async fn missing_file_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "nope.txt"})).await;
        assert!(!result.success);
        assert!(result.output.contains("failed to read"));
    }

    #[tokio::test]
    async fn reading_a_directory_path_returns_directory_hint() {
        // Regression (backlog #89): file_read on a directory path returned
        // the raw OS error — on Windows "Access is denied. (os error 5)"
        // (opening a directory with ReadFile yields ERROR_ACCESS_DENIED),
        // which reads like a sandbox/ACL problem instead of a wrong path
        // KIND. The error must name the problem and point at a listing tool.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plans")).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "plans"})).await;
        assert!(!result.success, "expected error, got: {}", result.output);
        assert!(
            result.output.contains("is a directory, not a file"),
            "must name the path kind: {}",
            result.output
        );
        assert!(
            result.output.contains("read_files"),
            "must point at read_files for listing: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let outside = dir.path().parent().unwrap().join("outside_read.txt");
        std::fs::write(&outside, "secret").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "../outside_read.txt"})).await;
        assert!(!result.success);
        assert!(result.output.contains("path validation failed"));
    }

    #[tokio::test]
    async fn invalid_args_error() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        // Missing required "path" field.
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(result.output.contains("invalid arguments"));
    }

    #[tokio::test]
    async fn wrong_shape_files_array_hints_read_files() {
        // Regression (backlog #65): a model called `file_read` with
        // `read_files`-shaped args ({files: [...]}). The old error was only
        // "missing field `path`" — serde silently ignored the unknown `files`
        // key. The fix must name the received keys and point at read_files.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "files": [{"path": "a.txt", "start_line": 1, "end_line": 2}]
            }))
            .await;
        assert!(!result.success, "expected arg error, got success");
        assert!(
            result.output.contains("invalid arguments"),
            "{}",
            result.output
        );
        assert!(result.output.contains("received keys"), "{}", result.output);
        assert!(result.output.contains("files"), "{}", result.output);
        assert!(result.output.contains("read_files"), "{}", result.output);
    }

    #[tokio::test]
    async fn truncates_large_file_by_line_count() {
        // A file with more than DEFAULT_MAX_LINES (500) lines should be
        // truncated, and a note appended.
        let dir = tempdir().unwrap();
        let content: String = (0..3000)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("big.txt"), content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "big.txt"})).await;
        assert!(result.success);
        // The truncation note must be present.
        assert!(
            result.output.contains("truncated"),
            "expected truncation note, got: {}",
            &result.output[..result.output.len().min(200)]
        );
        assert!(result.output.contains("3000 lines total"));
        assert!(result.output.contains("showed 500"));
        // The last visible line should be line 500 (1-indexed). With {:>4}
        // formatting, 500 is 3 chars (one leading space).
        assert!(result.output.contains(" 500: line499"));
        // Line 501 should NOT be in the output (it was truncated).
        assert!(!result.output.contains("line500"));
    }

    #[tokio::test]
    async fn truncates_very_long_lines_by_bytes() {
        // A file with a few very long lines that exceed the byte cap.
        let dir = tempdir().unwrap();
        // One line of ~150 KB — exceeds DEFAULT_MAX_BYTES (100 KB).
        let long_line = "x".repeat(150_000);
        std::fs::write(dir.path().join("long.txt"), &long_line).unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "long.txt"})).await;
        assert!(result.success);
        assert!(
            result.output.contains("truncated"),
            "expected byte truncation note"
        );
        // The output (excluding the note) should be under the byte cap.
        let note_start = result.output.find("\n... (truncated").unwrap();
        assert!(note_start <= DEFAULT_MAX_BYTES);
    }

    #[tokio::test]
    async fn small_file_not_truncated() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("small.txt"), "a\nb\nc").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"path": "small.txt"})).await;
        assert!(result.success);
        assert!(
            !result.output.contains("truncated"),
            "small file should not be truncated"
        );
    }

    #[tokio::test]
    async fn explicit_max_lines_below_default_respected() {
        // If the caller asks for fewer lines than the default, that's honored
        // and no truncation note is added (they got exactly what they asked for).
        let dir = tempdir().unwrap();
        let content: String = (0..10)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(dir.path().join("ten.txt"), content).unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "ten.txt", "max_lines": 3}))
            .await;
        assert!(result.success);
        assert!(!result.output.contains("truncated"));
        assert!(result.output.contains("line0"));
        assert!(result.output.contains("line2"));
        assert!(!result.output.contains("line3"));
    }
}
