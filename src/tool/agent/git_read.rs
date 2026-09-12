// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Read-only git bridge tools — `git_log` and `git_show`.
//!
//! These bridge memory records (which carry `commit <hash>` pointers) to the
//! shipped code: `git_log` lists recent commits (optionally for one path),
//! `git_show` displays a commit's stat or full diff. Both are read-only by
//! construction — no subcommand dispatch, no write flags, no `args` passthrough
//! — so they are `AutoRun` (the `git` tool is `NeedsApproval` because it can
//! commit/merge/push; these two cannot).

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::provider::ToolSchema;
use crate::tool::agent::cap_tool_output;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// A commit-ish argument (`git_show`'s `commit` field): a hex hash
/// (`[0-9a-f]{7,40}`), a branch/tag name, or a revision expression like
/// `HEAD~2` / `main..feat`. Rejects anything starting with `-` (option
/// injection — `--output=x` must never reach git) and anything containing
/// whitespace or shell metacharacters.
fn validate_commitish(commit: &str) -> Result<(), String> {
    if commit.starts_with('-') {
        return Err(format!(
            "commit '{commit}' rejected — a commit-ish may not start with '-' \
             (option injection)"
        ));
    }
    if commit
        .chars()
        .any(|c| c.is_whitespace() || "|&;<>`$(){}*?\"'\\".contains(c))
    {
        return Err(format!(
            "commit '{commit}' rejected — whitespace and shell metacharacters \
             are not allowed in a commit-ish"
        ));
    }
    Ok(())
}

/// A repo-relative path argument (`git_log`'s `path` field): must be
/// relative, must not escape the repo (`..`), and must not start with `-`.
fn validate_repo_path(path: &str) -> Result<(), String> {
    if path.starts_with('-') {
        return Err(format!(
            "path '{path}' rejected — a path may not start with '-' (option injection)"
        ));
    }
    if std::path::Path::new(path).is_absolute() {
        return Err(format!("path '{path}' rejected — must be repo-relative"));
    }
    for component in std::path::Path::new(path).components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(format!(
                "path '{path}' rejected — '..' escapes the repository"
            ));
        }
    }
    Ok(())
}

/// Run a git command in `project_root`, returning the combined output with a
/// `$ git …` echo line and the exit code — the same shape as the `git` tool's
/// output (capped to the tool-output cap; read-only commands only).
async fn run_git_read(project_root: &PathBuf, args: &[&str]) -> ToolResult {
    let command_display = {
        let mut parts: Vec<&str> = vec!["git"];
        parts.extend_from_slice(args);
        parts.join(" ")
    };
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(project_root)
        .kill_on_drop(true)
        .stdin(Stdio::null());
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
                stdout.clone()
            } else if stdout.is_empty() {
                stderr.clone()
            } else {
                format!("{stdout}\n[stderr]\n{stderr}")
            };
            let full = format!("$ {command_display}\n{combined}\n[exit code: {code}]");
            ToolResult {
                success,
                output: cap_tool_output(full),
                data: Some(json!({"exit_code": code, "stdout": stdout, "stderr": stderr})),
            }
        }
        Err(e) => ToolResult::error(format!("$ {command_display}\nfailed to run git: {e}")),
    }
}

/// Arguments for `git_log`.
#[derive(Debug, Deserialize)]
struct GitLogArgs {
    /// Optional repo-relative path to filter the log to.
    #[serde(default)]
    path: Option<String>,
    /// Optional commit count (1..=100, default 20).
    #[serde(default)]
    limit: Option<usize>,
}

/// The `git_log` tool — list recent commits (optionally for one path).
pub struct GitLogTool {
    project_root: PathBuf,
}

