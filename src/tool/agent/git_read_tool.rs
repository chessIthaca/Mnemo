// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `git_read` — the single read-only view into git.
//!
//! Replaces `git_log`, `git_show` and `git_diff`, which were three tools for
//! one decision ("look at git history / the working tree") distinguished only
//! by which command they ran. The model had to pick between them with no
//! principled rule, and paid three schemas to do it.
//!
//! Deliberately does NOT absorb the write-capable `git` tool. That split is
//! load-bearing: these four are `AutoRun` (they never prompt, and they stay
//! visible in the read-only Planning and Complete states), while `git` is
//! `NeedsApproval` and hidden there. Folding them together would either make
//! reads prompt or make writes visible where no plan exists.
//!
//! Each op delegates to the original implementation, so validation, output
//! caps and the never-truncate-a-diff rule are unchanged.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::provider::ToolSchema;
use crate::tool::agent::git_diff::GitDiffTool;
use crate::tool::agent::git_read::{GitLogTool, GitShowTool, GitStatusTool};
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Which read to perform. Only `op` is inspected here; the per-op arguments
/// are parsed by the delegate that handles them.
#[derive(Debug, Deserialize)]
struct GitReadArgs {
    op: String,
}

/// The unified read-only git tool.
pub struct GitReadTool {
    log: GitLogTool,
    show: GitShowTool,
    diff: GitDiffTool,
    status: GitStatusTool,
}

impl GitReadTool {
    /// Create the tool bound to the project root (where the `.git` dir lives).
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        let root: PathBuf = project_root.into();
        Self {
            log: GitLogTool::new(root.clone()),
            show: GitShowTool::new(root.clone()),
            diff: GitDiffTool::new(root.clone()),
            status: GitStatusTool::new(root),
        }
    }
}

