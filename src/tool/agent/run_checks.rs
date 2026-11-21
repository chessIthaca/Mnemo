// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `run_checks` — run the project's check commands in ONE call (plan 5b85ab61
//! step 3).
//!
//! A worker used to type each `.coding/checks.toml` command into `shell`, read a
//! raw wall of output, and hope the host counted the run. `run_checks` runs
//! EVERY allow-listed check command of the session, in order, from the checkout
//! root, and answers with one block per command — `PASS`/`FAIL`, the exit code
//! and the run time — followed by the failing lines pulled out of the output
//! (a compiler `error`/`warning` header with its ` --> file:line:col`, failing
//! test names, jest/vitest failures, svelte-check errors), then a
//! `N passed, M failed` summary.
//!
//! # Recorded checks
//!
//! The tree probe is the same one `shell` runs for a counted check: the content
//! tree BEFORE and AFTER each command rides the result's `data`. The team pump
//! turns an entry that exited 0, ran the exact allow-listed command from the
//! checkout and measured an unchanged tree into a recorded check against the
//! tip (`pump::counted_checks`) — exactly what the equivalent counted `shell`
//! run records, no more.
//!
//! A passing command that changed the tree cannot certify the commit: its block
//! says `- changed files; not recorded` and the pump ignores it.
//!
//! # Safety
//!
//! `NeedsApproval`, like `shell`: it executes project commands. The commands are
//! never model text — they are the session's validated allow-list, the exact
//! strings `shell` counts, and the tool takes no command argument of its own.
//! One guard follows from that: an entry matching a configured core git
//! operation (`git push`, ...) is REFUSED instead of run, so the tool can never
//! become a way around the `never_auto_for` gate `shell` and `git` enforce. The
//! remaining checks still run.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::cap_tool_output;
use super::sandbox::Sandbox;
use super::shell::{
    combine_streams, command_is_core_git_op, run_command, validated_check_command, RunLimits,
    RunOutcome, DEFAULT_SILENCE, DEFAULT_TOTAL,
};
use crate::config::MachineConfig;
use crate::host::machine_slots::{wait_note, HeavySlots};
use crate::project::git_ops::content_tree;
use crate::provider::ToolSchema;
use crate::tool::agent::read_files::invalid_args_error;
use crate::tool::agent::tool_contract;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// What an empty check list reports: the fix is the project's own config file,
/// not a different tool.
const NO_CHECKS_MESSAGE: &str = "no checks configured: add commands to .coding/checks.toml";

/// How many diagnostic lines a FAIL block shows before it says how many it
/// dropped.
const MAX_DIAGNOSTIC_LINES: usize = 15;

/// How many raw output lines a FAIL block shows when nothing matched a
/// diagnostic pattern — the plain tail is all the evidence there is then.
const FALLBACK_TAIL_LINES: usize = 20;

/// Arguments for `run_checks`.
#[derive(Debug, Deserialize)]
struct RunChecksArgs {
    /// A short human-readable label of what this call does (e.g. "running the
    /// project checks"). The model fills this in so the UI can show it in the
    /// tool card header.
    ///
    /// Consumed by the frontend from the raw tool-call args (Mnemo2's
    /// `argLabel` helper), so it is never read in Rust — it is kept here so
    /// serde parses and validates it, like `shell`'s and `job`'s.
    #[expect(dead_code, reason = "consumed by the frontend from raw tool-call args")]
    purpose: String,
}

/// `run_checks` — run every allow-listed check command in one call.
pub struct RunChecksTool {
    sandbox: Sandbox,
    /// The session's check commands, validated for exact allow-list membership
    /// (the same filter and the same strings `ShellTool::with_check_commands`
    /// uses, so a run here can never be wider than a counted `shell` run).
    check_commands: Vec<String>,
    /// The shared core-operation list `ShellTool` holds: a check command that
    /// IS a core git operation is refused rather than run.
    core_operations: Arc<RwLock<Vec<String>>>,
    /// The silence limit and the hard cap applied to every check run (plan
    /// 5b85ab61): the shell defaults unless a test overrides them.
    limits: RunLimits,
    /// Machine-wide heavy-command limit (the `[general.machine]` section, plan
    /// d3336a8a). Every check run is heavy, so every check waits for one of the
    /// machine's slots.
    machine: Arc<RwLock<MachineConfig>>,
    /// The machine's heavy-slot directory (`<config dir>/run/heavy`). `None`
    /// (headless, unit tests) runs checks unslotted.
    slots_dir: Option<PathBuf>,
}

