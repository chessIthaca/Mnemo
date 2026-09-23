// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `shell` — shell execution (platform-aware: PowerShell on Windows, sh elsewhere).
//!
//! Captures stdout, stderr, and exit code. Goes through approval (mutations).
//! Working directory is confined to the project sandbox. Commands are bounded
//! by a default timeout; on expiry the child is killed (`kill_on_drop`).
//!
//! LIVE OUTPUT (user request 2027-01-16): both pipes are read incrementally by
//! two reader tasks while the child runs, and throttled, UTF-8-safe chunks go
//! to the call's [`OutputSink`] so the tool card shows progress live instead of
//! one wall of text at exit. The live view is display-only and capped; the
//! returned [`ToolResult`] is byte-for-byte what the non-streaming path
//! produced and carries the FULL output.
//!
//! GREP NUDGE: results of grep-family commands (grep/rg/git grep/findstr/
//! Select-String, including piped segments) carry a one-line advisory
//! pointing at the cheaper dedicated tools — `search` for file-content
//! search, graph_* for symbol wiring. Advisory only; the command itself is
//! never modified and the raw `data` fields stay nudge-free.
//!
//! CHAIN TRANSLATION (backlog 79a2755d): PowerShell 5.1 has no
//! pipeline-chain operators — bash-style `a && b` fails with "not a valid
//! statement separator". Top-level `&&` / `||` are auto-translated to
//! `if ($?)` gates before the child spawns (the ORIGINAL command is what
//! approval and classification saw; the translated string is only what
//! runs), and the result carries a note so the model can prefer `;` or
//! separate calls next time. `purpose` is required at deserialization — the
//! schema always advertised it, and a call without it is rejected with a
//! clear error.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::config::ShellFilterConfig;
use crate::provider::ToolSchema;
use crate::tool::agent::read_files::truncate_to_boundary;
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::agent::tool_contract;
use crate::tool::{OutputSink, SafetyLevel, Tool, ToolCategory, ToolOutputStream, ToolResult};

/// Default maximum wall-clock time a shell command may run before it is
/// killed. Five minutes covers typical builds/tests without hanging the agent
/// forever on a stuck process.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Live-view flush cadence: a stream emits at most every 60 ms (~16 emits/s),
/// far below the per-token chatter that once drove the WebView2 inbound-SEND
/// backlog (AppHangB1, 2026-08-20) and coarse enough that the forwarder needs
/// no special batching for these events.
const STREAM_FLUSH_INTERVAL: Duration = Duration::from_millis(60);

/// Live-view flush size: emit early once this much is pending, so a burst of
/// newline-free output (a progress bar, a minified bundle dump) is not held
/// invisible behind the timer.
const STREAM_FLUSH_BYTES: usize = 8 * 1024;

/// Live-view byte cap PER CALL (stdout + stderr together). Past it further
/// chunks are clamped to what still fits and the call's live view gets ONE
/// truncation note (both readers share the flag); the final [`ToolResult`] is
/// unaffected — it always carries the complete output.
const STREAM_CAP: usize = 256 * 1024;

/// The one-time note appended to the live view when [`STREAM_CAP`] is reached.
const STREAM_TRUNCATION_NOTE: &str =
    "\n[live output truncated — the full text is in the result]\n";

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
     hidden; the tool caps output itself — run the command unredirected.\n";

/// Backlog 79a2755d: the note prepended to a result whose command was
/// auto-translated — PowerShell 5.1 rejects bash-style && / || chaining, and
/// the model is told so it can prefer ; or separate calls next time. Like the
/// grep nudge it shapes only the displayed `output`; the raw stdout/stderr in
/// `data` stay note-free.
const CHAIN_TRANSLATION_NOTE: &str = "NOTE: the command's && / || chaining was \
     auto-translated to PowerShell 5.1 if ($?) gates (PowerShell 5.1 rejects \
     bash-style chaining) — prefer ; or separate calls.\n";

/// Whether the command redirects stdout/stderr to a null sink — `>$null`,
/// `>/dev/null`, or `>nul` (PowerShell / POSIX / cmd forms; `2>$null`,
/// `1>$null`, `2>&1>$null` all contain one of these substrings). C4: the
/// redirect hides failure details the tool would have surfaced, so the
/// result carries a one-line warning.
/// Whether a shell command runs a core git operation (`git merge`, `git push`,
/// or whatever `[git] core_operations` lists).
///
/// The shell tool bypassed the core-operation gate until 2027-01-11: `git`
/// raises it through `Tool::never_auto_for`, but `shell git push` sailed past
/// the always-on prompt that agent.md, the APP RULES and the merge-confirmation
/// dialog all promise (found while fixing merge_to_main, plan d826b9ad).
///
/// Conservative by construction: only a `git` executable at the START of a
/// command segment counts, so a command that merely mentions the words (a path,
/// an echoed string, a commit message) is never gated. Known residual, matching
/// the one the git tool's own guard documents: an alias, a wrapper script, or
/// `sh -c "git push"` still evades.
fn command_is_core_git_op(command: &str, core_operations: &[String]) -> bool {
    // LF and CR are built from bytes: an escaped char literal in this spot was
    // mangled by the editing tooling once already, and a byte-built char is
    // exact for ASCII.
    let lf = char::from(10u8);
    let cr = char::from(13u8);
    let lowered = command.to_ascii_lowercase();
    lowered
        .split(|c: char| c == ';' || c == '|' || c == '&' || c == lf || c == cr)
        .any(|segment| {
            let mut tokens = segment.split_whitespace();
            let Some(exe) = tokens.next() else {
                return false;
            };
            if exe != "git" && exe != "git.exe" {
                return false;
            }
            // Step over git's global options (and the value of the ones that
            // take one) to reach the subcommand.
            let mut sub = None;
            while let Some(tok) = tokens.next() {
                if matches!(
                    tok,
                    "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path"
                ) {
                    let _ = tokens.next();
                    continue;
                }
                if tok.starts_with('-') {
                    continue;
                }
                sub = Some(tok);
                break;
            }
            match sub {
                Some(sub) => core_operations.iter().any(|c| c.eq_ignore_ascii_case(sub)),
                None => false,
            }
        })
}

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

