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
use mnemo::tool::{Tool, ToolResult};
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex};

use crate::mnemo2::phase3;
use crate::mnemo2::usage::ProjectUsage;

use super::dispatch::BoxFuture;
use super::persistence::TeamPersistence;
use super::pump::{counted_checks_at, CheckoutRoot};
use super::pump::save_workers;
use super::record::{RemoteCheckout, WorkerCheck, WorkerRecord, WorkerStatus};
use super::remote::{checks_hash, environmental_failure, RemoteWorkers};
use super::review::CheckRecorder;
use super::spawn::load_check_commands;
use super::{now_key, BoardSignal, TeamRuntime};

#[cfg(test)]
#[path = "remote_testkit.rs"]
pub(super) mod remote_testkit;

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
    /// Tests only: the next spawned worker run panics at its start.
    #[cfg(test)]
    pub(super) panic_next: Arc<std::sync::atomic::AtomicBool>,
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

/// The first environmental failure among the failing outcomes, when one of them
/// names a shell or tool the host does not have.
///
/// This is what disqualifies a host for this project (`note_check`); a red test
/// at the lead's tip is the project's fault, not the host's, and stays a plain
/// failure.
fn environmental_reason(result: &ToolResult, report: &RunReport) -> Option<String> {
    let entries = result
        .data
        .as_ref()
        .and_then(|data| data.get("checks"))
        .and_then(Value::as_array)?;
    report.outcomes.iter().filter(|outcome| !outcome.passed).find_map(|outcome| {
        let entry = entries.iter().find(|entry| {
            entry.get("command").and_then(Value::as_str) == Some(outcome.command.as_str())
        })?;
        let exit = entry.get("exit_code").and_then(Value::as_i64);
        let text = if outcome.failing.is_empty() {
            outcome.headline.as_str()
        } else {
            outcome.failing.as_str()
        };
        if !environmental_failure(exit, text) {
            return None;
        }
        Some(
            outcome
                .failing
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or(outcome.headline.as_str())
                .to_string(),
        )
    })
}

/// Run every command with the `run_checks` tool in `checkout`.
///
/// `lead` marks the lead checkout's run: there a tree change confined to the
/// lead's own bookkeeping still counts as a pass ([`forgive_lead_bookkeeping`]).
/// A worker's run stays strict.
async fn run_all(
    checkout: &Path,
    commands: &[String],
    config_dir: &Path,
    lead: bool,
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
    report_with_lead_guard(checkout, commands, &args, &result, lead).await
}

/// One `run_checks` result as the run reports it: an outcome per entry, with the
/// lead checkout's bookkeeping-only tree change forgiven when `lead`.
///
/// [`run_all`] passes its flag here, so the branch a worker run takes
/// (`lead = false`: nothing forgiven) is the one the tests exercise.
async fn report_with_lead_guard(
    checkout: &Path,
    commands: &[String],
    args: &Value,
    result: &ToolResult,
    lead: bool,
) -> Result<RunReport, String> {
    let mut report = build_report(checkout, commands, args, result)?;
    if lead {
        forgive_lead_bookkeeping(checkout, result, &mut report).await;
    }
    Ok(report)
}

