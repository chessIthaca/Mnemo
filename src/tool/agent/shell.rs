// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `shell` — shell execution (platform-aware: PowerShell on Windows, sh elsewhere).
//!
//! Captures stdout, stderr, and exit code. Goes through approval (mutations).
//! Working directory is confined to the project sandbox. A command is killed
//! only when it prints NOTHING for [`DEFAULT_SILENCE`] (a silence timeout) or
//! when it runs longer than [`DEFAULT_TOTAL`] (a hard cap) — a long healthy
//! build that keeps printing survives, which is what lets a worker run a
//! multi-minute `cargo test` in one call instead of detaching it and polling
//! (plan 5b85ab61). On expiry — and when a call returns at all — the whole
//! process TREE the command started is killed (Job Object on Windows, process
//! group on Unix — see [`TreeKillGuard`]).
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
//!
//! CHECK NOTES (plan 0ff83146): a check command that ran in a form the host
//! cannot count, a counted run that changed the tree it measured, and a
//! shell-side file write each prepend a note saying what to do instead — same
//! shape as the nudges above (displayed `output` only, raw `data` untouched).
//!
//! TREE OWNERSHIP (plan bfe2b09f): the post-exit reader join is bounded by
//! [`READER_JOIN_GRACE`], and no process a command starts outlives the
//! call — a backgrounded dev server that inherited the pipes used to hang
//! the join forever and hold the agent's worktree open (blocking
//! team_merge). Workers must not run dev servers; the lead renders
//! previews.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::config::{MachineConfig, OptimizerConfig, ShellFilterConfig};
use crate::host::machine_slots::{is_heavy, wait_note, HeavySlots, SlotGuard};
// The shared Rust build cache (plan d3336a8a step 7): every spawned command
// gets RUSTC_WRAPPER=sccache when the machine has it and the user has not set
// a wrapper of their own.
use crate::host::sccache;
#[cfg(windows)]
use crate::process_tree::KillOnCloseJob;
use crate::provider::ToolSchema;
use crate::tool::agent::read_files::truncate_to_boundary;
use crate::tool::agent::sandbox::Sandbox;
use crate::project::git_ops::content_tree;
use crate::tool::agent::tool_contract;
use crate::tool::{OutputSink, SafetyLevel, Tool, ToolCategory, ToolOutputStream, ToolResult};

/// Default SILENCE limit (plan 5b85ab61): a shell command that writes nothing
/// to stdout or stderr for this long is killed. Five minutes is long enough
/// that a genuinely stuck process does not hold the agent forever, and short
/// enough that a worker notices quickly when a command wedges silently.
pub(crate) const DEFAULT_SILENCE: Duration = Duration::from_secs(5 * 60);

/// Default HARD CAP (plan 5b85ab61): no shell command runs longer than this in
/// total, however chatty it is — the backstop for a runaway command. One hour
/// covers a cold full-workspace build plus its test run.
pub(crate) const DEFAULT_TOTAL: Duration = Duration::from_secs(60 * 60);

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

/// Plan bfe2b09f: how long the post-exit reader join waits for EOF after
/// the shell itself has exited. A grandchild that inherited the pipe
/// handles (a backgrounded dev server, `npx` → `cmd` → `node`) keeps the
/// pipes open, so the readers may never see EOF — the unbounded join used
/// to hang the call forever (two workers sat 50-90 minutes on a vite dev
/// server). A few seconds let the readers drain everything the shell
/// actually wrote before the call stops them and kills the tree.
const READER_JOIN_GRACE: Duration = Duration::from_secs(3);

/// Plan bfe2b09f: the note that rides the output when the post-exit reader
/// join hit the [`READER_JOIN_GRACE`] deadline — a background process the
/// command started kept the pipes open, so the readers were stopped and
/// the tree was killed; the captured output may be truncated mid-stream.
/// Like the nudges above it shapes only the displayed `output`; the raw
/// stdout/stderr in `data` stay note-free.
const BACKGROUND_HOLD_NOTE: &str = "NOTE: a background process started by the command \
     kept the output pipe open; it was stopped and the captured output may be \
     truncated.\n";

/// Plan 0ff83146: the note prepended when an allow-listed check command ran
/// inside a longer command line, so the run cannot count as that check.
/// `command` is the check the worker meant to record; the text spells out the
/// three conditions the host measures (the exact command line, the checkout
/// root, nothing piped/chained/redirected) so the retry is right the first
/// time. Like the nudges above it shapes only the displayed `output`; the raw
/// stdout/stderr in `data` stay note-free.
fn check_not_counted_note(command: &str) -> String {
    format!(
        "NOTE: this run does not count as the `{command}` check. Mnemo2 counts a check only \
         when that exact command is the whole command line, run from the checkout root, with \
         nothing piped, chained or redirected. Run `{command}` alone to record it.\n"
    )
}

/// Plan 0ff83146: the note prepended to a COUNTED run that changed the tree it
/// just measured — a check that edits tracked files can never certify a later
/// commit, so the worker is told to commit or revert and run it again.
fn check_tree_changed_note(command: &str) -> String {
    format!(
        "NOTE: this run of `{command}` changed tracked files, so it does not count as a \
         check. Commit or revert those changes, then run it again.\n"
    )
}

/// Plan 0ff83146: the note prepended when the command writes files through the
/// shell. The file tools show a diff and keep line endings; a shell write is a
/// last resort the worker must justify.
const SHELL_FILE_WRITE_NOTE: &str = "NOTE: change files with file_edit or file_write, not the \
     shell: they show the diff and keep line endings. A shell write is a last resort; say why \
     in your report.\n";
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
pub(crate) fn command_is_core_git_op(command: &str, core_operations: &[String]) -> bool {
    // LF and CR are built from bytes: an escaped char literal in this spot was
    // mangled by the editing tooling once already, and a byte-built char is
    // exact for ASCII.
    let lf = char::from(10u8);
    let cr = char::from(13u8);
    let lowered = command.to_ascii_lowercase();
    lowered
        .split([';', '|', '&', lf, cr])
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

/// Plan 0ff83146: the allow-listed check command a longer command line
/// mentions — the LONGEST one when several match — or `None` when the run
/// names no check at all. Such a run is never measured (see
/// [`ShellTool::measures_check`]), so the caller has to say so.
fn mentioned_check<'a>(check_commands: &'a [String], command: &str) -> Option<&'a str> {
    check_commands
        .iter()
        .filter(|allowed| command.contains(allowed.as_str()))
        .max_by_key(|allowed| allowed.len())
        .map(|allowed| allowed.as_str())
}

/// Plan 0ff83146: whether the command writes files through the shell —
/// `Set-Content`/`Add-Content`/`Out-File`, `sed -i`, a `>>` append, or a `tee`
/// pipe. ASCII case-insensitive. `tee` counts only as a whole command word (a
/// leading `tee ` or a `| tee ` segment), so `guarantee` never fires.
fn looks_like_shell_file_write(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    const TOKENS: [&str; 5] = ["set-content", "add-content", "out-file", "sed -i", ">>"];
    TOKENS.iter().any(|token| lowered.contains(token))
        || lowered.contains("| tee ")
        || lowered.trim_start().starts_with("tee ")
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
            b'&' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
                splits.push((i, true));
                i += 1; // consume the second '&'
            }
            b'|' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
                splits.push((i, false));
                i += 1; // consume the second '|'
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
    /// Consumed by the frontend from the raw tool-call args (Mnemo2's
    /// `argLabel` helper, `mnemo2/src/lib/toolCardPaths.ts`), so it's never
    /// read in Rust — it's kept here so serde parses/validates it.
    #[expect(dead_code, reason = "consumed by the frontend from raw tool-call args")]
    purpose: String,
}

/// Trim a safe worker check command for exact allow-list membership.
///
/// Internal whitespace is preserved. Control characters and shell chaining,
/// piping, redirection, or substitution syntax make the command ineligible.
pub fn validated_check_command(command: &str) -> Option<&str> {
    if command.contains("$(")
        || command.chars().any(|character| {
            character.is_control()
                || matches!(character, ';' | '|' | '&' | '>' | '<' | '`')
        })
    {
        return None;
    }
    let command = command.trim();
    (!command.is_empty()).then_some(command)
}

/// The `shell` tool.
pub struct ShellTool {
    sandbox: Sandbox,
    /// The silence limit and the hard cap applied to every command run (plan
    /// 5b85ab61): a command is killed only after it prints nothing for
    /// `silence`, or after `total` regardless.
    limits: RunLimits,
    /// Shared command-aware output-filter config (the `[shell_filter]`
    /// section). Seeded from the loaded config by the factory; every ShellTool
    /// shares the handle, so a config save is observed on the next command
    /// without a registry rebuild.
    filter_config: Arc<RwLock<ShellFilterConfig>>,
    /// Shared, runtime-mutable core-operation list (`[git] core_operations`,
    /// the SAME handle `GitTool` holds). Seeded by the factory so a Settings →
    /// Git save takes effect on the next shell call without a registry rebuild.
    core_operations: Arc<RwLock<Vec<String>>>,
    /// Shared token-optimizer config (the `[general.optimizer]` section,
    /// backlog e4a50d22). Lever 2 (semantic command-output compression)
    /// reads the `compress_output` flag + knobs at call time; the handle is
    /// shared with every ShellTool, so a config save is observed on the next
    /// command without a registry rebuild. Defaults to all-off.
    optimizer: Arc<RwLock<OptimizerConfig>>,
    /// Exact commands whose before/after content trees are measured.
    check_commands: Vec<String>,
    /// Machine-wide heavy-command limit (the `[general.machine]` section, plan
    /// d3336a8a). The heavy policy reads the slot count at call time, so a
    /// Settings save applies to the next command without a registry rebuild.
    machine: Arc<RwLock<MachineConfig>>,
    /// The machine's heavy-slot directory (`<config dir>/run/heavy`), shared by
    /// every instance on the computer. `None` (headless, unit tests) runs heavy
    /// commands unslotted — the limit is an optimisation, never a failure mode.
    slots_dir: Option<PathBuf>,
}

