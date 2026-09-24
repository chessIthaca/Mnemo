// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `multi_edit` — atomic multi-file edits with ONE combined diff preview.
//!
//! `files: [{path, ops}]` — every file's ops are applied IN MEMORY, in order,
//! and only after ALL files prepare successfully is anything written: a
//! failing op aborts the whole call with NO write anywhere (every file stays
//! byte-identical). `ops` is the same polymorphic array `file_edit` carries —
//! compact line ops (`i`/`b`/`d`/`r` verbs, ranges, payloads) and anchor
//! objects (plan 2e27f896, engine in [`crate::tool::agent::edit_ops`]) — and
//! the approval prompt renders ONE combined diff covering every changed file
//! ([`ApprovalPreview::MultiDiff`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::error::Result;
use crate::provider::{ApprovalPreview, ToolSchema};
use crate::tool::agent::edit_ops::{apply_ops, compute_diff, parse_ops, validate_op_items};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Soft cap on the combined approval-preview diff size (chars). Mirrors
/// file_edit / file_write (M3).
const PREVIEW_DIFF_CHAR_BUDGET: usize = 120_000;

/// Arguments for `multi_edit`.
#[derive(Debug, Clone, Deserialize)]
pub struct MultiEditArgs {
    /// The files to edit, each with its own ops array.
    #[serde(default)]
    pub files: Option<Vec<MultiEditFile>>,
}

/// One file entry in a `multi_edit` call.
#[derive(Debug, Clone, Deserialize)]
pub struct MultiEditFile {
    /// Path to the file, relative to the project root.
    pub path: String,
    /// The ops array — the same forms as `file_edit`'s `ops` (compact line-op
    /// strings and anchor objects). The legacy `edits` key is accepted too.
    #[serde(default, alias = "edits")]
    pub ops: Option<Vec<serde_json::Value>>,
}

/// One file's fully prepared edit — still in memory; nothing is written until
/// every entry in the call has prepared successfully (the atomicity seam).
struct PreparedFile {
    /// The path as the caller named it (messages + success output).
    path: String,
    /// The sandbox-validated target (what the write phase writes).
    validated: PathBuf,
    /// This file's own unified diff (concatenated into the combined one).
    diff: String,
    new_content: String,
    notes: Vec<String>,
}

/// The `multi_edit` tool.
pub struct MultiEditTool {
    sandbox: Sandbox,
}

impl MultiEditTool {
    /// Create the tool bound to `sandbox` (every path check routes through it).
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Prefix a file entry's error with the tool name, the entry index, and the
/// path — the engine's own messages name the op index inside it, so every
/// rejection is traceable to one entry and one op. `NotFound` (the drift
/// class) is preserved so the stale-read steering gate still arms.
fn file_error(idx: usize, path: &str, source: crate::error::Error) -> crate::error::Error {
    match source {
        crate::error::Error::NotFound(msg) => {
            crate::error::Error::NotFound(format!("multi_edit: files[{idx}] '{path}': {msg}"))
        }
        other => crate::error::Error::InvalidInput(format!(
            "multi_edit: files[{idx}] '{path}': {other}"
        )),
    }
}

/// Validate the call's shape and parse every entry's ops: `files` non-empty,
/// every path non-empty, every `ops` non-empty and parseable with its anchor
/// items validated by the shared engine. Returns the parsed ops per entry.
/// Path uniqueness is NOT checked here — raw spellings are not the identity
/// (`a.txt` and `./a.txt` are one file), so the duplicate guard runs on the
/// sandbox-validated targets in `prepare_files`.
fn validate_files(args: &MultiEditArgs) -> Result<Vec<(String, Vec<crate::tool::agent::edit_ops::EditOp>)>> {
    let files = args.files.as_deref().ok_or_else(|| {
        crate::error::Error::InvalidInput(
            "files is required — provide at least one {path, ops} entry".into(),
        )
    })?;
    if files.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "files is empty — provide at least one {path, ops} entry".into(),
        ));
    }
    let mut out = Vec::with_capacity(files.len());
    for (idx, file) in files.iter().enumerate() {
        if file.path.trim().is_empty() {
            return Err(crate::error::Error::InvalidInput(format!(
                "files[{idx}].path is empty — every entry needs a file path"
            )));
        }
        let values = file.ops.as_deref().ok_or_else(|| {
            crate::error::Error::InvalidInput(format!(
                "files[{idx}] '{}': ops is required — provide at least one op",
                file.path
            ))
        })?;
        if values.is_empty() {
            return Err(crate::error::Error::InvalidInput(format!(
                "files[{idx}] '{}': ops is empty — provide at least one op",
                file.path
            )));
        }
        let ops = parse_ops(values).map_err(|e| file_error(idx, &file.path, e))?;
        validate_op_items(&ops).map_err(|e| file_error(idx, &file.path, e))?;
        out.push((file.path.clone(), ops));
    }
    Ok(out)
}