impl RunChecksTool {
    /// A tool confined to `sandbox`, using the shell's [`DEFAULT_SILENCE`] and
    /// [`DEFAULT_TOTAL`] limits.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            check_commands: Vec::new(),
            core_operations: Arc::new(RwLock::new(vec!["merge".to_string(), "push".to_string()])),
            limits: RunLimits {
                silence: Some(DEFAULT_SILENCE),
                total: DEFAULT_TOTAL,
            },
            machine: Arc::new(RwLock::new(MachineConfig::default())),
            slots_dir: None,
        }
    }

    /// Wire the shared `[general.machine]` handle and the machine's slot
    /// directory: EVERY check command is heavy, so each one waits for a free
    /// machine slot before it spawns (plan d3336a8a step 3). The slot is taken
    /// per command, so two instances can interleave their check runs.
    pub fn with_machine(mut self, cfg: Arc<RwLock<MachineConfig>>, slots_dir: PathBuf) -> Self {
        self.machine = cfg;
        self.slots_dir = Some(slots_dir);
        self
    }

    /// The heavy-slot policy for one check command: `None` when the machine
    /// handle is not wired.
    fn heavy_slots(&self) -> Option<HeavySlots> {
        let dir = self.slots_dir.clone()?;
        let slots = self
            .machine
            .read()
            .expect("machine config lock poisoned")
            .heavy_commands;
        Some(HeavySlots::new(dir, slots))
    }

    /// The session's check commands to run, in order. Invalid entries (chained,
    /// piped, redirected, or control-character commands) are dropped exactly as
    /// `shell` drops them.
    pub fn with_check_commands(mut self, commands: Vec<String>) -> Self {
        self.check_commands = commands
            .into_iter()
            .filter_map(|command| validated_check_command(&command).map(str::to_string))
            .collect();
        self
    }

    /// Wire the shared core-operation list (mirrors `ShellTool::with_core_operations`)
    /// so a `git push` check command is refused under the configured list.
    pub fn with_core_operations(mut self, operations: Arc<RwLock<Vec<String>>>) -> Self {
        self.core_operations = operations;
        self
    }

    /// Override the limits independently (primarily for tests): `silence` is
    /// the no-output deadline (`None` disables it) and `total` the hard cap.
    pub fn with_limits(mut self, silence: Option<Duration>, total: Duration) -> Self {
        self.limits = RunLimits { silence, total };
        self
    }

    /// Whether the session configured at least one valid check command — the
    /// factory registers the tool only when it did.
    pub fn has_checks(&self) -> bool {
        !self.check_commands.is_empty()
    }

    /// Whether `command` is one of the configured core git operations, which
    /// must be refused rather than executed.
    fn is_core_git_operation(&self, command: &str) -> bool {
        let operations = self
            .core_operations
            .read()
            .expect("core_operations lock poisoned");
        command_is_core_git_op(command, &operations)
    }
}

