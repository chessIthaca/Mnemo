// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Approval gate — oneshot channels for approve-each-action.
//!
//! When the agent wants to execute a mutating tool, it sends an
//! `ApprovalRequest` with an embedded `oneshot::Sender`. The UI answers
//! directly via the oneshot. While awaiting approval, the agent `select!`s
//! between the oneshot receiver and its command inbox — so the user can
//! `Interrupt` or send a `Suggestion` even while the agent awaits approval.
//!
//! ## Safety modes
//!
//! - **ApproveEachAction** — every `NeedsApproval` tool requires explicit
//!   approval.
//! - **AutoReadApproveWrites** — same as ApproveEachAction (reads are
//!   `AutoRun` anyway).
//! - **AutoApproveProject** — `NeedsApproval` tools whose call only touches
//!   files inside the project directory are auto-approved (no prompt). Tools
//!   that can't be statically proven project-scoped (e.g. `shell`) still
//!   require approval.
//! - **Autonomous** — no approval prompts at all.

use std::path::Path;

use serde_json::Value;
use tokio::sync::oneshot;

use crate::config::SafetyMode;
use crate::runtime::{AgentCommand, Approval};
use crate::tool::agent::git::{resolve_git_action, resolve_git_subcommand};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::SafetyLevel;

/// The outcome of an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The user approved the action.
    Approved,
    /// The user denied the action.
    Denied,
    /// The user denied all remaining approvals this turn.
    DeniedAll,
    /// The agent was interrupted while awaiting approval. The turn should end
    /// (keeping partial output) but the agent stays alive — the Stop button.
    Interrupted,
    /// The agent was cancelled while awaiting approval. The turn should end and
    /// the agent task should terminate (`Exited` fires) — the close-x.
    Cancelled,
    /// The approval channel was dropped (UI gone).
    ChannelClosed,
}

impl From<Approval> for ApprovalOutcome {
    fn from(a: Approval) -> Self {
        match a {
            Approval::Approve => ApprovalOutcome::Approved,
            Approval::Deny => ApprovalOutcome::Denied,
            Approval::DenyAll => ApprovalOutcome::DeniedAll,
        }
    }
}

/// Whether a tool call needs approval based on its safety level, the safety
/// mode, and (for `AutoApproveProject`) whether the call is project-scoped.
///
/// `tool_name` + `args` + `sandbox` are used only by `AutoApproveProject` to
/// determine whether the call stays inside the project directory. For the
/// other modes they're ignored.
pub fn needs_approval(
    safety: SafetyLevel,
    mode: SafetyMode,
    tool_name: &str,
    args: &Value,
    sandbox: &Sandbox,
) -> bool {
    match mode {
        SafetyMode::ApproveEachAction | SafetyMode::AutoReadApproveWrites => {
            safety == SafetyLevel::NeedsApproval
        }
        SafetyMode::AutoApproveProject => {
            if safety == SafetyLevel::AutoRun {
                return false;
            }
            // NeedsApproval — auto-approve if the call is project-scoped.
            !is_project_scoped(tool_name, args, sandbox)
        }
        SafetyMode::Autonomous => false,
    }
}