#[async_trait]
impl Tool for GitReadTool {
    fn name(&self) -> &str {
        "git_read"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "git_read",
            "Always pass `op` — e.g. {\"op\":\"log\"}. No zero-argument form; \
             on an 'op is required' error rewrite the full call, do not \
             resend the empty shape. If you catch yourself emitting \
             git_read with no op, stop — write the op first, then the \
             call. Read-only view into git. op=\"diff\": ALL uncommitted \
             changes (stat + full diff + untracked), never truncated — use \
             this to review what changed. op=\"log\": recent commits, \
             newest first, optionally for one path. op=\"show\": one commit, \
             stat by default. op=\"status\": the short working-tree status \
             (clean tree → an empty listing) — the \"is the tree clean?\" \
             check. The bridge from a memory record's commit pointer to the \
             shipped code. Never mutates git state; use the `git` tool for \
             that.",
            json!({
                "type": "object",
                "properties": {
                    "op": {
                        "type": "string",
                        "enum": ["diff", "log", "show", "status"],
                        "description": "Which read to perform."
                    },
                    "commit": {"type": "string", "description": "op=show: the commit-ish — a hex hash (7–40 chars), branch/tag, or revision like HEAD~2."},
                    "stat": {"type": "boolean", "description": "op=show: stat summary instead of the full diff (default true)."},
                    "path": {"type": "string", "description": "op=log: only commits touching this repo-relative path."},
                    "limit": {"type": "integer", "description": "op=log: commit count (1–100, default 20)."}
                },
                "required": ["op"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // Every op is read-only by construction — that is precisely why this
        // tool stays separate from the approval-gated `git`.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let op = match serde_json::from_value::<GitReadArgs>(args.clone()) {
            Ok(a) => a.op,
            // Backlog d9ad618e: the recovery rule rides the error (the
            // read_files precedent, backlog 26cdbaf8).
            Err(e) => {
                return ToolResult::error(crate::tool::agent::read_files::invalid_args_error(
                    "git_read",
                    &e,
                    &args,
                    "Always pass op — there is no zero-argument form; rewrite \
                     the full call, do not resend the empty shape.",
                ))
            }
        };
        match op.trim().to_ascii_lowercase().as_str() {
            "diff" => self.diff.execute(args).await,
            "log" => self.log.execute(args).await,
            "show" => self.show.execute(args).await,
            "status" => self.status.execute(args).await,
            other => ToolResult::error(format!(
                "unknown op '{other}' — valid: diff, log, show, status; \
                 for write operations (commit/merge/push/…) use the `git` tool"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    /// A repo with one commit and one uncommitted edit.
    fn repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let p = dir.path();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .expect("git runs");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@e.st"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-qm", "first commit"]);
        std::fs::write(p.join("a.txt"), "one\ntwo\n").unwrap();
        dir
    }

    #[tokio::test]
    async fn each_op_routes_to_its_delegate() {
        let dir = repo();
        let tool = GitReadTool::new(dir.path());

        let diff = tool.execute(json!({"op": "diff"})).await;
        assert!(diff.success, "{}", diff.output);
        assert!(diff.output.contains("a.txt"), "{}", diff.output);

        let log = tool.execute(json!({"op": "log"})).await;
        assert!(log.success, "{}", log.output);
        assert!(log.output.contains("first commit"), "{}", log.output);

        let show = tool.execute(json!({"op": "show", "commit": "HEAD"})).await;
        assert!(show.success, "{}", show.output);
        assert!(show.output.contains("first commit"), "{}", show.output);
    }

    #[tokio::test]
    async fn per_op_arguments_still_reach_the_delegate() {
        // The delegates parse their own args out of the same object, so an
        // op-specific field must survive the hop — a limit of 0 is rejected
        // by git_log's 1..=100 validation, which proves it arrived.
        let dir = repo();
        let tool = GitReadTool::new(dir.path());
        let r = tool.execute(json!({"op": "log", "limit": 0})).await;
        assert!(!r.success, "limit validation still applies: {}", r.output);
    }

    #[tokio::test]
    async fn status_op_lists_dirty_tree_and_summarizes() {
        // One modified tracked file (a.txt, from the fixture) plus one
        // untracked file: the short form lists both, and the appended
        // summary counts them (backlog 1aa7e456 — "is the tree clean?"
        // needs a read-only home on git_read).
        let dir = repo();
        std::fs::write(dir.path().join("b.txt"), "new\n").unwrap();
        let tool = GitReadTool::new(dir.path());

        let r = tool.execute(json!({"op": "status"})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("M a.txt"), "{}", r.output);
        assert!(r.output.contains("?? b.txt"), "{}", r.output);
        assert!(
            r.output.contains("(1 changed, 1 untracked)"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn status_op_reports_clean_tree() {
        // With everything committed the short form is empty and the summary
        // says so — the "is the tree clean?" answer at a glance.
        let dir = repo();
        let p = dir.path();
        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(p)
                .output()
                .expect("git runs");
        };
        run(&["add", "-A"]);
        run(&["commit", "-qm", "second commit"]);
        let tool = GitReadTool::new(p);

        let r = tool.execute(json!({"op": "status"})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("(working tree clean"), "{}", r.output);
        assert!(!r.output.contains("?? "), "{}", r.output);
    }

    #[test]
    fn schema_advertises_the_no_zero_argument_rule() {
        // Backlog d9ad618e (the read_files precedent, backlog 26cdbaf8).
        let dir = repo();
        let tool = GitReadTool::new(dir.path());
        let schema = tool.schema();
        assert!(
            schema.description.contains("No zero-argument form"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("do not resend the empty shape"),
            "{}",
            schema.description
        );
        assert!(
            schema.description.contains("e.g. {"),
            "the inline example shows the exact call shape: {}",
            schema.description
        );
        assert!(
            schema.description.starts_with("Always pass `op`"),
            "the contract sentence LEADS the description: {}",
            schema.description
        );
        assert!(
            schema.description.contains("If you catch yourself"),
            "the content-first anti-pattern clause: {}",
            schema.description
        );
    }

    #[tokio::test]
    async fn empty_call_error_carries_the_recovery_hint() {
        // Backlog d9ad618e: the recovery hint rides the error itself, so the
        // FIRST retry succeeds instead of waiting for the circuit breaker.
        let dir = repo();
        let tool = GitReadTool::new(dir.path());
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("parameter 'op' is required"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("rewrite the full call"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("do not resend the empty shape"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn unknown_or_missing_op_errors_with_the_valid_set() {
        let dir = repo();
        let tool = GitReadTool::new(dir.path());

        let r = tool.execute(json!({"op": "blame"})).await;
        assert!(!r.success);
        assert!(
            r.output.contains("diff, log, show, status"),
            "{}",
            r.output
        );
        assert!(
            r.output.contains("`git` tool"),
            "the unknown-op error must name the write-ops alternative: {}",
            r.output
        );

        let r = tool.execute(json!({})).await;
        assert!(!r.success, "op is required");
    }

    #[test]
    fn stays_auto_run_so_it_survives_the_read_only_states() {
        let dir = tempdir().unwrap();
        assert_eq!(GitReadTool::new(dir.path()).safety(), SafetyLevel::AutoRun);
    }
}