/// Prepare every file's edit IN MEMORY: validate the path through the
/// sandbox, refuse duplicate/directory/protected targets, read the file,
/// apply its ops, and diff. NO WRITE happens here — any error aborts the
/// whole call before a single byte reaches disk (the atomicity contract).
///
/// The duplicate guard keys on the sandbox-VALIDATED target, not the raw
/// spelling: `a.txt` and `./a.txt` are the same file, and two entries for one
/// file prepare against the same original, then write in order with the LAST
/// entry winning — silently dropping the first entry's edit while the
/// combined preview claimed both applied (review L2).
fn prepare_files(sandbox: &Sandbox, args: &MultiEditArgs) -> Result<Vec<PreparedFile>> {
    let entries = validate_files(args)?;
    let mut prepared = Vec::with_capacity(entries.len());
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(entries.len());
    for (idx, (path, ops)) in entries.into_iter().enumerate() {
        let validated = sandbox
            .validate(Path::new(&path))
            .map_err(|e| file_error(idx, &path, e))?;
        if !seen.insert(validated.clone()) {
            return Err(crate::error::Error::InvalidInput(format!(
                "files[{idx}] '{path}' appears twice — merge its ops into one entry"
            )));
        }
        if validated.is_dir() {
            return Err(crate::error::Error::InvalidInput(format!(
                "multi_edit: files[{idx}] '{path}' is a directory, not a file — \
                 read_files on it returns its listing"
            )));
        }
        sandbox
            .refuse_if_protected(&validated)
            .map_err(|e| file_error(idx, &path, e))?;
        let content = match std::fs::read_to_string(&validated) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(crate::error::Error::NotFound(format!(
                    "multi_edit: files[{idx}] '{path}' does not exist — multi_edit edits \
                     existing files; use file_write to create it first"
                )))
            }
            Err(e) => return Err(file_error(idx, &path, crate::error::Error::Io(e))),
        };
        let (new_content, notes) =
            apply_ops(&path, &content, &ops, false).map_err(|e| file_error(idx, &path, e))?;
        let diff = compute_diff(&path, &content, &new_content);
        prepared.push(PreparedFile {
            path,
            validated,
            diff,
            new_content,
            notes,
        });
    }
    Ok(prepared)
}

/// The combined diff: each file's own unified diff concatenated (every one
/// carries its own `--- path` / `+++ path` header, so the result renders as
/// one multi-file diff).
fn combined_diff(prepared: &[PreparedFile]) -> String {
    let mut out = String::new();
    for file in prepared {
        out.push_str(&file.diff);
    }
    out
}

/// Truncate a preview diff to the soft cap (the shared file_edit/file_write
/// shape — a preview is advisory, the write carries the full change).
fn truncate_preview(diff: String) -> String {
    if diff.chars().count() > PREVIEW_DIFF_CHAR_BUDGET {
        let mut d: String = diff.chars().take(PREVIEW_DIFF_CHAR_BUDGET).collect();
        d.push_str("\n… [preview truncated for size] …\n");
        d
    } else {
        diff
    }
}