/// Rewrite bash-style `&&` / `||` chaining into PowerShell 5.1-compatible
/// `if ($?)` gates (backlog 79a2755d). PowerShell 5.1 has no pipeline-chain
/// operators — `a && b` fails with "The token '&&' is not a valid statement
/// separator" — so the most common Windows trip-up is absorbed mechanically.
///
/// Only TOP-LEVEL operators are translated: the scan tracks single/double
/// quote state, so `echo "a && b"` is left alone. Single `&` / `|` (the call
/// operator, pipelines, `2>&1`) and `;` separators are valid PowerShell and
/// never touched. Backslash/backtick-escaped quotes are NOT modelled (the
/// scanner has no escape state): a backslash-escaped quote flips the quote
/// state, but the only consequence is a MISSED translation of a command that
/// was already invalid PowerShell 5.1 — the original then runs and errors
/// visibly, which is the same outcome as before this fix.
///
/// A degenerate chain (a leading/trailing operator, or an empty segment
/// between two) returns `None`: the input is invalid PowerShell either way,
/// and leaving it untranslated surfaces the original parser error instead of
/// a partially rewritten command.
///
/// The rewrite preserves short-circuit semantics: each gate checks `$?` of
/// the last executed statement, which is exactly how left-to-right `&&` /
/// `||` evaluation behaves — `a && b || c` becomes
/// `a; if ($?) { b }; if (-not $?) { c }` (b runs iff a succeeded; c runs
/// iff the statement before it failed). Returns `None` when there is no
/// top-level `&&` / `||` to translate.
fn translate_powershell_chaining(command: &str) -> Option<String> {
    // Top-level operator positions: (byte index, is_and).
    let mut splits: Vec<(usize, bool)> = Vec::new();
    let bytes = command.as_bytes();
    let (mut in_single, mut in_double) = (false, false);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'&' if !in_single && !in_double => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'&' {
                    splits.push((i, true));
                    i += 1; // consume the second '&'
                }
            }
            b'|' if !in_single && !in_double => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    splits.push((i, false));
                    i += 1; // consume the second '|'
                }
            }
            _ => {}
        }
        i += 1;
    }
    if splits.is_empty() {
        return None;
    }
    // L2: a degenerate chain (leading/trailing operator or an empty segment)
    // is invalid PowerShell either way — leave it untranslated so the
    // original parser error surfaces instead of a partially rewritten
    // command with empty gates.
    let mut prev_end = 0;
    for &(pos, _) in &splits {
        if command[prev_end..pos].trim().is_empty() {
            return None;
        }
        prev_end = pos + 2;
    }
    if command[prev_end..].trim().is_empty() {
        return None;
    }
    let mut out = String::with_capacity(command.len() + splits.len() * 24);
    // The first segment runs unconditionally; every later segment is gated
    // by the operator BEFORE it, and the trailing segment by the last one.
    let mut prev_end = 0;
    let mut prev_is_and = true;
    for (idx, &(pos, is_and)) in splits.iter().enumerate() {
        let segment = command[prev_end..pos].trim();
        if idx == 0 {
            out.push_str(segment);
        } else {
            out.push_str(if prev_is_and {
                "; if ($?) { "
            } else {
                "; if (-not $?) { "
            });
            out.push_str(segment);
            out.push_str(" }");
        }
        prev_end = pos + 2;
        prev_is_and = is_and;
    }
    let tail = command[prev_end..].trim();
    out.push_str(if prev_is_and {
        "; if ($?) { "
    } else {
        "; if (-not $?) { "
    });
    out.push_str(tail);
    out.push_str(" }");
    Some(out)
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
    /// REQUIRED (backlog 79a2755d): the schema always listed it in
    /// `required`, but `Option<String>` + `#[serde(default)]` let calls
    /// through without it — it was the first field dropped when a call went
    /// malformed, so the deserialization now enforces what the schema
    /// advertises.
    ///
    /// Consumed by the frontend from the raw tool-call args
    /// (`frontend/src/components/chat/Message.tsx` `argLabel`), so it's never
    /// read in Rust — it's kept here so serde parses/validates it.
    #[expect(dead_code, reason = "consumed by the frontend from raw tool-call args")]
    purpose: String,
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
    /// Shared, runtime-mutable core-operation list (`[git] core_operations`,
    /// the SAME handle `GitTool` holds). Seeded by the factory so a Settings →
    /// Git save takes effect on the next shell call without a registry rebuild.
    core_operations: Arc<RwLock<Vec<String>>>,
}

impl ShellTool {
    /// Create a shell tool confined to `sandbox`, using [`DEFAULT_TIMEOUT`].
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            timeout: DEFAULT_TIMEOUT,
            filter_config: Arc::new(RwLock::new(ShellFilterConfig::default())),
            core_operations: Arc::new(RwLock::new(vec!["merge".to_string(), "push".to_string()])),
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

