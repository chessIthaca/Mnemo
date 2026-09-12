// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `file_write` — create or overwrite a file (new-file preview for approval).

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::error::Result;
use crate::provider::{ApprovalPreview, ToolSchema};
use crate::tool::agent::line_endings::{detect_line_ending_path, normalize_line_endings};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Borrow helper so the append check reads the same value that is then
/// moved into the delegate.
fn args_ref(v: &serde_json::Value) -> &serde_json::Value {
    v
}

/// Arguments for `file_write`.
#[derive(Debug, Deserialize)]
pub struct FileWriteArgs {
    pub path: String,
    pub content: String,
}

/// The `file_write` tool — whole-file writes in both directions.
///
/// `mode: "append"` used to be a separate `file_append` tool. The two were one
/// decision ("put this content in that file") split by a flag, so the model
/// paid two schemas and had to choose. Append delegates to the original
/// implementation, so line-ending detection and the protected-path checks are
/// unchanged.
pub struct FileWriteTool {
    sandbox: Sandbox,
    append: crate::tool::agent::file_append::FileAppendTool,
}

impl FileWriteTool {
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            append: crate::tool::agent::file_append::FileAppendTool::new(sandbox.clone()),
            sandbox,
        }
    }

    /// Whether this call is an append. Anything other than `"append"` —
    /// including a missing or misspelled mode — is an overwrite, matching the
    /// schema default. Overwrite is the safe reading: it goes through the
    /// approval preview, so a wrong guess is shown to the user as a diff
    /// rather than silently appended.
    fn is_append(args: &serde_json::Value) -> bool {
        args.get("mode")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m.eq_ignore_ascii_case("append"))
    }
}

#[async_trait]
impl Tool for FileWriteTool {
    fn name(&self) -> &str {
        "file_write"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "file_write",
            "Write a whole file. Default mode overwrites (creating parent directories as \
             needed); mode=\"append\" adds to the end, creating the file if absent. To \
             write a large file, overwrite the first section then append the rest in \
             chunks of under ~4000 characters (large single calls risk truncation). For \
             a targeted change to an existing file, prefer file_edit.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file, relative to the project root."},
                    "content": {"type": "string", "description": "The content. Overwrites the file, or is appended to it when mode=\"append\"."},
                    "mode": {"type": "string", "enum": ["overwrite", "append"], "description": "Default \"overwrite\"."}
                },
                "required": ["path", "content"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    fn approval_preview(&self, args: &serde_json::Value) -> Option<ApprovalPreview> {
        // An append has no meaningful whole-file diff to show (the old
        // file_append offered no preview either) — fall back to the raw-args
        // approval prompt rather than rendering a misleading overwrite diff.
        if Self::is_append(args) {
            return None;
        }
        let args: FileWriteArgs = serde_json::from_value(args.clone()).ok()?;
        self.prepare_for_approval(&args).ok()
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        if Self::is_append(args_ref(&args)) {
            return self.append.execute(args).await;
        }
        let args: FileWriteArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        // (backlog 1db26c95) Restore boundary-token markers to the raw
        // tokens: the request layer escapes configured boundary tokens in
        // tool results (the serving layer strips/maps the raw token from
        // input, so the model's textual view shows a marker instead) —
        // restoring here makes a read-then-write round-trip faithful.
        let args = FileWriteArgs {
            content: crate::provider::boundary::restore_boundary_tokens(&args.content),
            ..args
        };

        // Offload the blocking validate + create-dir + line-ending detection +
        // write onto the blocking pool (Perf H1). The whole body runs inside
        // one closure so the exact error ordering (protected-check →
        // validate/create-dir → re-validate → write) is preserved.
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || {
            let path = Path::new(&args.path);

            // Validate the full target path through the sandbox's write ladder
            // (validate → creation-fallback → protected check → mkdir parents →
            // revalidate). This refuses protected paths (.coding
            // state/bookkeeping or the .git control plane) with a shared
            // message, and creates parent dirs for new nested paths.
            let validated = match sandbox.validate_for_write(path) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("path validation failed: {e}")),
            };

            // Line-ending preservation: when overwriting an existing file, detect
            // its line-ending style and normalize `content` to match so an LF
            // `content` doesn't introduce mixed endings in a CRLF file (and vice
            // versa). A brand-NEW file is pinned to LF — the repo's
            // .gitattributes declares eol=lf, so deterministic LF endings keep
            // future edits/trips away; an existing EMPTY file keeps the
            // caller's endings as-is (nothing to preserve, nothing to pin).
            let content_to_write = match detect_line_ending_path(&validated) {
                Some(le) => normalize_line_endings(&args.content, le),
                None if !validated.exists() => normalize_line_endings(&args.content, "\n"),
                None => args.content.clone(),
            };

            if let Err(e) = std::fs::write(&validated, &content_to_write) {
                return ToolResult::error(format!("failed to write '{}': {e}", args.path));
            }

            ToolResult::success(format!(
                "wrote {} ({} bytes)",
                args.path,
                content_to_write.len()
            ))
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("file write task failed: {e}")))
    }
}