#[async_trait]
impl Tool for RunChecksTool {
    fn name(&self) -> &str {
        "run_checks"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "run_checks",
            "Run ALL of the project's check commands (`.coding/checks.toml`) in one call, from the \
             checkout root, and report one PASS/FAIL block per command with the failing lines \
             (compiler errors with file:line, failing test names) and a `N passed, M failed` \
             summary. A passing run whose files are unchanged is recorded as a check against the \
             tip; a check typed into shell counts only when it is the exact command from the \
             checkout root. Each command keeps the shell's own limits.",
            json!({
                "type": "object",
                "properties": {
                    "purpose": {
                        "type": "string",
                        "description": "Short label of what this run does (e.g. 'running the project checks'), for the UI."
                    }
                },
                "required": ["purpose"]
            }),
        )
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        if self.check_commands.is_empty() {
            return ToolResult::error(NO_CHECKS_MESSAGE);
        }
        let args: RunChecksArgs = match serde_json::from_value(args.clone()) {
            Ok(args) => args,
            Err(error) => {
                return ToolResult::error(invalid_args_error(
                    "run_checks",
                    &error,
                    &args,
                    &tool_contract::recovery_hint("purpose"),
                ))
            }
        };
        let _ = &args;
        let cwd = self.sandbox.root().to_path_buf();
        let cwd_text = cwd.display().to_string();
        let mut blocks: Vec<String> = Vec::new();
        let mut entries: Vec<Value> = Vec::new();
        let mut passed = 0usize;
        let mut failed = 0usize;
        for command in &self.check_commands {
            if self.is_core_git_operation(command) {
                failed += 1;
                blocks.push(format!("FAIL {command} (refused: core git operation)"));
                entries.push(json!({
                    "command": command,
                    "exit_code": Value::Null,
                    "tree_before": "",
                    "tree_after": "",
                    "cwd": cwd_text,
                }));
                continue;
            }
            // The tree probe brackets the RUN, not the whole call: `content_tree`
            // copies the index and writes nothing into the checkout, so the two
            // hashes describe exactly what the command changed.
            let tree_before = content_tree(&cwd).await.unwrap_or_default();
            // Every check command is heavy (plan d3336a8a step 3): it waits for
            // one of the machine's slots, held for exactly this command.
            let slots = self.heavy_slots();
            let started = Instant::now();
            let (outcome, waited) = run_command(
                command,
                &cwd,
                self.limits,
                None,
                None,
                slots.as_ref(),
            )
            .await;
            let elapsed = started.elapsed();
            let tree_after = content_tree(&cwd).await.unwrap_or_default();
            let (exit_code, headline, combined) = match outcome {
                RunOutcome::Exited {
                    code,
                    stdout,
                    stderr,
                    ..
                } => {
                    let stdout = String::from_utf8_lossy(&stdout);
                    let stderr = String::from_utf8_lossy(&stderr);
                    (
                        Some(i64::from(code)),
                        format!("exit {code}, {}", seconds(elapsed)),
                        combine_streams(&stdout, &stderr),
                    )
                }
                RunOutcome::SilenceTimeout { stdout, stderr, .. } => {
                    let stdout = String::from_utf8_lossy(&stdout);
                    let stderr = String::from_utf8_lossy(&stderr);
                    (
                        None,
                        format!("silence timeout, {}", seconds(elapsed)),
                        combine_streams(&stdout, &stderr),
                    )
                }
                RunOutcome::TotalTimeout { stdout, stderr, .. } => {
                    let stdout = String::from_utf8_lossy(&stdout);
                    let stderr = String::from_utf8_lossy(&stderr);
                    (
                        None,
                        format!("hard cap, {}", seconds(elapsed)),
                        combine_streams(&stdout, &stderr),
                    )
                }
                RunOutcome::SpawnFailed(message) => {
                    (None, format!("could not start: {message}"), String::new())
                }
            };
            let mut block = if exit_code == Some(0) {
                passed += 1;
                format!("PASS {command} ({headline})")
            } else {
                failed += 1;
                format!("FAIL {command} ({headline})")
            };
            // Only a PASSING run can be recorded at all, so only a passing run
            // needs the note that its tree keeps it out of the records.
            if exit_code == Some(0) && !tree_before.is_empty() && !tree_before.eq(&tree_after) {
                block.push_str(" - changed files; not recorded");
            }
            if exit_code != Some(0) {
                if let Some(diagnostics) = diagnostics_block(&combined) {
                    block.push('\n');
                    block.push_str(&diagnostics);
                }
            }
            // Plan d3336a8a step 3: a check that queued behind another
            // instance's build says how long it waited.
            if let Some(note) = wait_note(waited) {
                block.push('\n');
                block.push_str(&note);
            }
            blocks.push(block);
            entries.push(json!({
                "command": command,
                "exit_code": exit_code,
                "tree_before": tree_before,
                "tree_after": tree_after,
                "cwd": cwd_text,
            }));
        }
        let mut text = blocks.join("\n\n");
        text.push_str(&format!("\n\n{passed} passed, {failed} failed"));
        ToolResult {
            success: true,
            output: cap_tool_output(text),
            data: Some(json!({ "checks": entries })),
        }
    }
}