    /// Wire the shared core-operation list (mirrors
    /// `GitTool::with_core_operations`). `git merge`/`git push` run through
    /// `shell` must raise the same always-on approval prompt the `git` tool
    /// raises; without this handle the shell guard could only use a hard-coded
    /// default and would drift from the Settings → Git list.
    pub fn with_core_operations(mut self, ops: Arc<RwLock<Vec<String>>>) -> Self {
        self.core_operations = ops;
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
            format!(
                "{} If you just made a shell call, the next one needs its own \
                 command. Execute a shell command — PowerShell 5.1 on Windows, sh \
                 on Unix. Requires approval. \
             On Windows chain with ; not && / || — PowerShell 5.1 rejects \
             bash-style chaining; it is auto-translated to if ($?) gates with a \
             note in the result, but prefer ; or separate calls. A result is \
             'successful' when the command RAN: check the exit code to see \
             whether it failed. Output streams live into the tool card while \
             the command runs; the RESULT is capped (~100 KiB) with a truncation \
             note, and well-known commands (cargo build/test, npm test/build, \
             git status) are noise-filtered — errors, warnings, and summaries \
             always survive, and the raw stdout/stderr ride the result's data \
                 field. Commands exceeding the timeout are killed.",
                tool_contract::contract(
                    "`command` + `purpose`",
                    "{\"command\":\"cargo test\",\"purpose\":\"running tests\"}"
                )
            ),
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command to execute."},
                    "cwd": {"type": "string", "description": "Working directory relative to project root (optional)."},
                    "purpose": {"type": "string", "description": "Short label of what the command does (e.g. 'running tests'). Required on every call."}
                },
                "required": ["command", "purpose"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    /// `git merge`/`git push` (and whatever `[git] core_operations` lists) must
    /// raise the interactive approval prompt even in Autonomous mode — the same
    /// always-on gate `GitTool` enforces. Without this override a shell-invoked
    /// `git push` ran UNPROMPTED, contradicting agent.md, the APP RULES and the
    /// merge-confirmation dialog (plan d826b9ad follow-up A, 2027-01-11).
    fn never_auto_for(&self, args: &serde_json::Value) -> bool {
        let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
            return false;
        };
        let ops = self
            .core_operations
            .read()
            .expect("core_operations lock poisoned");
        command_is_core_git_op(command, &ops)
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        // ONE capture path: the live view observes the plain path, it never
        // forks it — the same code produces both results, and the
        // byte-identical-result test below pins that.
        self.execute_streaming(args, OutputSink::none()).await
    }