/// Soft cap on approval-preview payload size (chars). Larger bodies are
/// truncated with a header so the IPC/UI path stays responsive (Phase 1 M3).
const PREVIEW_CONTENT_CHAR_BUDGET: usize = 120_000;

impl FileWriteTool {
    /// Prepare a new-file / overwrite preview for approval (no write).
    ///
    /// Uses `validate_for_creation` when the path does not exist yet so new
    /// files still get a preview. Content is normalized to the existing file's
    /// line endings when overwriting (same as `execute`).
    ///
    /// When the target already exists and is readable, returns
    /// [`ApprovalPreview::Diff`] against on-disk content so overwrites are
    /// reviewable (not a green full-body dump).
    pub fn prepare_for_approval(&self, args: &FileWriteArgs) -> Result<ApprovalPreview> {
        let path = Path::new(&args.path);
        let validated = match self.sandbox.validate(path) {
            Ok(p) => p,
            Err(_) => self.sandbox.validate_for_creation(path)?,
        };
        self.sandbox.refuse_if_protected(&validated)?;
        let content = match detect_line_ending_path(&validated) {
            Some(le) => normalize_line_endings(&args.content, le),
            // Mirror execute(): brand-new files are pinned to LF (repo
            // .gitattributes eol=lf); existing empty files stay as-is.
            None if !validated.exists() => normalize_line_endings(&args.content, "\n"),
            None => args.content.clone(),
        };
        let content = truncate_preview_text(&content, PREVIEW_CONTENT_CHAR_BUDGET);

        // Overwrite: prefer a unified diff vs existing bytes when readable.
        if validated.is_file() {
            if let Ok(old) = std::fs::read_to_string(&validated) {
                let old = truncate_preview_text(&old, PREVIEW_CONTENT_CHAR_BUDGET);
                let path_label = args.path.as_str();
                let diff = crate::tool::agent::file_edit::compute_diff(path_label, &old, &content);
                return Ok(ApprovalPreview::Diff {
                    path: validated,
                    diff: truncate_preview_text(&diff, PREVIEW_CONTENT_CHAR_BUDGET),
                });
            }
        }

        Ok(ApprovalPreview::NewFile {
            path: validated,
            content,
        })
    }
}