impl ShellTool {
    /// Create a shell tool confined to `sandbox`, using [`DEFAULT_SILENCE`]
    /// and [`DEFAULT_TOTAL`].
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            limits: RunLimits {
                silence: Some(DEFAULT_SILENCE),
                total: DEFAULT_TOTAL,
            },
            filter_config: Arc::new(RwLock::new(ShellFilterConfig::default())),
            core_operations: Arc::new(RwLock::new(vec!["merge".to_string(), "push".to_string()])),
            optimizer: Arc::new(RwLock::new(OptimizerConfig::default())),
            check_commands: Vec::new(),
            machine: Arc::new(RwLock::new(MachineConfig::default())),
            slots_dir: None,
        }
    }

    /// Wire the shared `[general.machine]` handle and the machine's slot
    /// directory (`<config dir>/run/heavy`): a heavy command then waits for one
    /// of the machine's slots before it spawns (plan d3336a8a step 3). Both are
    /// read per call, so a Settings save and a changed slot count land on the
    /// next command.
    pub fn with_machine(mut self, cfg: Arc<RwLock<MachineConfig>>, slots_dir: PathBuf) -> Self {
        self.machine = cfg;
        self.slots_dir = Some(slots_dir);
        self
    }

    /// The heavy-slot policy for `command`: `None` when the machine handle is
    /// not wired or the command is light, so a light command never waits.
    fn heavy_slots(&self, command: &str) -> Option<HeavySlots> {
        let dir = self.slots_dir.clone()?;
        if !is_heavy(command, &self.check_commands) {
            return None;
        }
        let slots = self
            .machine
            .read()
            .expect("machine config lock poisoned")
            .heavy_commands;
        Some(HeavySlots::new(dir, slots))
    }

    /// Override BOTH limits to the same value (primarily for tests): a command
    /// that prints nothing hits the silence path at `timeout`.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.limits = RunLimits {
            silence: Some(timeout),
            total: timeout,
        };
        self
    }

    /// Override the limits independently (primarily for tests): `silence` is
    /// the no-output deadline (`None` disables it) and `total` the hard cap.
    pub fn with_limits(mut self, silence: Option<Duration>, total: Duration) -> Self {
        self.limits = RunLimits { silence, total };
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

    /// Measure before/after trees only for these exact validated commands.
    pub fn with_check_commands(mut self, commands: Vec<String>) -> Self {
        self.check_commands = commands
            .into_iter()
            .filter_map(|command| validated_check_command(&command).map(str::to_string))
            .collect();
        self
    }

    fn measures_check(&self, command: &str, cwd: &Path) -> bool {
        let Ok(root) = self.sandbox.root().canonicalize() else {
            return false;
        };
        let Ok(cwd) = cwd.canonicalize() else {
            return false;
        };
        if cwd != root {
            return false;
        }
        let Some(command) = validated_check_command(command) else {
            return false;
        };
        self.check_commands.iter().any(|allowed| allowed == command)
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

    /// Wire the shared `[general.optimizer]` config (token-optimizer lever 2,
    /// backlog e4a50d22). Called once by the factory at startup; every
    /// ShellTool shares the handle, so a config save is observed on the next
    /// command without a registry rebuild (mirrors `with_filter_config`).
    pub fn with_optimizer(mut self, cfg: Arc<RwLock<OptimizerConfig>>) -> Self {
        self.optimizer = cfg;
        self
    }

    /// Resolve and sandbox-validate the working directory.
    ///
    /// - `None` / empty → sandbox root.
    /// - Otherwise the path is validated via [`Sandbox::validate`]; traversal
    ///   (`..`), absolute paths outside the root, and non-directories fail.
    fn resolve_cwd(&self, cwd: Option<&str>) -> Result<std::path::PathBuf, String> {
        resolve_cwd(&self.sandbox, cwd)
    }
}

/// Resolve and sandbox-validate a working directory — the shared contract of
/// `shell` and the job tools (plan 5b85ab61 step 2 reuses it instead of
/// copying the rules, so `job_start`'s `cwd` is validated exactly like
/// `shell`'s).
///
/// - `None` / empty → the sandbox root.
/// - Otherwise the path goes through [`Sandbox::validate`]; traversal (`..`),
///   absolute paths outside the root, and non-directories fail.
pub(crate) fn resolve_cwd(sandbox: &Sandbox, cwd: Option<&str>) -> Result<std::path::PathBuf, String> {
    let cwd = cwd.map(str::trim).filter(|s| !s.is_empty());
    let path = match cwd {
        None => sandbox.root().to_path_buf(),
        Some(c) => sandbox
            .validate(Path::new(c))
            .map_err(|e| format!("cwd validation failed: {e}"))?,
    };
    if !path.is_dir() {
        return Err(format!("cwd is not a directory: {}", path.display()));
    }
    Ok(path)
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
                 command. Execute a shell command: PowerShell 5.1 on Windows, \
                 sh on Unix. Requires approval. On Windows chain with ; not && \
                 / || (bash-style chaining is auto-translated with a note, but \
                 prefer ; or separate calls). A result is 'successful' when \
                 the command RAN: check the exit code. Output is capped (~100 \
                 KiB) with a truncation note; well-known commands (cargo \
                 build/test, npm test/build, git status) are noise-filtered, \
                 keeping errors, warnings and summaries. A command is killed \
                 after 5 minutes without output or 60 minutes in total.",
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

        // Plan 0ff83146: a counted check run is measured AROUND the command —
        // the tree is read before the spawn and again after the command exits,
        // so a run that wrote files can never certify a later commit.
        let measure_tree = self.measures_check(&args.command, &cwd);
        let tree_before = if measure_tree {
            content_tree(&cwd).await.ok()
        } else {
            None
        };

        // ONE capture path for every platform and every consumer (plan
        // 5b85ab61): the shared runner owns spawn, streaming, the wait loop,
        // the deadline kill and the reader join; this tool keeps the shaping —
        // argument parsing, cwd validation, filtering, notes, caps and the
        // structured `data`.
        // `None` for the job-log tap: the shell tool's own result is the whole
        // record of a call that completes here (plan 5b85ab61 step 2 added the
        // tap for the job tools, which outlive their call).
        // Plan d3336a8a step 3: a HEAVY command takes one of the machine's
        // shared slots first — `None` for a light command, which never waits.
        let slots = self.heavy_slots(&args.command);
        let (outcome, waited) = run_command(
            &args.command,
            &cwd,
            self.limits,
            Some(sink),
            None,
            slots.as_ref(),
        )
        .await;
        let (code, stdout_bytes, stderr_bytes, background_held, translated) = match outcome {
            RunOutcome::SpawnFailed(message) => return ToolResult::error(message),
            RunOutcome::SilenceTimeout {
                stdout,
                stderr,
                elapsed: _elapsed,
            } => {
                let label = format_duration(self.limits.silence.unwrap_or(self.limits.total));
                return killed_result(
                    format!(
                        "command printed nothing for {label} and was killed (silence \
                         timeout); a command that works silently for long should print \
                         progress, or run it with job_start"
                    ),
                    "silence",
                    stdout,
                    stderr,
                    waited,
                );
            }
            RunOutcome::TotalTimeout {
                stdout,
                stderr,
                elapsed: _elapsed,
            } => {
                let label = format_duration(self.limits.total);
                return killed_result(
                    format!("command ran longer than {label} and was killed (hard cap)"),
                    "hard_cap",
                    stdout,
                    stderr,
                    waited,
                );
            }
            RunOutcome::Exited {
                code,
                stdout,
                stderr,
                background_held,
                translated,
                elapsed: _elapsed,
            } => (code, stdout, stderr, background_held, translated),
        };


        let tree_after = if measure_tree {
            content_tree(&cwd).await.ok()
        } else {
            None
        };

        // The runner joined both readers (BOUNDED by READER_JOIN_GRACE, plan
        // bfe2b09f) before returning, so on this path every delta landed on
        // the fan-in channel ahead of the ToolResult (same FIFO channel). That
        // ordering is a property of a call that COMPLETES here: a dropped
        // (interrupted) call can still leave a reader emitting after its
        // synthetic result — which is why every consumer ALSO gates on the call
        // having no result yet (the frontend reducers drop such a chunk) instead
        // of trusting arrival order alone. A grandchild that kept the pipes open
        // was stopped after the grace and is reported by `background_held`.
        let raw_stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
        let raw_stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
        // Command-aware noise filtering (PandaFilter idea, in-process).
        // Shapes ONLY the `output` field shown to the LLM — the raw
        // stdout/stderr (kept separately below) stay untouched in `data` for
        // the frontend, and the command execution itself is unaffected.
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
        // Plan 75b41222: stderr LEADS the model-facing text. Both lossy
        // surfaces downstream — cap_tool_output (keeps the first 100 KiB) and
        // the archive preview (keeps a 1,500-char head) — preserve the FRONT,
        // so an error after a long stdout was the first thing cut and the
        // agent had to expand the archive just to find it. A clean run (empty
        // stderr) keeps the exact pre-change shape; the `[stdout]` label
        // rides only when stdout has something to show.
        let mut combined = combine_streams(&stdout, &stderr);
        // Token-optimizer lever 2 (backlog e4a50d22): semantic command-output
        // compression. Runs AFTER the shell_filter above (which shaped
        // `stdout`/`stderr`) and ONLY when the flag is on and the filtered
        // output exceeds `compress_min_chars`. It substitutes the LLM-facing
        // `combined` only — the raw stdout/stderr in `data` stay untouched, and
        // a non-family command or a compact form that is not meaningfully
        // smaller leaves `combined` byte-identical.
        let mut savings: Option<serde_json::Value> = None;
        {
            let compact_cfg = self
                .optimizer
                .read()
                .expect("optimizer config lock poisoned")
                .clone();
            if compact_cfg.compress_output
                && combined.chars().count() >= compact_cfg.compress_min_chars
            {
                if let Some(compacted) =
                    super::output_compactor::compress(&args.command, &stdout, &stderr, code, &compact_cfg)
                {
                    if compacted.text.chars().count() < combined.chars().count() {
                        let before = super::optimizer::estimate_tokens(&combined);
                        let after = super::optimizer::estimate_tokens(&compacted.text);
                        combined = compacted.text;
                        savings = Some(super::optimizer::savings_event(
                            "compression",
                            &args.command,
                            before,
                            after,
                        ));
                    }
                }
            }
        }
        // Advisory steering (see GREP_NUDGE): prepended AFTER the command ran,
        // so the nudge rides even a failing grep; the raw stdout/stderr in
        // `data` stay nudge-free.
        if is_grep_family(&args.command) {
            combined.insert_str(0, GREP_NUDGE);
        }
        // C4: redirection warning — may co-occur with the grep nudge; each note
        // ends with `\n`, so they stack as separate leading lines.
        if has_blinding_redirection(&args.command) {
            combined.insert_str(0, REDIRECT_NOTE);
        }
        // Backlog 79a2755d: the && / || auto-translate note — rides even a
        // failing command, like the nudges above.
        if translated {
            combined.insert_str(0, CHAIN_TRANSLATION_NOTE);
        }
        // Plan bfe2b09f: the reader join hit the grace deadline — a background
        // process the command started kept the pipes open. Say that it was
        // stopped (the tree kill on the guard's drop) and that the captured
        // output may be truncated.
        if background_held {
            combined.insert_str(0, BACKGROUND_HOLD_NOTE);
        }
        // Plan d3336a8a step 3: a real wait for a machine build slot is the
        // user's answer to "why is this call slow?" — say how long it was.
        if let Some(note) = wait_note(waited) {
            combined.insert_str(0, &format!("{note}\n"));
        }
        // Plan 0ff83146: say what the tool itself measured, at the moment of
        // the mistake, so no rule has to ask for it. These lead the output —
        // inserted last, in reverse: from the top the order is
        // CHECK_NOT_COUNTED, CHECK_TREE_CHANGED, SHELL_FILE_WRITE, then the
        // notes above. Like the nudges they shape only the displayed `output`;
        // raw `data` stays note-free.
        if looks_like_shell_file_write(&args.command) {
            combined.insert_str(0, SHELL_FILE_WRITE_NOTE);
        }
        let check_command = validated_check_command(&args.command);
        let tree_changed = matches!(
            (tree_before.as_deref(), tree_after.as_deref()),
            (Some(before), Some(after)) if before != after
        );
        // A measured run that changed the tree it measured cannot certify a
        // later commit; an unmeasured one that merely MENTIONS a check command
        // was not recorded at all.
        let changed_check = if measure_tree && tree_changed {
            check_command
        } else {
            None
        };
        let uncounted_check = if measure_tree {
            None
        } else {
            mentioned_check(&self.check_commands, &args.command)
        };
        if let Some(command) = changed_check {
            combined.insert_str(0, &check_tree_changed_note(command));
        }
        if let Some(command) = uncounted_check {
            combined.insert_str(0, &check_not_counted_note(command));
        }
        // Cap the displayed output to prevent unbounded context (M3). Preserve
        // full raw data for the LLM's structured `data` field.
        let capped = super::cap_tool_output(format!("{combined}\n[exit code: {code}]"));
        // Always report success when the command executed — the exit code is
        // data for the LLM to interpret, not a tool failure. Many commands
        // return non-zero exit codes while still doing what was intended (e.g.
        // grep finding no matches, PowerShell cmdlets with Write-Error). The
        // LLM decides if it failed.
        let mut result_data = json!({
            "exit_code": code,
            "stdout": raw_stdout,
            "stderr": raw_stderr
        });
        if let Some(tree) = tree_before {
            result_data["tree_before"] = json!(tree);
        }
        if let Some(tree) = tree_after {
            result_data["tree_after"] = json!(tree);
        }
        if let Some(event) = savings {
            // Lever 2's before/after counts ride `data.savings`; the turn loop
            // records each as a savings_events ledger row.
            result_data["savings"] = serde_json::Value::Array(vec![event]);
        }
        ToolResult {
            success: true,
            output: capped,
            data: Some(result_data),
        }

    }
}