/// A run time as a block shows it: `0.4s`, `12.3s`.
fn seconds(elapsed: Duration) -> String {
    format!("{:.1}s", elapsed.as_secs_f64())
}

/// The failure detail for a FAIL block: up to [`MAX_DIAGNOSTIC_LINES`] matched
/// diagnostic lines, or — when nothing matched — the last
/// [`FALLBACK_TAIL_LINES`] lines of the raw output. `None` when the output is
/// empty.
fn diagnostics_block(text: &str) -> Option<String> {
    let matched = diagnostic_lines(text);
    if !matched.is_empty() {
        let total = matched.len();
        let mut lines: Vec<String> = matched.into_iter().take(MAX_DIAGNOSTIC_LINES).collect();
        if total > MAX_DIAGNOSTIC_LINES {
            let dropped = total - MAX_DIAGNOSTIC_LINES;
            if let Some(last) = lines.last_mut() {
                *last = format!("... ({dropped} more diagnostic lines)");
            }
        }
        return Some(lines.join("\n"));
    }
    let tail = last_lines(text, FALLBACK_TAIL_LINES);
    (!tail.trim().is_empty()).then_some(tail)
}

/// Every diagnostic line in `text`, in order of appearance.
///
/// Recognised: a `rustc`/clippy `error`/`warning` header (and the ` --> line`
/// that follows it), a failing cargo test (`test <name> ... FAILED`), the test
/// names under a `failures:` marker, a jest/vitest failure line (`FAIL <file>`,
/// `✗`/`×`), and a svelte-check `Error:` line with the `file:line:col` printed
/// above it.
fn diagnostic_lines(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut matched: Vec<String> = Vec::new();
    // Set by a `failures:` marker: the indented names under it are the failing
    // tests, and a blank line (or the next unindented line) ends the list.
    let mut in_failures = false;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        let trimmed = line.trim_start();
        if trimmed == "failures:" {
            in_failures = true;
            matched.push(line.to_string());
            index += 1;
            continue;
        }
        if in_failures {
            if trimmed.is_empty() {
                in_failures = false;
            } else if line.starts_with(char::is_whitespace) {
                matched.push(line.to_string());
                index += 1;
                continue;
            } else {
                in_failures = false;
            }
        }
        if is_rustc_header(trimmed) {
            matched.push(line.to_string());
            // rustc prints the location on one of the next two lines.
            for candidate in lines.iter().skip(index + 1).take(2) {
                if candidate.trim_start().starts_with("-->") {
                    matched.push((*candidate).to_string());
                    break;
                }
            }
        } else if is_failing_test(trimmed) || is_js_failure(trimmed) {
            matched.push(line.to_string());
        } else if trimmed.starts_with("Error:") {
            // svelte-check prints `path:line:col` on the line above the error.
            if let Some(previous) = index.checked_sub(1).map(|previous| lines[previous]) {
                if looks_like_location(previous.trim()) {
                    matched.push(previous.to_string());
                }
            }
            matched.push(line.to_string());
        }
        index += 1;
    }
    matched
}

/// A `rustc`/clippy diagnostic header: `error: ...`, `error[E0425]: ...`,
/// `warning: ...`, `warning[...]: ...`.
fn is_rustc_header(line: &str) -> bool {
    ["error", "warning"].iter().any(|prefix| {
        line.strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with(':') || rest.starts_with('['))
    })
}

/// A failing cargo test line: `test <name> ... FAILED` (the `test result:`
/// summary is not a name).
fn is_failing_test(line: &str) -> bool {
    line.starts_with("test ") && line.ends_with("FAILED") && !line.starts_with("test result")
}

/// A jest/vitest failure line.
fn is_js_failure(line: &str) -> bool {
    line.starts_with("FAIL ") || line.contains('✗') || line.contains('×')
}

