// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Mnemo's own run of the project checks (`team_run_checks`).
//!
//! A fix round used to end with the worker running all four
//! `.coding/checks.toml` commands, and the lead then ran them again before the
//! merge. Here Mnemo runs them once, at the reviewed tip, in the worker's
//! checkout (or the lead's), through the machine's heavy-command slots, and
//! records each command as a check the merge gate reads exactly like a
//! lead-recorded one.
//!
//! The run reuses the `run_checks` tool the worker sessions get (same command
//! validation, same slots, same failing-line extraction); this module only
//! decides when it runs, what blocks while it does, and how its blocks become
//! checks and one notice. The run happens on a spawned task and the tool
//! returns at once, like the proofs ([`super::proving`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, RwLock};

use mnemo::app::worktrees::current_branch;
use mnemo::tool::agent::run_checks::RunChecksTool;
use mnemo::tool::agent::sandbox::Sandbox;
use mnemo::tool::Tool;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use crate::mnemo2::phase3;
use crate::mnemo2::usage::ProjectUsage;

use super::dispatch::BoxFuture;
use super::persistence::TeamPersistence;
use super::pump::counted_checks;
use super::pump::save_workers;
use super::record::{WorkerCheck, WorkerRecord, WorkerStatus};
use super::review::CheckRecorder;
use super::spawn::load_check_commands;
use super::{now_key, BoardSignal, TeamRuntime};

/// The in-progress key of a run in the lead checkout (no worker owns it).
const LEAD_KEY: &str = "@lead";

/// What the tool says when the project configured no checks.
const NO_CHECKS: &str = "no checks configured: add commands to .coding/checks.toml";

/// The worker ids (or [`LEAD_KEY`]) whose check runs are in progress.
///
/// A worker whose checks run must not merge, take a message, resume or get a
/// routed fix: each would move the checkout or record a verdict under the run.
#[derive(Clone, Debug, Default)]
pub(super) struct CheckRuns {
    ids: Arc<StdMutex<HashSet<String>>>,
}

impl CheckRuns {
    /// Whether `key`'s run is in progress.
    pub(super) fn is_active(&self, key: &str) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(key)
    }

    /// Hold a worker's run for the rest of the test. Tests only: production
    /// claims through [`CheckRuns::claim`], which releases on drop.
    #[cfg(test)]
    pub(super) fn hold_for_test(&self, key: &str) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string())
    }

    /// Claim `key`, or `None` when its run is already in progress.
    fn claim(&self, key: &str) -> Option<CheckRunGuard> {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        if !ids.insert(key.to_string()) {
            return None;
        }
        Some(CheckRunGuard {
            runs: self.clone(),
            key: key.to_string(),
        })
    }
}

/// The claim one running check run holds; dropping it frees the worker.
struct CheckRunGuard {
    runs: CheckRuns,
    key: String,
}

impl Drop for CheckRunGuard {
    fn drop(&mut self) {
        self.runs
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

/// The refusal every entry point that would move or judge a worker's checkout
/// returns while Mnemo runs its checks.
pub(super) fn check_run_error(worker_id: &str) -> String {
    format!("Mnemo is running the checks for {worker_id}; wait for its notice.")
}

/// The refusal for a checkout with uncommitted changes: Mnemo checks only a
/// committed tip.
fn dirty_error(worker_id: &str) -> String {
    format!(
        "{worker_id}'s checkout has uncommitted changes; Mnemo checks only a committed tip. Commit or discard them first."
    )
}

/// What a command that exited 0 but changed files is reported with.
const CHANGED_FILES: &str = ", but it changed the checkout's files, so it does not count. Commit or discard the change, or fix the command.";

/// One command's outcome as the run reports and records it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CommandOutcome {
    /// The exact allow-listed command.
    command: String,
    /// Whether it exited 0.
    passed: bool,
    /// The block's headline inside the parentheses: `exit 0, 1.2s`.
    headline: String,
    /// The failing lines `run_checks` extracted, one per line; empty on a pass.
    failing: String,
    /// The command exited 0 but the pump's counting rule rejected it (it
    /// changed the checkout's files), so it is recorded as failed.
    changed_files: bool,
}

impl CommandOutcome {
    /// The check detail: `run by Mnemo:`, the exit code and run time, then the
    /// failing lines.
    fn detail(&self) -> String {
        let mut detail = format!("run by Mnemo: {}", self.headline);
        if self.changed_files {
            detail.push_str(CHANGED_FILES);
        }
        if !self.failing.is_empty() {
            detail.push('\n');
            detail.push_str(&self.failing);
        }
        detail
    }

