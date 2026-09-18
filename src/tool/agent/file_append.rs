// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `file_append` — append content to an existing file (or create it).
//!
//! For large files, the model can write the first chunk with `file_write`
//! then append subsequent chunks with `file_append`. This avoids truncation
//! from massive single tool calls.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::line_endings::{detect_line_ending_path, normalize_line_endings};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Arguments for `file_append`.
#[derive(Debug, Deserialize)]
struct FileAppendArgs {
    path: String,
    content: String,
}

/// The `file_append` tool.
pub struct FileAppendTool {
    sandbox: Sandbox,
}

impl FileAppendTool {
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for FileAppendTool {
    fn name(&self) -> &str {
        "file_append"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "file_append",
            "Append content to the end of a file. Creates the file if it doesn't exist. \
             Use this to write large files in chunks: write the first section with file_write, \
             then append subsequent sections with file_append. Each call should be  \
             under 4000 characters (large single calls risk truncation).",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to the project root."
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to append to the end of the file. Keep each call under ~4000 chars (large single calls risk truncation); for large files, write the first section with file_write then append subsequent sections."
                    }
                },
                "required": ["path", "content"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: FileAppendArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        // (backlog 1db26c95) Restore boundary-token markers to the raw
        // tokens: the request layer escapes configured boundary tokens in
        // tool results (the serving layer strips/maps the raw token from
        // input, so the model's textual view shows a marker instead) —
        // restoring here makes a read-then-write round-trip faithful on
        // the chunked-write path (file_write mode="append" delegates here
        // BEFORE its own restore; review H1).
        let args = FileAppendArgs {
            content: crate::provider::boundary::restore_boundary_tokens(&args.content),
            ..args
        };

        // Offload the blocking validate + line-ending detection + append +
        // metadata onto the blocking pool (Perf H1). The whole body runs in
        // one closure so the exact error ordering (validate → protected →
        // open → write → metadata) is preserved.
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || {
            let path = Path::new(&args.path);

            // Validate through the sandbox's write ladder (validate →
            // creation-fallback → protected check → mkdir parents → revalidate).
            // This refuses protected paths (.coding state/bookkeeping or the
            // .git control plane) with a shared message, and creates parent
            // dirs for new nested paths
            // (consistency with file_write — file_append previously did NOT
            // create parents).
            let validated = match sandbox.validate_for_write(path) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("path validation failed: {e}")),
            };

            // Line-ending preservation: when appending to an existing file, detect
            // its line-ending style and normalize `content` to match so an LF
            // `content` doesn't introduce mixed endings in a CRLF file (and vice
            // versa). A brand-NEW file is pinned to LF (repo .gitattributes
            // eol=lf — deterministic endings, no mixed files later); an
            // existing EMPTY file keeps the caller's endings as-is. Uses a
            // bounded 8 KB prefix read (not the whole file) since file_append
            // is documented for writing large files in chunks.
            let content_to_write = match detect_line_ending_path(&validated) {
                Some(le) => normalize_line_endings(&args.content, le),
                None if !validated.exists() => normalize_line_endings(&args.content, "\n"),
                None => args.content.clone(),
            };

            // Append (or create) using OpenOptions.
            use std::fs::OpenOptions;
            let mut file = match OpenOptions::new()
                .create(true)
                .append(true)
                .open(&validated)
            {
                Ok(f) => f,
                Err(e) => return ToolResult::error(format!("failed to open '{}': {e}", args.path)),
            };

            use std::io::Write;
            if let Err(e) = file.write_all(content_to_write.as_bytes()) {
                return ToolResult::error(format!("failed to append to '{}': {e}", args.path));
            }

            // Report the new file size.
            let new_size = std::fs::metadata(&validated).map(|m| m.len()).unwrap_or(0);

            ToolResult::success(format!(
                "appended {} bytes to '{}' (file is now {} bytes)",
                content_to_write.len(),
                args.path,
                new_size
            ))
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("file append task failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &Path) -> FileAppendTool {
        FileAppendTool::new(Sandbox::new(dir).unwrap())
    }

    #[tokio::test]
    async fn creates_new_file() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "new.txt", "content": "hello"}))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello"
        );
    }

    /// Backlog #48: a file CREATED by append is pinned to LF (repo
    /// .gitattributes eol=lf), mirroring file_write's new-file behavior.
    #[tokio::test]
    async fn new_file_created_with_lf() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "fresh.md", "content": "a\r\nb\r\n"}))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("fresh.md")).unwrap(),
            "a\nb\n",
            "files created by append must use LF endings"
        );
    }

    #[tokio::test]
    async fn appends_to_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "content": " world"}))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn appends_in_chunks() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        // Write first chunk.
        tool.execute(json!({"path": "big.md", "content": "# Title\n\n"}))
            .await;
        // Append second chunk.
        tool.execute(json!({"path": "big.md", "content": "## Section 1\n\nContent here.\n"}))
            .await;
        // Append third chunk.
        tool.execute(json!({"path": "big.md", "content": "## Section 2\n\nMore content.\n"}))
            .await;
        let content = std::fs::read_to_string(dir.path().join("big.md")).unwrap();
        assert!(content.contains("# Title"));
        assert!(content.contains("Section 1"));
        assert!(content.contains("Section 2"));
    }

    #[tokio::test]
    async fn refuses_memory_db() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/memory.db"), "data").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": ".coding/memory.db", "content": "x"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("protected"));
    }

    // ---- line-ending preservation ----

    #[tokio::test]
    async fn append_lf_to_crlf_file_normalizes() {
        // Appending LF content to a CRLF file must convert to CRLF so the file
        // doesn't end up with mixed line endings.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line one\r\nline two\r\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "content": "line three\nline four\n"}))
            .await;
        assert!(result.success, "{}", result.output);
        let written = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(
            written,
            "line one\r\nline two\r\nline three\r\nline four\r\n"
        );
        // No bare LF should remain.
        assert!(
            written.matches("\r\n").count() == written.matches('\n').count(),
            "expected all CRLF, got: {written:?}"
        );
    }

    #[tokio::test]
    async fn append_crlf_to_lf_file_normalizes() {
        // Appending CRLF content to an LF file must convert to LF.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line one\nline two\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "content": "line three\r\nline four\r\n"}))
            .await;
        assert!(result.success, "{}", result.output);
        let written = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(written, "line one\nline two\nline three\nline four\n");
        // No CR should be present.
        assert!(!written.contains('\r'), "expected all LF, got: {written:?}");
    }

    #[tokio::test]
    async fn append_restores_boundary_markers() {
        // Backlog 1db26c95, review H1: appended content carrying a
        // boundary-token marker must land the RAW token on disk — the
        // chunked-write path (file_write mode="append" delegates here
        // BEFORE its own restore) round-trips token-containing files
        // faithfully. Transport-safe: built from escapes — raw
        // angle-bracket tag text is stripped in text transports.
        let endoftext = "\u{3c}|endoftext|\u{3e}";
        let marker = format!(
            "\u{27e6}raw:{}\u{27e7}",
            endoftext
                .chars()
                .map(|c| format!("\\u{:04x}", c as u32))
                .collect::<String>()
        );
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "chunk.md", "content": format!("a {marker} b")}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("chunk.md")).unwrap(),
            format!("a {endoftext} b"),
            "appended markers must restore to the raw boundary token"
        );
    }
}