/// Whether a tool call only accesses files inside the project directory.
///
/// This is a static, best-effort check used by `AutoApproveProject` mode to
/// decide whether a `NeedsApproval` call can be auto-approved:
///
/// - **file_edit / file_write / file_append / file_read /
///   convert_line_endings** — validate the
///   `path` arg through the sandbox. If the sandbox accepts it (the path is
///   inside the project root), the call is project-scoped.
/// - **multi_edit** — every `files[].path` entry must validate through the
///   sandbox: all-or-nothing, since the call writes every entry (one
///   out-of-project path makes the whole call not project-scoped).
/// - **search** — always true. The search tool walks the project tree only
///   (it skips build artifacts + deps and never escapes the sandbox root).
/// - **git** — only *read-only* subcommands (`status`, `diff`, `log`, and
///   `branch` list). Mutating subcommands (`commit`, `checkout`, `stash`,
///   branch create/delete, `merge`, `push`, …) return false so they still
///   prompt under `AutoApproveProject`. (`merge`/`push` are additionally
///   forced via `never_auto_for` even in Autonomous.)
/// - **shell** — always false. A shell command can run anything (`rm -rf /`,
///   `curl | bash`), so it can never be statically proven project-scoped.
/// - **any other tool** — false (conservative: unknown tools require
///   approval).
///
/// **Performance trade-off (P3, documented):** `sandbox.validate()` does a
/// blocking `canonicalize()` filesystem syscall. This runs on the async
/// runtime (in `dispatch.rs`'s `needs_approval` call). It's one syscall per
/// `NeedsApproval` tool call under `AutoApproveProject` mode only — infrequent
/// for a single-agent coding session. Wrapping it in `spawn_blocking` would
/// require making `needs_approval` (and its callers) async — a cross-cutting
/// seam change not worth the churn for the current single-user scale. If
/// multi-agent concurrency under `AutoApproveProject` becomes a goal, wrap
/// the `is_project_scoped` call in `spawn_blocking` at the dispatch site, or
/// cache the canonicalized path per `(tool_name, path_arg)` within a turn.
pub fn is_project_scoped(tool_name: &str, args: &Value, sandbox: &Sandbox) -> bool {
    match tool_name {
        "file_edit" | "file_write" | "convert_line_endings" => {
            // Extract the `path` arg and validate it through the sandbox.
            // A missing or non-string path is not project-scoped (conservative).
            let path_str = match args.get("path").and_then(|v| v.as_str()) {
                Some(p) => p,
                None => return false,
            };
            sandbox.validate(Path::new(path_str)).is_ok()
        }
        "multi_edit" => {
            // All-or-nothing: a single out-of-project entry makes the whole
            // call not project-scoped (the call writes every entry).
            let files = match args.get("files").and_then(|v| v.as_array()) {
                Some(files) if !files.is_empty() => files,
                _ => return false,
            };
            files.iter().all(|f| {
                f.get("path")
                    .and_then(|v| v.as_str())
                    .is_some_and(|p| sandbox.validate(Path::new(p)).is_ok())
            })
        }
        "search" | "search_read" => true,
        "git" => is_git_read_only(args),
        _ => false,
    }
}

/// Whether a `git` tool call is a read-only subcommand that can be
/// auto-approved under `AutoApproveProject`.
///
/// Read-only: `status`, `diff`, `log`, and `branch` with action `list`
/// (the default when `action` is omitted). Everything else — including
/// missing/unknown subcommands — is treated as mutating and returns false.
///
/// The subcommand is resolved forgivingly (shared with the git tool itself):
/// it may arrive in either the `subcommand` or `action` field, and
/// branch/stash action names in either field resolve to `branch`/`stash` —
/// so `action: "delete"` or `subcommand: "pop"` are correctly NOT
/// auto-approvable.
///
/// Any call carrying a non-empty `args` array returns false regardless of
/// subcommand (defence in depth, review 2026-08-22 HIGH 1): the git tool
/// validates forwarded read-query args against write/exec/sandbox-escape
/// flags, but the validator is heuristic — flagged queries get human review
/// so the auto-approved path is never strictly more permissive than the
/// shell (which always prompts under `AutoApproveProject`).
fn is_git_read_only(args: &Value) -> bool {
    let has_extra_args = args
        .get("args")
        .and_then(|v| v.as_array())
        .is_some_and(|a| !a.is_empty());
    if has_extra_args {
        return false;
    }
    let subcommand = match resolve_git_subcommand(args) {
        Some(s) => s,
        None => return false,
    };
    match subcommand.as_str() {
        "status" | "diff" | "log" => true,
        "branch" => {
            // Default action is list (mirrors GitTool::execute). Only list is
            // read-only; create/delete mutate refs.
            resolve_git_action("branch", args).as_deref() == Some("list")
        }
        // commit / checkout / stash / merge / push / unknown → not auto-approvable
        _ => false,
    }
}

