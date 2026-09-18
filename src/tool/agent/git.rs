// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `git` — git status / diff / log / commit / merge / checkout / stash /
//! branch / push / restore.
//!
//! Every subcommand goes through the standard approval flow (the tool is
//! `NeedsApproval`); `commit` auto-stages all changes (`git add -A`) only
//! when nothing is already staged — when the index has staged changes it
//! commits only what's staged, so a deliberate selective `git add` (via
//! `shell`) is respected. The *core*
//! operations — `merge` (lands commits on the target branch) and `push`
//! (publishes to a remote) — additionally force the interactive approval
//! prompt via [`Tool::never_auto_for`](crate::tool::Tool::never_auto_for),
//! so they can never be auto-run (Autonomous) or auto-approved (a stored
//! safety rule) regardless of the active safety mode. This enforces the
//! constitution's "never commit to main without a contemporaneous user
//! sanction" invariant.
//!
//! Precise read queries: `status`, `diff`, `log`, and `branch list` accept an
//! optional `args` list of extra flags/refs/paths, appended after the built-in
//! defaults — so agents don't need the `shell` tool for queries like
//! `git diff main..feat --stat`, `git log -3`, or `git branch -vv`. status/diff/
//! log use a denylist (flags that write files or run commands are refused);
//! `branch list` uses an ALLOWLIST of read-only listing flags instead, because
//! a positional arg to `git branch` would create a branch (and some single-arg
//! flags like `--set-upstream-to=` mutate the current branch).
//!
//! **Restore (discards work):** the `restore` subcommand restores files from
//! a ref — discarding uncommitted worktree/staged changes, or overwriting a
//! file with its content from another commit. It uses only structured,
//! validated fields (`source` tree-ish, `paths` pathspecs, `target`
//! worktree/staged/both) — never free-form `args`, which stay read-only per
//! the git-tool decision (plan 47472e35); writes get structured params, and
//! non-empty `args` on any write subcommand errors loudly.
//!
//! **Output-cap EXCEPTION for `diff`:** its output is NEVER truncated — a
//! truncated diff could hide part of the changes under review. Every other
//! subcommand is capped to the 100 KiB tool-output cap; `diff` alone bypasses
//! it (user-mandated, mirroring [`super::git_diff::GitDiffTool`]).
//!
//! **Forgiving action/subcommand fields.** The `subcommand` and `action`
//! fields are interchangeable carriers of the subcommand name: a known
//! subcommand in either field wins, and branch/stash action names
//! (list/delete/create/push/pop/drop) are accepted in the `subcommand` field
//! (e.g. `subcommand: "delete"` = branch delete, `subcommand: "pop"` = stash
//! pop). A call repeating the subcommand name in `action` (`action: "branch"`
//! with `subcommand: "branch"`) falls back to the default action (list for
//! branch, push for stash). The schema marks `subcommand` required, but the
//! runtime still forgives an `action`-only call. Error messages list the
//! valid values.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use std::sync::{Arc, RwLock};
use tokio::process::Command;

use crate::project::git_ops::valid_branch_name;
use crate::provider::ToolSchema;
use crate::tool::agent::cap_tool_output;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Arguments for `git`.
///
/// Note: the `subcommand` and `action` fields are NOT deserialized here — they
/// are resolved from the raw arguments by [`resolve_git_subcommand`] /
/// [`resolve_git_action`] before deserialization (the two fields are
/// interchangeable carriers of the subcommand name, and branch/stash action
/// names are accepted in either field).
#[derive(Debug, Deserialize)]
struct GitArgs {
    /// For commit/merge: the commit/merge message.
    #[serde(default)]
    message: Option<String>,
    /// For merge/checkout/push: the branch name.
    #[serde(default)]
    branch: Option<String>,
    /// For push: the remote to push to (default "origin").
    #[serde(default)]
    remote: Option<String>,
    /// For restore: the tree-ish to restore FROM (branch/tag/commit/HEAD~1).
    /// Defaults to the index for worktree restores and to HEAD for staged
    /// restores — matching `git restore`'s own defaults. Validated like a
    /// branch name (no leading `-`, no whitespace) so it can never carry a
    /// flag.
    #[serde(default)]
    source: Option<String>,
    /// For restore: the files/dirs to restore (required; at least one).
    /// Passed as pathspecs after a mandatory `--` separator so a path can
    /// never parse as a flag.
    #[serde(default)]
    paths: Option<Vec<String>>,
    /// For restore: where the restored content lands — "worktree" (default),
    /// "staged" (index only), or "both" (index + worktree).
    #[serde(default)]
    target: Option<String>,
    /// Extra flags/refs/paths for READ queries only (status, diff, log,
    /// `branch list`), appended after the built-in defaults. Rejected for
    /// write subcommands (incl. `branch create`/`delete`). `branch list` uses
    /// an allowlist of read-only listing flags (positionals refused).
    #[serde(default)]
    args: Option<Vec<String>>,
}

/// The valid git subcommand names, in the order shown in error messages.
pub(crate) const GIT_SUBCOMMANDS: &[&str] = &[
    "status", "diff", "log", "commit", "merge", "checkout", "stash", "branch", "push", "restore",
];

/// The valid `branch` action names.
pub(crate) const BRANCH_ACTIONS: &[&str] = &["list", "delete", "create"];

/// The valid `stash` action names.
pub(crate) const STASH_ACTIONS: &[&str] = &["push", "pop", "drop", "list"];

/// Resolve the git subcommand from a tool call's raw arguments, forgivingly.
///
/// The `subcommand` and `action` fields are interchangeable carriers of the
/// subcommand name — models routinely put the action in `subcommand` or
/// repeat the subcommand in `action`. Resolution order:
///
/// 1. A known subcommand in the `subcommand` field wins.
/// 2. A known subcommand in the `action` field wins.
/// 3. A branch action name (`list`/`delete`/`create`) in either field maps
///    to `branch`.
/// 4. A stash action name (`push`/`pop`/`drop`/`list`) in either field maps
///    to `stash` (`push` is a subcommand first, so it never reaches this rule).
///
/// Matching is case-insensitive (the returned value is normalized to
/// lowercase), so `subcommand: "MERGE"` resolves to `merge` — the same
/// tolerance the core-operations list has always had.
///
/// Returns `None` when neither field names anything recognizable — the caller
/// reports the valid values.
pub(crate) fn resolve_git_subcommand(args: &serde_json::Value) -> Option<String> {
    let sub = args
        .get("subcommand")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if GIT_SUBCOMMANDS.contains(&sub.as_str()) {
        return Some(sub);
    }
    if GIT_SUBCOMMANDS.contains(&action.as_str()) {
        return Some(action);
    }
    if BRANCH_ACTIONS.contains(&sub.as_str()) || BRANCH_ACTIONS.contains(&action.as_str()) {
        return Some("branch".to_string());
    }
    if STASH_ACTIONS.contains(&sub.as_str()) || STASH_ACTIONS.contains(&action.as_str()) {
        return Some("stash".to_string());
    }
    None
}

