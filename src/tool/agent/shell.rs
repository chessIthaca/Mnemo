// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `shell` — shell execution (platform-aware: PowerShell on Windows, sh elsewhere).
//!
//! Captures stdout, stderr, and exit code. Goes through approval (mutations).
//! Working directory is confined to the project sandbox. Commands are bounded
//! by a default timeout; on expiry the child is killed (`kill_on_drop`).
//!
//! GREP NUDGE: results of grep-family commands (grep/rg/git grep/findstr/
//! Select-String, including piped segments) carry a one-line advisory
//! pointing at the cheaper dedicated tools — `search` for file-content
//! search, graph_* for symbol wiring. Advisory only; the command itself is
//! never modified and the raw `data` fields stay nudge-free.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::config::ShellFilterConfig;
use crate::provider::ToolSchema;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Default maximum wall-clock time a shell command may run before it is
/// killed. Five minutes covers typical builds/tests without hanging the agent
/// forever on a stuck process.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// The one-line advisory prepended to results of grep-family commands.
/// Steering: `search` is the sandboxed, content-indexed text-search tool;
/// the graph tools answer symbol wiring in one lookup — both cheaper than
/// spawning a grep process and slicing its output by hand.
const GREP_NUDGE: &str = "TIP: for file-content search use the `search` tool (sandboxed, \
     content-indexed, skips build output/deps); for symbol wiring (callers/callees/impact) \
     use graph_search/graph_context — both cheaper than grep.\n";

/// C4: the one-line advisory prepended to results of commands whose output
/// is redirected to a null sink. Redirection (`2>$null`, `>/dev/null`)
/// hides exactly the failure details a retry loop needs — and the tool
/// already caps output for the context budget, so the redirect buys
/// nothing. Counted as the fired-only `shell-redirect` steering marker.
const REDIRECT_NOTE: &str = "TIP: output redirection detected — failure details may be \
     hidden; the tool caps output itself, prefer running the command \
     unredirected.\n";

/// Whether the command redirects stdout/stderr to a null sink — `>$null`,
/// `>/dev/null`, or `>nul` (PowerShell / POSIX / cmd forms; `2>$null`,
/// `1>$null`, `2>&1>$null` all contain one of these substrings). C4: the
/// redirect hides failure details the tool would have surfaced, so the
/// result carries a one-line warning.
fn has_blinding_redirection(command: &str) -> bool {
    command.contains(">$null") || command.contains(">/dev/null") || command.contains(">nul")
}

/// Whether the command invokes a grep-family text search — `grep`/`egrep`/
/// `fgrep`, `rg`, `ag`, `ack`, `git grep`, `findstr`, or PowerShell's
/// `Select-String`/`sls` — as the first token of the command or of any
/// piped segment. Conservative by construction: only leading tokens are
/// inspected, so `rg` never fires inside `cargo` or a path argument, and
/// ordinary build/test commands stay nudge-free.
fn is_grep_family(command: &str) -> bool {
    command.to_ascii_lowercase().split('|').any(|segment| {
        let mut tokens = segment.split_whitespace();
        let first = tokens.next().unwrap_or_default();
        matches!(
            first,
            "grep" | "egrep" | "fgrep" | "rg" | "ag" | "ack" | "findstr" | "select-string" | "sls"
        ) || (first == "git" && tokens.next() == Some("grep"))
    })
}

/// Arguments for `shell`.
#[derive(Debug, Deserialize)]
struct ShellArgs {
    command: String,
    /// Optional working directory (relative to project root, sandboxed).
    #[serde(default)]
    cwd: Option<String>,
    /// A short human-readable label of what this command does (e.g. "running
    /// tests", "building the project"). The model fills this in so the UI can
    /// show it in the tool card header — `shell (running tests)` instead of
    /// just `shell`.
    ///
    /// Consumed by the frontend from the raw tool-call args
    /// (`frontend/src/components/chat/Message.tsx` `argLabel`), so it's never
    /// read in Rust — it's kept here so serde parses/validates it.
    #[serde(default)]
    #[expect(dead_code, reason = "consumed by the frontend from raw tool-call args")]
    purpose: Option<String>,
}

/// The `shell` tool.
pub struct ShellTool {
    sandbox: Sandbox,
    /// Maximum time a single command may run before being killed.
    timeout: Duration,
    /// Shared command-aware output-filter config (the `[shell_filter]`
    /// section). Seeded from the loaded config by the factory; every ShellTool
    /// shares the handle, so a config save is observed on the next command
    /// without a registry rebuild.
    filter_config: Arc<RwLock<ShellFilterConfig>>,
}

