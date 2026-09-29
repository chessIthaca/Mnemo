// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Harness-run deterministic verify at plan-step boundaries (the cost-saving
//! chain's verification item): with `[general.verify] enabled` on and a
//! non-empty `test_command`, the harness runs the command ITSELF at each plan
//! step boundary — a successful skeleton `complete_step` — and feeds back a
//! compact evidence note (exit status, distinct error lines, counts) instead
//! of the model spending a roundtrip on "run the tests". Precedent: the
//! failure-triage tier-1 auto-retry ([`super::failure_triage`]) — a harness
//! action on deterministic evidence, no model roundtrip.
//!
//! # What is evidence, what is failure
//!
//! A verification that FAILS never fails the `complete_step` it rides: the
//! step DID tick, and a false result would drive the tier-1 auto-retry into
//! re-ticking a completed step (and the tier-2 classifier into blaming a
//! successful action). The failure signal lives in the NOTE text —
//! `FAILED (exit N)` / `timed out after Ns and was killed` — which is the
//! triage-able shape: deterministic evidence in the model's context, acted on
//! by its next decision.
//!
//! # Inert by default
//!
//! Flag off or an empty command => [`StepVerifyHandle::run_checks`] returns
//! `None` WITHOUT spawning a process, so a config that never touched
//! `[general.verify]` behaves byte-identically to the pre-feature app.
//!
//! # Multi-platform
//!
//! The command runs through the platform shell (`powershell -Command` on
//! Windows, `sh -c` elsewhere — the `shell` tool's own idiom). It is run
//! VERBATIM: the shell tool's `&&`/`||` chain translation is private to that
//! tool, so `test_command` should be a single command (or a script). The
//! timeout kills the child cleanly (`kill_on_drop`), so a hanging suite can
//! never wedge the plan.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::process::Command;

use crate::config::{OptimizerConfig, VerifyConfig};
use crate::tool::agent::output_compactor;

/// The greppable head tag on every evidence note.
const NOTE_TAG: &str = "[verify]";

/// Hard cap on an evidence note, in bytes. The compressor path is already
/// compact; this bounds the FALLBACK path (an unknown command family, a
/// pathological failure dump) so a note can never paste a log into context.
const MAX_NOTE_BYTES: usize = 2000;

/// Appended when a note hits [`MAX_NOTE_BYTES`].
const TRUNCATION_MARKER: &str = "\n[...truncated]";

/// Most distinct output lines the fallback body keeps.
const MAX_FALLBACK_LINES: usize = 20;

/// Longest passing summary kept (the runner's own tail line).
const MAX_SUMMARY_CHARS: usize = 200;

/// What one harness-run verification produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// The command exited 0.
    Passed {
        /// A compact tail of the output (typically the runner's own summary,
        /// e.g. `test result: ok. 1320 passed; 0 failed`); empty when the
        /// command printed nothing.
        summary: String,
    },
    /// The command exited non-zero, or could not be spawned at all
    /// (`exit_code` is then `-1` and `stderr` names the spawn error).
    Failed {
        /// The process exit code (`-1` for a spawn/IO failure).
        exit_code: i32,
        /// Raw stdout — compressed at note-build time, never pasted whole.
        stdout: String,
        /// Raw stderr — compressed at note-build time, never pasted whole.
        stderr: String,
    },
    /// The command was killed after the configured timeout elapsed.
    TimedOut {
        /// The timeout that elapsed, in seconds.
        secs: u64,
    },
}

/// Runs `[general.verify]`'s command at plan-step boundaries and renders the
/// evidence note. The factory builds ONE handle and clones it into both
/// consumers (the `complete_step` tool and the agent loop's action=verify
/// path); both read the same live config, so a Settings save is observed on
/// the next boundary with no rebuild.
pub struct StepVerifyHandle {
    cfg: Arc<RwLock<VerifyConfig>>,
    optimizer: Arc<RwLock<OptimizerConfig>>,
    root: PathBuf,
}