/// Truncate a preview string to `budget` chars, appending a clear marker.
fn truncate_preview_text(s: &str, budget: usize) -> String {
    if s.chars().count() <= budget {
        return s.to_string();
    }
    let mut out: String = s.chars().take(budget).collect();
    out.push_str("\n… [preview truncated for size] …\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &Path) -> FileWriteTool {
        FileWriteTool::new(Sandbox::new(dir).unwrap())
    }

    #[test]
    fn prepare_for_approval_new_file_is_pure() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let args = FileWriteArgs {
            path: "new.txt".into(),
            content: "hello".into(),
        };
        let preview = tool.prepare_for_approval(&args).unwrap();
        match preview {
            ApprovalPreview::NewFile { path, content } => {
                assert!(path.ends_with("new.txt"));
                assert_eq!(content, "hello");
            }
            other => panic!("expected NewFile, got {other:?}"),
        }
        assert!(
            !dir.path().join("new.txt").exists(),
            "preview must not create the file"
        );
    }

    #[test]
    fn prepare_for_approval_overwrite_returns_diff() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old line\n").unwrap();
        let tool = make_tool(dir.path());
        let args = FileWriteArgs {
            path: "a.txt".into(),
            content: "new line\n".into(),
        };
        let preview = tool.prepare_for_approval(&args).unwrap();
        match preview {
            ApprovalPreview::Diff { path, diff } => {
                assert!(path.ends_with("a.txt"));
                assert!(diff.contains("old") || diff.contains("-"));
                assert!(diff.contains("new") || diff.contains("+"));
            }
            other => panic!("expected Diff for overwrite, got {other:?}"),
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "old line\n",
            "preview must not write"
        );
    }

    #[tokio::test]
    async fn file_write_restores_boundary_markers() {
        // Backlog 1db26c95: the request layer escapes configured boundary
        // tokens in tool results to a visible marker (the serving layer
        // strips/maps the raw token, so the model's textual view shows an
        // empty string — a read-then-write round-trip silently corrupted
        // files). The write path must RESTORE the marker to the raw token
        // so the round-trip is faithful. Transport-safe: both the marker
        // and the expected raw token are built from escapes — raw
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
            .execute(json!({
                "path": "cfg.toml",
                "content": format!("stop = [\"{marker}\"]\n")
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cfg.toml")).unwrap(),
            format!("stop = [\"{endoftext}\"]\n"),
            "the marker must be restored to the raw boundary token on write"
        );
    }

    #[tokio::test]
    async fn append_mode_restores_boundary_markers() {
        // Backlog 1db26c95, review H1: mode="append" delegates to
        // FileAppendTool BEFORE this tool's restore — the append path
        // (the chunked-write path the schema steers large files onto)
        // must still restore boundary markers, or appended chunks land
        // literal markers. Transport-safe: built from escapes — raw
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
            .execute(json!({
                "path": "big.md",
                "content": format!("second {marker} chunk"),
                "mode": "append"
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("big.md")).unwrap(),
            format!("second {endoftext} chunk"),
            "append-mode markers must restore to the raw boundary token"
        );
    }

    #[test]
    fn approval_preview_trait_hook_for_write() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let preview = Tool::approval_preview(&tool, &json!({"path": "x.txt", "content": "c"}))
            .expect("preview");
        assert!(matches!(preview, ApprovalPreview::NewFile { .. }));
    }

    #[tokio::test]
    async fn writes_new_file() {
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

    /// Backlog #48: brand-new files are pinned to LF (repo .gitattributes
    /// eol=lf) even when the caller emits CRLF — deterministic endings, no
    /// mixed-ending files created.
    #[tokio::test]
    async fn new_file_written_with_lf() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "new.md", "content": "a\r\nb\r\n"}))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.md")).unwrap(),
            "a\nb\n",
            "new files must be written with LF endings"
        );
    }

    #[tokio::test]
    async fn creates_parent_dirs() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "src/deep/mod.rs", "content": "fn main(){}"}))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/deep/mod.rs")).unwrap(),
            "fn main(){}"
        );
    }

    #[tokio::test]
    async fn overwrites_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "content": "new"}))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "../escape.txt", "content": "x"}))
            .await;
        assert!(!result.success);
    }

    #[tokio::test]
    async fn refuses_to_write_memory_db() {
        // The memory DB is a live SQLite file the app holds an open connection
        // to. Writing to it via file_write could corrupt it.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/memory.db"), "data").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": ".coding/memory.db", "content": "corrupted"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("protected"));
        // The file must be unchanged.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".coding/memory.db")).unwrap(),
            "data"
        );
    }

    #[tokio::test]
    async fn refuses_to_write_bookkeeping_files() {
        // Quality M5: safety.toml, backlog.jsonl, and the plan stack/markdown
        // are model-writable bookkeeping that must only be touched by their
        // dedicated tools. file_write must refuse them even when they already
        // exist.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/plans")).unwrap();
        std::fs::write(dir.path().join(".coding/safety.toml"), "[rules]\n").unwrap();
        std::fs::write(dir.path().join(".coding/backlog.jsonl"), "[]").unwrap();
        std::fs::write(dir.path().join(".coding/plans/stack.json"), "[]").unwrap();
        std::fs::write(dir.path().join(".coding/plans/abc.md"), "# Plan\n").unwrap();
        let tool = make_tool(dir.path());
        for rel in [
            ".coding/safety.toml",
            ".coding/backlog.jsonl",
            ".coding/plans/stack.json",
            ".coding/plans/abc.md",
        ] {
            let result = tool
                .execute(json!({"path": rel, "content": "corrupted"}))
                .await;
            assert!(!result.success, "{rel} should be refused");
            assert!(result.output.contains("protected"), "{rel} refusal message");
        }
        // None of the files were changed.
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".coding/safety.toml")).unwrap(),
            "[rules]\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".coding/plans/stack.json")).unwrap(),
            "[]"
        );
    }

    #[tokio::test]
    async fn refuses_to_create_nonexistent_protected_path() {
        // The creation gap (Quality M5): a non-existent protected path must be
        // refused before file_write creates its parent dirs + the file.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        // .coding/plans/ does not exist yet.
        let result = tool
            .execute(json!({"path": ".coding/plans/stack.json", "content": "[\"fake\"]"}))
            .await;
        assert!(!result.success, "creating stack.json must be refused");
        assert!(result.output.contains("protected"));
        // The directory + file must NOT have been created.
        assert!(!dir.path().join(".coding/plans").exists());
    }

    #[tokio::test]
    async fn refuses_writing_review_reports() {
        // Review reports are authored ONLY by the reviewer's
        // write_review_report tool — the file tools must refuse every path
        // under .coding/reviews/ so the main agent cannot fabricate a report.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": ".coding/reviews/2026-04-08-review.md",
                "content": "# Review\nno findings\n"
            }))
            .await;
        assert!(!result.success, "writing a review report must be refused");
        assert!(result.output.contains("protected"));
        // The file must NOT have been created.
        assert!(!dir
            .path()
            .join(".coding/reviews/2026-04-08-review.md")
            .exists());
    }

    // ---- line-ending preservation ----

    #[tokio::test]
    async fn overwrite_crlf_file_with_lf_content_preserves_crlf() {
        // Overwriting a CRLF file with LF content must keep CRLF endings so the
        // file doesn't end up with mixed line endings.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line one\r\nline two\r\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "content": "alpha\nbeta\n"}))
            .await;
        assert!(result.success, "{}", result.output);
        let written = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(written, "alpha\r\nbeta\r\n");
        // No bare LF should remain.
        assert!(
            written.matches("\r\n").count() == written.matches('\n').count(),
            "expected all CRLF, got: {written:?}"
        );
    }

    #[tokio::test]
    async fn overwrite_lf_file_with_crlf_content_preserves_lf() {
        // Overwriting an LF file with CRLF content must keep LF endings.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line one\nline two\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "a.txt", "content": "alpha\r\nbeta\r\n"}))
            .await;
        assert!(result.success, "{}", result.output);
        let written = std::fs::read_to_string(dir.path().join("a.txt")).unwrap();
        assert_eq!(written, "alpha\nbeta\n");
        // No CRLF should be present.
        assert!(!written.contains("\r"), "expected all LF, got: {written:?}");
    }

    #[tokio::test]
    async fn new_file_lf_content_stays_verbatim() {
        // A brand-new file with LF content is written verbatim. (New files
        // are PINNED to LF per the repo's .gitattributes eol=lf policy —
        // CRLF content is normalized to LF, covered by
        // `new_file_written_with_lf` below.)
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());

        tool.execute(json!({"path": "lf.txt", "content": "a\nb\n"}))
            .await;
        assert_eq!(
            std::fs::read_to_string(dir.path().join("lf.txt")).unwrap(),
            "a\nb\n"
        );
    }

    #[tokio::test]
    async fn empty_existing_file_keeps_content_as_is() {
        // An empty existing file has no style to preserve, so the caller's
        // content is written verbatim — consistent with a brand-new file.
        // (Previously an empty file was detected as LF, silently converting
        // CRLF content to LF.)
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("empty.txt"), "").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "empty.txt", "content": "a\r\nb\r\n"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("empty.txt")).unwrap(),
            "a\r\nb\r\n"
        );
    }

    #[tokio::test]
    async fn append_mode_delegates_and_skips_the_overwrite_preview() {
        // mode:"append" is what the file_append tool used to be. Overwrite
        // stays the default for anything else, including a misspelled mode —
        // that reading is the safe one, because overwrite goes through the
        // approval diff while append does not.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let tool = FileWriteTool::new(sandbox);

        let r = tool
            .execute(json!({"path": "a.txt", "content": "one\n"}))
            .await;
        assert!(r.success, "{}", r.output);
        let r = tool
            .execute(json!({"path": "a.txt", "content": "two\n", "mode": "append"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "one\ntwo\n",
            "append adds rather than replacing"
        );

        // Default and unknown modes overwrite.
        for args in [
            json!({"path": "a.txt", "content": "fresh\n"}),
            json!({"path": "a.txt", "content": "fresh\n", "mode": "nonsense"}),
        ] {
            let r = tool.execute(args).await;
            assert!(r.success, "{}", r.output);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
                "fresh\n"
            );
        }

        // The approval preview is an overwrite diff — meaningless for an
        // append, so it is withheld there.
        assert!(tool
            .approval_preview(&json!({"path": "a.txt", "content": "x"}))
            .is_some());
        assert!(tool
            .approval_preview(&json!({"path": "a.txt", "content": "x", "mode": "append"}))
            .is_none());
    }

    #[tokio::test]
    async fn append_mode_creates_a_missing_file() {
        // The schema promises "creating the file if absent" — pinned here at
        // the file_write facade (the behavior lives in the delegated
        // FileAppendTool, tested there too).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let tool = FileWriteTool::new(sandbox);
        let r = tool
            .execute(json!({"path": "new.txt", "content": "hello\n", "mode": "append"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "hello\n"
        );
    }
}