/// Resolve the action for a `branch` or `stash` call from the raw arguments.
///
/// Forgiving rules (mirrors [`resolve_git_subcommand`]):
///
/// - A valid action in the `action` field wins.
/// - A valid action carried in the `subcommand` field wins (swapped calls like
///   `subcommand: "delete"` + `action: "branch"`).
/// - A missing/empty `action`, or one repeating the subcommand name
///   (`action: "branch"` with `subcommand: "branch"`, or `action: "stash"`
///   with `subcommand: "stash"`), falls back to the default action (`list`
///   for branch, `push` for stash).
///
/// Matching is case-insensitive (the returned value is normalized to
/// lowercase). Returns `None` when the action is invalid — the caller reports
/// the valid values.
pub(crate) fn resolve_git_action(subcommand: &str, args: &serde_json::Value) -> Option<String> {
    let valid: &[&str] = if subcommand == "branch" {
        BRANCH_ACTIONS
    } else {
        STASH_ACTIONS
    };
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if valid.contains(&action.as_str()) {
        return Some(action);
    }
    // Swapped call: the action name arrived in the `subcommand` field.
    let sub = args
        .get("subcommand")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if valid.contains(&sub.as_str()) {
        return Some(sub);
    }
    // No action given (missing/empty), or the action field repeating the
    // subcommand name → default action.
    if action.is_empty() || action == subcommand {
        return Some(
            if subcommand == "branch" {
                "list"
            } else {
                "push"
            }
            .to_string(),
        );
    }
    None
}

/// Validate user-supplied args for a READ subcommand (status/diff/log).
///
/// Args are passed to git as discrete argv (no shell), so the risk is
/// git-level only: flags that write files, run commands, or read files
/// outside the project sandbox. Refused:
/// - `--output…` and the short form `-o` (write files)
/// - `--exec…` (runs a command)
/// - `--no-index…` (diffs arbitrary paths outside the repo — a sandbox escape)
/// - `--ext-diff…` / `--textconv…` (execute repo-configured external commands)
///
/// Everything else — refs (`main..feat`), paths after `--`, `-n`/`-5`,
/// `--stat`, `--cached`, `--porcelain`, `--format=`, … — is allowed. Returns
/// `Err` with the refused flag.
fn validate_read_args(args: &[String]) -> Result<(), String> {
    for a in args {
        let short_output = a.starts_with("-o") && !a.starts_with("--");
        if a.starts_with("--output")
            || a.starts_with("--exec")
            || a.starts_with("--no-index")
            || a.starts_with("--ext-diff")
            || a.starts_with("--textconv")
            || short_output
        {
            return Err(format!(
                "unsafe flag '{a}' rejected — read queries may not write files, read files \
                 outside the project, or run commands (--output, -o, --exec, --no-index, \
                 --ext-diff, --textconv are refused)"
            ));
        }
    }
    Ok(())
}

/// Build argv for a read subcommand: the built-in defaults plus the validated
/// user args appended after them (git resolves repeated flags with
/// later-wins, so a user `-5` overrides the default `-20`).
fn read_argv<'a>(base: &[&'a str], extra: &'a [String]) -> Result<Vec<&'a str>, String> {
    validate_read_args(extra)?;
    let mut argv: Vec<&'a str> = base.to_vec();
    argv.extend(extra.iter().map(String::as_str));
    Ok(argv)
}

/// Validate user-supplied args for the `branch list` action.
///
/// Unlike [`validate_read_args`] (used by status/diff/log), this uses an
/// ALLOWLIST rather than a denylist — because `git branch <positional>` CREATES
/// a branch, and some single-arg flags mutate the current branch without any
/// positional (`--set-upstream-to=origin/main`, `--unset-upstream`,
/// `--edit-description`). The status/diff/log denylist permits positionals
/// (refs/paths are safe there), so it cannot be reused for `branch`. Only
/// known-safe read-only listing flags pass; everything else is rejected.
///
/// Refused:
/// - any positional arg (does not start with `-`) — it would create/move/
///   delete a branch;
/// - any flag not on the safe-read allowlist below.
///
/// Allowed (read-only listing flags): `-v`/`-vv`/`--verbose`, `-a`/`--all`,
/// `-r`/`--remotes`, `--list`, `-q`/`--quiet`, `--no-color`, `--color=…`,
/// `--format=…`, `--sort=…`, `--merged`/`--no-merged`/`--contains`/
/// `--no-contains` (default to HEAD with no positional), `-i`/`--ignore-case`.
fn validate_branch_list_args(args: &[String]) -> Result<(), String> {
    let safe_exact: &[&str] = &[
        "-v",
        "-vv",
        "--verbose",
        "-a",
        "--all",
        "-r",
        "--remotes",
        "--list",
        "-q",
        "--quiet",
        "--no-color",
        "--merged",
        "--no-merged",
        "--contains",
        "--no-contains",
        "-i",
        "--ignore-case",
    ];
    let safe_prefix: &[&str] = &["--format=", "--sort=", "--color="];
    for a in args {
        if !a.starts_with('-') {
            return Err(format!(
                "positional arg '{a}' refused for `git branch list` — only read-only listing \
                 flags are accepted (a positional would create/move/delete a branch)"
            ));
        }
        let exact = safe_exact.contains(&a.as_str());
        let prefix = safe_prefix.iter().any(|p| a.starts_with(p));
        if !exact && !prefix {
            return Err(format!(
                "flag '{a}' is not allowed for `git branch list` — only read-only listing flags \
                 are accepted: -v/-vv/--verbose, -a/--all, -r/--remotes, --list, --format=, \
                 --sort=, --color=/--no-color, --merged/--no-merged/--contains/--no-contains, \
                 -i/--ignore-case"
            ));
        }
    }
    Ok(())
}

/// Build argv for the `branch list` action: `branch` plus the allowlisted
/// user args appended after it. Mirrors [`read_argv`] but uses the
/// branch-list allowlist ([`validate_branch_list_args`]) instead of the
/// status/diff/log denylist.
fn branch_list_argv<'a>(extra: &'a [String]) -> Result<Vec<&'a str>, String> {
    validate_branch_list_args(extra)?;
    let mut argv: Vec<&'a str> = vec!["branch"];
    argv.extend(extra.iter().map(String::as_str));
    Ok(argv)
}

/// The `git` tool.
pub struct GitTool {
    project_root: std::path::PathBuf,
    /// The runtime-mutable list of git subcommands that are *core operations*
    /// (always force the approval prompt, even in Autonomous mode or under a
    /// safety rule). Shared via an `Arc<RwLock<...>>` so a Settings save can
    /// update it live (the factory holds the same handle and pushes the new
    /// list on every `save_settings`). Defaults to `["merge", "push"]`.
    core_operations: Arc<RwLock<Vec<String>>>,
}