/// Limits applied to one shared-runner command run (plan 5b85ab61).
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunLimits {
    /// Kill the command after this long with NO output on stdout or stderr;
    /// `None` disables the silence limit entirely (the job tools: a job is
    /// watched with `job_wait`, not with a per-call deadline).
    pub silence: Option<Duration>,
    /// Hard cap on the whole run, whatever it prints.
    pub total: Duration,
}

/// How [`run_command`] ended (plan 5b85ab61).
#[derive(Debug)]
pub(crate) enum RunOutcome {
    /// The shell exited on its own. `stdout`/`stderr` hold every byte read.
    Exited {
        code: i32,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        /// The bounded post-exit reader join hit [`READER_JOIN_GRACE`]: a
        /// background process the command started kept the pipes open and was
        /// stopped by the tree kill.
        background_held: bool,
        /// The command ran through the PowerShell `&&`/`||` translation.
        translated: bool,
        elapsed: Duration,
    },
    /// Killed because it printed nothing for the silence limit. The captured
    /// bytes are whatever the readers had returned when the kill landed.
    SilenceTimeout {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        elapsed: Duration,
    },
    /// Killed because it hit the hard cap.
    TotalTimeout {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        elapsed: Duration,
    },
    /// The command could not be spawned (or waited on); the message is already
    /// model-facing.
    SpawnFailed(String),
}

/// Which deadline ended a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineKind {
    Silence,
    Total,
}

/// Run `command` in `cwd` under `limits`, streaming both pipes to `sink`, and
/// own the whole process tree for the run.
///
/// This is the ONE spawn/stream/wait/join core shared by the `shell` tool and
/// the job tools (plan 5b85ab61): PowerShell on Windows (with the `&&`/`||`
/// translation applied here) and `sh -c` elsewhere, `CREATE_NO_WINDOW`, one
/// reader task per pipe, a bounded post-exit reader join, and a
/// [`TreeKillGuard`] that kills the tree on every exit path — including both
/// timeout paths.
///
/// Deadline model: the command is killed when it has written NOTHING for
/// `limits.silence`, or when it has run for `limits.total`, whichever comes
/// first. A long command that keeps printing is never killed by the silence
/// limit — that is the point: a multi-minute `cargo test` finishes inside one
/// call instead of being detached and polled.
///
/// Machine-wide slots (plan d3336a8a step 3): with `slots` set, the command
/// waits for one of the machine's heavy slots BEFORE it spawns, and both clocks
/// start after the wait — a command queued behind another instance's build is
/// never mistaken for a silent one. The wait is announced once on the live view,
/// and it is returned alongside the outcome so the caller can say how long it
/// took.
pub(crate) async fn run_command(
    command: &str,
    cwd: &Path,
    limits: RunLimits,
    sink: Option<OutputSink>,
    log: Option<JobLogTap>,
    slots: Option<&HeavySlots>,
) -> (RunOutcome, Duration) {
    // Take the machine slot before anything else: the clocks below start after
    // this returns, and the live view hears about the wait exactly once.
    let announce_sink = sink.clone();
    let announce_log = log.clone();
    let slot = match slots {
        Some(slots) => {
            slots
                .acquire(|in_use| {
                    let line = format!(
                        "waiting for a build slot ({in_use} in use on this machine)...\n"
                    );
                    if let Some(sink) = &announce_sink {
                        sink.emit(ToolOutputStream::Stdout, &line);
                    }
                    if let Some(log) = &announce_log {
                        log.push(ToolOutputStream::Stdout, line.as_bytes());
                    }
                })
                .await
        }
        None => SlotGuard::none(),
    };
    let waited = slot.waited();
    let (program, flag) = if cfg!(target_os = "windows") {
        ("powershell", "-Command")
    } else {
        ("sh", "-c")
    };

    // Backlog 79a2755d: PowerShell 5.1 rejects bash-style && / || —
    // auto-translate top-level chains to if ($?) gates. The ORIGINAL command
    // is what approval and classification saw; the translated string is only
    // what runs, and `translated` rides the outcome so the caller can say so.
    let (command, translated) = if cfg!(target_os = "windows") {
        match translate_powershell_chaining(command) {
            Some(translated) => (translated, true),
            None => (command.to_string(), false),
        }
    } else {
        (command.to_string(), false)
    };

    let mut cmd = Command::new(program);
    cmd.arg(flag)
        .arg(&command)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // On Windows, suppress the console window that would otherwise pop up when
    // spawning a child process from a GUI app (CREATE_NO_WINDOW).
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    // Unix half of the tree ownership (see [`TreeKillGuard`]): the shell leads
    // its own process group, so the group kill on drop reaches the whole tree
    // at once.
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    // Plan d3336a8a step 7: hand the command the shared Rust build cache when
    // the machine has one. Inert for a non-cargo command, and it never
    // overrides a wrapper the user chose.
    sccache::apply_env(&mut cmd);

    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                RunOutcome::SpawnFailed(format!("failed to execute command: {e}")),
                waited,
            )
        }
    };

    // Plan bfe2b09f: own the whole process TREE from here on. The shell is in a
    // Job Object (Windows) / leads its own process group (Unix), so when this
    // guard drops — the success return, either timeout, or an agent interrupt
    // dropping this future — every process the command started dies with the
    // call. `kill_on_drop` kills only the shell itself: before this guard a
    // backgrounded grandchild (a vite dev server) outlived the call, hung the
    // reader join forever and held the agent's worktree open, blocking
    // team_merge. That no command may leave a server behind is intended
    // policy: workers must not run dev servers — the lead renders previews.
    let _tree_guard = TreeKillGuard::attach(&child);

    // Take both pipes so two reader tasks can stream them while the child
    // runs — they are `Some` because both were piped just above.
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    // The state both readers share with the waiter: the live-view budget
    // counter, ONE truncation note per CALL (not per stream — the card renders
    // a single merged tail, so a call whose stdout and stderr both reach the
    // budget still says it once), and the SILENCE clock: millis since `started`
    // at the last non-empty read on either pipe, so any output at all — a
    // progress line, a test name, a warning — keeps a long healthy command
    // alive.
    let state = StreamState {
        streamed: Arc::new(AtomicUsize::new(0)),
        noted: Arc::new(AtomicBool::new(false)),
        last_output: Arc::new(AtomicU64::new(0)),
        started,
    };
    let sink = sink.unwrap_or_else(OutputSink::none);
    // Graceful-stop channels for the BOUNDED post-exit join below — one per
    // reader, because a oneshot receiver is single-consumer. Sending makes that
    // reader break out of its read loop NORMALLY and return the bytes it
    // captured so far (an abort would discard the task's stack, and the bytes
    // with it); dropping the senders without sending is a harmless wakeup on
    // already-finished tasks. The timeout path uses the same channels for the
    // same reason: the killed command's output tail must reach the model.
    let (stop_out, stop_out_rx) = tokio::sync::oneshot::channel::<()>();
    let (stop_err, stop_err_rx) = tokio::sync::oneshot::channel::<()>();
    let out_task = tokio::spawn(pump_stream(
        stdout_pipe,
        ToolOutputStream::Stdout,
        sink.clone(),
        state.clone(),
        stop_out_rx,
        log.clone(),
    ));
    let err_task = tokio::spawn(pump_stream(
        stderr_pipe,
        ToolOutputStream::Stderr,
        sink,
        state.clone(),
        stop_err_rx,
        log,
    ));
    // Abort the readers if this run never reaches a join: an agent interrupt
    // DROPS this future, and dropping a `JoinHandle` does not cancel its task —
    // an orphaned reader would otherwise keep emitting into a card whose
    // synthetic result already landed.
    let mut readers = ReaderGuard {
        out: out_task,
        err: err_task,
    };

    /// Which way the wait loop ended.
    enum WaitEnd {
        /// The shell exited (or the wait itself failed).
        Exited(std::io::Result<std::process::ExitStatus>),
        /// A deadline passed; the tree kills when the guard drops.
        Deadlined(DeadlineKind),
    }

    // Wait for exit or the earliest deadline. The silence deadline is derived
    // from the activity clock, and the clock moves while the sleep arm is
    // ARMED — so a fired sleep is only a WAKEUP: the loop recomputes against the
    // freshest `last_output` and re-arms if output has landed since, which is
    // exactly what keeps a chatty command alive indefinitely. `Child::wait` is
    // cancel safe, so re-arming it on each iteration is sound. When both
    // deadlines fall at the same instant (`with_timeout` sets silence == total)
    // the SILENCE reason wins — it names the more actionable cause: the command
    // said nothing at all.
    let end = loop {
        let silence_at = limits.silence.map(|silence| {
            started + Duration::from_millis(state.last_output.load(Ordering::Relaxed)) + silence
        });
        let total_at = started + limits.total;
        let (deadline, kind) = match silence_at {
            Some(silence_at) if silence_at <= total_at => (silence_at, DeadlineKind::Silence),
            _ => (total_at, DeadlineKind::Total),
        };
        let deadline = tokio::time::Instant::from_std(deadline);
        // Re-checked on every wake: the deadline may have moved past now (new
        // output arrived) or be reached (nothing did) — the latter breaks
        // instead of spinning on an already-expired sleep.
        if tokio::time::Instant::now() >= deadline {
            break WaitEnd::Deadlined(kind);
        }
        tokio::select! {
            result = child.wait() => break WaitEnd::Exited(result),
            _ = tokio::time::sleep_until(deadline) => {}
        }
    };
    let elapsed = started.elapsed();

    if let WaitEnd::Deadlined(kind) = end {
        // Same ownership as a normal exit: stop the readers GRACEFULLY (an
        // abort would discard the bytes captured so far — the model needs the
        // output tail to see how far the command got), await them under the
        // same grace bound, and let the tree guard's drop kill the process and
        // everything under it. Nothing may keep streaming after this call
        // returns, so both readers are joined here.
        let _ = stop_out.send(());
        let _ = stop_err.send(());
        let grace = tokio::time::Instant::now() + READER_JOIN_GRACE;
        let stdout = join_reader_at(&mut readers.out, grace).await;
        let stderr = join_reader_at(&mut readers.err, grace).await;
        return match kind {
            DeadlineKind::Silence => (
                RunOutcome::SilenceTimeout {
                    stdout,
                    stderr,
                    elapsed,
                },
                waited,
            ),
            DeadlineKind::Total => (
                RunOutcome::TotalTimeout {
                    stdout,
                    stderr,
                    elapsed,
                },
                waited,
            ),
        };
    }
    let status = match end {
        WaitEnd::Exited(result) => result,
        // Handled above; the loop breaks with `Exited` on every other path.
        WaitEnd::Deadlined(_) => unreachable!("deadline handled before the join"),
    };

    // Join both readers BEFORE returning the result, so on this path every
    // delta lands on the fan-in channel ahead of the ToolResult (same FIFO
    // channel). That ordering is a property of a call that COMPLETES here: a
    // dropped (interrupted) call can still leave a reader emitting after its
    // synthetic result — which is why every consumer ALSO gates on the call
    // having no result yet (the frontend reducers drop such a chunk) instead of
    // trusting arrival order alone.
    //
    // The join is BOUNDED (plan bfe2b09f): a grandchild the command started
    // inherits the pipe handles, so after the shell itself exits the readers
    // may never see EOF — an unbounded join hung the call forever. Give the
    // readers a short grace period to drain and finish, then stop them
    // gracefully (they return what they captured — an abort would discard it)
    // and note it; the `_tree_guard` drop when this function returns kills the
    // tree that held the pipes.
    let (stdout_bytes, stderr_bytes, background_held) =
        match tokio::time::timeout(READER_JOIN_GRACE, async {
            let out = (&mut readers.out).await.unwrap_or_default();
            let err = (&mut readers.err).await.unwrap_or_default();
            (out, err)
        })
        .await
        {
            Ok((out, err)) => (out, err, false),
            Err(_grace_expired) => {
                // The shell has exited but something it started still holds the
                // pipes open. Stop both readers — a no-op for one that already
                // reached EOF — and take the bytes they captured so far.
                let _ = stop_out.send(());
                let _ = stop_err.send(());
                let out = (&mut readers.out).await.unwrap_or_default();
                let err = (&mut readers.err).await.unwrap_or_default();
                (out, err, true)
            }
        };

    let outcome = match status {
        Ok(status) => RunOutcome::Exited {
            code: status.code().unwrap_or(-1),
            stdout: stdout_bytes,
            stderr: stderr_bytes,
            background_held,
            translated,
            elapsed,
        },
        Err(e) => RunOutcome::SpawnFailed(format!("failed to execute command: {e}")),
    };
    (outcome, waited)
}