    /// Execute with a live partial-output sink. See the module docs: the sink
    /// receives throttled, UTF-8-safe chunks while the child runs, while the
    /// returned result is exactly what [`Tool::execute`] returns.
    async fn execute_streaming(&self, args: serde_json::Value, sink: OutputSink) -> ToolResult {
        let args: ShellArgs = match serde_json::from_value(args.clone()) {
            Ok(a) => a,
            // Backlog d9ad618e: the recovery rule rides the error itself (the
            // read_files precedent, backlog 26cdbaf8) — the model reads this at
            // retry time, so the FIRST retry succeeds instead of waiting for
            // the circuit breaker.
            Err(e) => {
                return ToolResult::error(crate::tool::agent::read_files::invalid_args_error(
                    "shell",
                    &e,
                    &args,
                    &tool_contract::recovery_hint("command + purpose"),
                ))
            }
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

        // Backlog 79a2755d: PowerShell 5.1 rejects bash-style && / || —
        // auto-translate top-level chains to if ($?) gates. The ORIGINAL
        // command is what approval and classification saw; the translated
        // string is only what runs.
        let (command, translated) = if cfg!(target_os = "windows") {
            match translate_powershell_chaining(&args.command) {
                Some(translated) => (translated, true),
                None => (args.command.clone(), false),
            }
        } else {
            (args.command.clone(), false)
        };

        let mut cmd = Command::new(program);
        cmd.arg(flag)
            .arg(&command)
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

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolResult::error(format!("failed to execute command: {e}")),
        };

        // Take both pipes so two reader tasks can stream them while the child
        // runs — they are `Some` because both were piped just above.
        let stdout_pipe = child.stdout.take().expect("stdout was piped");
        let stderr_pipe = child.stderr.take().expect("stderr was piped");
        let streamed = Arc::new(AtomicUsize::new(0));
        // ONE truncation note per CALL, not per stream: both readers share this
        // flag and the card renders a single merged tail, so a call whose stdout
        // and stderr both reach the budget still says it once.
        let noted = Arc::new(AtomicBool::new(false));
        let out_task = tokio::spawn(pump_stream(
            stdout_pipe,
            ToolOutputStream::Stdout,
            sink.clone(),
            Arc::clone(&streamed),
            Arc::clone(&noted),
        ));
        let err_task = tokio::spawn(pump_stream(
            stderr_pipe,
            ToolOutputStream::Stderr,
            sink,
            Arc::clone(&streamed),
            Arc::clone(&noted),
        ));
        // Abort the readers if this call never reaches the join below: an agent
        // interrupt DROPS this future, and dropping a `JoinHandle` does not
        // cancel its task — an orphaned reader would otherwise keep emitting
        // into a card whose synthetic result already landed.
        let mut readers = ReaderGuard {
            out: out_task,
            err: err_task,
        };

        // Bound the wait. The child is MOVED INTO the future: on timeout the
        // future is dropped, which drops the Child, and `kill_on_drop(true)`
        // then kills the process — the guarantee `wait_with_output` gave by
        // consuming it. The same path covers agent-interrupt cancellation of
        // this future.
        let status =
            match tokio::time::timeout(self.timeout, async move { child.wait().await }).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    // Nothing may keep streaming after this call returns: abort
                    // BOTH readers and WAIT for the cancellation to land — a
                    // reader mid-poll on another worker can otherwise still emit
                    // a chunk after the error result. `abort` takes effect at
                    // the reader's next await point, so these awaits return
                    // promptly; the guard's Drop then re-aborts the finished
                    // tasks, which is a no-op.
                    readers.out.abort();
                    readers.err.abort();
                    let _ = (&mut readers.out).await;
                    let _ = (&mut readers.err).await;
                    let label = format_duration(self.timeout);
                    return ToolResult::error(format!(
                        "command timed out after {label} and was killed"
                    ));
                }
            };

        // Join both readers BEFORE returning the result, so on this path every
        // delta lands on the fan-in channel ahead of the ToolResult (same FIFO
        // channel). That ordering is a property of a call that COMPLETES here: a
        // timed-out call awaits the aborted readers above, but a dropped
        // (interrupted) call can still leave a reader emitting after its
        // synthetic result — which is why every consumer ALSO gates on the call
        // having no result yet (the frontend reducers drop such a chunk) instead
        // of trusting arrival order alone.
        let stdout_bytes = (&mut readers.out).await.unwrap_or_default();
        let stderr_bytes = (&mut readers.err).await.unwrap_or_default();

        match status {
            Ok(status) => {
                let raw_stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
                let raw_stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
                let code = status.code().unwrap_or(-1);
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
                // Backlog 79a2755d: the && / || auto-translate note — rides
                // even a failing command, like the nudges above.
                if translated {
                    combined.insert_str(0, CHAIN_TRANSLATION_NOTE);
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

/// Owns both reader tasks and ABORTS them when dropped, so a call that never
/// reaches the join — the tool future is dropped by an agent interrupt — still
/// stops streaming into a card whose synthetic result already landed. Aborting
/// an already-finished task is a no-op, so the success and timeout paths can
/// hold this guard while awaiting the handles through `&mut`.
struct ReaderGuard {
    out: JoinHandle<Vec<u8>>,
    err: JoinHandle<Vec<u8>>,
}

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.out.abort();
        self.err.abort();
    }
}

/// Pump one child pipe to EOF: every byte lands in the returned `Vec` (the
/// final result's source of truth), while the live view receives throttled,
/// UTF-8-safe chunks through `sink`.
///
/// Throttle: emit when [`STREAM_FLUSH_INTERVAL`] has passed since the last
/// emit, or the pending buffer reached [`STREAM_FLUSH_BYTES`], or the pipe hit
/// EOF — a short command must not wait the timer out. The interval is enforced
/// by a TIMER arm, not merely checked when bytes arrive, so a command that
/// prints one line and then works quietly still shows that line promptly.
/// Only complete UTF-8 sequences are emitted, so a multi-byte character split
/// across two reads never surfaces as a replacement char in the live view.
async fn pump_stream<R>(
    mut pipe: R,
    stream: ToolOutputStream,
    sink: OutputSink,
    streamed: Arc<AtomicUsize>,
    noted: Arc<AtomicBool>,
) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut all: Vec<u8> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    let mut last_emit = Instant::now();
    let live = sink.is_active();
    let mut buf = [0u8; 4096];
    loop {
        tokio::select! {
            read = pipe.read(&mut buf) => {
                let n = match read {
                    // EOF — or a read error on a killed child's pipe: either way
                    // the bytes already read stand, and the result is what it
                    // would have been for a short read.
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                all.extend_from_slice(&buf[..n]);
                if !live {
                    continue;
                }
                pending.extend_from_slice(&buf[..n]);
                if last_emit.elapsed() >= STREAM_FLUSH_INTERVAL
                    || pending.len() >= STREAM_FLUSH_BYTES
                {
                    emit_pending(&mut pending, false, stream, &sink, &streamed, &noted);
                    last_emit = Instant::now();
                }
            }
            // TIMER arm: flush a pending chunk once the interval has elapsed
            // with no further bytes. Without it the throttle would only be
            // EVALUATED when data arrives, so a line printed just before a quiet
            // stretch — or a sub-interval burst, which `last_emit.elapsed()` is
            // still too young to flush — would sit invisible until the next
            // output or EOF, the very "looks like a hang" gap this feature
            // removes. Guarded on `live` + pending so an idle read costs no
            // wakeups, and re-armed from `last_emit` so a flush resets it.
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(
                last_emit + STREAM_FLUSH_INTERVAL,
            )), if live && !pending.is_empty() => {
                emit_pending(&mut pending, false, stream, &sink, &streamed, &noted);
                last_emit = Instant::now();
            }
        }
    }
    if live {
        // Final flush: decode the whole remainder (nothing follows it, so an
        // incomplete trailing sequence may decode lossily here — the result
        // lossy-decodes those same bytes identically).
        emit_pending(&mut pending, true, stream, &sink, &streamed, &noted);
    }
    all
}