impl GitLogTool {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

#[async_trait]
impl Tool for GitLogTool {
    fn name(&self) -> &str {
        "git_log"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "git_log",
            "List recent commits (one line each, newest first) — optionally filtered to \
             one repo-relative path. The read-only bridge from memory records (which carry \
             commit pointers) to shipped code. Auto-run: read-only by construction.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Optional repo-relative path (e.g. \"src/memory/mod.rs\") — only commits touching that path are listed."},
                    "limit": {"type": "integer", "description": "Optional commit count (1–100, default 20)."}
                }
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: GitLogArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        let limit = args.limit.unwrap_or(20);
        if !(1..=100).contains(&limit) {
            return ToolResult::error(format!("limit must be between 1 and 100 (got {limit})"));
        }
        let mut argv: Vec<String> = vec!["log".into(), "--oneline".into(), format!("-{limit}")];
        if let Some(path) = &args.path {
            if let Err(e) = validate_repo_path(path) {
                return ToolResult::error(e);
            }
            argv.push("--".into());
            argv.push(path.clone());
        }
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        run_git_read(&self.project_root, &argv).await
    }
}

/// Arguments for `git_show`.
#[derive(Debug, Deserialize)]
struct GitShowArgs {
    /// The commit-ish to show (hash, branch, tag, or `HEAD~N`).
    commit: String,
    /// Show the stat summary instead of the full diff (default true).
    #[serde(default = "default_true")]
    stat: bool,
}

fn default_true() -> bool {
    true
}

/// The `git_show` tool — display one commit (stat or full diff).
pub struct GitShowTool {
    project_root: PathBuf,
}

impl GitShowTool {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }
}

#[async_trait]
impl Tool for GitShowTool {
    fn name(&self) -> &str {
        "git_show"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "git_show",
            "Display one commit: `git show --stat <commit>` (default) or the full diff \
             (`stat: false`). The read-only bridge from memory records (which carry \
             `commit <hash>` pointers) to the actual shipped change. Auto-run: read-only \
             by construction.",
            json!({
                "type": "object",
                "properties": {
                    "commit": {"type": "string", "description": "The commit-ish to show: a hex hash (7–40 chars), branch/tag name, or a revision like HEAD~2."},
                    "stat": {"type": "boolean", "description": "Show the stat summary instead of the full diff (default true)."}
                },
                "required": ["commit"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: GitShowArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        if let Err(e) = validate_commitish(&args.commit) {
            return ToolResult::error(e);
        }
        let argv: Vec<&str> = if args.stat {
            vec!["show", "--stat", &args.commit]
        } else {
            vec!["show", &args.commit]
        };
        run_git_read(&self.project_root, &argv).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // NOTE: git is an external, battle-tested tool — these tests do NOT spawn
    // git subprocesses. They cover the git_log/git_show TOOLS' own validation
    // logic only (commit-ish + repo-path validation, limit bounds) — the guards
    // that fire BEFORE any git invocation. No repo fixture, no git subprocess.

    #[tokio::test]
    async fn git_log_limit_bounds_and_validates() {
        // The limit + path validations fire before git is ever invoked, so no
        // repo is needed (git is an external tool we don't re-verify here).
        let dir = tempdir().unwrap();
        let tool = GitLogTool::new(dir.path());
        // Out-of-range limit errors.
        let result = tool.execute(json!({"limit": 0})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("between 1 and 100"),
            "{}",
            result.output
        );
        // Path validation: '..' escapes the repo.
        let result = tool.execute(json!({"path": "../secret"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("escapes the repository"),
            "{}",
            result.output
        );
        // Option injection rejected.
        let result = tool.execute(json!({"path": "--output=x"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("option injection"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn git_show_rejects_option_injection_and_metacharacters() {
        // validate_commitish fires before git is ever invoked, so no repo is
        // needed (git is an external tool we don't re-verify here).
        let dir = tempdir().unwrap();
        let tool = GitShowTool::new(dir.path());
        // A leading '-' is option injection — never executed.
        let result = tool.execute(json!({"commit": "--output=x"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("option injection"),
            "{}",
            result.output
        );
        // Shell metacharacters are rejected (no shell is involved, but the
        // guard keeps the contract explicit).
        let result = tool.execute(json!({"commit": "HEAD; rm -rf /"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("metacharacters"),
            "{}",
            result.output
        );
    }
}