/// Await one reader's captured bytes no later than `deadline`. A reader that
/// has not returned by then (or a task that was aborted) contributes an empty
/// capture — the timeout path keeps whatever landed inside the grace window.
async fn join_reader_at(
    handle: &mut JoinHandle<Vec<u8>>,
    deadline: tokio::time::Instant,
) -> Vec<u8> {
    match tokio::time::timeout_at(deadline, &mut *handle).await {
        Ok(Ok(bytes)) => bytes,
        _ => Vec::new(),
    }
}

/// How much of a killed command's captured output rides its error result
/// (plan 5b85ab61). The END is kept: how far the command got is what the model
/// needs — a cold build's early "Compiling crate A" lines are useless, the
/// lines just before the kill are not.
const KILLED_OUTPUT_TAIL: usize = 16 * 1024;

/// The note that prefixes a captured tail cut at [`KILLED_OUTPUT_TAIL`].
const KILLED_OUTPUT_TRUNCATED_NOTE: &str = "[... earlier output omitted ...]\n";

/// Build the error result for a command the runner killed (plan 5b85ab61).
///
/// Shape: the reason line, a blank line, then the captured output with stderr
/// leading (the same order a normal result uses), keeping the END of it. No
/// shell filter and no compactor run on this path — the tail is exactly what
/// the command printed. `data` mirrors a normal result so the tool card can
/// render the output, with `exit_code: -1` (no exit status exists) and
/// `killed: "silence" | "hard_cap"` naming the reason.
///
/// `waited` is the machine-slot wait this call already spent (plan d3336a8a): a
/// killed run still reports it, or the model reads "printed nothing" on a call
/// that in fact queued 90 s behind another instance's build (finding
/// killedWaitNote).
fn killed_result(
    reason: String,
    killed: &str,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    waited: Duration,
) -> ToolResult {
    let raw_stdout = String::from_utf8_lossy(&stdout).to_string();
    let raw_stderr = String::from_utf8_lossy(&stderr).to_string();
    let combined = combine_streams(&raw_stdout, &raw_stderr);
    let mut output = match wait_note(waited) {
        Some(note) => format!("{note}\n{reason}"),
        None => reason,
    };
    if !combined.is_empty() {
        output.push_str("\n\n");
        if combined.len() > KILLED_OUTPUT_TAIL {
            output.push_str(KILLED_OUTPUT_TRUNCATED_NOTE);
        }
        output.push_str(tail_of(&combined, KILLED_OUTPUT_TAIL));
    }
    ToolResult {
        success: false,
        output,
        data: Some(json!({
            "exit_code": -1,
            "stdout": raw_stdout,
            "stderr": raw_stderr,
            "killed": killed,
        })),
    }
}