/// Emit the longest UTF-8-complete prefix of `pending` to the live view,
/// carrying an incomplete trailing sequence over to the next flush (unless
/// `force`, which decodes the whole remainder).
///
/// Enforces the per-call budget: past [`STREAM_CAP`] a chunk is CLAMPED to what
/// still fits, ONE note goes to the live view for the whole call (both readers
/// share `noted`), and nothing more is emitted — while [`pump_stream`]'s
/// accumulator keeps capturing, so the result stays complete.
fn emit_pending(
    pending: &mut Vec<u8>,
    force: bool,
    stream: ToolOutputStream,
    sink: &OutputSink,
    streamed: &AtomicUsize,
    noted: &AtomicBool,
) {
    if pending.is_empty() {
        return;
    }
    let cut = if force {
        pending.len()
    } else {
        std::str::from_utf8(pending).map_or_else(|e| e.valid_up_to(), |_| pending.len())
    };
    if cut == 0 {
        return;
    }
    let text = String::from_utf8_lossy(&pending[..cut]).to_string();
    pending.drain(..cut);
    // RESERVE the budget, then clamp to what was reserved. A plain load + add
    // would let two racing readers both spend the SAME room and overshoot
    // [`STREAM_CAP`] together; the CAS makes the cap a real bound, because the
    // reservation always equals what is emitted and a retry can only shrink. The
    // note's own bytes are not counted — this is a display budget, not a wire
    // limit.
    let mut emit_text = text;
    let mut used = streamed.load(Ordering::Relaxed);
    let mut clamped = false;
    loop {
        let room = STREAM_CAP.saturating_sub(used);
        if emit_text.len() > room {
            truncate_to_boundary(&mut emit_text, room);
            clamped = true;
        }
        if emit_text.is_empty() {
            break;
        }
        match streamed.compare_exchange_weak(
            used,
            used + emit_text.len(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => used = actual,
        }
    }
    if !emit_text.is_empty() {
        sink.emit(stream, &emit_text);
    }
    if clamped && !noted.swap(true, Ordering::Relaxed) {
        // The FIRST stream to be truncated says it — once for the CALL, so a
        // card whose stdout and stderr both cross the cap still notes it once.
        sink.emit(stream, STREAM_TRUNCATION_NOTE);
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
    fn core_git_op_detection_is_conservative_and_list_driven() {
        // Follow-up A (2027-01-11): `shell git push` used to bypass the
        // always-on core-operation prompt that the git TOOL raises. The guard
        // only fires on a `git` executable at the START of a command segment,
        // so a command that merely mentions the words stays auto-runnable.
        let ops = vec!["merge".to_string(), "push".to_string()];
        for cmd in [
            "git push origin main",
            "git merge --no-ff wt/mnemo",
            "git.exe push",
            "git -C repo push",
            "git --no-pager merge",
            "cd repo && git push",
            "foo; git push",
            "git push | Select-String x",
        ] {
            assert!(command_is_core_git_op(cmd, &ops), "{cmd} must be gated");
        }
        for cmd in [
            "git status",
            "git log --oneline -5",
            "git pull --no-rebase",
            "cargo test",
            "echo git-push-note",
            "rg 'git push' src/",
            "git commit -m \"mention git push\"",
        ] {
            assert!(
                !command_is_core_git_op(cmd, &ops),
                "{cmd} must stay auto-runnable"
            );
        }
        // The list is runtime-driven (Settings → Git), exactly like the git
        // tool's guard.
        let custom = vec!["checkout".to_string()];
        assert!(command_is_core_git_op("git checkout main", &custom));
        assert!(!command_is_core_git_op("git push", &custom));
    }

    #[test]
    fn never_auto_for_reads_the_command_argument() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        assert!(tool.never_auto_for(&serde_json::json!({"command": "git push"})));
        assert!(!tool.never_auto_for(&serde_json::json!({"command": "ls"})));
        // No command argument at all (a malformed call) must not gate.
        assert!(!tool.never_auto_for(&serde_json::json!({})));
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

    #[test]
    fn powershell_chain_translation_rewrites_both_operators() {
        // Backlog 79a2755d: PowerShell 5.1 rejects bash-style && / || — the
        // rewrite gates each segment on $? of the last executed statement,
        // which is exactly left-to-right short-circuit semantics.
        assert_eq!(
            translate_powershell_chaining("a && b"),
            Some("a; if ($?) { b }".to_string())
        );
        assert_eq!(
            translate_powershell_chaining("a || b"),
            Some("a; if (-not $?) { b }".to_string())
        );
        assert_eq!(
            translate_powershell_chaining("a && b && c"),
            Some("a; if ($?) { b }; if ($?) { c }".to_string())
        );
        assert_eq!(
            translate_powershell_chaining("a && b || c"),
            Some("a; if ($?) { b }; if (-not $?) { c }".to_string())
        );
    }

    #[test]
    fn powershell_chain_translation_leaves_valid_commands_alone() {
        // Quoted operators, single & / |, 2>&1, and ; separators are all
        // valid PowerShell — no translation, no note.
        assert_eq!(translate_powershell_chaining("echo \"a && b\""), None);
        assert_eq!(translate_powershell_chaining("echo 'a || b'"), None);
        assert_eq!(translate_powershell_chaining("a | b"), None);
        assert_eq!(translate_powershell_chaining("a & b"), None);
        assert_eq!(translate_powershell_chaining("cargo test 2>&1"), None);
        assert_eq!(translate_powershell_chaining("a; b"), None);
        assert_eq!(translate_powershell_chaining("echo hi"), None);
        // L2 (review round 1): degenerate chains stay untranslated so the
        // original PowerShell error surfaces instead of empty gates.
        assert_eq!(translate_powershell_chaining("&& b"), None);
        assert_eq!(translate_powershell_chaining("a &&"), None);
        assert_eq!(translate_powershell_chaining("a && && b"), None);
        assert_eq!(translate_powershell_chaining("a ||"), None);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn powershell_chain_translation_runs_and_notes() {
        // Backlog 79a2755d: `a && b` would be rejected by PowerShell 5.1 —
        // the auto-translate runs both commands and the result carries the
        // note; a clean command carries no note.
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let r = tool
            .execute(json!({
                "command": "Write-Output one && Write-Output two",
                "purpose": "chain translation smoke test"
            }))
            .await;
        assert!(r.success, "command executed: {}", r.output);
        assert!(r.output.contains("one"), "{}", r.output);
        assert!(r.output.contains("two"), "{}", r.output);
        assert!(
            r.output.contains("auto-translated to PowerShell 5.1"),
            "{}",
            r.output
        );
        let r = tool
            .execute(json!({"command": "Write-Output three", "purpose": "chain control"}))
            .await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("auto-translated"),
            "no note without chaining: {}",
            r.output
        );
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
        let result = tool.execute(json!({"command": cmd, "purpose": "simple command"})).await;
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
        let result = tool.execute(json!({"command": cmd, "purpose": "exit code capture"})).await;
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
        let result = tool.execute(json!({"command": cmd, "purpose": "stderr capture"})).await;
        // The stderr text should appear somewhere in the output.
        assert!(result.output.to_lowercase().contains("oops"));
    }

    #[tokio::test]
    async fn invalid_args_error() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let result = tool.execute(json!({})).await;
        assert!(!result.success);
        // Sanitized (plan 21118961): the instructive form.
        assert!(
            result.output.starts_with("Error: The tool 'shell' failed"),
            "{}",
            result.output
        );
        assert!(result.output.contains("parameter 'command' is required"));
        // Backlog d9ad618e: the recovery hint rides the error.
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
    async fn parses_purpose_field() {
        // `purpose` is required (backlog 79a2755d). It's not used during
        // execution (it's for the UI), but it must parse without error.
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
    async fn missing_purpose_is_rejected() {
        // Backlog 79a2755d: `purpose` is required at deserialization — the
        // schema always listed it in `required`, but `Option<String>` +
        // `#[serde(default)]` let calls through without it, so it was the
        // first field dropped when a call went malformed. The error now
        // names the missing parameter.
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let result = tool.execute(json!({"command": "echo hi"})).await;
        assert!(!result.success);
        assert!(
            result.output.contains("parameter 'purpose' is required"),
            "{}",
            result.output
        );
    }

    #[test]
    fn schema_description_names_the_calling_traps() {
        // Backlog 79a2755d: the description must warn about the two observed
        // failure classes — the empty-argument trap and PowerShell chaining.
        let tool = tool_in(std::path::Path::new("."));
        let schema = tool.schema();
        assert!(
            schema.description.starts_with("Always pass `command` + `purpose`"),
            "the contract sentence LEADS the description: {}",
            schema.description
        );
        // The content-first clause lives once in TOOL_CALL_DISCIPLINE
        // (src/agent/prompt.rs) — the per-tool copy was the trim's point.
        assert!(
            !schema.description.contains("If you catch yourself"),
            "the content-first clause lives once in TOOL_CALL_DISCIPLINE, not per tool: {}",
            schema.description
        );
        assert!(
            schema.description.contains("No zero-argument form"),
            "{}",
            schema.description
        );
        // Review L1 (2026-09-23): the shell-specific hint for the observed
        // failure pattern (a successful call followed by an empty one) —
        // dropped once by the trim, restored, now pinned.
        assert!(
            schema.description.contains("the next one needs its own command"),
            "the shell-specific empty-call hint rides the substance: {}",
            schema.description
        );
        assert!(
            schema.description.contains("chain with ; not && / ||"),
            "{}",
            schema.description
        );
        // Backlog d9ad618e: the inline example + recovery rule.
        assert!(
            schema.description.contains("cargo test"),
            "the inline example shows the exact call shape: {}",
            schema.description
        );
        assert!(
            schema.description.contains("do not resend the empty shape"),
            "{}",
            schema.description
        );
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
        let result = tool.execute(json!({"command": cmd, "cwd": "..", "purpose": "traversal probe"})).await;
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
        let result = tool.execute(json!({"command": cmd, "cwd": outside, "purpose": "outside probe"})).await;
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
        // The raw data field is untouched — all four lines stay in
        // data.stdout (the card renders the filtered text).
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

    /// A sink that forwards `(stream, text)` pairs into an unbounded channel so
    /// a test can watch the live view while — and after — the child runs.
    fn recording_sink() -> (
        OutputSink,
        tokio::sync::mpsc::UnboundedReceiver<(ToolOutputStream, String)>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = OutputSink::new(move |stream, text| {
            let _ = tx.send((stream, text.to_string()));
        });
        (sink, rx)
    }

    /// Platform-aware printer of `count` identical lines on stdout. On Windows
    /// the write goes straight to the pipe via `[Console]::Out` — PowerShell's
    /// formatter buffers `Write-Output` when stdout is redirected, which would
    /// defeat an incremental-streaming test.
    fn print_lines(word: &str, count: usize) -> String {
        if cfg!(target_os = "windows") {
            format!("1..{count} | ForEach-Object {{ [Console]::Out.WriteLine('{word}') }}")
        } else {
            format!("for i in $(seq 1 {count}); do echo {word}; done")
        }
    }

    /// User request 2027-01-16: a long command must show output WHILE it runs.
    ///
    /// The child prints `one`, then BLOCKS until the test creates `marker.txt`,
    /// then prints `two`. The first delta is observed before the marker exists,
    /// and the child cannot exit before the marker exists — so the delta
    /// provably arrived while the child was still alive. No wall-clock race:
    /// the timeouts only bound a hang.
    #[tokio::test]
    async fn streams_incremental_deltas_before_the_child_exits() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "[Console]::Out.WriteLine('one'); while (-not (Test-Path marker.txt)) { \
             Start-Sleep -Milliseconds 25 }; [Console]::Out.WriteLine('two')"
        } else {
            "echo one; while [ ! -f marker.txt ]; do sleep 0.05; done; echo two"
        };
        let (sink, mut rx) = recording_sink();
        let call = tokio::spawn(async move {
            tool.execute_streaming(
                json!({"command": cmd, "purpose": "incremental streaming test"}),
                sink,
            )
            .await
        });
        let (stream, first) = tokio::time::timeout(Duration::from_secs(30), rx.recv())
            .await
            .expect("the child never produced its first line")
            .expect("the sink must emit at least one chunk");
        assert_eq!(stream, ToolOutputStream::Stdout);
        assert!(first.contains("one"), "first chunk: {first:?}");
        // Unblock the child: its second line can only be produced after this
        // point, and the delta above can only have been produced before it.
        std::fs::write(dir.path().join("marker.txt"), "go").expect("write marker");

        let result = tokio::time::timeout(Duration::from_secs(30), call)
            .await
            .expect("the call finished")
            .expect("the call task joined");
        assert!(result.success, "output: {}", result.output);
        let mut streamed = first;
        while let Ok((_, text)) = rx.try_recv() {
            streamed.push_str(&text);
        }
        assert!(streamed.contains("two"), "streamed: {streamed:?}");
        assert!(
            result.output.contains("one") && result.output.contains("two"),
            "result: {}",
            result.output
        );
    }

    /// The live view is an observer, never a second source of truth: the same
    /// command through both paths must produce an identical result.
    #[tokio::test]
    async fn streaming_result_is_byte_identical_to_the_plain_path() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "[Console]::Out.WriteLine('alpha'); [Console]::Out.WriteLine('beta'); \
             [Console]::Error.WriteLine('err-line')"
        } else {
            "echo alpha; echo beta; echo err-line 1>&2"
        };
        let plain = tool
            .execute(json!({"command": cmd, "purpose": "byte parity"}))
            .await;
        let (sink, mut rx) = recording_sink();
        let streamed = tool
            .execute_streaming(json!({"command": cmd, "purpose": "byte parity"}), sink)
            .await;
        assert!(plain.success && streamed.success, "{}", streamed.output);
        assert_eq!(
            plain.output, streamed.output,
            "the LLM-facing output must be unchanged"
        );
        assert_eq!(plain.data, streamed.data, "raw data must be unchanged");
        let mut live = String::new();
        while let Ok((_, text)) = rx.try_recv() {
            live.push_str(&text);
        }
        assert!(
            live.contains("alpha") && live.contains("err-line"),
            "the live view really carried the text: {live:?}"
        );
    }

    /// Past `STREAM_CAP` the note is ONE per CALL, not one per stream: a call
    /// whose stdout AND stderr both cross the budget says it once (the card
    /// renders a single merged tail), the clamped emissions stay within the cap,
    /// and the result keeps both streams complete.
    #[tokio::test]
    async fn stream_cap_note_is_once_per_call_across_both_streams() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        // ~400 KiB on stdout AND ~400 KiB on stderr — each past the 256 KiB cap.
        let cmd = if cfg!(target_os = "windows") {
            "$line = 'x' * 100; 1..4000 | ForEach-Object { [Console]::Out.WriteLine($line); [Console]::Error.WriteLine($line) }"
        } else {
            "yes | head -c 400000; yes | head -c 400000 1>&2"
        };
        let (sink, mut rx) = recording_sink();
        let result = tool
            .execute_streaming(json!({"command": cmd, "purpose": "cap test (both streams)"}), sink)
            .await;
        assert!(result.success, "output: {}", result.output);
        let mut live = String::new();
        while let Ok((_, text)) = rx.try_recv() {
            live.push_str(&text);
        }
        // The WHOLE note (newlines included): the clamped tail fills the cap
        // exactly, so a trimmed pattern would leave 2 bytes behind.
        let note = STREAM_TRUNCATION_NOTE;
        assert_eq!(
            live.matches(note).count(),
            1,
            "one note per CALL even when both streams cross the cap (live view: {} KiB)",
            live.len() / 1024
        );
        let emitted = live.replace(note, "");
        assert!(
            emitted.len() <= STREAM_CAP,
            "the live view must stay within the cap, got {} bytes",
            emitted.len()
        );
        let data = result.data.as_ref().expect("data");
        assert!(
            data["stdout"].as_str().unwrap_or_default().len() >= 400_000,
            "the result still carries the full stdout"
        );
        assert!(
            data["stderr"].as_str().unwrap_or_default().len() >= 400_000,
            "the result still carries the full stderr"
        );
    }

    /// Past `STREAM_CAP` the live view is truncated once, with a note — while
    /// the RESULT keeps the complete output.
    #[tokio::test]
    async fn stream_cap_emits_one_truncation_note_and_keeps_the_full_result() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        // ~400 KiB of stdout, comfortably past the 256 KiB live cap.
        let cmd = if cfg!(target_os = "windows") {
            "$line = 'x' * 100; 1..4000 | ForEach-Object { [Console]::Out.WriteLine($line) }"
        } else {
            "yes | head -c 400000"
        };
        let (sink, mut rx) = recording_sink();
        let result = tool
            .execute_streaming(json!({"command": cmd, "purpose": "cap test"}), sink)
            .await;
        assert!(result.success, "output: {}", result.output);
        let mut live = String::new();
        while let Ok((_, text)) = rx.try_recv() {
            live.push_str(&text);
        }
        // Compare and strip the WHOLE note, surrounding newlines included: the
        // live view now fills the cap EXACTLY (chunks past it are clamped), so
        // leaving those two newline bytes behind would read as an overrun.
        let note = STREAM_TRUNCATION_NOTE;
        assert_eq!(
            live.matches(note).count(),
            1,
            "exactly one truncation note (live view: {} KiB)",
            live.len() / 1024
        );
        let emitted = live.replace(note, "");
        assert!(
            emitted.len() <= STREAM_CAP,
            "the live view must stay within the cap, got {} bytes",
            emitted.len()
        );
        assert!(
            emitted.len() > STREAM_CAP / 2,
            "the live view should fill most of the cap, got {} bytes",
            emitted.len()
        );
        let raw = result.data.as_ref().expect("data")["stdout"]
            .as_str()
            .expect("data.stdout");
        assert!(
            raw.len() >= 400_000,
            "the RESULT must carry the full output, got {} bytes",
            raw.len()
        );
    }

    /// Non-UTF-8 bytes are lossy-decoded on both the live view and the result —
    /// and never panic.
    #[tokio::test]
    async fn non_utf8_output_streams_lossily_without_panicking() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "$s = [Console]::OpenStandardOutput(); $s.Write([byte[]](0xFF, 0x0A), 0, 2); $s.Flush()"
        } else {
            "printf '\\377\\n'"
        };
        let (sink, mut rx) = recording_sink();
        let result = tool
            .execute_streaming(json!({"command": cmd, "purpose": "lossy decode"}), sink)
            .await;
        assert!(result.success, "output: {}", result.output);
        let mut live = String::new();
        while let Ok((_, text)) = rx.try_recv() {
            live.push_str(&text);
        }
        assert!(
            live.contains('\u{FFFD}'),
            "the live view lossy-decodes the byte, got: {live:?}"
        );
        let raw = result.data.as_ref().expect("data")["stdout"]
            .as_str()
            .expect("data.stdout");
        assert!(raw.contains('\u{FFFD}'), "raw data: {raw:?}");
    }

    /// stderr is a separate stream on the wire, so the card can label it.
    #[tokio::test]
    async fn stderr_text_is_tagged_stderr() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "[Console]::Error.WriteLine('boom-stderr')"
        } else {
            "echo boom-stderr 1>&2"
        };
        let (sink, mut rx) = recording_sink();
        let result = tool
            .execute_streaming(json!({"command": cmd, "purpose": "stderr tag"}), sink)
            .await;
        assert!(result.success, "output: {}", result.output);
        let mut chunks = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            chunks.push(chunk);
        }
        assert!(
            chunks
                .iter()
                .any(|(s, t)| *s == ToolOutputStream::Stderr && t.contains("boom-stderr")),
            "expected a Stderr-tagged chunk, got: {chunks:?}"
        );
        assert!(
            !chunks.iter().any(|(s, _)| *s == ToolOutputStream::Stdout),
            "nothing streamed on stdout: {chunks:?}"
        );
    }

    /// Concurrent calls each stream to their OWN sink — the routing key is the
    /// call, not the tool instance.
    #[tokio::test]
    async fn concurrent_calls_route_to_their_own_sinks() {
        let dir = tempdir().unwrap();
        let cmd_a = print_lines("alpha-only", 5);
        let cmd_b = print_lines("beta-only", 5);
        let (sink_a, mut rx_a) = recording_sink();
        let (sink_b, mut rx_b) = recording_sink();
        let tool_a = tool_in(dir.path());
        let tool_b = tool_in(dir.path());
        let call_a = tokio::spawn(async move {
            tool_a
                .execute_streaming(json!({"command": cmd_a, "purpose": "route a"}), sink_a)
                .await
        });
        let call_b = tokio::spawn(async move {
            tool_b
                .execute_streaming(json!({"command": cmd_b, "purpose": "route b"}), sink_b)
                .await
        });
        let (a, b) = tokio::join!(call_a, call_b);
        assert!(a.expect("call a joined").success);
        assert!(b.expect("call b joined").success);
        let mut live_a = String::new();
        while let Ok((_, text)) = rx_a.try_recv() {
            live_a.push_str(&text);
        }
        let mut live_b = String::new();
        while let Ok((_, text)) = rx_b.try_recv() {
            live_b.push_str(&text);
        }
        assert!(
            live_a.contains("alpha-only") && !live_a.contains("beta-only"),
            "sink a: {live_a:?}"
        );
        assert!(
            live_b.contains("beta-only") && !live_b.contains("alpha-only"),
            "sink b: {live_b:?}"
        );
    }
}