/// Wait for an approval response, while also listening for interrupts.
///
/// The `rx` is the oneshot from the UI. The `cmd_rx` is the agent's command
/// inbox — we check for `Interrupt` and `Cancel` while waiting. Non-interrupt
/// commands (e.g. `Suggestion`) are buffered and returned so the caller can
/// re-inject them after the approval resolves.
pub async fn await_approval(
    mut rx: oneshot::Receiver<Approval>,
    cmd_rx: &mut tokio::sync::mpsc::Receiver<AgentCommand>,
) -> (ApprovalOutcome, Vec<AgentCommand>) {
    let mut buffered = Vec::new();
    loop {
        tokio::select! {
            result = &mut rx => {
                let outcome = match result {
                    Ok(approval) => approval.into(),
                    Err(_) => ApprovalOutcome::ChannelClosed,
                };
                return (outcome, buffered);
            }
            cmd = cmd_rx.recv() => match cmd {
                Some(AgentCommand::Interrupt) => {
                    return (ApprovalOutcome::Interrupted, buffered);
                }
                Some(AgentCommand::Cancel) => {
                    return (ApprovalOutcome::Cancelled, buffered);
                }
                Some(other) => {
                    // Buffer non-interrupt commands (e.g. Suggestion) —
                    // the caller re-injects them after the approval resolves.
                    buffered.push(other);
                    // Continue waiting for the actual approval.
                }
                None => return (ApprovalOutcome::ChannelClosed, buffered),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SafetyMode;
    use tempfile::tempdir;

    // --- needs_approval ---

    #[test]
    fn needs_approval_approve_each() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"path": "test.txt"});
        assert!(needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::ApproveEachAction,
            "file_write",
            &args,
            &sandbox
        ));
        assert!(!needs_approval(
            SafetyLevel::AutoRun,
            SafetyMode::ApproveEachAction,
            "file_read",
            &args,
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_autonomous_never() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"path": "test.txt"});
        assert!(!needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::Autonomous,
            "file_write",
            &args,
            &sandbox
        ));
        assert!(!needs_approval(
            SafetyLevel::AutoRun,
            SafetyMode::Autonomous,
            "file_read",
            &args,
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_auto_approve_project_scoped_file() {
        // A file_write to a path inside the project is project-scoped → no
        // approval needed in AutoApproveProject mode.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // Create the file so the sandbox validate succeeds.
        std::fs::write(dir.path().join("main.rs"), "").unwrap();
        let args = serde_json::json!({"path": "main.rs", "content": "hi"});
        assert!(!needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "file_write",
            &args,
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_auto_approve_project_outside_file() {
        // A file_write to a path outside the project is NOT project-scoped →
        // still needs approval in AutoApproveProject mode.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let outside = dir.path().parent().unwrap().join("outside.txt");
        let args = serde_json::json!({"path": outside.to_string_lossy(), "content": "hi"});
        assert!(needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "file_write",
            &args,
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_auto_approve_project_shell() {
        // shell can never be proven project-scoped → always needs approval.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"command": "echo hi"});
        assert!(needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "shell",
            &args,
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_auto_approve_project_search_and_git() {
        // search is always project-scoped; read-only git (status) is too →
        // no approval. Mutating git (commit) still needs approval.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let search_args = serde_json::json!({"pattern": "foo"});
        assert!(!needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "search",
            &search_args,
            &sandbox
        ));
        let git_status = serde_json::json!({"subcommand": "status"});
        assert!(!needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "git",
            &git_status,
            &sandbox
        ));
        let git_commit = serde_json::json!({"subcommand": "commit", "message": "x"});
        assert!(needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "git",
            &git_commit,
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_auto_approve_project_auto_run() {
        // AutoRun tools never need approval regardless of mode.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"path": "test.txt"});
        assert!(!needs_approval(
            SafetyLevel::AutoRun,
            SafetyMode::AutoApproveProject,
            "file_read",
            &args,
            &sandbox
        ));
    }

    // --- is_project_scoped ---

    #[test]
    fn is_project_scoped_file_inside() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // Create the file so the sandbox validate succeeds.
        std::fs::write(dir.path().join("test.txt"), "hi").unwrap();
        let args = serde_json::json!({"path": "test.txt"});
        assert!(is_project_scoped("file_write", &args, &sandbox));
        assert!(is_project_scoped("file_edit", &args, &sandbox));
        assert!(is_project_scoped("convert_line_endings", &args, &sandbox));
        // file_read and file_append were folded into read_files / file_write
        // mode:"append" — the names no longer reach dispatch.
        assert!(!is_project_scoped("file_read", &args, &sandbox));
        assert!(!is_project_scoped("file_append", &args, &sandbox));
    }

    #[test]
    fn is_project_scoped_file_outside() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let outside = dir.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "hi").unwrap();
        let args = serde_json::json!({"path": outside.to_string_lossy()});
        assert!(!is_project_scoped("file_read", &args, &sandbox));
        assert!(!is_project_scoped("file_write", &args, &sandbox));
    }

    #[test]
    fn is_project_scoped_missing_path_arg() {
        // A file tool with no `path` arg is not project-scoped (conservative).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"content": "hi"});
        assert!(!is_project_scoped("file_write", &args, &sandbox));
    }

    #[test]
    fn is_project_scoped_multi_edit_is_all_or_nothing() {
        // multi_edit writes EVERY entry, so one out-of-project path makes the
        // whole call not project-scoped (plan 2e27f896) — the conservative
        // direction: an `AutoApproveProject` grant is withheld and the call
        // prompts instead.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        std::fs::write(dir.path().join("b.txt"), "hi").unwrap();
        let inside = serde_json::json!({
            "files": [
                {"path": "a.txt", "ops": ["i0:x"]},
                {"path": "b.txt", "ops": ["d1"]}
            ]
        });
        assert!(is_project_scoped("multi_edit", &inside, &sandbox));
        // One entry outside the project taints the whole call.
        let outside = dir.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "hi").unwrap();
        let mixed = serde_json::json!({
            "files": [
                {"path": "a.txt", "ops": ["i0:x"]},
                {"path": outside.to_string_lossy(), "ops": ["i0:x"]}
            ]
        });
        assert!(!is_project_scoped("multi_edit", &mixed, &sandbox));
        // Shape failures fail closed: no `files` arg, an empty array, or an
        // entry without a usable path.
        assert!(!is_project_scoped(
            "multi_edit",
            &serde_json::json!({}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "multi_edit",
            &serde_json::json!({"files": []}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "multi_edit",
            &serde_json::json!({"files": [{"ops": ["d1"]}]}),
            &sandbox
        ));
    }

    #[test]
    fn is_project_scoped_search_always_true() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"pattern": "foo"});
        assert!(is_project_scoped("search", &args, &sandbox));
        // search_read is the search+read combo — also always project-scoped.
        assert!(is_project_scoped("search_read", &args, &sandbox));
    }

    #[test]
    fn is_git_read_only_false_with_forwarded_args() {
        // Defence in depth (review 2026-08-22 HIGH 1): a read subcommand WITH
        // forwarded `args` is NOT auto-approvable under AutoApproveProject —
        // the denylist is heuristic, so flagged queries get human review.
        assert!(!is_git_read_only(
            &serde_json::json!({"subcommand": "log", "args": ["-1"]})
        ));
        assert!(!is_git_read_only(
            &serde_json::json!({"subcommand": "diff", "args": ["--no-index", "a", "b"]})
        ));
        // Bare read subcommands and empty args stay auto-approvable.
        assert!(is_git_read_only(&serde_json::json!({"subcommand": "log"})));
        assert!(is_git_read_only(
            &serde_json::json!({"subcommand": "log", "args": []})
        ));
        assert!(is_git_read_only(
            &serde_json::json!({"subcommand": "branch", "action": "list"})
        ));
    }

    #[test]
    fn is_git_read_only_resolves_either_field() {
        // The subcommand may arrive in the `action` field — read-only gating
        // must still apply (and mutating action names must NOT auto-approve).
        assert!(is_git_read_only(&serde_json::json!({"action": "status"})));
        assert!(is_git_read_only(&serde_json::json!({"action": "log"})));
        assert!(is_git_read_only(&serde_json::json!({"action": "branch"})));
        // Branch/stash action names in either field resolve to branch/stash.
        assert!(!is_git_read_only(
            &serde_json::json!({"action": "delete", "branch": "x"})
        ));
        assert!(!is_git_read_only(
            &serde_json::json!({"subcommand": "create"})
        ));
        assert!(!is_git_read_only(&serde_json::json!({"subcommand": "pop"})));
        assert!(!is_git_read_only(&serde_json::json!({"action": "push"})));
        // Forwarded args still force human review even via the action field.
        assert!(!is_git_read_only(
            &serde_json::json!({"action": "status", "args": ["-1"]})
        ));
    }

    #[test]
    fn is_project_scoped_git_read_only_true() {
        // Read-only git subcommands are project-scoped under AutoApproveProject.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert!(is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "status"}),
            &sandbox
        ));
        assert!(is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "diff"}),
            &sandbox
        ));
        assert!(is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "log"}),
            &sandbox
        ));
        // branch list (default action) is read-only.
        assert!(is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "branch"}),
            &sandbox
        ));
        assert!(is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "branch", "action": "list"}),
            &sandbox
        ));
    }

    #[test]
    fn is_project_scoped_git_mutating_false() {
        // Mutating git subcommands are NOT project-scoped — they must prompt
        // under AutoApproveProject (merge/push additionally use never_auto_for).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "commit", "message": "x"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "checkout", "branch": "main"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "stash"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "stash", "action": "pop"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "branch", "action": "create", "branch": "feat"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "branch", "action": "delete", "branch": "feat"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "merge", "branch": "feat"}),
            &sandbox
        ));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "push"}),
            &sandbox
        ));
        // Missing / unknown subcommand → conservative false.
        assert!(!is_project_scoped("git", &serde_json::json!({}), &sandbox));
        assert!(!is_project_scoped(
            "git",
            &serde_json::json!({"subcommand": "frobnicate"}),
            &sandbox
        ));
    }

    #[test]
    fn needs_approval_auto_approve_project_git_commit_vs_status() {
        // Under AutoApproveProject: status auto-runs, commit still prompts.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        assert!(!needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "git",
            &serde_json::json!({"subcommand": "status"}),
            &sandbox
        ));
        assert!(needs_approval(
            SafetyLevel::NeedsApproval,
            SafetyMode::AutoApproveProject,
            "git",
            &serde_json::json!({"subcommand": "commit", "message": "x"}),
            &sandbox
        ));
    }

    #[test]
    fn is_project_scoped_shell_always_false() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({"command": "echo hi"});
        assert!(!is_project_scoped("shell", &args, &sandbox));
    }

    #[test]
    fn is_project_scoped_unknown_tool_false() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let args = serde_json::json!({});
        assert!(!is_project_scoped("memory_write", &args, &sandbox));
    }

    // --- await_approval (unchanged behavior) ---

    #[tokio::test]
    async fn await_approval_approved() {
        let (tx, rx) = oneshot::channel();
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(1);
        // Send approval.
        tx.send(Approval::Approve).unwrap();
        let (outcome, buffered) = await_approval(rx, &mut cmd_rx).await;
        assert_eq!(outcome, ApprovalOutcome::Approved);
        assert!(buffered.is_empty());
    }

    #[tokio::test]
    async fn await_approval_denied() {
        let (tx, rx) = oneshot::channel();
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(1);
        tx.send(Approval::Deny).unwrap();
        let (outcome, _) = await_approval(rx, &mut cmd_rx).await;
        assert_eq!(outcome, ApprovalOutcome::Denied);
    }

    #[tokio::test]
    async fn await_approval_denied_all() {
        let (tx, rx) = oneshot::channel();
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(1);
        tx.send(Approval::DenyAll).unwrap();
        let (outcome, _) = await_approval(rx, &mut cmd_rx).await;
        assert_eq!(outcome, ApprovalOutcome::DeniedAll);
    }

    #[tokio::test]
    async fn await_approval_interrupted() {
        let (_tx, rx) = oneshot::channel::<Approval>();
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
        });
        let (outcome, _) = await_approval(rx, &mut cmd_rx).await;
        assert_eq!(outcome, ApprovalOutcome::Interrupted);
    }

    #[tokio::test]
    async fn await_approval_cancelled() {
        // Cancel during an approval wait must return `Cancelled` (distinct from
        // `Interrupted`) so the dispatch layer can propagate the stop signal
        // that terminates the agent task (Exited), not just end the turn.
        let (_tx, rx) = oneshot::channel::<Approval>();
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        });
        let (outcome, _) = await_approval(rx, &mut cmd_rx).await;
        assert_eq!(outcome, ApprovalOutcome::Cancelled);
    }

    #[tokio::test]
    async fn await_approval_channel_closed() {
        let (tx, rx) = oneshot::channel::<Approval>();
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(1);
        drop(tx); // UI gone
        let (outcome, _) = await_approval(rx, &mut cmd_rx).await;
        assert_eq!(outcome, ApprovalOutcome::ChannelClosed);
    }

    #[tokio::test]
    async fn await_approval_suggestion_does_not_interrupt() {
        // A suggestion arriving during approval wait should NOT cause an interrupt.
        // We send the approval immediately (no race) and a suggestion that
        // may or may not be buffered — either way the outcome must be Approved.
        let (tx, rx) = oneshot::channel::<Approval>();
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);
        // Send approval first so it's ready before we start awaiting.
        tx.send(Approval::Approve).unwrap();
        // Also send a suggestion — if it wins the select race, it should be
        // buffered (not cause an interrupt), and the loop continues until
        // the approval is received.
        let _ = cmd_tx.try_send(AgentCommand::Suggestion("be careful".into()));
        let (outcome, buffered) = await_approval(rx, &mut cmd_rx).await;
        // The outcome must be Approved, NOT Interrupted.
        assert_eq!(outcome, ApprovalOutcome::Approved);
        // If the suggestion was seen, it should be buffered — not cause an interrupt.
        // (It may or may not be in `buffered` depending on select race, but
        // the outcome is always Approved.)
        let _ = buffered;
    }
}