/// Count a lead checkout command as passed when the only files that changed
/// under it were the lead's own bookkeeping.
///
/// The counting rule ([`counted_checks`]) rejects a command whose content tree
/// differs before and after it ran. The lead keeps editing and committing its
/// plan, review and knowledge files while a long command runs, which changes
/// that tree without touching anything the command was meant to produce — and a
/// merge already ignores those files ([`phase3::require_clean_lead`]). So the
/// LEAD checkout's run diffs the two trees the entry measured and forgives the
/// change when every path between them is lead bookkeeping. Fail closed: a
/// missing tree id, a git error, an empty diff or one path outside the prefixes
/// leaves the outcome exactly as the counting rule left it. Worker runs never
/// come here.
async fn forgive_lead_bookkeeping(checkout: &Path, result: &ToolResult, report: &mut RunReport) {
    let Some(entries) = result
        .data
        .as_ref()
        .and_then(|data| data.get("checks"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for outcome in &mut report.outcomes {
        if !outcome.changed_files {
            continue;
        }
        // The command names its entry: `build_report` skips an entry without
        // one, so position alone would drift.
        let Some(entry) = entries.iter().find(|entry| {
            entry.get("command").and_then(Value::as_str) == Some(outcome.command.as_str())
        }) else {
            continue;
        };
        let Some(before) = entry.get("tree_before").and_then(Value::as_str) else {
            continue;
        };
        let Some(after) = entry.get("tree_after").and_then(Value::as_str) else {
            continue;
        };
        if before.is_empty() || after.is_empty() {
            continue;
        }
        let args = ["diff-tree", "-r", "--name-only", "-z", before, after];
        let Ok(diff) = phase3::git_value(checkout, &args).await else {
            continue;
        };
        let paths: Vec<&str> = diff.split('\0').filter(|path| !path.is_empty()).collect();
        if paths.is_empty() || !paths.iter().all(|path| phase3::is_lead_bookkeeping(path)) {
            continue;
        }
        outcome.passed = true;
        outcome.changed_files = false;
        outcome.failing = String::new();
    }
}

/// Turn the `run_checks` result into one outcome per `data.checks` entry. Split
/// out of [`run_all`] so a result of any shape can be tested.
fn build_report(
    checkout: &Path,
    commands: &[String],
    args: &Value,
    result: &ToolResult,
) -> Result<RunReport, String> {
    build_report_at(
        &CheckoutRoot::Local(checkout),
        commands,
        args,
        result,
    )
}

/// [`build_report`] against a hub or node checkout.
fn build_report_at(
    checkout: &CheckoutRoot<'_>,
    commands: &[String],
    args: &Value,
    result: &ToolResult,
) -> Result<RunReport, String> {
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
    let counted = counted_checks_at("run_checks", args, result, checkout, commands);
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
    /// The remote-worker seam: the node check run and the host measurement.
    remote: Arc<RemoteWorkers>,
    /// Tests only: this run panics at its start.
    #[cfg(test)]
    inject_panic: bool,
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
        #[cfg(test)]
        if self.inject_panic {
            panic!("injected check run panic");
        }
        let header = format!("worker_id={worker_id}");
        let outcomes = run_all(&worktree, &commands, &self.config_dir, false).await;
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

    /// The same run for a remote worker: the commands run in its node checkout,
    /// at the node's tip, and the hub shadow is refreshed onto that tip so the
    /// review, diff and merge read it locally.
    async fn run_worker_remote(
        &self,
        worker_id: &str,
        remote: RemoteCheckout,
        tip: String,
        commands: Vec<String>,
        guard: CheckRunGuard,
    ) {
        #[cfg(test)]
        if self.inject_panic {
            panic!("injected check run panic");
        }
        let header = format!("worker_id={worker_id}");
        let host = remote.node_name.clone();
        let text = match self.remote.run_checks(&remote, &commands, &tip).await {
            Err(error) => format!(
                "{header}\nMnemo could not run the checks for {worker_id} on host {host}: {error}"
            ),
            Ok(answer) => match build_report_at(
                &CheckoutRoot::Remote(&remote.path),
                &commands,
                &json!({}),
                &answer.result,
            ) {
                Err(error) => format!(
                    "{header}\nMnemo could not run the checks for {worker_id} on host {host}: {error}"
                ),
                Ok(report) => {
                    // The run measured the host, whatever the worker's tip does
                    // next: a shell or tool the host lacks disqualifies it; a red
                    // test does not.
                    self.remote.note_check(
                        &remote.node_id,
                        &checks_hash(&commands),
                        report.outcomes.iter().any(|outcome| outcome.passed),
                        environmental_reason(&answer.result, &report),
                    );
                    if answer.tip_before != tip || answer.tip_after != tip {
                        format!(
                            "{header}\nMnemo ran the checks for {worker_id} on host {host} but its tip moved from {} to {} while they ran, so nothing was recorded. Run team_run_checks again.",
                            short(&answer.tip_before),
                            short(&answer.tip_after)
                        )
                    } else {
                        let recorder = CheckRecorder::new(
                            &self.records,
                            &self.usage,
                            &self.save_lock,
                            &self.persistence,
                        );
                        let mut problem = None;
                        for outcome in &report.outcomes {
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
                        // The shadow must hold the tip those checks measured:
                        // review, diff and merge read it, not the node.
                        let record = {
                            let records = self.records.lock().await;
                            records
                                .iter()
                                .find(|worker| worker.id == worker_id)
                                .cloned()
                        };
                        if let Some(record) = record {
                            match self.remote.sync_shadow(&record, true).await {
                                Ok(_) => {
                                    // Persist the tip the shadow now holds: the
                                    // offline message and the next sync read it.
                                    if let Err(error) = save_workers(
                                        &self.records,
                                        &self.usage,
                                        &self.save_lock,
                                        &self.persistence,
                                    )
                                    .await
                                    {
                                        eprintln!(
                                            "team: could not save the synced tip of {worker_id}: {error}"
                                        );
                                    }
                                }
                                Err(error) => eprintln!(
                                    "team: could not sync the shadow of {worker_id} onto the tip its checks measured: {error}"
                                ),
                            }
                        }
                        let next = match (&problem, report.all_passed()) {
                            (Some(error), _) => {
                                format!("The results could not be recorded: {error}")
                            }
                            (None, true) => "Merge it with team_merge.".to_string(),
                            (None, false) => "Route the failures with team_route_fix.".to_string(),
                        };
                        Self::notice_text(
                            Some(&header),
                            &format!(
                                "Mnemo ran the checks for {worker_id} at {} on host {host}",
                                short(&tip)
                            ),
                            &report,
                            &next,
                        )
                    }
                }
            },
        };
        // The claim goes before the notice, so the lead's next call is not
        // refused for a run that already ended.
        drop(guard);
        self.notice(format!("checks:{worker_id}:{}", now_key()), text);
    }

    /// The same run in the lead checkout; nothing is recorded.
    async fn run_lead(&self, tip: String, branch: String, commands: Vec<String>, guard: CheckRunGuard) {
        let outcomes = run_all(&self.workspace_root, &commands, &self.config_dir, true).await;
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
            remote: self.remote.clone(),
            #[cfg(test)]
            inject_panic: self
                .check_runs
                .panic_next
                .swap(false, std::sync::atomic::Ordering::SeqCst),
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
            if let Some(remote) = worker.remote.clone() {
                // A remote worker's checkout lives on its node: take the
                // authoritative tip from the node and refuse a node tree the
                // worker still edits. The hub shadow follows that tip.
                let sync = self.sync_remote(worker_id).await?;
                if !sync.dirty.is_empty() {
                    return Err(super::proving::dirty_remote_error(
                        worker_id,
                        worker.host_label(),
                        &sync.dirty,
                    ));
                }
                let tip = sync.tip;
                let ctx = self.check_ctx();
                let id = worker_id.to_string();
                let started = short(&tip).to_string();
                let signals = ctx.signals.clone();
                let host = remote.node_name.clone();
                let stopped = format!(
                    "worker_id={worker_id}\nMnemo's check run for {worker_id} on host {host} stopped unexpectedly; nothing was recorded. Run team_run_checks again."
                );
                let run = tokio::spawn(async move {
                    ctx.run_worker_remote(&id, remote, tip, commands, guard)
                        .await
                });
                supervise(signals, worker_id.to_string(), stopped, run);
                return Ok(format!(
                    "Mnemo is running the checks for {worker_id} at {started} on host {host}; a notice follows and records each command on that tip."
                ));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(command: &str, checkout: &Path) -> Value {
        json!({
            "command": command,
            "exit_code": 0,
            "tree_before": "tree",
            "tree_after": "tree",
            "cwd": checkout.display().to_string(),
        })
    }

    /// The allow-listed command the lead-guard tests report.
    const LEAD_GUARD_COMMAND: &str = "cargo test";

    /// Run Git in `root`, returning its trimmed stdout; panic on failure.
    fn git_in(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git process");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Write a fixture file, creating its directory.
    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory");
        }
        std::fs::write(path, contents).expect("fixture file");
    }

    /// A committed project holding a source file and the lead's plan file, both
    /// at `first`. Returns its directory and the commit's tree id.
    fn tree_repo() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("repo");
        let root = dir.path();
        git_in(root, &["init", "-q", "-b", "main"]);
        git_in(root, &["config", "user.name", "Check Run Test"]);
        git_in(root, &["config", "user.email", "check-run@example.invalid"]);
        git_in(root, &["config", "core.autocrlf", "false"]);
        write(root, "src/a.rs", "first\n");
        write(root, ".coding/plans/x.md", "first\n");
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-qm", "base"]);
        let tree = git_in(root, &["rev-parse", "HEAD^{tree}"]);
        (dir, tree)
    }

    /// Commit the worktree as it stands and return the new commit's tree id.
    fn commit_tree(root: &Path, message: &str) -> String {
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-qm", message]);
        git_in(root, &["rev-parse", "HEAD^{tree}"])
    }

    /// The `run_checks` result of one exit-0 [`LEAD_GUARD_COMMAND`] that changed
    /// the tree from `before` to `after`, in the tool's own shape.
    fn changed_result(checkout: &Path, before: &str, after: &str) -> ToolResult {
        ToolResult {
            success: true,
            output: format!(
                "PASS {LEAD_GUARD_COMMAND} (exit 0, 1.0s) - changed files; not recorded\n\n1 passed, 0 failed"
            ),
            data: Some(json!({"checks": [{
                "command": LEAD_GUARD_COMMAND,
                "exit_code": 0,
                "tree_before": before,
                "tree_after": after,
                "cwd": checkout.display().to_string(),
            }]})),
        }
    }

    /// The lead commits a plan file while a long command runs: in the LEAD
    /// checkout's run that tree change is the lead's bookkeeping, so the command
    /// passes instead of failing with "changed the checkout's files".
    #[tokio::test]
    async fn lead_run_forgives_a_bookkeeping_only_tree_change() {
        let (dir, before) = tree_repo();
        let checkout = dir.path();
        write(checkout, ".coding/plans/x.md", "second\n");
        let after = commit_tree(checkout, "plan");
        let result = changed_result(checkout, &before, &after);
        let commands = vec![LEAD_GUARD_COMMAND.to_string()];

        // The counting rule alone still fails it, as the defect reported.
        let strict = build_report(checkout, &commands, &json!({}), &result).expect("a report");
        assert!(strict.outcomes[0].changed_files, "{:?}", strict.outcomes);
        assert!(!strict.all_passed());

        let report = report_with_lead_guard(checkout, &commands, &json!({}), &result, true)
            .await
            .expect("a report");
        let outcome = &report.outcomes[0];
        assert!(outcome.passed, "{outcome:?}");
        assert!(!outcome.changed_files, "{outcome:?}");
        assert!(outcome.failing.is_empty(), "{outcome:?}");
        assert_eq!(outcome.detail(), "run by Mnemo: exit 0, 1.0s");
        assert!(report.all_passed(), "the lead branch passes the checks");
    }

    /// A tree change that touches a source file — alone, or beside the plan
    /// file — still fails the lead checkout's run: only bookkeeping is forgiven.
    #[tokio::test]
    async fn lead_run_still_fails_a_source_change() {
        let (dir, before) = tree_repo();
        let checkout = dir.path();
        write(checkout, "src/a.rs", "second\n");
        let source = commit_tree(checkout, "source");
        write(checkout, ".coding/plans/x.md", "second\n");
        let both = commit_tree(checkout, "source and plan");
        let commands = vec![LEAD_GUARD_COMMAND.to_string()];

        for after in [source.as_str(), both.as_str()] {
            let result = changed_result(checkout, &before, after);
            let report = report_with_lead_guard(checkout, &commands, &json!({}), &result, true)
                .await
                .expect("a report");
            let outcome = &report.outcomes[0];
            assert!(!outcome.passed, "{after}: {outcome:?}");
            assert!(outcome.changed_files, "{after}: {outcome:?}");
            assert!(
                outcome.detail().contains("changed the checkout's files"),
                "{after}: {}",
                outcome.detail()
            );
            assert!(!report.all_passed(), "{after}");
        }
    }

    /// A worker checkout's run never forgives: the same bookkeeping-only tree
    /// change stays a failed command there.
    #[tokio::test]
    async fn worker_run_never_forgives_bookkeeping() {
        let (dir, before) = tree_repo();
        let checkout = dir.path();
        write(checkout, ".coding/plans/x.md", "second\n");
        let after = commit_tree(checkout, "plan");
        let result = changed_result(checkout, &before, &after);
        let commands = vec![LEAD_GUARD_COMMAND.to_string()];

        let report = report_with_lead_guard(checkout, &commands, &json!({}), &result, false)
            .await
            .expect("a report");
        let outcome = &report.outcomes[0];
        assert!(!outcome.passed, "{outcome:?}");
        assert!(outcome.changed_files, "{outcome:?}");
        assert!(!report.all_passed(), "a worker's tree change stays failed");
    }

    /// A tree id Git cannot diff keeps the failure: the guard fails closed
    /// instead of counting an unreadable change as bookkeeping.
    #[tokio::test]
    async fn lead_run_keeps_the_failure_when_git_cannot_diff_the_trees() {
        let (dir, before) = tree_repo();
        let checkout = dir.path();
        let result = changed_result(checkout, &before, "not-a-tree");
        let commands = vec![LEAD_GUARD_COMMAND.to_string()];

        let report = report_with_lead_guard(checkout, &commands, &json!({}), &result, true)
            .await
            .expect("a report");
        let outcome = &report.outcomes[0];
        assert!(!outcome.passed, "{outcome:?}");
        assert!(outcome.changed_files, "{outcome:?}");
        assert!(!report.all_passed());
    }

    /// An outcome per data entry, a cut-off block never reads as a clean run,
    /// a command with no entry is named, and neither (nor an empty list) is
    /// ever all-passed, so the notice never says to merge.
    #[test]
    fn cut_off_and_missing_results_never_say_merge() {
        let dir = tempfile::tempdir().expect("checkout");
        let commands: Vec<String> = ["git --version", "git status", "git log"]
            .iter()
            .map(|command| command.to_string())
            .collect();
        // The text carries the first command's block only; the data carries
        // the first two commands; the third has no entry at all.
        let result = ToolResult {
            success: true,
            output: "PASS git --version (exit 0, 0.1s)\n\n1 passed, 0 failed".to_string(),
            data: Some(json!({"checks": [
                entry("git --version", dir.path()),
                entry("git status", dir.path()),
            ]})),
        };
        let report = build_report(dir.path(), &commands, &json!({}), &result).expect("a report");
        assert_eq!(report.outcomes.len(), 2, "{:?}", report.outcomes);
        assert!(report.outcomes[0].passed);
        let cut = &report.outcomes[1];
        assert_eq!(cut.command, "git status");
        assert_eq!(cut.detail(), "run by Mnemo: exit 0; its output was cut off");
        assert_eq!(report.missing, vec!["git log".to_string()]);
        assert!(!report.all_passed(), "a missing command is never a pass");
        let notice = CheckCtx::notice_text(None, "subject", &report, "NEXT");
        assert!(
            notice.contains("NO RESULT git log (the tool returned no entry)"),
            "{notice}"
        );
        // All three entries present and counted: only then does it pass.
        let complete = RunReport {
            outcomes: report.outcomes.clone(),
            missing: Vec::new(),
        };
        assert!(complete.all_passed());
        let empty = RunReport {
            outcomes: Vec::new(),
            missing: Vec::new(),
        };
        assert!(!empty.all_passed(), "an empty list is never a pass");
    }

    /// A remote worker's checks run on its node, are recorded at the NODE's tip,
    /// and leave the hub shadow on that same tip — the checkout review, diff and
    /// merge read.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_check_run_records_at_node_tip() {
        let rig = remote_testkit::Rig::start().await;
        let (record, remote, tip) = rig.worker_on_node("abcd1234", true).await;
        // A command every host has, so the test measures the run and not the
        // node's toolchain.
        write(
            rig.root.path(),
            ".coding/checks.toml",
            "commands = [\"git --version\"]\n",
        );
        let team = rig.team().await;
        team.records.lock().await.push(record.clone());
        // The notice is posted after the checks are recorded AND the shadow is
        // synced onto the tip they measured; wait for it so the node outlives
        // the whole run.
        let mut signals = team.signals.subscribe();

        let started = team
            .run_checks_tool(Some("abcd1234"))
            .await
            .expect("start the remote check run");
        assert!(started.contains("on host node-a"), "{started}");
        let notice = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            loop {
                match signals.recv().await {
                    Ok(BoardSignal::Notice { key, text })
                        if key.starts_with("checks:abcd1234:") =>
                    {
                        return text;
                    }
                    Ok(_) => continue,
                    Err(error) => panic!("the board signal channel closed: {error}"),
                }
            }
        })
        .await
        .expect("the check run posts its notice");
        assert!(notice.contains("on host node-a"), "{notice}");
        assert!(!notice.contains("could not"), "{notice}");

        let checks = team
            .records
            .lock()
            .await
            .iter()
            .find(|record| record.id == "abcd1234")
            .map(|record| record.checks.clone())
            .expect("the worker record");
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!(checks[0].name, "git --version");
        assert_eq!(checks[0].status, "passed");
        assert_eq!(checks[0].tip, tip, "recorded at the node's tip");

        let shadow = record.worktree.clone();
        assert_eq!(
            phase3::tip(&shadow).await.expect("the shadow tip"),
            tip,
            "the hub shadow holds the tip the checks measured"
        );
        let node_checkout = std::path::PathBuf::from(&remote.path);
        assert_eq!(
            super::super::remote::git_value(&node_checkout, &["rev-parse", "HEAD"])
                .await
                .expect("the node tip"),
            tip,
            "the node checkout stayed at its tip"
        );
    }
}