impl GitTool {
    /// Create a `git` tool with the default core-operations list
    /// (`["merge", "push"]`). Used by tests and any path that doesn't wire in
    /// a shared config handle.
    pub fn new(project_root: impl Into<std::path::PathBuf>) -> Self {
        Self::with_core_operations(
            project_root,
            Arc::new(RwLock::new(Self::default_core_operations())),
        )
    }

    /// Create a `git` tool sharing a runtime-mutable core-operations handle.
    /// The factory passes its own `Arc<RwLock<Vec<String>>>` here so a
    /// `save_settings` call (which calls `factory.set_core_operations`) is
    /// observed by every live `GitTool` on its next `never_auto_for` check —
    /// no registry rebuild needed.
    pub fn with_core_operations(
        project_root: impl Into<std::path::PathBuf>,
        core_operations: Arc<RwLock<Vec<String>>>,
    ) -> Self {
        Self {
            project_root: project_root.into(),
            core_operations,
        }
    }

    /// The default core-operations list (`["merge", "push"]`) — the historical
    /// hardcoded set, used when no `[git]` config section is present.
    fn default_core_operations() -> Vec<String> {
        vec!["merge".into(), "push".into()]
    }

    /// Run a git command and return its result, capped to the tool-output cap
    /// (~100 KiB with a truncation note).
    async fn run_git(&self, args: &[&str]) -> ToolResult {
        self.run_git_impl(args, true).await
    }

    /// Run a git command and return its FULL result without the tool-output
    /// cap. Used only by `diff` (the user-mandated exception): a truncated
    /// diff could hide part of the changes under review — mirrors
    /// [`super::git_diff::GitDiffTool`].
    async fn run_git_uncapped(&self, args: &[&str]) -> ToolResult {
        self.run_git_impl(args, false).await
    }

    async fn run_git_impl(&self, args: &[&str], cap_output: bool) -> ToolResult {
        // Build a human-readable echo of the command being run (e.g.
        // `git status --short`) so the output shows what was executed, not
        // just its result. The args are passed to git as discrete argv, so
        // joining them with spaces is display-only — there's no shell.
        let command_display = {
            let mut parts: Vec<&str> = vec!["git"];
            parts.extend_from_slice(args);
            parts.join(" ")
        };

        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(&self.project_root);
        // Kill the child process if the future is dropped (e.g. an Interrupt
        // during tool execution drops the pinned dispatch future). Without
        // this, a cancelled git command would be orphaned.
        cmd.kill_on_drop(true);

        // On Windows, suppress the console window that would otherwise pop up
        // when spawning a child process from a GUI app (CREATE_NO_WINDOW).
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let output = cmd.output().await;
        match output {
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
                // Cap the displayed output to prevent unbounded context (M3) —
                // except `diff`, the user-mandated exception (run_git_uncapped).
                let full = format!("$ {command_display}\n{combined}\n[exit code: {code}]");
                let displayed = if cap_output {
                    cap_tool_output(full)
                } else {
                    full
                };
                ToolResult {
                    success,
                    output: displayed,
                    data: Some(json!({"exit_code": code, "stdout": stdout, "stderr": stderr})),
                }
            }
            Err(e) => ToolResult::error(format!("$ {command_display}\nfailed to run git: {e}")),
        }
    }
}

#[async_trait]
impl Tool for GitTool {
    fn name(&self) -> &str {
        "git"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "git",
            "Run git commands in the project. `commit` auto-stages all changes \
             (git add -A) only when nothing is already staged (otherwise \
             commits only what's staged) and REQUIRES `message`. merge and \
             push ALWAYS require approval, even in Autonomous mode. `restore` \
             restores files from a ref and DISCARDS uncommitted/staged \
             changes (structured fields only: paths/source/target). Output is \
             capped ~100 KiB except `diff`, which is never truncated. \
             Required fields per subcommand: commit→message, merge/checkout→branch, \
             restore→paths, branch delete/create→branch.",
            json!({
                "type": "object",
                "properties": {
                    "subcommand": {
                        "type": "string",
                        // The enum lists every value the runtime accepts in
                        // this field — the 10 canonical subcommands PLUS the
                        // branch/stash action names — so strict-schema
                        // providers (constrained decoding) can still emit the
                        // forgiving forms.
                        "enum": ["status", "diff", "log", "commit", "merge", "checkout", "stash", "branch", "push", "restore", "list", "delete", "create", "pop", "drop"],
                        "description": "The git subcommand. May also be given in `action`. Branch/stash action names work here too (\"delete\" = branch delete, \"pop\" = stash pop)."
                    },
                    "message": {"type": "string", "description": "Required for commit; optional custom merge-commit message."},
                    "branch": {"type": "string", "description": "Required for merge, checkout, branch delete/create; optional for push (default: current branch)."},
                    "action": {"type": "string", "description": "Action for stash (push/pop/drop/list, default push) or branch (list/delete/create, default list). A subcommand name here means its default action."},
                    "remote": {"type": "string", "description": "Remote name for push (default origin)."},
                    "source": {
                        "type": "string",
                        "description": "restore only: the tree-ish to restore FROM (branch, tag, commit, HEAD~1). Default: the index (worktree restores) or HEAD (staged restores)."
                    },
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "restore only: REQUIRED. The files/dirs to restore (e.g. [\"src/lib.rs\"]); passed after `--` so they are never read as flags. A singular `path` string is also accepted."
                    },
                    "target": {
                        "type": "string",
                        "description": "restore only: where restored content lands — \"worktree\" (default), \"staged\" (unstage), or \"both\" (discard staged + worktree changes).",
                        "enum": ["worktree", "staged", "both"]
                    },
                    "args": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Extra flags/refs/paths for READ queries only (status, diff, log, branch list), appended after the built-in defaults (e.g. [\"--stat\"], [\"-5\"], [\"main..feat\", \"--\", \"src/\"]). Refused for write subcommands; --output/-o/--exec/--no-index/--ext-diff/--textconv rejected as unsafe. branch list: read-only listing flags only (positionals refused — they create branches); split combined short flags (use [\"-a\",\"-v\"] not \"-av\")."
                    }
                },
                "required": ["subcommand"],
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // The git tool's safety depends on the subcommand, but the Tool trait
        // returns a single level. We mark it NeedsApproval so that commit
        // (a write op) goes through approval. Read ops are still safe; the
        // agent loop can auto-run reads by checking the subcommand if desired.
        // For simplicity, all git ops require approval here.
        SafetyLevel::NeedsApproval
    }

    fn never_auto_for(&self, args: &serde_json::Value) -> bool {
        // Core operations — git subcommands that land commits on `main` or
        // push to a remote — must always show the interactive approval prompt,
        // even under Autonomous mode or a matching safety rule. This is the
        // subcommand-aware companion to `never_auto`: the git tool is one
        // tool with many subcommands, only some of which are core.
        //
        // The list is runtime-mutable (shared via an `Arc<RwLock<...>>` with
        // the factory) so a Settings → Git save takes effect on the next tool
        // call without a registry rebuild. Defaults to `["merge", "push"]`.
        // Matched case-insensitively against the resolved subcommand (either
        // field, so `action: "push"` still forces the prompt).
        let sub = match resolve_git_subcommand(args) {
            Some(s) => s,
            None => return false, // defensive: no subcommand → not a core op
        };
        let ops = self
            .core_operations
            .read()
            .expect("core_operations lock poisoned");
        ops.iter().any(|c| c.eq_ignore_ascii_case(&sub))
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        // Resolve the subcommand from the raw arguments FIRST — the
        // `subcommand`/`action` fields are interchangeable carriers of the
        // subcommand name, and branch/stash action names are accepted in
        // either field. This runs before deserialization so a missing
        // `subcommand` field yields a helpful message instead of serde's
        // hard "missing field `subcommand`" failure.
        let subcommand = match resolve_git_subcommand(&args) {
            Some(s) => s,
            None => {
                let given = args
                    .get("subcommand")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        args.get("action")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                    });
                return match given {
                    Some(s) => ToolResult::error(format!(
                        "unknown git subcommand '{s}'. Use: status, diff, log, commit, merge, \
                         checkout, stash, branch, push, restore (branch/stash actions like \
                         delete or pop are also accepted here)"
                    )),
                    None => ToolResult::error(format!(
                        "git requires a 'subcommand' field (status, diff, log, commit, merge, \
                         checkout, stash, branch, push, restore) — or an 'action' naming one \
                         of them. Per-subcommand required fields: commit→message, \
                         merge/checkout→branch, restore→paths, branch \
                         delete/create→branch"
                    )),
                };
            }
        };

        // For branch/stash, resolve the action from the raw arguments too
        // (the action may be carried in either field, or the subcommand name
        // may be repeated in `action`). An invalid action errors with the
        // valid values; no action at all falls back to the default.
        let action = if matches!(subcommand.as_str(), "branch" | "stash") {
            match resolve_git_action(&subcommand, &args) {
                Some(a) => Some(a),
                None => {
                    let given = args
                        .get("action")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty());
                    if let Some(g) = given {
                        let valid = if subcommand == "branch" {
                            "list, delete, create"
                        } else {
                            "push, pop, drop, list"
                        };
                        return ToolResult::error(format!(
                            "unknown {subcommand} action '{g}'. Use: {valid}"
                        ));
                    }
                    None // no action given → default
                }
            }
        } else {
            None
        };