    /// The notice line(s) for this command.
    fn notice_block(&self) -> String {
        let word = if self.passed { "PASS" } else { "FAIL" };
        let mut block = format!("{word} {} ({})", self.command, self.headline);
        if self.changed_files {
            block.push_str(CHANGED_FILES);
        }
        if !self.failing.is_empty() {
            block.push('\n');
            block.push_str(&self.failing);
        }
        block
    }
}

/// The headline and failing lines one command's block carries in the
/// `run_checks` text.
struct Block {
    /// What sits inside the parentheses of the block's first line.
    headline: String,
    /// The extracted failing lines, one per line.
    failing: String,
}

/// Find each command's block in the `run_checks` answer, aligned with
/// `commands`: `None` for a command whose block is not in the text (the text
/// is capped, so a long run can lose its tail). The tool prints one block per
/// command, in order, each opening with `PASS <command> (` or `FAIL <command>
/// (`, then a `N passed, M failed` summary.
fn parse_blocks(commands: &[String], text: &str) -> Vec<Option<Block>> {
    let lines: Vec<&str> = text.lines().collect();
    // (line index, the text after the command) per found command.
    let mut found: Vec<Option<(usize, &str)>> = Vec::new();
    let mut from = 0;
    for command in commands {
        let hit = lines.iter().enumerate().skip(from).find_map(|(index, line)| {
            let rest = line
                .strip_prefix("PASS ")
                .or_else(|| line.strip_prefix("FAIL "))?;
            rest.strip_prefix(command.as_str())
                .filter(|tail| tail.starts_with(" ("))
                .map(|tail| (index, tail))
        });
        if let Some((index, _)) = hit {
            from = index + 1;
        }
        found.push(hit);
    }
    let mut blocks = Vec::new();
    for (position, hit) in found.iter().enumerate() {
        let Some((index, tail)) = hit else {
            blocks.push(None);
            continue;
        };
        // The block ends where the next found block starts.
        let end = found[position + 1..]
            .iter()
            .flatten()
            .next()
            .map_or(lines.len(), |next| next.0);
        let mut body: Vec<&str> = lines[index + 1..end].to_vec();
        if found[position + 1..].iter().all(Option::is_none) {
            // The summary line closes the last block.
            if let Some(summary) = body
                .iter()
                .rposition(|line| line.contains(" passed, ") && line.ends_with(" failed"))
            {
                body.truncate(summary);
            }
        }
        while body.last().is_some_and(|line| line.trim().is_empty()) {
            body.pop();
        }
        let headline = tail
            .trim_start_matches(" (")
            .split(')')
            .next()
            .unwrap_or("")
            .to_string();
        blocks.push(Some(Block {
            headline,
            failing: body.join("\n"),
        }));
    }
    blocks
}

/// What one run found: an outcome per `data.checks` entry, and the commands
/// that had no entry at all.
struct RunReport {
    outcomes: Vec<CommandOutcome>,
    /// Allow-listed commands the tool returned no entry for.
    missing: Vec<String>,
}

impl RunReport {
    /// Everything ran and counted: an empty list, or a command with no entry,
    /// is never a pass.
    fn all_passed(&self) -> bool {
        !self.outcomes.is_empty()
            && self.missing.is_empty()
            && self.outcomes.iter().all(|outcome| outcome.passed)
    }
}

/// Run every command with the `run_checks` tool in `checkout`.
async fn run_all(
    checkout: &Path,
    commands: &[String],
    config_dir: &Path,
) -> Result<RunReport, String> {
    let sandbox = Sandbox::new(checkout.to_path_buf())
        .map_err(|error| format!("cannot open the checkout {}: {error}", checkout.display()))?;
    let mut tool = RunChecksTool::new(sandbox).with_check_commands(commands.to_vec());
    // The same slots and the same core-operation list the worker sessions'
    // tool gets (`agent/factory.rs`): the saved `[general.machine]` limit, the
    // slot files under the config dir, and `[git] core_operations`. A config
    // that cannot be read runs unslotted with the default list; the limit is
    // never a failure mode.
    if let Ok(config) = mnemo::config::Config::load(config_dir) {
        tool = tool
            .with_machine(
                Arc::new(RwLock::new(config.general.general.machine.clone())),
                mnemo::machine_slots::default_dir(config_dir),
            )
            .with_core_operations(Arc::new(RwLock::new(
                config.general.git.core_operations.clone(),
            )));
    }
    if !tool.has_checks() {
        return Err(NO_CHECKS.to_string());
    }
    let args = json!({"purpose": "Mnemo running the project checks"});
    let result = tool.execute(args.clone()).await;
    if !result.success {
        return Err(result.output);
    }
    let Some(entries) = result
        .data
        .as_ref()
        .and_then(|data| data.get("checks"))
        .and_then(Value::as_array)
    else {
        return Err("the check tool returned no per-command results".to_string());
    };
    // The pump's own counting rule decides what passed: exit 0, an allowed
    // command, run from the checkout, files unchanged.
    let counted = counted_checks("run_checks", &args, &result, checkout, commands);
    let run: Vec<String> = entries
        .iter()
        .filter_map(|entry| entry.get("command").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    // One outcome per entry; the capped text only supplies the headline and
    // the failing lines.
    let blocks = parse_blocks(&run, &result.output);
    let mut outcomes = Vec::new();
    for (entry, block) in entries.iter().zip(blocks) {
        let Some(command) = entry.get("command").and_then(Value::as_str) else {
            continue;
        };
        let exit = entry.get("exit_code").and_then(Value::as_i64);
        let is_counted = counted.iter().any(|(counted, _)| counted == command);
        let (headline, failing) = match block {
            Some(block) => (block.headline, block.failing),
            None => (
                format!(
                    "exit {}; its output was cut off",
                    exit.map_or_else(|| "none".to_string(), |code| code.to_string())
                ),
                String::new(),
            ),
        };
        outcomes.push(CommandOutcome {
            command: command.to_string(),
            passed: is_counted,
            headline,
            failing: if is_counted { String::new() } else { failing },
            changed_files: exit == Some(0) && !is_counted,
        });
    }
    let missing = commands
        .iter()
        .filter(|command| !run.contains(command))
        .cloned()
        .collect();
    Ok(RunReport { outcomes, missing })
}

/// Watch a spawned run: when it panics or is cancelled, the lead still gets a
/// notice (the claim is released as the task unwinds).
fn supervise(
    signals: broadcast::Sender<BoardSignal>,
    key: String,
    text: String,
    run: tokio::task::JoinHandle<()>,
) {
    tokio::spawn(async move {
        if run.await.is_err() {
            let _ = signals.send(BoardSignal::Notice {
                key: format!("checks:{key}:stopped:{}", now_key()),
                text,
            });
        }
    });
}

/// The first eight characters of a commit id.
fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

/// The handles one background run reads from the runtime.
struct CheckCtx {
    workspace_root: PathBuf,
    config_dir: PathBuf,
    records: Arc<Mutex<Vec<WorkerRecord>>>,
    usage: Arc<Mutex<ProjectUsage>>,
    save_lock: Arc<Mutex<()>>,
    persistence: TeamPersistence,
    signals: broadcast::Sender<BoardSignal>,
}

impl CheckCtx {
    /// The run's notice text. `subject` is what ran, `tip` where.
    fn notice_text(
        header: Option<&str>,
        subject: &str,
        report: &RunReport,
        next: &str,
    ) -> String {
        let outcomes = &report.outcomes;
        let passed = outcomes.iter().filter(|outcome| outcome.passed).count();
        let failed = outcomes.len() - passed;
        let mut text = String::new();
        if let Some(header) = header {
            text.push_str(header);
            text.push('\n');
        }
        text.push_str(&format!("{subject}: {passed} passed, {failed} failed.\n"));
        for outcome in outcomes {
            text.push_str(&outcome.notice_block());
            text.push('\n');
        }
        for command in &report.missing {
            text.push_str(&format!("NO RESULT {command} (the tool returned no entry)\n"));
        }
        text.push_str(next);
        text
    }

    fn notice(&self, key: String, text: String) {
        let _ = self.signals.send(BoardSignal::Notice { key, text });
    }

    /// The whole background run for a worker: run, compare the tip, record,
    /// release the claim, tell the lead.
    async fn run_worker(
        &self,
        worker_id: &str,
        worktree: PathBuf,
        tip: String,
        commands: Vec<String>,
        guard: CheckRunGuard,
    ) {
        let header = format!("worker_id={worker_id}");
        let outcomes = run_all(&worktree, &commands, &self.config_dir).await;
        let text = match outcomes {
            Err(error) => format!("{header}\nMnemo could not run the checks for {worker_id}: {error}"),
            Ok(report) => {
                let outcomes = &report.outcomes;
                let now = phase3::tip(&worktree).await.unwrap_or_default();
                if now != tip {
                    format!(
                        "{header}\nMnemo ran the checks for {worker_id} but its tip moved from {} to {} while they ran, so nothing was recorded. Run team_run_checks again.",
                        short(&tip),
                        short(&now)
                    )
                } else {
                    let recorder = CheckRecorder::new(
                        &self.records,
                        &self.usage,
                        &self.save_lock,
                        &self.persistence,
                    );
                    let mut problem = None;
                    for outcome in outcomes {
                        let check = WorkerCheck {
                            name: outcome.command.clone(),
                            status: if outcome.passed { "passed" } else { "failed" }.to_string(),
                            detail: Some(outcome.detail()),
                            tip: tip.clone(),
                        };
                        if let Err(error) = recorder.record_at_tip(worker_id, check).await {
                            problem = Some(error);
                            break;
                        }
                    }
                    if problem.is_none() {
                        problem = save_workers(
                            &self.records,
                            &self.usage,
                            &self.save_lock,
                            &self.persistence,
                        )
                        .await
                        .err();
                    }
                    let next = match (&problem, report.all_passed()) {
                        (Some(error), _) => format!("The results could not be recorded: {error}"),
                        (None, true) => "Merge it with team_merge.".to_string(),
                        (None, false) => "Route the failures with team_route_fix.".to_string(),
                    };
                    Self::notice_text(
                        Some(&header),
                        &format!("Mnemo ran the checks for {worker_id} at {}", short(&tip)),
                        &report,
                        &next,
                    )
                }
            }
        };
        // The claim goes before the notice, so the lead's next call is not
        // refused for a run that already ended.
        drop(guard);
        self.notice(format!("checks:{worker_id}:{}", now_key()), text);
    }

    /// The same run in the lead checkout; nothing is recorded.
    async fn run_lead(&self, tip: String, branch: String, commands: Vec<String>, guard: CheckRunGuard) {
        let outcomes = run_all(&self.workspace_root, &commands, &self.config_dir).await;
        let subject = format!(
            "Mnemo ran the checks in the lead checkout ({branch} at {})",
            short(&tip)
        );
        let text = match outcomes {
            Err(error) => format!("Mnemo could not run the checks in the lead checkout: {error}"),
            Ok(report) => {
                let now = phase3::tip(&self.workspace_root).await.unwrap_or_default();
                if now != tip {
                    format!(
                        "Mnemo ran the checks in the lead checkout, but the lead branch moved from {} to {} while the checks ran; run team_run_checks again.",
                        short(&tip),
                        short(&now)
                    )
                } else {
                    let next = if report.all_passed() {
                        "The lead branch passes the checks."
                    } else {
                        "The lead branch fails the checks; nothing was recorded on any worker."
                    };
                    Self::notice_text(None, &subject, &report, next)
                }
            }
        };
        drop(guard);
        self.notice(format!("checks:lead:{}", now_key()), text);
    }
}

impl TeamRuntime {
    fn check_ctx(&self) -> CheckCtx {
        CheckCtx {
            workspace_root: self.workspace_root.clone(),
            config_dir: self.config_dir.clone(),
            records: self.records.clone(),
            usage: self.usage.clone(),
            save_lock: self.save_lock.clone(),
            persistence: self.persistence.clone(),
            signals: self.signals.clone(),
        }
    }

    /// `team_run_checks`: start Mnemo's check run for `worker_id`, or in the
    /// lead checkout when it is `None`. It returns at once; the run's notice
    /// follows. Boxed because the lead tool dispatcher awaits it and its
    /// future must stay small.
    pub(super) fn run_checks_tool<'a>(
        &'a self,
        worker_id: Option<&'a str>,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let commands: Vec<String> = load_check_commands(&self.workspace_root).as_ref().clone();
            if commands.is_empty() {
                return Err(NO_CHECKS.to_string());
            }
            let Some(worker_id) = worker_id else {
                let Some(guard) = self.check_runs.claim(LEAD_KEY) else {
                    return Err("Mnemo is already running the checks in the lead checkout; wait for its notice.".to_string());
                };
                let tip = phase3::tip(&self.workspace_root).await?;
                let branch = current_branch(&self.workspace_root)
                    .map_err(|error| format!("cannot read the lead branch: {error}"))?;
                let ctx = self.check_ctx();
                let started = format!("{branch} at {}", short(&tip));
                let signals = ctx.signals.clone();
                let run = tokio::spawn(async move {
                    ctx.run_lead(tip, branch, commands, guard).await
                });
                supervise(
                    signals,
                    "lead".to_string(),
                    "Mnemo's check run in the lead checkout stopped unexpectedly; nothing was recorded. Run team_run_checks again.".to_string(),
                    run,
                );
                return Ok(format!(
                    "Mnemo is running the checks in the lead checkout ({started}); a notice follows. Nothing is recorded."
                ));
            };
            // The merge lock serializes this claim with every entry point that
            // takes the checkout (they check `proving_guard` after taking it).
            let _lock = self.merge_lock.lock().await;
            let worker = self.worker(worker_id).await?;
            if worker.kind != "implementation" {
                return Err("only implementation workers are checked".to_string());
            }
            if matches!(
                worker.status,
                WorkerStatus::Merging | WorkerStatus::Merged | WorkerStatus::Cancelled
            ) {
                return Err(format!(
                    "worker '{worker_id}' is {:?}; its tip can no longer be checked",
                    worker.status
                ));
            }
            if self.proving.is_active(worker_id) {
                return Err(super::proving::proving_error(worker_id));
            }
            if self.check_runs.is_active(worker_id) {
                return Err(check_run_error(worker_id));
            }
            if self.worker_is_busy(worker_id).await {
                return Err(format!(
                    "worker '{worker_id}' is busy; the checks run in its checkout while it is idle"
                ));
            }
            let Some(guard) = self.check_runs.claim(worker_id) else {
                return Err(check_run_error(worker_id));
            };
            if phase3::require_clean(&worker.worktree, "worker").await.is_err() {
                return Err(dirty_error(worker_id));
            }
            let tip = self.worker_tip(&worker).await?;
            let worktree = worker.worktree.clone();
            let ctx = self.check_ctx();
            let id = worker_id.to_string();
            let started = short(&tip).to_string();
            let signals = ctx.signals.clone();
            let stopped = format!(
                "worker_id={worker_id}\nMnemo's check run for {worker_id} stopped unexpectedly; nothing was recorded. Run team_run_checks again."
            );
            let run = tokio::spawn(async move {
                ctx.run_worker(&id, worktree, tip, commands, guard).await
            });
            supervise(signals, worker_id.to_string(), stopped, run);
            Ok(format!(
                "Mnemo is running the checks for {worker_id} at {started}; a notice follows and records each command on that tip."
            ))
        })
    }

    /// After a review with no open findings: start the missing checks. The
    /// answer is the line the `team_review` result appends, `None` when no run
    /// was needed or none could start.
    pub(super) fn start_missing_checks<'a>(
        &'a self,
        reviewed: &'a WorkerRecord,
    ) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            if reviewed.kind != "implementation"
                || reviewed.findings.iter().any(|finding| finding.status == "open")
            {
                return None;
            }
            let tip = reviewed.review.as_ref()?.tip.clone();
            let commands = load_check_commands(&self.workspace_root);
            let missing = commands.iter().any(|command| {
                !reviewed.checks.iter().any(|check| {
                    &check.name == command && check.status == "passed" && check.tip == tip
                })
            });
            if !missing {
                return None;
            }
            match self.run_checks_tool(Some(&reviewed.id)).await {
                Ok(_) => Some(format!(
                    "Mnemo is running the missing checks at {}; merge after its notice.",
                    short(&tip)
                )),
                // A dirty checkout is the lead's to know: the review result says so.
                Err(error) if error == dirty_error(&reviewed.id) => Some(error),
                Err(error) => {
                    eprintln!("team: the missing checks of {} did not start: {error}", reviewed.id);
                    None
                }
            }
        })
    }
}