impl ShellTool {
    /// Create a shell tool confined to `sandbox`, using [`DEFAULT_TIMEOUT`].
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            filter_config: Arc::new(RwLock::new(ShellFilterConfig::default())),
        }
    }

    /// Override the command timeout (primarily for tests).
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Wire a shared output-filter config handle (mirrors
    /// `GitTool::with_core_operations`). A later `set_shell_filter_config`
    /// on the factory is observed by every built ShellTool on its next
    /// command.
    pub fn with_filter_config(mut self, cfg: Arc<RwLock<ShellFilterConfig>>) -> Self {
        self.filter_config = cfg;
        self
    }

    /// Resolve and sandbox-validate the working directory.
    ///
    /// - `None` / empty → sandbox root.
    /// - Otherwise the path is validated via [`Sandbox::validate`]; traversal
    ///   (`..`), absolute paths outside the root, and non-directories fail.
    fn resolve_cwd(&self, cwd: Option<&str>) -> Result<std::path::PathBuf, String> {
        let cwd = cwd.map(str::trim).filter(|s| !s.is_empty());
        let path = match cwd {
            None => self.sandbox.root().to_path_buf(),
            Some(c) => self
                .sandbox
                .validate(Path::new(c))
                .map_err(|e| format!("cwd validation failed: {e}"))?,
        };
        if !path.is_dir() {
            return Err(format!("cwd is not a directory: {}", path.display()));
        }
        Ok(path)
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "shell",
            "Execute a shell command — PowerShell on Windows, sh on Unix. Requires \
             approval. A result is 'successful' when the command RAN: check the exit code \
             to see whether it failed. Output is capped (~100 KiB) with a truncation note, \
             and well-known commands (cargo build/test, npm test/build, git status) are \
             noise-filtered — errors, warnings and summaries always survive, and the raw \
             output stays in the Output tab. Commands exceeding the timeout are killed.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command to execute."},
                    "cwd": {"type": "string", "description": "Working directory relative to project root (optional)."},
                    "purpose": {"type": "string", "description": "Short label of what the command does (e.g. 'running tests'). Always provide this."}
                },
                "required": ["command", "purpose"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: ShellArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };

        let cwd = match self.resolve_cwd(args.cwd.as_deref()) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(e),
        };

        let (program, flag) = if cfg!(target_os = "windows") {
            ("powershell", "-Command")
        } else {
            ("sh", "-c")
        };

        let mut cmd = Command::new(program);
        cmd.arg(flag)
            .arg(&args.command)
            .current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // On Windows, suppress the console window that would otherwise pop up
        // when spawning a child process from a GUI app (CREATE_NO_WINDOW).
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("failed to execute command: {e}")),
        };

        // Bound the wait. On timeout the `wait_with_output` future is dropped,
        // which drops the Child; `kill_on_drop(true)` then kills the process.
        // The same path covers agent-interrupt cancellation of this future.
        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result,
            Err(_elapsed) => {
                let label = format_duration(self.timeout);
                return ToolResult::error(format!(
                    "command timed out after {label} and was killed"
                ));
            }
        };

        match output {
            Ok(output) => {
                let raw_stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let raw_stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let code = output.status.code().unwrap_or(-1);
                // Command-aware noise filtering (PandaFilter idea, in-process).
                // Shapes ONLY the `output` field shown to the LLM — the raw
                // stdout/stderr (kept separately below) stay untouched in
                // `data` for the frontend, and the command execution itself
                // is unaffected.
                let filter_cfg = self
                    .filter_config
                    .read()
                    .expect("shell filter config lock poisoned")
                    .clone();
                let (stdout, stderr) = if filter_cfg.enabled {
                    super::shell_filter::filter_shell_output(
                        &args.command,
                        &raw_stdout,
                        &raw_stderr,
                        &filter_cfg,
                    )
                } else {
                    (raw_stdout.clone(), raw_stderr.clone())
                };
                let mut combined = if stderr.is_empty() {
                    stdout.clone()
                } else {
                    format!("{stdout}\n[stderr]\n{stderr}")
                };
                // Advisory steering (see GREP_NUDGE): prepended AFTER the
                // command ran, so the nudge rides even a failing grep; the
                // raw stdout/stderr in `data` stay nudge-free.
                if is_grep_family(&args.command) {
                    combined.insert_str(0, GREP_NUDGE);
                }
                // C4: redirection warning — may co-occur with the grep
                // nudge; each note ends with `\n`, so they stack as
                // separate leading lines.
                if has_blinding_redirection(&args.command) {
                    combined.insert_str(0, REDIRECT_NOTE);
                }
                // Cap the displayed output to prevent unbounded context (M3).
                // Preserve full raw data for the LLM's structured `data` field.
                let capped = super::cap_tool_output(format!("{combined}\n[exit code: {code}]"));
                // Always report success when the command executed — the exit
                // code is data for the LLM to interpret, not a tool failure.
                // Many commands return non-zero exit codes while still doing
                // what was intended (e.g. grep finding no matches, PowerShell
                // cmdlets with Write-Error). The LLM decides if it failed.
                ToolResult {
                    success: true,
                    output: capped,
                    data: Some(json!({
                        "exit_code": code,
                        "stdout": raw_stdout,
                        "stderr": raw_stderr
                    })),
                }
            }
            Err(e) => ToolResult::error(format!("failed to execute command: {e}")),
        }
    }
}