        // Forgiving restore args: models routinely emit the singular `path`
        // field (as for file tools) instead of the `paths` array. When the
        // subcommand is `restore` and a string `path` is present with no
        // `paths`, lift it into a one-element `paths` array before
        // deserialization — the same forgiving-args spirit as the
        // subcommand/action interchangeability above.
        let mut args = args;
        if subcommand == "restore" {
            let has_paths = args
                .get("paths")
                .map(|v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false))
                .unwrap_or(false);
            if !has_paths {
                if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                    args["paths"] = json!([path]);
                }
            }
        }

        let args: GitArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };

        // Free-form `args` are only forwarded for read queries (status, diff,
        // log, and `branch list`); write subcommands must never receive them —
        // error loudly rather than silently drop (silent dropping was the
        // original defect this parameter fixes). `branch list` is a read too
        // (it only lists branches), so it passes this gate; its args are then
        // validated by an allowlist (validate_branch_list_args) that refuses
        // positionals — a positional on `git branch` would create a branch —
        // and any flag not on the safe-read list. `branch create`/`delete`
        // stay writes and reject args here.
        let is_read_sub = matches!(subcommand.as_str(), "status" | "diff" | "log")
            || (subcommand == "branch" && matches!(action.as_deref(), None | Some("list")));
        if !is_read_sub && args.args.as_ref().is_some_and(|a| !a.is_empty()) {
            return ToolResult::error(format!(
                "args are not supported for git {subcommand} (flags are only forwarded for read \
                 queries: status, diff, log, branch list)"
            ));
        }

        match subcommand.as_str() {
            "status" => {
                let extra = args.args.unwrap_or_default();
                match read_argv(&["status", "--short"], &extra) {
                    Ok(argv) => self.run_git(&argv).await,
                    Err(e) => ToolResult::error(e),
                }
            }
            "diff" => {
                let extra = args.args.unwrap_or_default();
                match read_argv(&["diff"], &extra) {
                    // The user-mandated exception to the tool-output cap: a
                    // truncated diff could hide part of the changes under
                    // review (mirrors git_diff). Never capped.
                    Ok(argv) => self.run_git_uncapped(&argv).await,
                    Err(e) => ToolResult::error(e),
                }
            }
            "log" => {
                let extra = args.args.unwrap_or_default();
                match read_argv(&["log", "--oneline", "-20"], &extra) {
                    Ok(argv) => self.run_git(&argv).await,
                    Err(e) => ToolResult::error(e),
                }
            }
            "commit" => {
                let message = match args.message {
                    Some(m) if !m.is_empty() => m,
                    _ => {
                        return ToolResult::error(
                            "commit requires a 'message' field. Expected arguments: \
                             {\"subcommand\": \"commit\", \"message\": \"<commit message>\"} — \
                             commit takes no other fields (branch is merge/checkout-only, \
                             paths/source/target are restore-only)",
                        )
                    }
                };
                // Stage all changes first so a commit call succeeds without a
                // separate staging step — BUT only when nothing is already
                // staged. `git diff --cached --quiet` exits 0 (success) when the
                // index is clean (nothing staged) → auto-stage with `git add -A`
                // (respects `.gitignore`, stages new/modified/deleted across the
                // whole tree). It exits non-zero when something IS staged → the
                // agent (or a skill) deliberately staged a subset via `shell`,
                // and a blanket `add -A` would override that selection and sweep
                // in stray files. In that case commit ONLY what's staged.
                let nothing_staged = self.run_git(&["diff", "--cached", "--quiet"]).await;
                if nothing_staged.success {
                    let add = self.run_git(&["add", "-A"]).await;
                    if !add.success {
                        return add;
                    }
                }
                // --no-verify skips pre-commit/commit-msg/pre-merge-commit so
                // planted hooks of those kinds never execute via the agent's
                // git path (defense-in-depth on top of the .git write
                // protection). prepare-commit-msg and post-* hooks still run:
                // acceptable because .git writes are refused by the sandbox and
                // shell is the documented approval-gated residual. The user's
                // own terminal git is unaffected.
                self.run_git(&["commit", "--no-verify", "-m", &message]).await
            }
            "merge" => {
                // Merge a branch into the current branch with a merge commit
                // (--no-ff). This is a CORE OPERATION: never_auto_for returns
                // true, so the dispatch layer always forces the approval prompt.
                let branch = match args.branch {
                    Some(b) if !b.is_empty() => b,
                    _ => {
                        return ToolResult::error(
                            "merge requires a 'branch' field (the branch to merge). \
                             Expected arguments: {\"subcommand\": \"merge\", \
                             \"branch\": \"<branch>\", \"message\": \"<optional \
                             merge-commit message>\"}",
                        )
                    }
                };
                if !valid_branch_name(&branch) {
                    return ToolResult::error(format!(
                        "invalid branch name '{branch}': must not start with '-' or contain whitespace"
                    ));
                }
                match args.message {
                    Some(m) if !m.is_empty() => {
                        self.run_git(&["merge", "--no-verify", "--no-ff", &branch, "-m", &m]).await
                    }
                    _ => self.run_git(&["merge", "--no-verify", "--no-ff", &branch]).await,
                }
            }
            "checkout" => {
                let branch = match args.branch {
                    Some(b) if !b.is_empty() => b,
                    _ => {
                        return ToolResult::error(
                            "checkout requires a 'branch' field (the branch to switch \
                             to). Expected arguments: {\"subcommand\": \
                             \"checkout\", \"branch\": \"<branch>\"} — checkout \
                             takes no other fields",
                        )
                    }
                };
                if !valid_branch_name(&branch) {
                    return ToolResult::error(format!(
                        "invalid branch name '{branch}': must not start with '-' or contain whitespace"
                    ));
                }
                self.run_git(&["checkout", &branch]).await
            }
            "restore" => {
                // Restore files from a ref — DISCARDS uncommitted worktree
                // and/or staged changes (or overwrites files with content
                // from another commit). Structured fields only: free-form
                // `args` are rejected for every write subcommand above, and
                // pathspecs go after a mandatory `--` separator so a path
                // can never parse as a flag.
                let paths: Vec<String> = match args.paths {
                    Some(p) if !p.is_empty() => p,
                    _ => {
                        return ToolResult::error(
                            "restore requires a 'paths' field (the files/dirs to \
                             restore). Expected arguments: {\"subcommand\": \
                             \"restore\", \"paths\": [\"<path>\"], \
                             \"source\": \"<optional ref>\", \"target\": \
                             \"<optional worktree|staged>\"}",
                        )
                    }
                };
                for p in &paths {
                    if p.is_empty() || p.starts_with('-') {
                        return ToolResult::error(format!(
                            "invalid restore path '{p}': must not be empty or start with '-'"
                        ));
                    }
                }
                let source = match args.source.as_deref() {
                    // An empty source is treated as absent (git's own
                    // defaults apply: index for worktree, HEAD for staged).
                    Some(s) if !s.is_empty() => {
                        if !valid_branch_name(s) {
                            return ToolResult::error(format!(
                                "invalid source '{s}': must not start with '-' or contain \
                                 whitespace"
                            ));
                        }
                        Some(s)
                    }
                    _ => None,
                };
                let target = args
                    .target
                    .as_deref()
                    .unwrap_or("worktree")
                    .to_ascii_lowercase();
                let mut argv: Vec<&str> = match target.as_str() {
                    "worktree" => vec!["restore"],
                    "staged" => vec!["restore", "--staged"],
                    "both" => vec!["restore", "--staged", "--worktree"],
                    other => {
                        return ToolResult::error(format!(
                            "unknown restore target '{other}'. Use: worktree, staged, both"
                        ))
                    }
                };
                let source_flag;
                if let Some(src) = source {
                    source_flag = format!("--source={src}");
                    argv.push(&source_flag);
                }
                argv.push("--");
                for p in &paths {
                    argv.push(p);
                }
                self.run_git(&argv).await
            }
            "stash" => {
                // action defaults to "push"; supports push/pop/drop/list.
                // Resolved forgivingly from the raw args (either field, or the
                // subcommand name repeated in `action`).
                let action = action.unwrap_or_else(|| "push".to_string());
                match action.as_str() {
                    "push" => self.run_git(&["stash", "push"]).await,
                    "pop" => self.run_git(&["stash", "pop"]).await,
                    "drop" => self.run_git(&["stash", "drop"]).await,
                    "list" => self.run_git(&["stash", "list"]).await,
                    other => ToolResult::error(format!(
                        "unknown stash action '{other}'. Use: push, pop, drop, list"
                    )),
                }
            }
            "branch" => {
                // action defaults to "list"; supports list/delete/create.
                // `git branch` with no args lists branches, so listing is the
                // natural default when no action is given (mirrors stash's
                // default-to-push pattern). Resolved forgivingly from the raw
                // args (either field, or the subcommand name repeated in
                // `action`).
                let action = action.unwrap_or_else(|| "list".to_string());
                match action.as_str() {
                    "list" => {
                        let extra = args.args.unwrap_or_default();
                        match branch_list_argv(&extra) {
                            Ok(argv) => self.run_git(&argv).await,
                            Err(e) => ToolResult::error(e),
                        }
                    }
                    "delete" => {
                        let name =
                            match args.branch {
                                Some(n) if !n.is_empty() => n,
                                _ => return ToolResult::error(
                                    "branch delete requires a 'branch' field (the name \
                                     to delete). Expected arguments: {\"subcommand\": \
                                     \"branch\", \"action\": \"delete\", \
                                     \"branch\": \"<name>\"}",
                                ),
                            };
                        if !valid_branch_name(&name) {
                            return ToolResult::error(format!(
                                "invalid branch name '{name}': must not start with '-' or contain whitespace"
                            ));
                        }
                        self.run_git(&["branch", "-d", &name]).await
                    }
                    "create" => {
                        let name =
                            match args.branch {
                                Some(n) if !n.is_empty() => n,
                                _ => return ToolResult::error(
                                    "branch create requires a 'branch' field (the name \
                                     to create). Expected arguments: {\"subcommand\": \
                                     \"branch\", \"action\": \"create\", \
                                     \"branch\": \"<name>\"}",
                                ),
                            };
                        if !valid_branch_name(&name) {
                            return ToolResult::error(format!(
                                "invalid branch name '{name}': must not start with '-' or contain whitespace"
                            ));
                        }
                        // Creates a branch from the current HEAD (does not switch).
                        self.run_git(&["branch", &name]).await
                    }
                    other => ToolResult::error(format!(
                        "unknown branch action '{other}'. Use: list, delete, create"
                    )),
                }
            }
            "push" => {
                // Push to a remote. This is a CORE OPERATION: never_auto_for
                // returns true, so the dispatch layer always forces the
                // approval prompt.
                let remote = args
                    .remote
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("origin");
                if !valid_branch_name(remote) {
                    return ToolResult::error(format!(
                        "invalid remote name '{remote}': must not start with '-' or contain whitespace"
                    ));
                }
                match args.branch {
                    Some(b) if !b.is_empty() => {
                        if !valid_branch_name(&b) {
                            return ToolResult::error(format!(
                                "invalid branch name '{b}': must not start with '-' or contain whitespace"
                            ));
                        }
                        self.run_git(&["push", remote, &b]).await
                    }
                    _ => self.run_git(&["push", remote]).await,
                }
            }
            other => ToolResult::error(format!(
                "unknown git subcommand '{other}'. Use: status, diff, log, commit, merge, \
                 checkout, stash, branch, push, restore"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &std::path::Path) -> GitTool {
        GitTool::new(dir)
    }

    // NOTE: git is an external, battle-tested tool — these tests do NOT spawn
    // git subprocesses. They cover the git TOOL's own logic only: subcommand/
    // action resolution, the core-operations gating (never_auto_for), arg
    // validation (the read-args denylist + branch-list allowlist), and the
    // structured-field validation that fires BEFORE any git invocation. Every
    // test below either calls a pure helper or asserts a validation rejection
    // that happens before run_git — no repo fixture, no git subprocess.

    // ---- shared resolvers: subcommand/action interchangeability ----

    #[test]
    fn resolve_subcommand_from_either_field() {
        // Known subcommand in the `subcommand` field.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "status"})).as_deref(),
            Some("status")
        );
        // Known subcommand in the `action` field.
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "status"})).as_deref(),
            Some("status")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "log"})).as_deref(),
            Some("log")
        );
        // Branch action names in either field map to `branch`.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "delete"})).as_deref(),
            Some("branch")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "delete"})).as_deref(),
            Some("branch")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "create"})).as_deref(),
            Some("branch")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "list"})).as_deref(),
            Some("branch")
        );
        // Stash action names in either field map to `stash`.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "pop"})).as_deref(),
            Some("stash")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "pop"})).as_deref(),
            Some("stash")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "drop"})).as_deref(),
            Some("stash")
        );
        // `push` is a subcommand first, even in the action field.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "push"})).as_deref(),
            Some("push")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "push"})).as_deref(),
            Some("push")
        );
        // `restore` resolves from either field like every other subcommand.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "restore"})).as_deref(),
            Some("restore")
        );
        assert_eq!(
            resolve_git_subcommand(&json!({"action": "restore"})).as_deref(),
            Some("restore")
        );
        // The subcommand field takes precedence over the action field.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "branch", "action": "status"})).as_deref(),
            Some("branch")
        );
        // Nothing recognizable → None.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "frobnicate"})),
            None
        );
        assert_eq!(resolve_git_subcommand(&json!({})), None);
        assert_eq!(resolve_git_subcommand(&json!({"action": ""})), None);
        // Empty subcommand field with a valid action field still resolves.
        assert_eq!(
            resolve_git_subcommand(&json!({"subcommand": "", "action": "status"})).as_deref(),
            Some("status")
        );
    }

    #[test]
    fn resolve_action_from_either_field() {
        // Branch: no action → default list.
        assert_eq!(
            resolve_git_action("branch", &json!({"subcommand": "branch"})).as_deref(),
            Some("list")
        );
        // Branch: action field repeating the subcommand name → default list.
        assert_eq!(
            resolve_git_action(
                "branch",
                &json!({"subcommand": "branch", "action": "branch"})
            )
            .as_deref(),
            Some("list")
        );
        // Branch: explicit action wins.
        assert_eq!(
            resolve_git_action(
                "branch",
                &json!({"subcommand": "branch", "action": "delete"})
            )
            .as_deref(),
            Some("delete")
        );
        // Branch: swapped call — action name in the subcommand field.
        assert_eq!(
            resolve_git_action(
                "branch",
                &json!({"subcommand": "delete", "action": "branch"})
            )
            .as_deref(),
            Some("delete")
        );
        assert_eq!(
            resolve_git_action("branch", &json!({"subcommand": "delete"})).as_deref(),
            Some("delete")
        );
        // Branch: action-only call (subcommand resolved from the action field).
        assert_eq!(
            resolve_git_action("branch", &json!({"action": "create"})).as_deref(),
            Some("create")
        );
        // Branch: invalid action → None.
        assert_eq!(
            resolve_git_action("branch", &json!({"action": "frobnicate"})),
            None
        );
        // Stash: no action → default push.
        assert_eq!(
            resolve_git_action("stash", &json!({"subcommand": "stash"})).as_deref(),
            Some("push")
        );
        // Stash: action field repeating the subcommand name → default push.
        assert_eq!(
            resolve_git_action("stash", &json!({"subcommand": "stash", "action": "stash"}))
                .as_deref(),
            Some("push")
        );
        // Stash: swapped call — action name in the subcommand field.
        assert_eq!(
            resolve_git_action("stash", &json!({"subcommand": "pop", "action": "stash"}))
                .as_deref(),
            Some("pop")
        );
        assert_eq!(
            resolve_git_action("stash", &json!({"action": "pop"})).as_deref(),
            Some("pop")
        );
        // Stash: empty action → default push.
        assert_eq!(
            resolve_git_action("stash", &json!({"action": ""})).as_deref(),
            Some("push")
        );
        // Stash: invalid action → None.
        assert_eq!(
            resolve_git_action("stash", &json!({"action": "frobnicate"})),
            None
        );
    }

    #[tokio::test]
    async fn missing_subcommand_errors_with_valid_values() {
        // A call with neither field naming a subcommand must not hard-fail
        // with serde's "missing field `subcommand`" — it lists the valid
        // values instead.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(result.output.contains("requires a 'subcommand'"));
        assert!(result.output.contains("status"));
        assert!(result.output.contains("push"));
        // The error also carries the per-subcommand required-fields map so
        // the model knows what each subcommand needs without re-reading the
        // full schema.
        assert!(result.output.contains("commit→message"));
        assert!(result.output.contains("restore→paths"));
    }

    #[test]
    fn schema_requires_subcommand_and_maps_required_fields() {
        // Tool-call robustness: the parameters schema carries
        // `required: ["subcommand"]` (uniformly true — every call needs a
        // subcommand; the runtime resolves it first and errors helpfully,
        // so nothing breaks, and strict-schema providers gain one
        // unambiguous required field), and the description carries the
        // per-subcommand required-fields map.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let schema = tool.schema();
        let required = schema
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .expect("required array present");
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("subcommand"));
        assert!(schema.description.contains("commit→message"));
        assert!(schema.description.contains("restore→paths"));
    }

    #[tokio::test]
    async fn invalid_type_errors() {
        // A genuine serde type error still reports "invalid arguments".
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "status", "branch": 42}))
            .await;
        assert!(!result.success);
        // Sanitized (plan 21118961): the instructive form — never
        // serde's raw "invalid type:" vocabulary.
        assert!(
            result.output.starts_with("Error: The tool 'git' failed"),
            "{}",
            result.output
        );
        assert!(result.output.contains("must be a string, not an integer"));
        assert!(!result.output.contains("invalid type:"));
    }

    #[tokio::test]
    async fn unknown_subcommand_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"subcommand": "frobnicate"})).await;
        assert!(!result.success);
        assert!(result.output.contains("unknown git subcommand"));
        // The error lists the valid values so the model can self-correct.
        assert!(result.output.contains("status"));
        assert!(result.output.contains("push"));
        assert!(result.output.contains("delete"));
    }

    #[tokio::test]
    async fn commit_requires_message() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"subcommand": "commit"})).await;
        assert!(!result.success);
        assert!(result.output.contains("requires a 'message'"));
        // Expected-shape enrichment: the error carries the exact expected
        // arguments plus the sibling-field rejection note (the live incident
        // conflated a restore-only `target` into a commit call).
        assert!(result.output.contains("Expected arguments"));
        assert!(result.output.contains("\"message\""));
        assert!(result.output.contains("restore-only"));
    }

    // ---- never_auto_for: core operations (merge/push) force the prompt ----

    #[test]
    fn never_auto_for_true_for_core_operations() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        // merge + push are core operations → always force the approval prompt.
        assert!(tool.never_auto_for(&json!({"subcommand": "merge", "branch": "feat"})));
        assert!(tool.never_auto_for(&json!({"subcommand": "push"})));
        assert!(tool
            .never_auto_for(&json!({"subcommand": "push", "remote": "origin", "branch": "main"})));
    }

    #[test]
    fn never_auto_for_resolves_action_field() {
        // The subcommand may arrive in the `action` field — core-op gating
        // must still apply (a swapped `action: "push"` must force the prompt).
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        assert!(tool.never_auto_for(&json!({"action": "push"})));
        assert!(tool.never_auto_for(&json!({"action": "merge", "branch": "feat"})));
        // Non-core subcommands in the action field stay non-core.
        assert!(!tool.never_auto_for(&json!({"action": "status"})));
        assert!(!tool.never_auto_for(&json!({"action": "branch"})));
        // Branch/stash action names resolve to branch/stash — never core.
        assert!(!tool.never_auto_for(&json!({"subcommand": "pop"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "delete"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "stash", "action": "push"})));
    }

    #[test]
    fn never_auto_for_false_for_non_core_subcommands() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        for sub in [
            "status", "diff", "log", "commit", "checkout", "stash", "branch",
        ] {
            assert!(
                !tool.never_auto_for(&json!({"subcommand": sub})),
                "{sub} should not be a core operation"
            );
        }
        // Missing subcommand → not a core operation (defensive default).
        assert!(!tool.never_auto_for(&json!({})));
    }

    // ---- never_auto_for: the core-operations list is runtime-configurable ----

    #[test]
    fn never_auto_for_respects_configured_list() {
        // A GitTool built with a custom core-operations handle must consult it
        // instead of the hardcoded merge/push default. Here `checkout` is added
        // to the list, so it forces the prompt; `status` is not, so it doesn't.
        let dir = tempdir().unwrap();
        let ops = Arc::new(RwLock::new(vec![
            "merge".to_string(),
            "push".to_string(),
            "checkout".to_string(),
        ]));
        let tool = GitTool::with_core_operations(dir.path(), ops);
        assert!(tool.never_auto_for(&json!({"subcommand": "merge"})));
        assert!(tool.never_auto_for(&json!({"subcommand": "push"})));
        assert!(tool.never_auto_for(&json!({"subcommand": "checkout"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "status"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "commit"})));
    }

    #[test]
    fn never_auto_for_empty_list_forces_nothing() {
        // An empty core-operations list means no subcommand forces the prompt
        // — not even merge/push. This is the "fully opt out of core-op gating"
        // extreme; the default list keeps merge/push gated.
        let dir = tempdir().unwrap();
        let ops = Arc::new(RwLock::new(Vec::new()));
        let tool = GitTool::with_core_operations(dir.path(), ops);
        assert!(!tool.never_auto_for(&json!({"subcommand": "merge"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "push"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "status"})));
    }

    #[test]
    fn never_auto_for_live_update_takes_effect_without_rebuild() {
        // The core-operations handle is shared (Arc<RwLock>), so mutating it
        // (as save_settings does via factory.set_core_operations) is observed
        // by an already-built GitTool on its next never_auto_for check — no
        // registry rebuild needed.
        let dir = tempdir().unwrap();
        let ops = Arc::new(RwLock::new(vec!["merge".to_string(), "push".to_string()]));
        let tool = GitTool::with_core_operations(dir.path(), ops.clone());
        // Initially checkout is NOT a core op.
        assert!(!tool.never_auto_for(&json!({"subcommand": "checkout"})));
        // Mutate the shared handle (mirrors factory.set_core_operations).
        *ops.write().unwrap() = vec![
            "merge".to_string(),
            "push".to_string(),
            "checkout".to_string(),
        ];
        // Now checkout IS a core op — observed live.
        assert!(tool.never_auto_for(&json!({"subcommand": "checkout"})));
    }

    #[test]
    fn never_auto_for_matches_case_insensitively() {
        // The configured list is matched case-insensitively against the
        // subcommand, so a config value of "Merge" still gates "merge".
        let dir = tempdir().unwrap();
        let ops = Arc::new(RwLock::new(vec!["Merge".to_string(), "PUSH".to_string()]));
        let tool = GitTool::with_core_operations(dir.path(), ops);
        assert!(tool.never_auto_for(&json!({"subcommand": "merge"})));
        assert!(tool.never_auto_for(&json!({"subcommand": "MERGE"})));
        assert!(tool.never_auto_for(&json!({"subcommand": "push"})));
        assert!(!tool.never_auto_for(&json!({"subcommand": "status"})));
    }

    // ---- merge ----

    #[tokio::test]
    async fn merge_requires_branch() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"subcommand": "merge"})).await;
        assert!(!result.success);
        assert!(result.output.contains("requires a 'branch'"));
        // Expected-shape enrichment: the error carries the exact expected
        // arguments for the subcommand.
        assert!(result.output.contains("Expected arguments"));
        assert!(result.output.contains("\"branch\""));
    }

    #[tokio::test]
    async fn merge_rejects_flag_injection() {
        // A branch starting with '-' would be parsed by git as a flag —
        // valid_branch_name rejects it before git is ever invoked (no repo
        // needed; git is an external tool we don't re-verify here).
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "merge", "branch": "--no-commit"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid branch name"),
            "got: {}",
            result.output
        );
    }

    // ---- checkout ----

    #[tokio::test]
    async fn checkout_rejects_flag_injection() {
        // A branch starting with '-' would be parsed by git as a flag —
        // valid_branch_name rejects it before git is ever invoked (no repo
        // needed; git is an external tool we don't re-verify here).
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "checkout", "branch": "-b"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid branch name"),
            "got: {}",
            result.output
        );
    }

    // ---- stash ----

    #[tokio::test]
    async fn stash_unknown_action_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "stash", "action": "frobnicate"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("unknown stash action"));
    }

    // ---- branch ----

    #[tokio::test]
    async fn branch_rejects_flag_injection() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "branch", "action": "create", "branch": "--force"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid branch name"),
            "got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn branch_unknown_action_errors() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "branch", "action": "frobnicate"}))
            .await;
        assert!(!result.success);
        assert!(result.output.contains("unknown branch action"));
        // The error lists the valid values so the model can self-correct.
        assert!(result.output.contains("list, delete, create"));
    }

    // ---- push ----

    #[tokio::test]
    async fn push_rejects_flag_injection_in_remote() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "push", "remote": "--all"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid remote name"),
            "got: {}",
            result.output
        );
    }

    // ---- read-side `args` validation (status / diff / log / branch list) ----
    //
    // These cover the arg-validation layer (the denylist for status/diff/log
    // and the allowlist for branch list) — pure logic that fires before any
    // git invocation, so no repo fixture is needed.

    #[test]
    fn branch_list_allowlist_rejects_positionals_and_write_flags() {
        // The branch-list allowlist (validate_branch_list_args) is a pure-logic,
        // security-relevant validator: a positional would create a branch via
        // `git branch <positional>`, and write flags like --set-upstream-to=
        // mutate the current branch. Tested directly (no git spawn).
        // Positional → refused.
        assert!(validate_branch_list_args(&["newbranch".into()]).is_err());
        assert!(validate_branch_list_args(&["newbranch".into()])
            .unwrap_err()
            .contains("positional"));
        // Write/mutate flags → refused (not on the read-only allowlist).
        for flag in [
            "--set-upstream-to=origin/main",
            "--unset-upstream",
            "--edit-description",
            "-d",
            "-m",
            "--delete",
            "--move",
            "--force",
        ] {
            let err = validate_branch_list_args(&[flag.into()]).unwrap_err();
            assert!(err.contains("not allowed"), "{flag}: {err}");
        }
    }

    #[test]
    fn branch_list_allowlist_accepts_read_only_flags() {
        // The accept path: known-safe read-only listing flags pass.
        for ok in [
            "-v",
            "-vv",
            "--verbose",
            "-a",
            "--all",
            "-r",
            "--remotes",
            "--list",
            "-q",
            "--quiet",
            "--no-color",
            "--merged",
            "--no-merged",
            "--contains",
            "--no-contains",
            "-i",
            "--ignore-case",
        ] {
            assert!(
                validate_branch_list_args(&[ok.into()]).is_ok(),
                "{ok} should be allowed"
            );
        }
        // Prefix matches: --format=/--sort=/--color= with a value.
        assert!(validate_branch_list_args(&["--format=%(refname:short)".into()]).is_ok());
        assert!(validate_branch_list_args(&["--sort=-committerdate".into()]).is_ok());
        assert!(validate_branch_list_args(&["--color=always".into()]).is_ok());
        // Multiple flags at once.
        assert!(validate_branch_list_args(&["-vv".into(), "-a".into()]).is_ok());
    }

    #[test]
    fn branch_list_argv_builds_correct_argv() {
        // branch_list_argv mirrors read_argv: ["branch"] + validated args.
        // No args → bare ["branch"] (the no-args path, unchanged behavior).
        assert_eq!(branch_list_argv(&[]).unwrap(), vec!["branch"]);
        // With args → ["branch", "-vv"].
        assert_eq!(
            branch_list_argv(&["-vv".into()]).unwrap(),
            vec!["branch", "-vv"]
        );
        // A rejected arg propagates the error (no argv built).
        assert!(branch_list_argv(&["newbranch".into()]).is_err());
    }

    #[tokio::test]
    async fn read_args_reject_write_exec_flags() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        for bad in [
            "--output=C:/evil.patch",
            "--exec=evil",
            "-o",
            "-ox.patch",
            "--no-index",
            "--ext-diff",
            "--textconv",
        ] {
            let result = tool
                .execute(json!({"subcommand": "log", "args": [bad]}))
                .await;
            assert!(!result.success, "{bad} must be rejected");
            assert!(
                result.output.contains("unsafe flag"),
                "{bad}: {}",
                result.output
            );
        }
        // The same denial on the other read subcommands.
        let result = tool
            .execute(json!({"subcommand": "diff", "args": ["--output=x"]}))
            .await;
        assert!(!result.success);
        let result = tool
            .execute(json!({"subcommand": "status", "args": ["--output=x"]}))
            .await;
        assert!(!result.success);
    }

    #[tokio::test]
    async fn args_rejected_on_write_subcommands() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        for call in [
            json!({"subcommand": "commit", "message": "x", "args": ["--amend"]}),
            json!({"subcommand": "merge", "branch": "feat", "args": ["--squash"]}),
            json!({"subcommand": "checkout", "branch": "main", "args": ["--force"]}),
            json!({"subcommand": "branch", "action": "create", "branch": "x", "args": ["-f"]}),
            json!({"action": "delete", "branch": "x", "args": ["-f"]}),
            json!({"subcommand": "push", "args": ["--force"]}),
            json!({"subcommand": "stash", "args": ["-u"]}),
        ] {
            let result = tool.execute(call.clone()).await;
            assert!(!result.success, "{call} must be rejected");
            assert!(
                result.output.contains("not supported"),
                "{call}: {}",
                result.output
            );
        }
    }

    // ---- restore (structured-field validation only) ----

    #[tokio::test]
    async fn restore_requires_paths() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(json!({"subcommand": "restore"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("requires a 'paths'"),
            "got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn restore_invalid_target_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "restore", "paths": ["a.txt"], "target": "frobnicate"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("unknown restore target"),
            "got: {}",
            result.output
        );
        assert!(result.output.contains("worktree"));
        assert!(result.output.contains("staged"));
        assert!(result.output.contains("both"));
    }

    #[tokio::test]
    async fn restore_rejects_flag_like_values() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        // A flag-shaped source must be rejected before git runs.
        let result = tool
            .execute(json!({"subcommand": "restore", "paths": ["a.txt"], "source": "-C"}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid source"),
            "got: {}",
            result.output
        );
        // Flag-shaped pathspecs too.
        let result = tool
            .execute(json!({"subcommand": "restore", "paths": ["-m"]}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("invalid restore path"),
            "got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn args_rejected_on_restore() {
        // restore is a WRITE subcommand: free-form args stay rejected (the
        // read-only-args decision), even though it takes paths of its own —
        // those are structured fields, not forwarded flags.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"subcommand": "restore", "paths": ["b.txt"], "args": ["--ours"]}))
            .await;
        assert!(!result.success);
        assert!(
            result.output.contains("not supported"),
            "got: {}",
            result.output
        );
    }
}
