// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Agent tools — the coding tools.
//!
//! - `file_read` (auto-run; routes path through sandbox)
//! - `read_files` (auto-run; batch read of multiple files, each with an
//!   optional per-file line range — cuts multi-file exploration to one call)
//! - `file_edit` (string-replace; generates diff for approval via `similar`)
//! - `file_write` (create/overwrite; new-file preview for approval)
//! - `file_append` (append to a file; for large files in chunks)
//! - `convert_line_endings` (CRLF↔LF conversion in place, without a shell
//!   invocation)
//! - `shell` (platform-aware: PowerShell on Windows, sh elsewhere)
//! - `expand_result` (auto-run; serves back a tool result that was archived
//!   for size — by row id, or a keyword search over the archive. Always
//!   registered, because archive rows can outlive the flag that created them)
//! - `search` (grep + glob)
//! - `search_read` (grep + glob, then auto-read the top-N matched files in
//!   one call — collapses the search→read round-trip)
//! - `git` (status, diff, log, commit)
//! - `image_*` tools (analyze an image file via the configured vision model;
//!   also hosts the shared vision plumbing for the attachment fallback)
//! - `spawn_agent` (start a background agent on a task, in parallel)
//! - `list_models` (list configured endpoints + their models, for spawn_agent's
//!   `model` parameter)
//! - `graph_search` / `graph_context` / `graph_impact` / `graph_path`
//!   (read-only queries over the CodeGraph knowledge graph, when enabled)

pub mod codegraph;
pub mod convert_line_endings;
pub mod edit_ops;
pub mod expand_result;
pub mod file_append;
pub mod file_edit;
pub mod file_read;
pub mod file_write;
pub mod git;
pub mod git_diff;
pub mod git_read;
pub mod git_read_tool;
pub mod image_tools;
pub mod line_endings;
pub mod list_models;
pub mod load_tools;
pub mod multi_edit;
pub mod optimizer;
pub mod output_compactor;
pub mod pattern;
pub mod read_cache;
pub mod read_files;
pub mod sandbox;
pub mod search;
pub mod search_read;
pub mod shell;
pub mod shell_filter;
pub mod spawn_agent;
pub mod tool_contract;
pub mod web_fetch;
pub mod write_review_report;

/// Apply a byte cap to a tool output string, appending a truncation note when
/// the content was cut. Returns the (possibly truncated) text. Shared by the
/// `shell` + `git` tools (defined here so neither couples to the other).
///
/// Uses char-boundary-safe truncation: a naive `String::truncate` panics if the
/// cut point lands inside a multi-byte UTF-8 character (common in CJK/emoji
/// source comments). `truncate_to_boundary` backs up to the nearest char
/// boundary so the result is always valid UTF-8.
pub(crate) fn cap_tool_output(mut s: String) -> String {
    const DEFAULT_TOOL_OUTPUT_CAP: usize = 100 * 1024; // 100 KiB
    if s.len() > DEFAULT_TOOL_OUTPUT_CAP {
        crate::tool::agent::read_files::truncate_to_boundary(&mut s, DEFAULT_TOOL_OUTPUT_CAP);
        s.push_str("\n... (truncated: output exceeded size limit)");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_tool_output_under_cap_unchanged() {
        let s = "hello world".to_string();
        assert_eq!(cap_tool_output(s.clone()), s);
    }

    #[test]
    fn cap_tool_output_truncates_and_notes() {
        let s = "x".repeat(200_000);
        let out = cap_tool_output(s);
        assert!(out.len() <= 100 * 1024 + 64); // cap + note
        assert!(out.contains("truncated: output exceeded size limit"));
    }

    #[test]
    fn cap_tool_output_multibyte_no_panic() {
        // 200 KB of a 3-byte CJK char — the cut point lands inside a multi-byte
        // char. Must not panic and must produce valid UTF-8.
        let s = "日".repeat(70_000); // ~210 KB
        let out = cap_tool_output(s);
        // The result is valid UTF-8 (no panic, no mid-char cut).
        assert!(out.is_char_boundary(out.len()));
        assert!(out.contains("truncated: output exceeded size limit"));
        // The truncation backed up to a char boundary, so the cap is at most
        // the limit (a 3-byte char may have been dropped to stay on a boundary).
        assert!(out.len() < 210_000);
    }
}