/// Human-readable duration for timeout error messages.
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs >= 1.0 && secs.fract() < 1e-9 {
        format!("{}s", secs as u64)
    } else if secs >= 1.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}ms", d.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn tool_in(dir: &std::path::Path) -> ShellTool {
        ShellTool::new(Sandbox::new(dir).unwrap())
    }

    #[test]
    fn grep_family_detector_is_conservative() {
        // Leading tokens (and piped segments) of grep-family commands match.
        assert!(is_grep_family("grep -rn foo src/"));
        assert!(is_grep_family("git grep foo"));
        assert!(is_grep_family("rg pattern --json"));
        assert!(is_grep_family("cargo build 2>&1 | Select-String \"error\""));
        assert!(is_grep_family("findstr /s /i foo *.rs"));
        // Ordinary commands stay nudge-free — `rg` inside `cargo`, or grep
        // words in NON-leading token positions, must never fire.
        assert!(!is_grep_family("cargo test"));
        assert!(!is_grep_family("git status"));
        assert!(!is_grep_family("cat target/rg/config"));
        assert!(!is_grep_family("echo rg grep findstr"));
    }

    #[tokio::test]
    async fn grep_family_results_carry_the_nudge() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        // Piped grep terminates on both PowerShell and sh (grep itself may
        // be absent on Windows — the command still RAN, and the nudge rides
        // the output regardless of the exit code).
        let r = tool
            .execute(json!({
                "command": "echo hi | grep hi",
                "purpose": "nudge smoke test"
            }))
            .await;
        assert!(r.success, "command executed: {}", r.output);
        assert!(r.output.contains("TIP:"), "grep result carries the nudge");
        assert!(r.output.contains("`search`"), "nudge points at search");
    }

    #[test]
    fn redirection_detector_matches_null_sinks_only() {
        // C4: the null-sink redirects that blind failure output.
        assert!(has_blinding_redirection("cargo test 2>$null"));
        assert!(has_blinding_redirection("npm run build 1>$null"));
        assert!(has_blinding_redirection("cmd /c foo >nul"));
        assert!(has_blinding_redirection("sh -c \"make 2>/dev/null\""));
        // Legitimate redirections stay silent.
        assert!(!has_blinding_redirection("cargo test"));
        assert!(!has_blinding_redirection("git status"));
        assert!(!has_blinding_redirection("echo x > out.txt"));
    }

    #[tokio::test]
    async fn redirected_output_carries_the_warning() {
        // C4: commands redirecting to a null sink carry the warning; clean
        // commands never do (the grep nudge may co-occur on its own terms).
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let r = tool
            .execute(json!({
                "command": "echo hi 2>$null",
                "purpose": "redirect warning smoke test"
            }))
            .await;
        assert!(r.success, "command executed: {}", r.output);
        assert!(
            r.output.contains("TIP: output redirection detected"),
            "{}",
            r.output
        );
        let r = tool
            .execute(json!({
                "command": "echo hi",
                "purpose": "clean command smoke test"
            }))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("output redirection detected"),
            "{}",
            r.output
        );
    }

    #[tokio::test]
    async fn ordinary_commands_do_not_carry_the_nudge() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let r = tool
            .execute(json!({"command": "echo hello", "purpose": "nudge control"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(!r.output.contains("TIP:"), "non-grep result stays lean");
    }

    #[tokio::test]
    async fn executes_simple_command() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Output hello"
        } else {
            "echo hello"
        };
        let result = tool.execute(json!({"command": cmd})).await;
        assert!(result.success, "output: {}", result.output);
        assert!(result.output.contains("hello"));
    }

    #[tokio::test]
    async fn captures_exit_code() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "exit 42"
        } else {
            "exit 42"
        };
        let result = tool.execute(json!({"command": cmd})).await;
        // Shell always reports success when the command ran — the exit code
        // is data for the LLM, not a tool failure.
        assert!(result.success, "shell should report success when it ran");
        assert!(result.output.contains("42"));
        assert_eq!(result.data.unwrap()["exit_code"], 42);
    }

    #[tokio::test]
    async fn captures_stderr() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Error 'oops'"
        } else {
            "echo oops 1>&2"
        };
        let result = tool.execute(json!({"command": cmd})).await;
        // The stderr text should appear somewhere in the output.
        assert!(result.output.to_lowercase().contains("oops"));
    }

    #[tokio::test]
    async fn invalid_args_error() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        assert!(result.output.contains("invalid arguments"));
    }

    #[tokio::test]
    async fn parses_purpose_field() {
        // The shell tool accepts an optional `purpose` field. It's not used
        // during execution (it's for the UI), but it must parse without error.
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Output hi"
        } else {
            "echo hi"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "running tests"}))
            .await;
        assert!(result.success, "output: {}", result.output);
    }

    #[tokio::test]
    async fn rejects_cwd_path_traversal() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Output hi"
        } else {
            "echo hi"
        };
        let result = tool.execute(json!({"command": cmd, "cwd": ".."})).await;
        assert!(!result.success, "traversal cwd must be rejected");
        assert!(
            result.output.contains("cwd validation failed"),
            "output: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn rejects_cwd_absolute_outside_project() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Output hi"
        } else {
            "echo hi"
        };
        let outside = if cfg!(windows) {
            "C:\\Windows\\System32"
        } else {
            "/tmp"
        };
        let result = tool.execute(json!({"command": cmd, "cwd": outside})).await;
        assert!(!result.success, "outside absolute cwd must be rejected");
        assert!(
            result.output.contains("cwd validation failed"),
            "output: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn accepts_relative_cwd_inside_project() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        // Marker file so we can confirm the command ran inside `sub`.
        std::fs::write(sub.join("marker.txt"), "yes").unwrap();

        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            // PowerShell: print contents of marker in cwd.
            "Get-Content marker.txt"
        } else {
            "cat marker.txt"
        };
        let result = tool
            .execute(json!({"command": cmd, "cwd": "sub", "purpose": "cwd smoke"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            result.output.contains("yes"),
            "expected marker contents from sub/, got: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn times_out_long_running_command() {
        let dir = tempdir().unwrap();
        // Short timeout so the test stays fast.
        let tool = tool_in(dir.path()).with_timeout(Duration::from_millis(500));
        let cmd = if cfg!(target_os = "windows") {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "timeout test"}))
            .await;
        assert!(!result.success, "timed-out command must be a tool error");
        assert!(
            result.output.contains("timed out"),
            "output: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn filters_repeated_noise_from_output_keeps_raw_data() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "1..4 | ForEach-Object { 'same' }"
        } else {
            "for i in 1 2 3 4; do echo same; done"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "dedup test"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        // The LLM-visible transcript is deduped to one occurrence + a
        // collapse note.
        assert_eq!(
            result.output.matches("same").count(),
            1,
            "filtered output should show the repeated line once, got: {}",
            result.output
        );
        assert!(
            result.output.contains("3 identical lines collapsed"),
            "expected a collapse note, got: {}",
            result.output
        );
        // The raw data field is untouched — the frontend Output tab still
        // sees all four lines.
        let raw_stdout = result.data.unwrap()["stdout"]
            .as_str()
            .expect("data.stdout")
            .to_string();
        assert_eq!(
            raw_stdout.matches("same").count(),
            4,
            "raw data must be unfiltered, got: {raw_stdout}"
        );
    }

    #[tokio::test]
    async fn filter_disabled_passes_output_through() {
        let dir = tempdir().unwrap();
        let tool =
            tool_in(dir.path()).with_filter_config(Arc::new(RwLock::new(ShellFilterConfig {
                enabled: false,
                overrides: vec![],
            })));
        let cmd = if cfg!(target_os = "windows") {
            "1..4 | ForEach-Object { 'same' }"
        } else {
            "for i in 1 2 3 4; do echo same; done"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "passthrough test"}))
            .await;
        assert!(result.success, "output: {}", result.output);
        // enabled=false → no filtering at all: four lines, no collapse note.
        assert_eq!(
            result.output.matches("same").count(),
            4,
            "disabled filter must pass output through unchanged, got: {}",
            result.output
        );
        assert!(!result.output.contains("collapsed"));
    }
}
