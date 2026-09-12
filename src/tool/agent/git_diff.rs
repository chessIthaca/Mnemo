// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `git_diff` — a read-only view of all uncommitted changes.
//!
//! Runs `git diff HEAD --stat`, `git diff HEAD`, and `git status --short` in
//! the project root and concatenates them into a single result. This is the
//! read-only reviewer subagent's only way to see what changed (it has no
//! `shell`/`git` tool). `AutoRun` — it only reads git state, never mutates it.
//!
//! Mirrors [`GitTool::run_git`](super::git::GitTool) for process spawning
//! (Windows `CREATE_NO_WINDOW`).
//!
//! **Output-cap EXCEPTION:** unlike every other tool, `git_diff` does NOT run
//! its result through [`cap_tool_output`](super::cap_tool_output) — a
//! truncated diff could hide part of the changes under review, and the
//! reviewer must always see the FULL uncommitted diff. Do not re-add capping
//! here (user-mandated exception to the 100 KiB tool-output cap).

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

use crate::provider::ToolSchema;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// The `git_diff` tool — a read-only diff of all uncommitted changes.
pub struct GitDiffTool {
    project_root: PathBuf,
}

impl GitDiffTool {
    /// Create the tool bound to the given project root (where the `.git` dir lives).
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    /// Run a single git subcommand in the project root, returning its combined
    /// stdout/stderr + exit code. Mirrors `GitTool::run_git` (Windows
    /// `CREATE_NO_WINDOW`, no shell — discrete argv).
    async fn run_git(&self, args: &[&str]) -> (String, i32, bool) {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(&self.project_root);
        // Kill the child process if the future is dropped (e.g. an Interrupt
        // during tool execution drops the pinned dispatch future). Without
        // this, a cancelled git command would be orphaned. Mirrors git.rs.
        cmd.kill_on_drop(true);

        // On Windows, suppress the console window that would otherwise pop up
        // when spawning a child process from a GUI app (CREATE_NO_WINDOW).
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        match cmd.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let code = output.status.code().unwrap_or(-1);
                let success = output.status.success();
                let combined = if stderr.is_empty() {
                    stdout
                } else if stdout.is_empty() {
                    stderr
                } else {
                    format!("{stdout}\n[stderr]\n{stderr}")
                };
                (combined, code, success)
            }
            Err(e) => (format!("failed to run git: {e}"), -1, false),
        }
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "git_diff",
            "Read-only view of ALL uncommitted changes in the working tree. Runs \
             `git diff HEAD --stat` (a per-file summary), `git diff HEAD` (the full \
             unified diff), and `git status --short` (untracked + modified files), \
             concatenated into one result. Auto-run — it never mutates git state. \
             Use this to review what changed before writing a review report. \
             NOTE: the output is NEVER truncated — the full diff is always returned \
             (this tool is the exception to the tool-output cap).",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Read-only: only inspects git state, never mutates it.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        // Three read-only git invocations, concatenated. Each is a discrete
        // argv (no shell), so there's no injection surface.
        let (stat, stat_code, stat_ok) = self.run_git(&["diff", "HEAD", "--stat"]).await;
        let (full, full_code, full_ok) = self.run_git(&["diff", "HEAD"]).await;
        let (status, status_code, status_ok) = self.run_git(&["status", "--short"]).await;

        let combined = format!(
            "$ git diff HEAD --stat\n{stat}\n[exit code: {stat_code}]\n\n\
             $ git diff HEAD\n{full}\n[exit code: {full_code}]\n\n\
             $ git status --short\n{status}\n[exit code: {status_code}]"
        );
        // NOTE: intentionally NOT capped — a truncated diff could hide part
        // of the changes under review. This is the one tool exempt from
        // cap_tool_output (user-mandated exception; see the module doc).
        let success = stat_ok && full_ok && status_ok;
        ToolResult {
            success,
            output: combined,
            data: Some(json!({
                "stat_exit_code": stat_code,
                "diff_exit_code": full_code,
                "status_exit_code": status_code,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: git is an external, battle-tested tool — these tests do NOT spawn
    // git subprocesses. Only the tool's static surface (name/category/safety)
    // is covered here; the diff-output behavior is git's own, not re-verified.

    #[test]
    fn name_and_category() {
        let tool = GitDiffTool::new("/tmp");
        assert_eq!(tool.name(), "git_diff");
        assert_eq!(tool.category(), ToolCategory::Agent);
        assert_eq!(tool.safety(), SafetyLevel::AutoRun);
    }
}