/// The write-phase failure report: which files fully landed, the FAILED file
/// called out in its own clause (a `std::fs::write` truncates its target
/// before writing, so that file may be truncated or partial — it is never
/// "byte-identical"), and only the tail AFTER it claimed untouched. Slicing
/// the untouched suffix at the failed index instead would list the failed
/// file under "not written" (review L3).
fn write_failure_message(
    prepared: &[PreparedFile],
    failed_idx: usize,
    written: &[String],
    err: &std::io::Error,
) -> String {
    let failed = prepared[failed_idx].path.as_str();
    let not_written: Vec<&str> = prepared[failed_idx + 1..]
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    format!(
        "multi_edit: failed to write '{failed}': {err} — written: {written:?}; \
         '{failed}' may be truncated/partial; not written (byte-identical): \
         {not_written:?}"
    )
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        "multi_edit"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "multi_edit",
            &format!(
                "{} Apply ops to SEVERAL files ATOMICALLY: nothing is written until \
                 every file applies cleanly, so a failing op leaves ALL files untouched. \
                 Entries are {{path, ops}} — the ops array is file_edit's (compact line \
                 ops + anchor objects, applied IN ORDER, line numbers as left by the \
                 preceding ops). One combined diff is returned. Use file_edit for a \
                 single file.",
                tool_contract::contract(
                    "`files`",
                    "{\"files\":[{\"path\":\"a.rs\",\"ops\":[\"d202-205\"]}]}"
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "files": {"type": "array", "description": "The files to edit, each with its own ops. Nothing is written until ALL files succeed — a failing op aborts the whole call with no write anywhere.", "items": {"type": "object", "properties": {
                        "path": {"type": "string", "description": "Path to the file, relative to the project root."},
                        "ops": {"type": "array", "description": "The ops to apply IN ORDER — the same forms as file_edit's ops array: compact line-op strings (e.g. 'i101:text' insert after line 101, 'b101:text' insert before, 'd202-205' delete, 'd100-' to EOF, 'r102:text' replace; {i|b|d|r}{N|N-M|N-}[:payload], 1-indexed) or anchor objects {old_string, new_string, count?, fuzzy_whitespace?} (an anchor must match exactly once unless it sets count). Line numbers refer to the content as left by the preceding ops.", "items": {"anyOf": [{"type": "string"}, {"type": "object", "properties": {"old_string": {"type": "string", "description": "The exact text to find (EOL-agnostic)."}, "new_string": {"type": "string", "description": "The replacement text."}, "count": {"type": "integer", "description": "Max occurrences to replace (default 1)."}, "fuzzy_whitespace": {"type": "boolean", "description": "Whitespace-tolerant match (default false)."}}, "required": ["old_string", "new_string"]}]}}
                    }, "required": ["path", "ops"]}}
                },
                "required": ["files"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    fn approval_preview(&self, args: &serde_json::Value) -> Option<ApprovalPreview> {
        let args: MultiEditArgs = serde_json::from_value(args.clone()).ok()?;
        self.prepare_for_approval(&args).ok()
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: MultiEditArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => {
                return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(
                    self.name(),
                    &e,
                ))
            }
        };
        let sandbox = self.sandbox.clone();
        // Offload the blocking validate + read + write onto the blocking pool
        // (Perf H1, the file_edit shape). Every prepare runs before the first
        // write: that ordering IS the atomicity guarantee.
        tokio::task::spawn_blocking(move || {
            let prepared = match prepare_files(&sandbox, &args) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("{e}")),
            };
            // Every file prepared — only now write. A write-phase failure (an
            // I/O error, never an op failure) reports exactly which files were
            // written, calls the failed file out on its own, and claims only
            // the untouched tail byte-identical.
            let mut written: Vec<String> = Vec::with_capacity(prepared.len());
            for (idx, file) in prepared.iter().enumerate() {
                if let Err(e) = std::fs::write(&file.validated, &file.new_content) {
                    return ToolResult::error(write_failure_message(
                        &prepared,
                        idx,
                        &written,
                        &e,
                    ));
                }
                written.push(file.path.clone());
            }
            let mut output = String::new();
            for file in &prepared {
                output.push_str(&format!("edited {}\n", file.path));
            }
            for file in &prepared {
                for note in &file.notes {
                    output.push_str(&format!("{}: {note}\n", file.path));
                }
            }
            ToolResult {
                success: true,
                output: output.trim_end().to_string(),
                data: Some(json!({
                    "diff": combined_diff(&prepared),
                    "paths": prepared.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
                })),
            }
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("multi edit task failed: {e}")))
    }
}