/// The last `cap` bytes of `text`, advanced to the next char boundary so the
/// slice is always valid UTF-8.
pub(crate) fn tail_of(text: &str, cap: usize) -> &str {
    if text.len() <= cap {
        return text;
    }
    let mut start = text.len() - cap;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// The model-facing text of a run: stderr LEADS (plan 75b41222), with the
/// `[stderr]` / `[stdout]` labels only when both streams have something to
/// show. Shared by the exit path and the killed-command path.
pub(crate) fn combine_streams(stdout: &str, stderr: &str) -> String {
    if stderr.is_empty() {
        stdout.to_string()
    } else if stdout.is_empty() {
        format!("[stderr]\n{stderr}")
    } else {
        format!("[stderr]\n{stderr}\n[stdout]\n{stdout}")
    }
}

/// A line-preserving tap on a run's output — the job log's source (plan
/// 5b85ab61 step 2).
///
/// The live-view [`OutputSink`] is the wrong source for a job log: it is
/// throttled ([`STREAM_FLUSH_INTERVAL`]) and byte-capped ([`STREAM_CAP`]), so a
/// chatty build's log would be silently truncated. This tap is the opposite —
/// every COMPLETE line the readers see is handed on, in arrival order, tagged
/// with its stream; the trailing partial line at EOF is handed on too so
/// nothing is lost, and nothing here is capped.
///
/// The tap carries no deadline logic of its own: the run still ends through
/// [`run_command`]'s wait loop, so a job needs only different [`RunLimits`].
#[derive(Clone)]
pub(crate) struct JobLogTap {
    tx: tokio::sync::mpsc::UnboundedSender<(ToolOutputStream, String)>,
}

impl JobLogTap {
    /// A tap that hands every line to `tx` (the job registry's log writer).
    pub(crate) fn new(tx: tokio::sync::mpsc::UnboundedSender<(ToolOutputStream, String)>) -> Self {
        Self { tx }
    }

    /// Hand one line to the writer — with or without its terminating newline;
    /// the writer makes every record end in exactly one.
    fn push(&self, stream: ToolOutputStream, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        // A closed receiver (the writer task already ended) drops the line; the
        // run's own result is unaffected either way.
        let _ = self.tx.send((stream, String::from_utf8_lossy(line).into_owned()));
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

/// Plan bfe2b09f: owns the lifetime of the whole process tree a shell
/// command starts, so no process it starts outlives the call — a leaked
/// `node.exe` once held its agent's worktree open and blocked team_merge.
/// Windows: the spawned shell is assigned to a Job Object created with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and descendants inherit the job,
/// so closing the handle this guard owns terminates the whole job. Unix:
/// the shell is spawned in its own process group (`process_group(0)` in
/// [`run_command`]) and the group is killed at drop.
///
/// The guard drops on EVERY exit path — the success return, the timeout
/// error, and an agent interrupt dropping the tool future — which is why
/// the kill lives in [`Drop`] rather than in the happy path. That a shell
/// command cannot leave a dev server behind is intended policy: workers
/// must not run servers (the lead renders previews).
///
/// Windows race, accepted and documented (plan bfe2b09f): assignment
/// happens immediately after spawn, but the shell could in principle
/// spawn a grandchild in the microseconds between `spawn()` returning
/// and `AssignProcessToJobObject` — in practice the shell needs hundreds
/// of milliseconds (loading powershell.exe) before it can run any
/// command. A grandchild spawned before assignment would escape the
/// job; starting the shell CREATE_SUSPENDED to close the window is not
/// supported by this spawn path.
///
/// If the job cannot be created or assigned the guard goes inert (the
/// pre-fix behaviour — only `kill_on_drop` of the shell) rather than
/// failing every command.
struct TreeKillGuard {
    #[cfg(windows)]
    job: Option<KillOnCloseJob>,
    #[cfg(unix)]
    pid: u32,
}

impl TreeKillGuard {
    /// Attach to the freshly spawned shell so its whole tree dies with
    /// this guard. Never fails the call: on any setup failure the guard
    /// is inert and the command still runs.
    fn attach(child: &tokio::process::Child) -> Self {
        #[cfg(windows)]
        {
            // The child's raw handle exists only while it is not reaped;
            // right after spawn it is always present. If it were not, the
            // process is already gone and there is nothing to own. A failed
            // job setup leaves the guard inert, preserving the pre-fix
            // behaviour (only kill_on_drop of the shell).
            let job = child.raw_handle().and_then(KillOnCloseJob::assign_raw);
            Self { job }
        }
        #[cfg(unix)]
        {
            // `process_group(0)` in run_command made the shell the leader of
            // its own group, so the group id IS the shell's pid (0 only if the
            // child somehow exited already — drop then has nothing to kill).
            Self {
                pid: child.id().unwrap_or(0),
            }
        }
    }
}

impl Drop for TreeKillGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // Dropping the job closes the handle owned here exclusively;
            // closing the last handle terminates the job
            // (KILL_ON_JOB_CLOSE was set at attach) — every process the
            // command started.
            drop(self.job.take());
        }
        #[cfg(unix)]
        {
            if self.pid != 0 {
                // The shell leads its own process group, so the negative
                // pid reaches the whole group. std has no signal API and
                // this crate deliberately does not depend on libc, so the
                // POSIX `kill` utility does the group kill — a ~1 ms child
                // on the drop path, and a non-zero status (no such group)
                // is the ordinary no-op case for commands that left
                // nothing running.
                let pgid = format!("-{}", self.pid);
                let _ = std::process::Command::new("kill")
                    .arg("-9")
                    .arg(pgid)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
}

/// The state one run's two readers share with its waiter (plan 5b85ab61):
/// the live-view budget counter, the one-per-call truncation note, and the
/// SILENCE activity clock. Cloned per reader; the waiter keeps its own copy
/// and reads `last_output` to derive the silence deadline.
#[derive(Clone)]
struct StreamState {
    streamed: Arc<AtomicUsize>,
    noted: Arc<AtomicBool>,
    /// Millis since `started` at the last NON-EMPTY read on either pipe.
    last_output: Arc<AtomicU64>,
    /// When the run started, so a reader can stamp `last_output`.
    started: Instant,
}

/// Pump one child pipe to EOF — or to the caller's stop signal (see
/// [`READER_JOIN_GRACE`]): every byte lands in the returned `Vec` (the
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
    state: StreamState,
    mut stop: tokio::sync::oneshot::Receiver<()>,
    log: Option<JobLogTap>,
) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut all: Vec<u8> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    // Complete lines not yet handed to the job log (see [`JobLogTap`]).
    let mut log_pending: Vec<u8> = Vec::new();
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
                // Activity for the SILENCE limit: any non-empty read counts, on
                // either pipe — a command that prints anything keeps running.
                // Stored as millis since the run started, so the waiter can
                // derive the deadline from a plain atomic.
                state.last_output.store(
                    state.started.elapsed().as_millis() as u64,
                    Ordering::Relaxed,
                );
                // Job log (plan 5b85ab61 step 2): the readers are the only place
                // every byte of both streams passes through, so the
                // line-preserving tap lives here — complete lines as they
                // arrive, the remainder at EOF. Uncapped, and filled whether or
                // not a live sink is attached.
                if let Some(log) = &log {
                    log_pending.extend_from_slice(&buf[..n]);
                    while let Some(idx) = log_pending.iter().position(|b| *b == b'\n') {
                        log.push(stream, &log_pending[..=idx]);
                        log_pending.drain(..=idx);
                    }
                }
                if !live {
                    continue;
                }
                pending.extend_from_slice(&buf[..n]);
                if last_emit.elapsed() >= STREAM_FLUSH_INTERVAL
                    || pending.len() >= STREAM_FLUSH_BYTES
                {
                    emit_pending(&mut pending, false, stream, &sink, &state.streamed, &state.noted);
                    last_emit = Instant::now();
                }
            }
            // STOP arm: the caller fires `stop` (or drops its sender) once
            // the shell has exited and READER_JOIN_GRACE elapsed without EOF
            // — a grandchild the command started holds the pipe's write end
            // open. Break NORMALLY so the bytes read so far reach the caller
            // through the JoinHandle; an abort would discard the task's
            // stack, and the captured bytes with it. A sender dropped without
            // sending (every other exit path) is a harmless wakeup for an
            // already-finished or about-to-be-aborted task.
            _ = &mut stop => break,
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
                emit_pending(&mut pending, false, stream, &sink, &state.streamed, &state.noted);
                last_emit = Instant::now();
            }
        }
    }
    if let Some(log) = &log {
        // The trailing partial line at EOF carries no newline; the writer adds
        // one so the log stays line-oriented.
        log.push(stream, &log_pending);
    }
    if live {
        // Final flush: decode the whole remainder (nothing follows it, so an
        // incomplete trailing sequence may decode lossily here — the result
        // lossy-decodes those same bytes identically).
        emit_pending(&mut pending, true, stream, &sink, &state.streamed, &state.noted);
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

/// Test-only shell snippets whose syntax differs by platform (plan 5b85ab61
/// step 2).
///
/// The job tools' tests need a `sleep` and an endless heartbeat loop, and this
/// file is the one that already owns the Windows/macOS command split — the
/// multi-platform policy forbids a second library file from repeating
/// `cfg(windows)`, so the split lives here and `jobs.rs` asks for a finished
/// command string.
///
/// A shell command that sleeps for `duration`.
#[cfg(test)]
pub(crate) fn test_sleep(duration: Duration) -> String {
    if cfg!(windows) {
        format!("Start-Sleep -Milliseconds {}", duration.as_millis())
    } else {
        format!("sleep {:.1}", duration.as_secs_f64())
    }
}

/// A shell loop that appends one line to `path` (relative to the run's cwd)
/// every 200 ms, forever — the heartbeat a test watches to prove a job's
/// process tree is dead.
#[cfg(test)]
pub(crate) fn test_heartbeat(path: &str) -> String {
    if cfg!(windows) {
        format!(
            "while ($true) {{ Add-Content -Path {path} -Value x; Start-Sleep -Milliseconds 200 }}"
        )
    } else {
        format!("while true; do echo x >> {path}; sleep 0.2; done")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::machine_slots::SlotAttempt;
    use tempfile::tempdir;

    fn tool_in(dir: &std::path::Path) -> ShellTool {
        ShellTool::new(Sandbox::new(dir).unwrap())
    }

    /// A three-second script with no output: the fastest HONEST "heavy build"
    /// stand-in, because it is a session CHECK command — heavy by definition
    /// (plan d3336a8a). Three seconds because a wait under 2 s is deliberately
    /// NOT reported in the result.
    fn slow_heavy_command() -> String {
        if cfg!(target_os = "windows") {
            "Start-Sleep -Milliseconds 3000".to_string()
        } else {
            "sleep 3".to_string()
        }
    }

    /// A tool sharing ONE machine slot (a `run/heavy` dir the test owns), its
    /// heavy command being [`slow_heavy_command`], with `silence` as the
    /// silence deadline.
    fn slotted_tool(dir: &std::path::Path, slots: &std::path::Path, silence: Duration) -> ShellTool {
        tool_in(dir)
            .with_check_commands(vec![slow_heavy_command()])
            .with_machine(
                Arc::new(RwLock::new(MachineConfig {
                    heavy_commands: 1,
                })),
                slots.to_path_buf(),
            )
            .with_limits(Some(silence), Duration::from_secs(60))
    }

    /// Wait until the ONE-slot machine dir is HELD by another caller: the only
    /// deterministic way to know the first command really took the slot before
    /// the second one runs (a fixed sleep loses under a loaded test runtime).
    async fn wait_until_slot_is_held(slots: &std::path::Path) {
        let probe = HeavySlots::new(slots.to_path_buf(), 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        loop {
            if matches!(probe.try_take(), SlotAttempt::Busy) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no command ever took the only slot"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Two instances' heavy commands, one slot: the second WAITS, reports how
    /// long, and the live view announces it ONCE (plan d3336a8a step 3).
    #[tokio::test]
    async fn second_heavy_command_waits_for_the_slot() {
        let dir = tempdir().unwrap();
        let slots = tempdir().unwrap();
        let command = slow_heavy_command();
        let first_tool = slotted_tool(dir.path(), slots.path(), Duration::from_secs(30));
        let second_tool = slotted_tool(dir.path(), slots.path(), Duration::from_secs(30));
        let first_command = command.clone();
        let first = tokio::spawn(async move {
            first_tool
                .execute(json!({"command": first_command, "purpose": "first heavy"}))
                .await
        });
        // Wait until the first command really holds the only slot.
        wait_until_slot_is_held(slots.path()).await;

        let live = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&live);
        let sink = OutputSink::new(move |_stream, text| {
            seen.lock().expect("live sink").push(text.to_string());
        });
        let started = std::time::Instant::now();
        let second = second_tool
            .execute_streaming(
                json!({"command": command, "purpose": "second heavy"}),
                sink,
            )
            .await;
        let elapsed = started.elapsed();

        let first = first.await.expect("first join");
        assert!(first.success, "{}", first.output);
        assert!(
            second.success,
            "a queued command still runs: {}",
            second.output
        );
        assert!(
            elapsed >= Duration::from_millis(700),
            "the second heavy command waited for the slot, took {elapsed:?}"
        );
        assert!(
            second.output.contains("NOTE: waited"),
            "the result says how long it waited: {}",
            second.output
        );
        let live = live.lock().expect("live sink");
        assert!(
            live.iter()
                .any(|chunk| chunk.contains("waiting for a build slot (1 in use on this machine)")),
            "the live view announces the wait once: {live:?}"
        );
        assert_eq!(
            live.iter()
                .filter(|chunk| chunk.contains("waiting for a build slot"))
                .count(),
            1,
            "the wait line is announced once, not per poll: {live:?}"
        );
    }

    /// A script that prints a line every ~200 ms for ~1.2 s: its own gaps are
    /// far under the silence window below, so only a deadline that WRONGLY
    /// counted the slot wait could kill it.
    fn chatty_command() -> String {
        if cfg!(target_os = "windows") {
            "1..6 | ForEach-Object { Write-Output \"tick $_\"; Start-Sleep -Milliseconds 200 }"
                .to_string()
        } else {
            "for i in 1 2 3 4 5 6; do echo tick $i; sleep 0.2; done".to_string()
        }
    }

    /// The silence clock starts AFTER the slot is acquired: a command queued for
    /// longer than its silence window is not killed as "silent" (plan d3336a8a).
    #[tokio::test]
    async fn slot_wait_is_not_counted_as_silence() {
        let dir = tempdir().unwrap();
        let slots = tempdir().unwrap();
        let held = HeavySlots::new(slots.path().to_path_buf(), 1)
            .try_take()
            .guard()
            .expect("the only slot is free");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(8000));
            drop(held);
        });
        // A 4 s silence window is generous for PowerShell's own startup on a
        // loaded machine, and the wait below is longer still: counting the wait
        // as silence would kill the command before it ever spawned.
        let limits = RunLimits {
            silence: Some(Duration::from_millis(4000)),
            total: Duration::from_secs(60),
        };
        let slots_for_call = HeavySlots::new(slots.path().to_path_buf(), 1);
        let (outcome, waited) = run_command(
            &chatty_command(),
            dir.path(),
            limits,
            None,
            None,
            Some(&slots_for_call),
        )
        .await;
        release.join().expect("release thread");
        assert!(
            waited >= Duration::from_millis(2500),
            "the call waited for the slot: {waited:?}"
        );
        match outcome {
            RunOutcome::Exited { code, .. } => {
                assert_eq!(code, 0, "the queued command ran to completion")
            }
            other => panic!("the wait must not count as silence: {other:?}"),
        }
    }

    /// A light command NEVER waits, even while the machine's only slot is held.
    #[tokio::test]
    async fn light_command_never_waits() {
        let dir = tempdir().unwrap();
        let slots = tempdir().unwrap();
        // The holder's heavy command runs for 8 s, so a light command that
        // WRONGLY queued would take at least that long; a plain PowerShell
        // start under full-suite load stays well under the 5 s bound.
        let held_command = if cfg!(target_os = "windows") {
            "Start-Sleep -Milliseconds 8000".to_string()
        } else {
            "sleep 8".to_string()
        };
        let holder_tool = tool_in(dir.path())
            .with_check_commands(vec![held_command.clone()])
            .with_machine(
                Arc::new(RwLock::new(MachineConfig {
                    heavy_commands: 1,
                })),
                slots.path().to_path_buf(),
            )
            .with_limits(Some(Duration::from_secs(60)), Duration::from_secs(120));
        let holder = tokio::spawn(async move {
            holder_tool
                .execute(json!({"command": held_command, "purpose": "holder"}))
                .await
        });
        // Wait until the holder really took the only slot (acquired before its
        // spawn), instead of guessing with a fixed sleep.
        wait_until_slot_is_held(slots.path()).await;

        let light = tool_in(dir.path()).with_machine(
            Arc::new(RwLock::new(MachineConfig {
                heavy_commands: 1,
            })),
            slots.path().to_path_buf(),
        );
        let started = std::time::Instant::now();
        let result = light
            .execute(json!({"command": "echo hi", "purpose": "light"}))
            .await;
        let elapsed = started.elapsed();
        assert!(result.success, "{}", result.output);
        assert!(
            elapsed < Duration::from_millis(5000),
            "a light command never waits for a build slot (the held heavy command \
             runs for 8 s), took {elapsed:?}"
        );
        assert!(
            !result.output.contains("waited"),
            "a light command reports no wait: {}",
            result.output
        );
        // The holder outlives the assertion on purpose; drop it instead of
        // waiting out its 8 s.
        holder.abort();

    }

    #[test]
    fn check_measurement_requires_an_exact_safe_command() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path())
            .with_check_commands(vec!["cargo test --workspace".to_string()]);
        assert!(tool.measures_check("  cargo test --workspace  ", dir.path()));
        for command in [
            "cargo\ntest --workspace",
            "\ncargo test --workspace",
            "cargo  test --workspace",
            "cargo\ttest --workspace",
            "cargo\u{7}test --workspace",
            "cargo test --workspace; echo no",
            "cargo test --workspace | Select-String x",
            "cargo test --workspace && echo no",
            "cargo test --workspace > log.txt",
            "cargo test --workspace < input.txt",
            "cargo test --workspace `echo no`",
            "cargo test --workspace $(echo no)",
        ] {
            assert!(
                !tool.measures_check(command, dir.path()),
                "{command:?} must not be measured"
            );
        }
    }

    /// A command printing 40 identical lines. The shell filter's repeat
    /// collapse is disabled in the lever-2 tests, so the COMPACTOR is what
    /// acts on these lines.
    fn repetitive_cmd() -> &'static str {
        if cfg!(target_os = "windows") {
            "1..40 | ForEach-Object { 'repeated build noise line' }"
        } else {
            "for i in $(seq 40); do echo 'repeated build noise line'; done"
        }
    }

    /// The extra-command pattern matching [`repetitive_cmd`] — lever 2's
    /// user-extensible allowlist path, so exercising the call site needs no
    /// real `cargo`/`npm` build.
    fn repetitive_pattern() -> &'static str {
        if cfg!(target_os = "windows") {
            "ForEach-Object"
        } else {
            "seq 40"
        }
    }

    /// A filter-disabled tool with lever 2 wired to `cfg`, isolating the
    /// compactor from the (separately tested) shell filter.
    fn compactor_tool(dir: &std::path::Path, cfg: OptimizerConfig) -> ShellTool {
        ShellTool::new(Sandbox::new(dir).unwrap())
            .with_filter_config(Arc::new(RwLock::new(ShellFilterConfig {
                enabled: false,
                overrides: vec![],
            })))
            .with_optimizer(Arc::new(RwLock::new(cfg)))
    }

    #[tokio::test]
    async fn compression_off_passes_output_through_byte_identically() {
        // The opt-out pin: the levers default ON, so this test opts lever 2
        // out explicitly — off, the call site changes nothing, every line
        // survives, and no savings data rides the result.
        let dir = tempdir().unwrap();
        let tool = compactor_tool(
            dir.path(),
            OptimizerConfig {
                compress_output: false,
                ..Default::default()
            },
        );
        let result = tool
            .execute(json!({"command": repetitive_cmd(), "purpose": "compression off"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            result.output.matches("repeated build noise line").count(),
            40,
            "{}",
            result.output
        );
        assert!(!result.output.contains("compressed"), "{}", result.output);
        let data = result.data.expect("data");
        assert!(data.get("savings").is_none(), "no savings row when off: {data}");
    }

    #[tokio::test]
    async fn compression_collapses_output_and_preserves_raw_data() {
        let dir = tempdir().unwrap();
        let cfg = OptimizerConfig {
            compress_output: true,
            compress_min_chars: 100,
            compress_extra_commands: vec![repetitive_pattern().to_string()],
            ..Default::default()
        };
        let tool = compactor_tool(dir.path(), cfg);
        let result = tool
            .execute(json!({"command": repetitive_cmd(), "purpose": "compression on"}))
            .await;
        assert!(result.success, "{}", result.output);
        // The LLM-facing output is the compact form…
        assert!(result.output.contains("compressed generic output"), "{}", result.output);
        assert!(result.output.contains("(repeated 40×)"), "{}", result.output);
        // …while the raw stdout in `data` is untouched (the shell_filter
        // contract: the card renders the raw text).
        let data = result.data.expect("data");
        let raw = data["stdout"].as_str().expect("data.stdout");
        assert_eq!(raw.matches("repeated build noise line").count(), 40, "{raw}");
        // The before/after counts ride data.savings for the ledger.
        let savings = data["savings"].as_array().expect("savings array");
        assert_eq!(savings.len(), 1);
        assert_eq!(savings[0]["kind"], "compression");
        let before = savings[0]["tokens_before"].as_i64().unwrap();
        let after = savings[0]["tokens_after"].as_i64().unwrap();
        assert!(after < before, "{before} -> {after}");
    }

    #[tokio::test]
    async fn compression_skips_commands_no_family_or_pattern_matches() {
        // Lever 2 on, but neither a known family nor an extra pattern
        // matches: the output passes through unchanged, no savings row.
        let dir = tempdir().unwrap();
        let cfg = OptimizerConfig {
            compress_output: true,
            compress_min_chars: 100,
            ..Default::default()
        };
        let tool = compactor_tool(dir.path(), cfg);
        let result = tool
            .execute(json!({"command": repetitive_cmd(), "purpose": "no match"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(result.output.matches("repeated build noise line").count(), 40);
        assert!(!result.output.contains("compressed"), "{}", result.output);
        assert!(result.data.expect("data").get("savings").is_none());
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
        let cmd = "exit 42";
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

    /// Plan 75b41222: the lossy surfaces downstream — the 100 KiB cap and
    /// the archive preview's 1,500-char head — keep the FRONT of the model
    /// text, so the shell tool leads with stderr: an error must never sit
    /// behind a long stdout again.
    #[tokio::test]
    async fn stderr_comes_before_stdout_in_the_model_text() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Output 'OUT-LINE'; [Console]::Error.WriteLine('ERR-LINE')"
        } else {
            "echo OUT-LINE; echo ERR-LINE 1>&2"
        };
        let result = tool.execute(json!({"command": cmd, "purpose": "stderr first"})).await;
        assert!(result.success, "output: {}", result.output);
        let err_tag = result.output.find("[stderr]").expect("a [stderr] section");
        let err_line = result.output.find("ERR-LINE").expect("the stderr line");
        let out_tag = result.output.find("[stdout]").expect("a [stdout] section");
        let out_line = result.output.find("OUT-LINE").expect("the stdout line");
        assert!(
            err_tag < err_line && err_line < out_tag && out_tag < out_line,
            "stderr must lead stdout in the model text: {}",
            result.output
        );
        assert!(
            result.output.ends_with("[exit code: 0]"),
            "the exit-code line must close the text: {}",
            result.output
        );
    }

    /// Plan 75b41222: a clean run reads exactly as before — plain stdout
    /// and the exit-code line, no stream labels.
    #[tokio::test]
    async fn a_clean_run_reads_as_before() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            "Write-Output hello"
        } else {
            "echo hello"
        };
        let result = tool.execute(json!({"command": cmd, "purpose": "clean run"})).await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(result.output, "hello\n[exit code: 0]");
        assert!(
            !result.output.contains("[stderr]") && !result.output.contains("[stdout]"),
            "a clean run carries no stream labels: {}",
            result.output
        );
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
        // Short limits so the test stays fast. `with_timeout` sets BOTH limits
        // to the same value; a command that prints nothing hits the SILENCE
        // deadline (when the two deadlines coincide the silence reason is
        // reported — it names the actionable cause).
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
            result.output.contains("silence timeout"),
            "output: {}",
            result.output
        );
        let data = result.data.expect("data");
        assert_eq!(data["exit_code"].as_i64(), Some(-1), "{data}");
        assert_eq!(data["killed"].as_str(), Some("silence"), "{data}");
    }

    /// Time a trivial command to learn how long a shell on THIS host can take
    /// to print its first line right now (a PowerShell boot plus its output),
    /// so the limit tests can size their silence window from it. The suite
    /// spawns shells in parallel: measured boots ranged from ~1.7 s on an idle
    /// box to more than 8 s under load, which makes any fixed constant flaky.
    async fn measured_shell_boot(dir: &std::path::Path) -> Duration {
        let started = Instant::now();
        let result = tool_in(dir)
            .execute(json!({"command": "echo boot", "purpose": "shell boot probe"}))
            .await;
        let boot = started.elapsed();
        assert!(result.success, "boot probe failed: {}", result.output);
        boot
    }

    /// The silence window the limit tests use: comfortably above the measured
    /// boot, so the command's FIRST line always lands inside it. Capped so a
    /// pathologically slow boot cannot make a test's deadline outlive the
    /// command's own hold time.
    fn silence_window(boot: Duration) -> Duration {
        (boot * 4 + Duration::from_secs(8)).min(Duration::from_secs(30))
    }

    /// Plan 5b85ab61: a command that keeps printing must OUTLIVE the silence
    /// limit. Before the change the shell had one fixed wall clock, so a
    /// multi-minute build was killed mid-run and workers detached it and polled
    /// with `sleep`; a long healthy build must now finish in one call.
    #[tokio::test]
    async fn chatty_command_outlives_the_silence_limit() {
        let dir = tempdir().unwrap();
        let boot = measured_shell_boot(dir.path()).await;
        let silence = silence_window(boot);
        // Print for longer than the window: only the activity clock can keep
        // the command alive past `silence`.
        let print_for = silence + Duration::from_secs(3);
        let ticks = (print_for.as_millis() / 400).max(4) as u64;
        let tool =
            tool_in(dir.path()).with_limits(Some(silence), print_for + Duration::from_secs(30));
        let cmd = if cfg!(target_os = "windows") {
            format!("1..{ticks} | ForEach-Object {{ 'tick ' + $_; Start-Sleep -Milliseconds 400 }}")
        } else {
            format!("for i in $(seq 1 {ticks}); do echo tick $i; sleep 0.4; done")
        };
        let started = Instant::now();
        let result = tool
            .execute(json!({"command": cmd, "purpose": "silence limit chatty"}))
            .await;
        let elapsed = started.elapsed();
        assert!(
            result.success,
            "a chatty command must finish, got: {}",
            result.output
        );
        let data = result.data.expect("data");
        assert_eq!(data["exit_code"].as_i64(), Some(0), "{data}");
        let stdout = data["stdout"].as_str().expect("data.stdout");
        // Every line made it: the command ran to completion, PAST the silence
        // window — only the ticks moving the activity clock kept it alive.
        for line in 1..=ticks {
            assert!(stdout.contains(&format!("tick {line}")), "stdout: {stdout}");
        }
        assert!(
            elapsed > silence,
            "the command was killed inside the silence window: {elapsed:?} vs {silence:?}"
        );
    }

    /// Plan 5b85ab61: a command that prints NOTHING for the silence window is
    /// killed, the error names why, and the output it did print rides the error
    /// as a tail (before the change the timeout path discarded it).
    #[tokio::test]
    async fn silent_command_is_killed_by_the_silence_limit() {
        let dir = tempdir().unwrap();
        let boot = measured_shell_boot(dir.path()).await;
        let silence = silence_window(boot);
        // A command that prints NOTHING at all is what the silence limit is
        // for. The boot is part of the window, so no output is required from
        // the command here: whatever this host's load, the kill lands at most
        // one window after the spawn.
        let tool = tool_in(dir.path()).with_limits(Some(silence), Duration::from_secs(400));
        let cmd = if cfg!(target_os = "windows") {
            "Start-Sleep -Seconds 300"
        } else {
            "sleep 300"
        };
        let started = Instant::now();
        let result = tool
            .execute(json!({"command": cmd, "purpose": "silence limit kill"}))
            .await;
        let elapsed = started.elapsed();
        assert!(!result.success, "a silent command must be a tool error");
        assert!(
            result.output.contains("silence timeout"),
            "output: {}",
            result.output
        );
        assert!(
            elapsed < silence + Duration::from_secs(30),
            "the silence kill must not wait for the 60 s sleep: {elapsed:?} vs {silence:?}"
        );
        let data = result.data.expect("data");
        assert_eq!(data["killed"].as_str(), Some("silence"), "{data}");
    }

    /// Plan 5b85ab61: a killed command's error keeps the END of what it printed
    /// — the lines just before the kill — not the start of a cold build log,
    /// and the `data` mirrors a normal result so the tool card can show it.
    #[test]
    fn killed_result_keeps_the_output_tail() {
        let mut stdout = Vec::new();
        for line in 0..4_000 {
            stdout.extend_from_slice(format!("build line {line}\n").as_bytes());
        }
        assert!(stdout.len() > KILLED_OUTPUT_TAIL);
        let result = killed_result(
            "command ran longer than 60m and was killed (hard cap)".to_string(),
            "hard_cap",
            stdout,
            Vec::new(),
            Duration::ZERO,
        );
        assert!(!result.success, "a kill is a tool error");
        assert!(
            result
                .output
                .starts_with("command ran longer than 60m and was killed (hard cap)\n\n"),
            "the reason leads: {}",
            &result.output[..result.output.len().min(120)]
        );
        assert!(result.output.contains("build line 3999"), "the tail is kept");
        assert!(!result.output.contains("build line 0\n"), "the head is cut");
        assert!(
            result.output.contains(KILLED_OUTPUT_TRUNCATED_NOTE),
            "the cut is announced"
        );
        assert!(result.output.len() < KILLED_OUTPUT_TAIL + 256);
        let data = result.data.expect("data");
        assert_eq!(data["exit_code"].as_i64(), Some(-1));
        assert_eq!(data["killed"].as_str(), Some("hard_cap"));
    }

    /// Plan 5b85ab61: a command killed before it printed anything still gets the
    /// reason line and a renderable `data` object, without a dangling blank.
    #[test]
    fn killed_result_without_output_is_just_the_reason() {
        let reason = "command printed nothing for 5m and was killed (silence timeout)";
        let result = killed_result(
            reason.to_string(),
            "silence",
            Vec::new(),
            Vec::new(),
            Duration::ZERO,
        );
        assert!(!result.success);
        assert_eq!(result.output, reason);
        let data = result.data.expect("data");
        assert_eq!(data["stdout"].as_str(), Some(""));
        assert_eq!(data["stderr"].as_str(), Some(""));
        assert_eq!(data["killed"].as_str(), Some("silence"));
    }

    /// Plan d3336a8a (finding killedWaitNote): a KILLED run still reports the
    /// machine-slot wait it already spent — otherwise the model reads "printed
    /// nothing" on a call that mostly queued behind another instance's build.
    #[test]
    fn killed_result_keeps_the_slot_wait_note() {
        let result = killed_result(
            "command printed nothing for 5m and was killed (silence timeout)".to_string(),
            "silence",
            Vec::new(),
            Vec::new(),
            Duration::from_secs(80),
        );
        assert!(
            result
                .output
                .starts_with("NOTE: waited 1m20s for a build slot.\ncommand printed nothing"),
            "the wait note leads the killed result: {}",
            result.output
        );
        // A short wait stays silent, exactly like the exit path.
        let short = killed_result(
            "reason".to_string(),
            "hard_cap",
            Vec::new(),
            Vec::new(),
            Duration::from_millis(300),
        );
        assert_eq!(short.output, "reason");
    }

    /// Plan 5b85ab61: the hard cap kills a command that never stops printing.
    #[tokio::test]
    async fn chatty_command_is_killed_by_the_hard_cap() {
        let dir = tempdir().unwrap();
        // Silence is effectively off: only the cap can end this.
        let tool = tool_in(dir.path())
            .with_limits(Some(Duration::from_secs(30)), Duration::from_secs(2));
        let cmd = if cfg!(target_os = "windows") {
            "while ($true) { 'tick'; Start-Sleep -Milliseconds 200 }"
        } else {
            "while true; do echo tick; sleep 0.2; done"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "hard cap kill"}))
            .await;
        assert!(!result.success, "a runaway command must be a tool error");
        assert!(
            result.output.contains("hard cap"),
            "output: {}",
            result.output
        );
        let data = result.data.expect("data");
        assert_eq!(data["killed"].as_str(), Some("hard_cap"), "{data}");
        assert_eq!(data["exit_code"].as_i64(), Some(-1), "{data}");
    }

    /// Regression (plan 5b85ab61), silence path: a command killed for printing
    /// nothing must not leave the tree it started behind — the job/process-group
    /// kill runs on this path exactly as it does on a normal exit.
    #[tokio::test]
    async fn killed_command_leaves_no_process() {
        let dir = tempdir().unwrap();
        // The window covers the measured boot plus the spawn, the pid write and
        // the 'holding' line; the 60 s sleep after the line is what the silence
        // kill cuts. The grandchild must be SILENT: it inherits the pipes with
        // -NoNewWindow, and a chatty grandchild (a ping writing to our stdout)
        // would keep moving the silence clock and turn this into a hard-cap
        // kill — `ping >nul` inside cmd keeps it quiet.
        let boot = measured_shell_boot(dir.path()).await;
        let silence = silence_window(boot);
        let tool = tool_in(dir.path()).with_limits(Some(silence), Duration::from_secs(400));
        let cmd = if cfg!(target_os = "windows") {
            "$p = Start-Process -NoNewWindow -PassThru cmd -ArgumentList \
             '/c','ping -n 600 127.0.0.1 >nul'; \
             Set-Content grandchild.pid $p.Id; 'holding'; Start-Sleep -Seconds 300"
        } else {
            "sleep 600 & echo $! > grandchild.pid; echo holding; sleep 300"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "silence tree-kill regression"}))
            .await;
        assert!(
            !result.success,
            "the command must hit the silence timeout, got: {}",
            result.output
        );
        assert!(
            result.output.contains("silence timeout"),
            "output: {}",
            result.output
        );
        assert!(
            result.output.contains("holding"),
            "the killed command's output tail must ride the error: {}",
            result.output
        );
        let pid_text = std::fs::read_to_string(dir.path().join("grandchild.pid"))
            .expect("the shell wrote the grandchild's pid before the silence kill fired");
        let pid: u32 = pid_text
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .expect("the pid file holds the grandchild's pid");
        assert_eventually_dead(pid).await;
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

    /// Windows probe for the regression tests below: is the process with
    /// `pid` still running? Mirrors the app crate's `instance_marker::
    /// pid_alive` (this crate cannot import it): `OpenProcess` alone is not
    /// enough — an exited process keeps its kernel object alive while any
    /// process still holds a handle — so the pid counts as alive only
    /// while `GetExitCodeProcess` reports `STILL_ACTIVE`.
    #[cfg(windows)]
    fn pid_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: fixed query flags, no handle inheritance; the handle is
        // closed on every path below.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        // SAFETY: the handle comes from OpenProcess above; query access
        // suffices for GetExitCodeProcess.
        let mut exit_code: u32 = 0;
        let probed = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        // SAFETY: the handle is no longer used.
        unsafe { CloseHandle(handle) };
        probed != 0 && exit_code == STILL_ACTIVE as u32
    }

    /// Unix probe for the regression tests below — no libc dependency, the
    /// same choice the app crate's `pid_alive` makes.
    #[cfg(not(windows))]
    fn pid_alive(pid: u32) -> bool {
        std::process::Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Poll until the process `pid` is gone, bounded at ~10 s. The tree
    /// kill runs on the guard's drop right before the call returns, but a
    /// terminated process's exit code lands asynchronously, so the first
    /// probe can still see it alive.
    async fn assert_eventually_dead(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if !pid_alive(pid) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "process {pid} started by the command is still running after the call returned"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Regression (plan bfe2b09f): a grandchild that inherits the stdout
    /// pipe used to hang the post-exit reader join forever — the worker
    /// hung for the full tool timeout while the backgrounded process held
    /// the worktree open. The call must return within ~15 s, note the
    /// stopped background process, and leave the grandchild dead.
    #[tokio::test]
    async fn background_child_holding_the_pipes_does_not_hang_the_call() {
        let dir = tempdir().unwrap();
        // The tool's NORMAL timeout: the bug is the post-exit join, not the
        // timeout path, so nothing but the outer bound below may rescue the
        // call.
        let tool = tool_in(dir.path());
        let cmd = if cfg!(target_os = "windows") {
            // -NoNewWindow makes ping inherit the piped stdout handle — the
            // exact leak; `-n 60` bounds the damage if the kill ever
            // regresses. `.Id` prints the grandchild's pid for the
            // liveness check.
            "(Start-Process -NoNewWindow -PassThru ping -ArgumentList '-n','60','127.0.0.1').Id"
        } else {
            "sleep 60 & echo $!"
        };
        let started = Instant::now();
        // Outer bound: a regression FAILS here instead of hanging the test
        // binary (and, before the fix, the worker) until the tool timeout.
        let result = match tokio::time::timeout(
            Duration::from_secs(30),
            tool.execute(json!({"command": cmd, "purpose": "background grandchild regression"})),
        )
        .await
        {
            Ok(result) => result,
            Err(_never_returned) => panic!(
                "the shell call never returned — a background child hangs the post-exit reader join"
            ),
        };
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "call took {:?} — expected the few-seconds grace bound, not a hang",
            started.elapsed()
        );
        assert!(result.success, "output: {}", result.output);
        assert!(
            result.output.contains("background process"),
            "expected the note that a background process kept the output open and was \
             stopped, got: {}",
            result.output
        );
        // The grandchild's pid — printed by the command — must not survive
        // the call.
        let data = result.data.unwrap();
        let stdout = data["stdout"].as_str().unwrap();
        let pid = stdout
            .lines()
            .find_map(|line| line.trim().parse::<u32>().ok())
            .expect("the command printed the grandchild's pid");
        assert_eventually_dead(pid).await;
    }

    /// Regression (plan bfe2b09f), timeout path: `kill_on_drop` killed
    /// only the shell itself, so a grandchild a timed-out command had
    /// started kept running — and held the agent's worktree open. The
    /// whole tree the command started must die with the call.
    #[tokio::test]
    async fn timed_out_command_kills_the_background_grandchild() {
        let dir = tempdir().unwrap();
        // The window is derived from a measured shell boot, so the spawn and the
        // pid write always land before a deadline fires (a fixed 10 s cut the
        // shell mid-write once the machine was under concurrent-build load).
        // EITHER deadline may end this run: ping writes into the inherited pipe
        // (the leak under test), so on Windows its output keeps moving the
        // silence clock and the hard cap wins; on Unix the silent `sleep 60 &`
        // lets the silence deadline win. Both paths must kill the tree.
        let boot = measured_shell_boot(dir.path()).await;
        let silence = silence_window(boot);
        let tool = tool_in(dir.path()).with_limits(Some(silence), silence + Duration::from_secs(5));
        let cmd = if cfg!(target_os = "windows") {
            // Start the 60 s grandchild — -PassThru, or $p carries no pid to
            // record (the pid goes to a file so the test can find it without
            // parsing the result) — then hold the shell open so the call goes
            // through the kill.
            "$p = Start-Process -NoNewWindow -PassThru ping -ArgumentList \
             '-n','600','127.0.0.1'; Set-Content grandchild.pid $p.Id; \
             Start-Sleep -Seconds 300"
        } else {
            "sleep 600 & echo $! > grandchild.pid; sleep 300"
        };
        let result = tool
            .execute(json!({"command": cmd, "purpose": "timeout tree-kill regression"}))
            .await;
        assert!(
            !result.success,
            "the command must hit the tool timeout, got: {}",
            result.output
        );
        assert!(result.output.contains("was killed"), "output: {}", result.output);
        let data = result.data.expect("data");
        assert!(
            matches!(data["killed"].as_str(), Some("silence") | Some("hard_cap")),
            "{data}"
        );
        let pid_text = std::fs::read_to_string(dir.path().join("grandchild.pid"))
            .expect("the shell wrote the grandchild's pid before the timeout fired");
        let pid: u32 = pid_text
            .chars()
            .filter(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .expect("the pid file holds the grandchild's pid");
        assert_eventually_dead(pid).await;
    }

    /// Seed a real git repository, so `content_tree` has an index to copy.
    fn git_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Mnemo Test"],
            vec!["config", "user.email", "mnemo@example.invalid"],
            vec!["add", "README.md", "tracked.txt"],
            vec!["commit", "-m", "seed"],
        ] {
            let status = std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .status()
                .expect("run git");
            assert!(status.success(), "git {:?}", args);
            if args[0] == "init" {
                std::fs::write(dir.join("README.md"), "seed\n").expect("seed file");
                std::fs::write(dir.join("tracked.txt"), "value = seed\n")
                    .expect("tracked file");
            }
        }
    }

    /// A configured command that leaves the checkout unchanged records the
    /// same content tree before and after the run (worker checks, plan
    /// 3b8f715a): the pair is what certifies the tested tree.
    #[tokio::test]
    async fn configured_unchanging_command_records_equal_trees() {
        let dir = tempdir().unwrap();
        git_repo(dir.path());
        let tool = tool_in(dir.path())
            .with_check_commands(vec!["git status".to_string()]);
        let result = tool
            .execute(json!({"command": "git status", "purpose": "measure the tree"}))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("data");
        assert_eq!(data["exit_code"].as_i64(), Some(0), "{data}");
        let before = data["tree_before"].as_str().expect("tree_before");
        let after = data["tree_after"].as_str().expect("tree_after");
        assert!(!before.is_empty(), "{data}");
        assert_eq!(before, after, "{data}");
    }

    /// A configured command that edits a tracked file records different
    /// trees (worker checks, plan 3b8f715a): a run that changed the tree it
    /// tested can never certify a later commit.
    #[tokio::test]
    async fn configured_editing_command_records_different_trees() {
        let dir = tempdir().unwrap();
        git_repo(dir.path());
        let command = "git config --file tracked.txt test.value changed";
        let tool = tool_in(dir.path()).with_check_commands(vec![command.to_string()]);
        let result = tool
            .execute(json!({"command": command, "purpose": "change the tree"}))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("data");
        assert_eq!(data["exit_code"].as_i64(), Some(0), "{data}");
        let before = data["tree_before"].as_str().expect("tree_before");
        let after = data["tree_after"].as_str().expect("tree_after");
        assert_ne!(before, after, "{data}");
    }

    /// A command outside the allow-list is never measured: no tree fields
    /// ride the result, so the pump cannot count it.
    #[tokio::test]
    async fn unconfigured_command_records_no_trees() {
        let dir = tempdir().unwrap();
        git_repo(dir.path());
        let tool = tool_in(dir.path());
        let result = tool
            .execute(json!({"command": "git status", "purpose": "not measured"}))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("data");
        assert!(data.get("tree_before").is_none(), "{data}");
        assert!(data.get("tree_after").is_none(), "{data}");
    }

    /// A configured command run from a directory other than the checkout
    /// root is never measured: only the root run can certify the tree.
    #[tokio::test]
    async fn configured_command_from_a_subdirectory_records_no_trees() {
        let dir = tempdir().unwrap();
        git_repo(dir.path());
        std::fs::create_dir(dir.path().join("sub")).expect("subdirectory");
        let command = "git status";
        let tool = tool_in(dir.path()).with_check_commands(vec![command.to_string()]);
        let result = tool
            .execute(json!({
                "command": command,
                "cwd": "sub",
                "purpose": "not the checkout root"
            }))
            .await;
        assert!(result.success, "{}", result.output);
        let data = result.data.expect("data");
        assert!(data.get("tree_before").is_none(), "{data}");
        assert!(data.get("tree_after").is_none(), "{data}");
    }

    /// Plan 0ff83146: an allow-listed check command run inside a longer
    /// command line cannot count — the result says so at the top and names the
    /// command to run alone. It rides even a failing run (this one is outside
    /// a checkout, so `git status` itself errors).
    #[tokio::test]
    async fn uncounted_check_run_gets_a_note() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path()).with_check_commands(vec!["git status".to_string()]);
        let result = tool
            .execute(json!({"command": "git status; echo done", "purpose": "chained check"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result
                .output
                .starts_with("NOTE: this run does not count as the `git status` check"),
            "{}",
            result.output
        );
    }

    /// Plan 0ff83146: the counted form (the exact command, at the checkout
    /// root) is not nagged about — the note is for the form that cannot count.
    #[tokio::test]
    async fn counted_check_run_gets_no_uncounted_note() {
        let dir = tempdir().unwrap();
        git_repo(dir.path());
        let tool = tool_in(dir.path()).with_check_commands(vec!["git status".to_string()]);
        let result = tool
            .execute(json!({"command": "git status", "purpose": "counted check"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(!result.output.contains("does not count"), "{}", result.output);
    }

    /// Plan 0ff83146: a COUNTED run that changed tracked files cannot certify
    /// the commit being checked, and the result says so.
    #[tokio::test]
    async fn check_run_that_edits_files_gets_tree_note() {
        let dir = tempdir().unwrap();
        git_repo(dir.path());
        let command = "git config --file tracked.txt test.value changed";
        let tool = tool_in(dir.path()).with_check_commands(vec![command.to_string()]);
        let result = tool
            .execute(json!({"command": command, "purpose": "change the tree"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("changed tracked files"),
            "{}",
            result.output
        );
    }

    /// Plan 0ff83146: writing a file through the shell gets the file-tools
    /// note — and a `tee` inside an ordinary word does not.
    #[tokio::test]
    async fn shell_file_write_gets_a_note() {
        let dir = tempdir().unwrap();
        let tool = tool_in(dir.path());
        let write = if cfg!(target_os = "windows") {
            "Set-Content x.txt hi"
        } else {
            "echo hi >> x.txt"
        };
        let result = tool
            .execute(json!({"command": write, "purpose": "write a file"}))
            .await;
        assert!(result.success, "{}", result.output);
        assert!(
            result.output.contains("change files with file_edit"),
            "{}",
            result.output
        );
        let unrelated = tool
            .execute(json!({"command": "echo guarantee", "purpose": "no write"}))
            .await;
        assert!(unrelated.success, "{}", unrelated.output);
        assert!(
            !unrelated.output.contains("change files with file_edit"),
            "{}",
            unrelated.output
        );
    }
}