impl StepVerifyHandle {
    /// Create a handle over the shared `[general.verify]` and
    /// `[general.optimizer]` config handles. The command runs in the process
    /// working directory until [`with_root`](Self::with_root) overrides it —
    /// the factory points it at the project sandbox root.
    pub fn new(cfg: Arc<RwLock<VerifyConfig>>, optimizer: Arc<RwLock<OptimizerConfig>>) -> Self {
        Self {
            cfg,
            optimizer,
            root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Override the directory the verification command runs in (the project
    /// root: a relative `test_command` and the tests themselves resolve
    /// against it).
    pub fn with_root(mut self, root: PathBuf) -> Self {
        self.root = root;
        self
    }

    /// Run the configured command once, or `None` when the feature is inert
    /// (`enabled` off or `test_command` blank) — an inert handle spawns NO
    /// process. The config is read live, so a mid-session Settings save takes
    /// effect at the next boundary. Returns the command that RAN together
    /// with its outcome, so the caller's note names exactly that command even
    /// if the live config changes right after.
    pub async fn run_checks(&self) -> Option<(String, VerifyOutcome)> {
        let (command, timeout_secs) = {
            let cfg = self.cfg.read().expect("verify config lock poisoned");
            if !cfg.enabled || cfg.test_command.trim().is_empty() {
                return None;
            }
            (cfg.test_command.clone(), cfg.timeout_secs)
        };
        let (program, flag) = if cfg!(target_os = "windows") {
            ("powershell", "-Command")
        } else {
            ("sh", "-c")
        };
        let mut cmd = Command::new(program);
        cmd.arg(flag)
            .arg(&command)
            .current_dir(&self.root)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Suppress the console window a GUI app would otherwise pop up (the
        // shell tool's CREATE_NO_WINDOW idiom).
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Some((
                    command,
                    VerifyOutcome::Failed {
                        exit_code: -1,
                        stdout: String::new(),
                        stderr: format!("failed to spawn verification command: {e}"),
                    },
                ))
            }
        };
        // `kill_on_drop` above is what makes the timeout a CLEAN cancel: when
        // the timeout elapses the wait future is dropped, the child drops with
        // it, and the process is killed — a hanging suite can never wedge the
        // plan (the shell tool's guarantee, mirrored).
        match tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
        {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                if output.status.success() {
                    Some((
                        command,
                        VerifyOutcome::Passed {
                            summary: pass_summary(&stdout, &stderr),
                        },
                    ))
                } else {
                    Some((
                        command,
                        VerifyOutcome::Failed {
                            exit_code: output.status.code().unwrap_or(-1),
                            stdout,
                            stderr,
                        },
                    ))
                }
            }
            Ok(Err(e)) => Some((
                command,
                VerifyOutcome::Failed {
                    exit_code: -1,
                    stdout: String::new(),
                    stderr: format!("verification command failed: {e}"),
                },
            )),
            Err(_elapsed) => Some((command, VerifyOutcome::TimedOut { secs: timeout_secs })),
        }
    }

    /// Render the compact evidence note for one outcome: a header naming the
    /// command and the status, then the compressed failure body (lever 2 when
    /// the command belongs to a known family, a distinct-lines fallback
    /// otherwise) — capped at [`MAX_NOTE_BYTES`] so no log ever lands in
    /// context whole.
    pub fn evidence_note(&self, command: &str, outcome: &VerifyOutcome) -> String {
        let head = match outcome {
            VerifyOutcome::Passed { .. } => format!("{NOTE_TAG} `{command}` — passed (exit 0)"),
            VerifyOutcome::Failed { exit_code, .. } => {
                format!("{NOTE_TAG} `{command}` — FAILED (exit {exit_code})")
            }
            VerifyOutcome::TimedOut { secs } => format!(
                "{NOTE_TAG} `{command}` — timed out after {secs}s and was killed (verification failed)"
            ),
        };
        let body = match outcome {
            VerifyOutcome::Passed { summary } => summary.clone(),
            VerifyOutcome::Failed {
                exit_code,
                stdout,
                stderr,
            } => {
                let compact_cfg = self
                    .optimizer
                    .read()
                    .expect("optimizer config lock poisoned")
                    .clone();
                match output_compactor::compress(command, stdout, stderr, *exit_code, &compact_cfg)
                {
                    Some(compacted) => compacted.text,
                    None => fallback_body(stdout, stderr),
                }
            }
            VerifyOutcome::TimedOut { .. } => String::new(),
        };
        let mut note = head;
        let body = body.trim_end();
        if !body.is_empty() {
            note.push('\n');
            note.push_str(body);
        }
        cap_note(note)
    }
}

/// The passing tail: the LAST non-empty output line (typically the runner's
/// own summary), redacted and length-capped.
fn pass_summary(stdout: &str, stderr: &str) -> String {
    let line = stdout
        .lines()
        .chain(stderr.lines())
        .map(|line| line.trim_end_matches('\r').trim())
        .filter(|line| !line.is_empty())
        .next_back()
        .unwrap_or("");
    let line = output_compactor::redact_secrets(line);
    if line.chars().count() > MAX_SUMMARY_CHARS {
        let mut out: String = line.chars().take(MAX_SUMMARY_CHARS).collect();
        out.push('…');
        out
    } else {
        line
    }
}

/// The fallback body for a command outside every compressor family (or a
/// compact form that was not meaningfully smaller): the first
/// [`MAX_FALLBACK_LINES`] distinct non-empty lines, each redacted and
/// annotated with its repeat count, plus a dropped-lines note.
fn fallback_body(stdout: &str, stderr: &str) -> String {
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut dropped = 0usize;
    for line in stdout.lines().chain(stderr.lines()) {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let line = output_compactor::redact_secrets(line);
        if let Some(&at) = index.get(&line) {
            if at != usize::MAX {
                seen[at].1 += 1;
            }
            continue;
        }
        if seen.len() < MAX_FALLBACK_LINES {
            index.insert(line.clone(), seen.len());
            seen.push((line, 1));
        } else {
            index.insert(line, usize::MAX);
            dropped += 1;
        }
    }
    let mut out = String::new();
    for (line, count) in &seen {
        if *count > 1 {
            out.push_str(&format!("{line} (repeated {count}×)\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if dropped > 0 {
        out.push_str(&format!(
            "… and {dropped} more distinct lines (raw output not shown)"
        ));
    }
    out.trim_end().to_string()
}

/// Cap a note at [`MAX_NOTE_BYTES`] on a char boundary, appending the
/// truncation marker inside the budget.
fn cap_note(mut note: String) -> String {
    if note.len() <= MAX_NOTE_BYTES {
        return note;
    }
    let budget = MAX_NOTE_BYTES.saturating_sub(TRUNCATION_MARKER.len());
    let mut end = budget;
    while end > 0 && !note.is_char_boundary(end) {
        end -= 1;
    }
    note.truncate(end);
    note.push_str(TRUNCATION_MARKER);
    note
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(enabled: bool, command: &str, timeout_secs: u64) -> StepVerifyHandle {
        let cfg = Arc::new(RwLock::new(VerifyConfig {
            enabled,
            test_command: command.to_string(),
            timeout_secs,
        }));
        let optimizer = Arc::new(RwLock::new(OptimizerConfig::default()));
        StepVerifyHandle::new(cfg, optimizer)
    }

    #[tokio::test]
    async fn inert_handle_does_not_run_even_a_bogus_command() {
        // `None` for a command that would certainly fail were it spawned
        // proves the inert path never reached a process: flag off and an
        // empty command are both no-ops.
        assert!(handle(false, "__mnemo_no_such_command__", 300)
            .run_checks()
            .await
            .is_none());
        assert!(handle(true, "", 300).run_checks().await.is_none());
        assert!(handle(true, "   ", 300).run_checks().await.is_none());
    }

    #[tokio::test]
    async fn passes_and_fails_with_the_real_exit_status() {
        match handle(true, "echo ok", 60).run_checks().await {
            Some((_, VerifyOutcome::Passed { summary })) => {
                assert!(summary.contains("ok"), "{summary}")
            }
            other => panic!("expected a pass, got {other:?}"),
        }
        match handle(true, "exit 7", 60).run_checks().await {
            Some((_, VerifyOutcome::Failed { exit_code, .. })) => assert_eq!(exit_code, 7),
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_releases_the_wait_promptly() {
        // The WAIT half of "cancels cleanly": a 10s sleep under a 1s timeout
        // must return in ~1s, not 10 — the harness future never wedges the
        // plan. The KILL half (the child actually dies) is pinned separately
        // by `timeout_kills_the_child_before_it_can_finish`.
        let sleep = if cfg!(target_os = "windows") {
            "Start-Sleep -Seconds 10"
        } else {
            "sleep 10"
        };
        let started = std::time::Instant::now();
        let outcome = handle(true, sleep, 1).run_checks().await;
        assert_eq!(
            outcome,
            Some((sleep.to_string(), VerifyOutcome::TimedOut { secs: 1 }))
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "the timeout did not release the wait: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn timeout_kills_the_child_before_it_can_finish() {
        // The KILL half, pinned observably (review finding L1): the command
        // would write a marker AFTER its sleep — were `kill_on_drop(true)`
        // ever removed, the wait would still return at the deadline but the
        // orphaned child would live on and write the marker. Waiting past the
        // child's own runtime before asserting absence gives a survivor every
        // chance to prove itself.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("kill-marker.txt");
        let command = if cfg!(target_os = "windows") {
            "Start-Sleep -Seconds 3; Set-Content -Path kill-marker.txt -Value done"
        } else {
            "sleep 3 && echo done > kill-marker.txt"
        };
        let verify = handle(true, command, 1).with_root(dir.path().to_path_buf());
        let outcome = verify.run_checks().await;
        assert_eq!(
            outcome,
            Some((command.to_string(), VerifyOutcome::TimedOut { secs: 1 }))
        );
        // The child would have finished at ~3s; wait past that before
        // asserting absence, so a survivor has every chance to write.
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !marker.exists(),
            "the timed-out child survived and wrote its marker — the kill half of the clean cancel is broken"
        );
    }

    #[test]
    fn passed_note_carries_the_summary_tail() {
        let note = handle(true, "cargo test", 300).evidence_note(
            "cargo test",
            &VerifyOutcome::Passed {
                summary: "test result: ok. 72 passed; 0 failed".into(),
            },
        );
        assert!(note.starts_with("[verify] `cargo test` — passed (exit 0)"));
        assert!(note.contains("72 passed"));
    }

    #[test]
    fn timed_out_note_is_failure_shaped() {
        let note = handle(true, "cargo test", 300)
            .evidence_note("cargo test", &VerifyOutcome::TimedOut { secs: 300 });
        assert!(note.contains("timed out after 300s and was killed"));
        assert!(note.contains("(verification failed)"));
    }

    #[test]
    fn failed_note_uses_the_lever_two_compressor_for_a_known_family() {
        let mut stdout = String::new();
        for i in 0..500 {
            stdout.push_str(&format!("test some::suite::case_{i} ... ok\n"));
        }
        stdout.push_str("test result: FAILED. 1 passed; 1 failed\n");
        stdout.push_str("error: test failed, to rerun pass `--lib`\n");
        let note = handle(true, "cargo test", 300).evidence_note(
            "cargo test",
            &VerifyOutcome::Failed {
                exit_code: 101,
                stdout,
                stderr: String::new(),
            },
        );
        assert!(note.starts_with("[verify] `cargo test` — FAILED (exit 101)"));
        assert!(
            note.contains("compressed cargo-test output — exit 101"),
            "{note}"
        );
        assert!(note.contains("error: test failed"));
        assert!(note.len() <= MAX_NOTE_BYTES);
    }

    #[test]
    fn failed_note_falls_back_to_distinct_lines_with_counts() {
        // An UNKNOWN command family (the fallback path): distinct lines with
        // repeat counts, plus a dropped-lines note once the cap is passed.
        let mut stdout = String::new();
        for _ in 0..5 {
            stdout.push_str("thread 'x' panicked at src/lib.rs:1\n");
        }
        for i in 0..40 {
            stdout.push_str(&format!("distinct diagnostic line {i}\n"));
        }
        let note = handle(true, "./run_checks.sh", 300).evidence_note(
            "./run_checks.sh",
            &VerifyOutcome::Failed {
                exit_code: 101,
                stdout,
                stderr: String::new(),
            },
        );
        assert!(note.starts_with("[verify] `./run_checks.sh` — FAILED (exit 101)"));
        assert!(note.contains("thread 'x' panicked at src/lib.rs:1 (repeated 5×)"));
        assert!(note.contains("more distinct lines"));
        assert!(note.len() <= MAX_NOTE_BYTES);
    }

    #[test]
    fn huge_failure_output_stays_compact() {
        // The acceptance's conciseness guarantee: a huge failing suite's
        // note never approaches the raw log — it is capped, with the exit
        // status still named.
        let mut stdout = String::new();
        for i in 0..10_000 {
            stdout.push_str(&format!(
                "thread 'worker_{i}' panicked at src/some/deep/file_{i}.rs:1:1\n"
            ));
        }
        let note = handle(true, "cargo test", 300).evidence_note(
            "cargo test",
            &VerifyOutcome::Failed {
                exit_code: 101,
                stdout,
                stderr: String::new(),
            },
        );
        assert!(note.len() <= MAX_NOTE_BYTES, "note was {} bytes", note.len());
        assert!(note.contains("exit 101"));
        assert!(note.contains("panicked"));
    }

    #[test]
    fn oversized_line_dumps_are_truncated_inside_the_cap() {
        // Three VERY long distinct lines (unknown family): the fallback
        // keeps them all, and the cap truncates the note on a char boundary.
        let long = "z".repeat(3_000);
        let stdout = format!("{long}\n{long}x\n{long}xy\n");
        let note = handle(true, "./run_checks.sh", 300).evidence_note(
            "./run_checks.sh",
            &VerifyOutcome::Failed {
                exit_code: 1,
                stdout,
                stderr: String::new(),
            },
        );
        assert!(note.len() <= MAX_NOTE_BYTES, "note was {} bytes", note.len());
        assert!(note.ends_with(TRUNCATION_MARKER));
    }
}