impl MultiEditTool {
    /// Prepare the call for approval (without applying): every file is
    /// validated, read, and spliced in memory, and the caller gets ONE
    /// combined diff covering every changed file. Does not write to disk.
    pub fn prepare_for_approval(&self, args: &MultiEditArgs) -> Result<ApprovalPreview> {
        let prepared = prepare_files(&self.sandbox, args)?;
        Ok(ApprovalPreview::MultiDiff {
            paths: prepared.iter().map(|f| f.validated.clone()).collect(),
            diff: truncate_preview(combined_diff(&prepared)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::agent::sandbox::Sandbox;
    use serde_json::json;
    use tempfile::tempdir;

    fn make_tool(root: &Path) -> MultiEditTool {
        MultiEditTool::new(Sandbox::new(root).unwrap())
    }

    #[tokio::test]
    async fn applies_every_file_and_returns_one_combined_diff() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x\ny\n").unwrap();
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt", "ops": ["d2"]},
                    {"path": "b.txt", "ops": [{"old_string": "y", "new_string": "Y"}]},
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\nc\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "x\nY\n"
        );
        assert!(result.output.contains("edited a.txt"), "{}", result.output);
        assert!(result.output.contains("edited b.txt"), "{}", result.output);
        let data = result.data.expect("result data");
        let diff = data["diff"].as_str().expect("combined diff");
        assert!(diff.contains("--- a.txt"), "{diff}");
        assert!(diff.contains("--- b.txt"), "{diff}");
        assert_eq!(data["paths"].as_array().map(|p| p.len()), Some(2));
    }

    /// THE atomicity regression: the second file's op fails, so the FIRST
    /// file's edit — which prepared fine — must not be written either. Every
    /// file stays byte-identical on disk.
    #[tokio::test]
    async fn a_failing_op_leaves_every_file_byte_identical() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let a_before = "a\nb\nc\n";
        let b_before = "x\ny\n";
        std::fs::write(dir.path().join("a.txt"), a_before).unwrap();
        std::fs::write(dir.path().join("b.txt"), b_before).unwrap();
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt", "ops": ["d1"]},
                    {"path": "b.txt", "ops": ["d9"]},
                ]
            }))
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(result.output.contains("files[1] 'b.txt'"), "{}", result.output);
        assert!(
            result.output.contains("past the end of the file"),
            "{}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            a_before
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            b_before
        );
    }

    #[tokio::test]
    async fn crlf_files_keep_their_own_style() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\r\nb\r\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x\ny\n").unwrap();
        let result = tool
            .execute(json!({
                "files": [
                    {"path": "a.txt", "ops": ["i1:X"]},
                    {"path": "b.txt", "ops": ["i1:X"]},
                ]
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\r\nX\r\nb\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "x\nX\ny\n"
        );
    }

    #[tokio::test]
    async fn rejects_bad_shapes_before_any_write() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
        for (args, needle) in [
            (json!({"files": []}), "files is empty"),
            // An exact duplicate AND an equivalent spelling of the same file
            // (`./a.txt` canonicalizes to `a.txt`): both are one target, and
            // letting them through would silently drop the first entry's
            // edit (last write wins — review L2).
            (
                json!({"files": [
                    {"path": "a.txt", "ops": ["d1"]},
                    {"path": "a.txt", "ops": ["d2"]},
                ]}),
                "appears twice",
            ),
            (
                json!({"files": [
                    {"path": "a.txt", "ops": ["d1"]},
                    {"path": "./a.txt", "ops": ["d2"]},
                ]}),
                "appears twice",
            ),
            (json!({"files": [{"path": "a.txt", "ops": []}]}), "ops is empty"),
            (json!({"files": [{"path": "a.txt"}]}), "ops is required"),
            (json!({}), "files is required"),
        ] {
            let result = tool.execute(args).await;
            assert!(!result.success, "{}", result.output);
            assert!(result.output.contains(needle), "{}", result.output);
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\nb\n"
        );
    }

    #[tokio::test]
    async fn refuses_protected_paths_and_escapes_without_writing() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        // `.coding/knowledge/**` is protected state (memory_amend is its
        // sanctioned writer) — the file tools must refuse it.
        std::fs::create_dir_all(dir.path().join(".coding").join("knowledge")).unwrap();
        let protected = dir.path().join(".coding").join("knowledge").join("note.md");
        std::fs::write(&protected, "keep\n").unwrap();
        let result = tool
            .execute(json!({"files": [{"path": ".coding/knowledge/note.md", "ops": ["d1"]}]}))
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(result.output.contains("protected"), "{}", result.output);
        assert_eq!(std::fs::read_to_string(&protected).unwrap(), "keep\n");
        let result = tool
            .execute(json!({"files": [{"path": "../escape.txt", "ops": ["d1"]}]}))
            .await;
        assert!(!result.success, "{}", result.output);
    }

    #[tokio::test]
    async fn missing_file_steers_to_file_write() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"files": [{"path": "nope.txt", "ops": ["d1"]}]}))
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(result.output.contains("does not exist"), "{}", result.output);
        assert!(result.output.contains("file_write"), "{}", result.output);
    }

    #[test]
    fn approval_preview_carries_every_path_and_one_diff() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        std::fs::write(dir.path().join("a.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "x\ny\n").unwrap();
        let args: MultiEditArgs = serde_json::from_value(json!({
            "files": [
                {"path": "a.txt", "ops": ["d1"]},
                {"path": "b.txt", "ops": [{"old_string": "y", "new_string": "Y"}]},
            ]
        }))
        .expect("args parse");
        match tool.prepare_for_approval(&args).expect("preview") {
            ApprovalPreview::MultiDiff { paths, diff } => {
                assert_eq!(paths.len(), 2, "{paths:?}");
                assert!(diff.contains("--- a.txt"), "{diff}");
                assert!(diff.contains("--- b.txt"), "{diff}");
            }
            other => panic!("expected MultiDiff, got {other:?}"),
        }
        // The preview is pure — nothing was written.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\nb\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "x\ny\n"
        );
    }

    /// L3 regression: the write-phase failure report must call the FAILED
    /// file out on its own (a partial write may have truncated it) and claim
    /// only the tail AFTER it byte-identical. Slicing at the failed index —
    /// the old `prepared[written.len()..]` — listed the failed file itself
    /// under "not written (byte-identical)".
    #[test]
    fn write_failure_message_excludes_the_failed_file_from_not_written() {
        let file = |path: &str| PreparedFile {
            path: path.to_string(),
            validated: PathBuf::from(path),
            diff: String::new(),
            new_content: String::new(),
            notes: Vec::new(),
        };
        let prepared = vec![file("a.txt"), file("b.txt"), file("c.txt")];
        let written = vec!["a.txt".to_string()];
        let err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let msg = write_failure_message(&prepared, 1, &written, &err);
        assert!(msg.contains("'b.txt' may be truncated/partial"), "{msg}");
        assert!(
            msg.contains(r#"not written (byte-identical): ["c.txt"]"#),
            "{msg}"
        );
        assert!(
            !msg.contains(r#"not written (byte-identical): ["b.txt""#),
            "the failed file must not be claimed byte-identical: {msg}"
        );
        assert!(msg.contains(r#"written: ["a.txt"]"#), "{msg}");
        assert!(msg.contains("denied"), "{msg}");
    }

    #[test]
    fn schema_and_metadata() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        assert_eq!(tool.name(), "multi_edit");
        assert_eq!(tool.category(), ToolCategory::Agent);
        assert_eq!(tool.safety(), SafetyLevel::NeedsApproval);
        let schema = tool.schema();
        assert_eq!(schema.name, "multi_edit");
        assert!(
            schema.description.contains("`files`"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("file_edit"),
            "{}",
            schema.description
        );
        assert_eq!(schema.parameters["required"], json!(["files"]));
        let entry = &schema.parameters["properties"]["files"]["items"];
        assert_eq!(entry["required"], json!(["path", "ops"]));
        assert!(
            entry["properties"]["ops"]["items"]["anyOf"].is_array(),
            "{entry}"
        );
    }
}