/// Whether `line` is a `path:line:col` location (what svelte-check prints
/// above its `Error:` line).
fn looks_like_location(line: &str) -> bool {
    let mut parts = line.rsplit(':');
    let column = parts.next().map(str::trim);
    let number = parts.next().map(str::trim);
    let numeric = |part: Option<&str>| {
        part.is_some_and(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    };
    numeric(number) && numeric(column)
}

/// The last `count` lines of `text`, joined with newlines.
fn last_lines(text: &str, count: usize) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.len() > count {
        lines.drain(..lines.len() - count);
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn tool_in(root: &Path) -> RunChecksTool {
        let sandbox = Sandbox::new(root.to_path_buf()).expect("sandbox");
        RunChecksTool::new(sandbox)
    }

    /// A committed git checkout: `content_tree` needs a real index.
    fn git_repo(root: &Path) {
        fn git(root: &Path, args: &[&str]) {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .expect("run git");
            assert!(status.success(), "git {args:?} failed");
        }
        git(root, &["init", "--quiet"]);
        std::fs::write(root.join("tracked.txt"), "one\n").expect("tracked file");
        git(root, &["add", "tracked.txt"]);
        git(
            root,
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "-m",
                "initial",
            ],
        );
    }

    /// The checkout root as the result's `cwd` canonicalizes to it.
    fn checkout(dir: &TempDir) -> PathBuf {
        dir.path().canonicalize().expect("canonical checkout")
    }

    #[tokio::test]
    async fn reports_pass_and_fail_per_command() {
        let dir = tempfile::tempdir().expect("temp dir");
        git_repo(dir.path());
        std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
        std::fs::write(
            dir.path().join("src/a.rs"),
            "fn main() {\n    // the failure is on the next line\n    missing_value\n}\n",
        )
        .expect("broken source");
        let tool = tool_in(dir.path())
            .with_check_commands(vec!["rustc src/a.rs".to_string(), "git status".to_string()]);

        let result = tool.execute(json!({ "purpose": "run the checks" })).await;

        assert!(
            result.success,
            "the tool itself ran, so the call succeeds: {}",
            result.output
        );
        assert!(
            result.output.contains("FAIL rustc src/a.rs (exit 1, "),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("PASS git status (exit 0, "),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("error[E0425]"),
            "the compiler header is missing: {}",
            result.output
        );
        assert!(
            result.output.replace('\\', "/").contains("src/a.rs:3:5"),
            "the location line is missing: {}",
            result.output
        );
        assert!(
            result.output.contains("1 passed, 1 failed"),
            "{}",
            result.output
        );

        let data = result.data.expect("data");
        let checks = data["checks"].as_array().expect("checks array");
        assert_eq!(checks.len(), 2, "{data}");
        assert_eq!(checks[0]["command"], json!("rustc src/a.rs"));
        assert_eq!(checks[0]["exit_code"], json!(1));
        assert_eq!(checks[1]["command"], json!("git status"));
        assert_eq!(checks[1]["exit_code"], json!(0));
        assert_eq!(checks[1]["tree_before"], checks[1]["tree_after"], "{data}");
        assert!(
            !checks[1]["tree_before"]
                .as_str()
                .expect("tree_before")
                .is_empty(),
            "{data}"
        );
        for entry in checks {
            let cwd = PathBuf::from(entry["cwd"].as_str().expect("cwd"));
            assert_eq!(cwd.canonicalize().expect("canonical cwd"), checkout(&dir));
        }
    }

    /// A cargo-test failure names the failing tests: the `test ... FAILED`
    /// line and the names under `failures:`. Pinned on a fixture because a
    /// real `cargo test` invocation cannot be an allow-listed check command —
    /// a compound line is rejected by `validated_check_command`.
    #[test]
    fn failing_test_names_are_listed() {
        let text = "running 2 tests\n\
                    test ok_case ... ok\n\
                    test module::broken ... FAILED\n\
                    \n\
                    failures:\n\
                    \n\
                    ---- module::broken stdout ----\n\
                    thread 'module::broken' panicked at src/lib.rs:9:5\n\
                    \n\
                    failures:\n\
                    \x20   module::broken\n\
                    \n\
                    test result: FAILED. 1 passed; 1 failed\n";
        let block = diagnostics_block(text).expect("diagnostics");
        assert!(block.contains("test module::broken ... FAILED"), "{block}");
        assert!(
            block.contains("    module::broken"),
            "the failure name is missing: {block}"
        );
    }

    #[tokio::test]
    async fn no_checks_configured_is_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let result = tool_in(dir.path())
            .execute(json!({ "purpose": "run the checks" }))
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(
            result
                .output
                .contains("no checks configured: add commands to .coding/checks.toml"),
            "{}",
            result.output
        );
        // A list whose every entry is ineligible (a chained command) is the
        // same state: nothing to record, nothing to run.
        let chained =
            tool_in(dir.path()).with_check_commands(vec!["git status; echo done".to_string()]);
        assert!(!chained.has_checks());
        let result = chained
            .execute(json!({ "purpose": "run the checks" }))
            .await;
        assert!(!result.success, "{}", result.output);
    }

    #[tokio::test]
    async fn a_check_that_changes_the_tree_is_marked() {
        let dir = tempfile::tempdir().expect("temp dir");
        git_repo(dir.path());
        let command = "git config --file tracked.txt test.value changed";
        let tool = tool_in(dir.path()).with_check_commands(vec![command.to_string()]);

        let result = tool.execute(json!({ "purpose": "change the tree" })).await;

        assert!(result.success, "{}", result.output);
        assert!(result.output.contains("PASS"), "{}", result.output);
        assert!(
            result.output.contains("changed files; not recorded"),
            "{}",
            result.output
        );
        let data = result.data.expect("data");
        let entry = &data["checks"][0];
        assert_ne!(entry["tree_before"], entry["tree_after"], "{data}");
    }

    /// A check command that IS a core git operation is refused, never run —
    /// otherwise a `git push` entry in `.coding/checks.toml` would bypass the
    /// gate `shell` raises — and the other checks still run.
    #[tokio::test]
    async fn a_core_git_op_check_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        git_repo(dir.path());
        let tool = tool_in(dir.path())
            .with_check_commands(vec!["git push".to_string(), "git status".to_string()])
            .with_core_operations(Arc::new(RwLock::new(vec!["push".to_string()])));

        let result = tool.execute(json!({ "purpose": "run the checks" })).await;

        assert!(result.success, "{}", result.output);
        assert!(
            result
                .output
                .contains("FAIL git push (refused: core git operation)"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("PASS git status (exit 0, "),
            "the remaining check still ran: {}",
            result.output
        );
        assert!(
            result.output.contains("1 passed, 1 failed"),
            "{}",
            result.output
        );
        let data = result.data.expect("data");
        let refused = &data["checks"][0];
        assert_eq!(refused["command"], json!("git push"));
        assert!(
            refused["exit_code"].is_null(),
            "a refused check never exits: {data}"
        );
    }

    /// A check that hits the silence limit is one failing block that names the
    /// reason, with no exit code. A passing check after a failing one is
    /// covered by `a_core_git_op_check_is_refused`, which uses the shell's own
    /// five-minute silence limit and so is not load-sensitive.
    #[tokio::test]
    async fn a_silent_check_fails_with_the_silence_timeout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let tool = tool_in(dir.path())
            .with_check_commands(vec![crate::tool::agent::shell::test_sleep(
                Duration::from_secs(30),
            )])
            .with_limits(Some(Duration::from_secs(1)), Duration::from_secs(60));

        let result = tool.execute(json!({ "purpose": "run the checks" })).await;

        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("FAIL ") && result.output.contains("silence timeout"),
            "the silence timeout is not named: {}",
            result.output
        );
        assert!(
            result.output.contains("0 passed, 1 failed"),
            "{}",
            result.output
        );
        let data = result.data.expect("data");
        assert!(
            data["checks"][0]["exit_code"].is_null(),
            "a killed check never exits: {data}"
        );
    }
}
