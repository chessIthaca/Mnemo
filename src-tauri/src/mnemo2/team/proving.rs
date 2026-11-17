// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The team's side of the two proofs: when they run, what they block, and what
//! their outcome adds to a worker's ledger.
//!
//! The engine ([`super::proof`]) only splits hunks, runs one command and
//! restores a checkout. This module decides that a finished bug-fix turn earns
//! a red check, keeps a worker whose tests are being proved out of every action
//! that would move its checkout, and turns each outcome into the check or the
//! finding the merge gate reads.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, Mutex};

use mnemo::app::plan_board::PlanBoard;
use mnemo::memory::plan_format::{PlanFile, PlanKind};

use crate::mnemo2::phase3;
use crate::mnemo2::phase3::Finding;
use crate::mnemo2::usage::ProjectUsage;

use super::findings_ledger;
use super::persistence::TeamPersistence;
use super::proof::{self, Mutant, MutationCfg, RedOutcome, RegressionCfg};
use super::pump::save_workers;
use super::record::{WorkerCheck, WorkerRecord, WorkerStatus};
use super::review::CheckRecorder;
use super::{now_key, BoardSignal, TeamRuntime};

/// The name of the check a red check records.
pub(super) const RED_CHECK: &str = "regression test fails without the fix";
/// The name of the check a mutation run with no missed mutant records.
pub(super) const MUTATION_CLEAN_CHECK: &str = "mutation: every changed line is caught by a test";
/// The finding a regression test that passes without the fix opens.
pub(super) const NOT_RED_ID: &str = "regression-not-red";
/// The finding a proof that cannot decide opens; the lead may wontfix it.
pub(super) const INCONCLUSIVE_ID: &str = "regression-proof-inconclusive";
/// How many missed mutants are reported one by one; the rest go in one
/// `mutant-more` finding.
pub(super) const MUTANT_CAP: usize = 20;

/// The worker ids whose proofs are running right now.
///
/// A worker being proved must not start a turn, take a message, or merge: each
/// of those moves its checkout under the proof, and the proof's restore would
/// then throw the move away.
#[derive(Clone, Debug, Default)]
pub(super) struct Proving {
    ids: Arc<StdMutex<HashSet<String>>>,
}

impl Proving {
    /// Whether `worker_id`'s proofs are running.
    pub(super) fn is_active(&self, worker_id: &str) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(worker_id)
    }

    /// Hold a worker's claim for the rest of the test. Tests only: production
    /// claims through [`ProofCtx::spawn`], which releases on drop.
    #[cfg(test)]
    pub(super) fn hold_for_test(&self, worker_id: &str) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(worker_id.to_string())
    }

    /// Claim `worker_id` for one proof run, or `None` when it is already
    /// claimed. The claim is released when the guard drops.
    fn claim(&self, worker_id: &str) -> Option<ProvingGuard> {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        if !ids.insert(worker_id.to_string()) {
            return None;
        }
        Some(ProvingGuard {
            proving: self.clone(),
            worker_id: worker_id.to_string(),
        })
    }

    fn release(&self, worker_id: &str) {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(worker_id);
    }
}

/// The claim one running proof holds; dropping it frees the worker.
struct ProvingGuard {
    proving: Proving,
    worker_id: String,
}

impl Drop for ProvingGuard {
    fn drop(&mut self) {
        self.proving.release(&self.worker_id);
    }
}

/// The refusal every entry point that would move a proved checkout returns.
pub(super) fn proving_error(worker_id: &str) -> String {
    format!("Mnemo2 is proving {worker_id}'s tests; wait for its notice")
}

/// The out-of-process work one red check needs.
#[derive(Clone, Debug)]
pub(crate) struct ProveRequest {
    /// The worker checkout to prove in; it must sit exactly at `tip`.
    pub(super) checkout: PathBuf,
    /// The merge-base of the lead branch and `tip`.
    pub(super) base: String,
    /// The worker commit under proof.
    pub(super) tip: String,
    /// The `[regression]` command and its proof-of-run text.
    pub(super) cfg: RegressionCfg,
    /// The plan's regression test name.
    pub(super) test: String,
}

/// The out-of-process work one mutation run needs.
#[derive(Clone, Debug)]
pub(crate) struct MutateRequest {
    /// The lead's project root (the `.coding` side-car lives there).
    pub(super) main_root: PathBuf,
    /// The worker checkout; the tool copies the tree beside it.
    pub(super) checkout: PathBuf,
    /// The diff base.
    pub(super) base: String,
    /// The worker commit the diff ends at.
    pub(super) tip: String,
    /// The `[mutation]` table.
    pub(super) cfg: MutationCfg,
    /// The worker id, which names the tool's output folder.
    pub(super) worker_id: String,
}

/// The two proof commands, behind a seam so a test can answer without a real
/// build.
#[async_trait]
pub(crate) trait ProofRunner: Send + Sync {
    /// Run the red check and report what it proved.
    async fn prove(&self, request: ProveRequest) -> Result<RedOutcome, String>;
    /// Run the mutation tool and report the mutants no test caught.
    async fn mutate(&self, request: MutateRequest) -> Result<Vec<Mutant>, String>;
}

/// The real runner: [`super::proof`]'s two entry points, unchanged.
pub(super) struct RealProofs;

#[async_trait]
impl ProofRunner for RealProofs {
    async fn prove(&self, request: ProveRequest) -> Result<RedOutcome, String> {
        proof::prove_regression(
            request.checkout,
            request.base,
            request.tip,
            request.cfg,
            request.test,
        )
        .await
    }

    async fn mutate(&self, request: MutateRequest) -> Result<Vec<Mutant>, String> {
        proof::run_mutation(
            request.main_root,
            request.checkout,
            request.base,
            request.tip,
            request.cfg,
            request.worker_id,
        )
        .await
    }
}

/// Which proof a worker is being asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProofJob {
    /// The regression red check.
    Regression,
    /// The mutation check over the change's diff.
    Mutation,
}

/// What the merge gate needs to know about a worker's proofs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct MergeProof {
    /// A proof is running for this worker right now.
    pub(super) running: bool,
    /// The worker's diff adds its plan's regression test, so the test must be
    /// proven red at the tip being merged.
    pub(super) regression_required: bool,
    /// The tip carries the passed red check.
    pub(super) regression_proven: bool,
    /// The lead resolved the proof finding with a written reason instead.
    pub(super) regression_waived: bool,
}

/// The data the proofs read from the runtime: the ledger handles, the plan
/// board, the live claim set, and the runner seam.
#[derive(Clone)]
pub(crate) struct ProofCtx {
    /// The lead's project root.
    pub(super) workspace_root: PathBuf,
    /// The worker ledger.
    pub(super) records: Arc<Mutex<Vec<WorkerRecord>>>,
    /// The usage accumulator [`save_workers`] writes.
    pub(super) usage: Arc<Mutex<ProjectUsage>>,
    /// The ledger save lock.
    pub(super) save_lock: Arc<Mutex<()>>,
    /// The ledger's persisted store.
    pub(super) persistence: TeamPersistence,
    /// The plan-board signal channel the lead notices ride.
    pub(super) signals: broadcast::Sender<BoardSignal>,
    /// The live claim set.
    pub(super) proving: Proving,
    /// The plan board, when the host attached one.
    pub(super) board: Option<Arc<PlanBoard>>,
    /// The proof commands.
    pub(super) runner: Arc<dyn ProofRunner>,
}

impl ProofCtx {
    /// One worker record.
    pub(super) async fn worker(&self, worker_id: &str) -> Result<WorkerRecord, String> {
        self.records
            .lock()
            .await
            .iter()
            .find(|worker| worker.id == worker_id)
            .cloned()
            .ok_or_else(|| format!("unknown worker '{worker_id}'"))
    }

    /// The plan a worker is dispatched under: its kind and its regression test.
    ///
    /// A worker with no plan, no board, or an unreadable plan has none.
    pub(super) async fn worker_plan(
        &self,
        worker: &WorkerRecord,
    ) -> Option<(PlanKind, Option<String>)> {
        let plan_id = worker.plan_id.clone()?;
        let markdown = self.board.as_ref()?.read_markdown(&plan_id).ok()?;
        let plan = PlanFile::parse(&markdown).ok()?;
        Some((plan.kind, plan.regression_test))
    }

    /// Start `job` for `worker_id` in the background, or refuse silently when a
    /// proof is already running for it. The claim is taken here, before the
    /// task is spawned, so two triggers can never both start.
    pub(super) fn spawn(&self, worker_id: &str, job: ProofJob) -> Result<(), String> {
        let Some(guard) = self.proving.claim(worker_id) else {
            return Err(proving_error(worker_id));
        };
        let ctx = self.clone();
        let id = worker_id.to_string();
        tokio::spawn(async move {
            let _guard = guard;
            let notice = match job {
                ProofJob::Regression => ctx.run_regression(&id).await,
                ProofJob::Mutation => ctx.run_mutation(&id).await,
            };
            ctx.notice(format!("proof:{id}:{}", now_key()), notice);
        });
        Ok(())
    }

    /// Send the lead one finished-proof notice.
    fn notice(&self, key: String, text: String) {
        let _ = self.signals.send(BoardSignal::Notice { key, text });
    }

    /// Record one passed check against `tip` on the same ledger path every
    /// other check uses.
    async fn record_check(
        &self,
        worker_id: &str,
        name: &str,
        detail: Option<String>,
        tip: &str,
    ) -> Result<(), String> {
        let check = WorkerCheck {
            name: name.to_string(),
            status: "passed".to_string(),
            detail,
            tip: tip.to_string(),
        };
        CheckRecorder::new(&self.records, &self.usage, &self.save_lock, &self.persistence)
            .record_at_tip(worker_id, check)
            .await?;
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
    }

    /// Open `findings` on the worker and log them to the findings ledger.
    ///
    /// An id the worker already carries is left alone: a proof that runs twice
    /// at one tip must not open the same finding twice.
    async fn open_findings(
        &self,
        worker_id: &str,
        findings: Vec<Finding>,
    ) -> Result<(), String> {
        if findings.is_empty() {
            return Ok(());
        }
        let mut opened = Vec::new();
        {
            let mut records = self.records.lock().await;
            let worker = records
                .iter_mut()
                .find(|worker| worker.id == worker_id)
                .ok_or_else(|| format!("unknown worker '{worker_id}'"))?;
            if matches!(worker.status, WorkerStatus::Merging | WorkerStatus::Merged) {
                return Ok(());
            }
            for finding in findings {
                if worker
                    .findings
                    .iter()
                    .any(|existing| existing.id == finding.id)
                {
                    continue;
                }
                opened.push(finding.clone());
                worker.findings.push(finding);
            }
        }
        if opened.is_empty() {
            return Ok(());
        }
        if let Ok(worker) = self.worker(worker_id).await {
            let _ = findings_ledger::append_new(&self.workspace_root, &worker, &opened);
        }
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
    }

    /// Set the worker's error and tell the lead: a restore that failed leaves
    /// the checkout in an unknown state, which no check may report as proof.
    async fn fail(&self, worker_id: &str, error: &str) {
        {
            let mut records = self.records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
                worker.error = Some(error.to_string());
            }
        }
        let _ =
            save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await;
    }

    /// The file a finding names when the proof cannot name one itself: the plan
    /// document when the worker has one, else the proof configuration.
    async fn fallback_file(&self, worker: &WorkerRecord) -> String {
        let has_plan = self.worker_plan(worker).await.is_some();
        match (&worker.plan_id, has_plan) {
            (Some(plan_id), true) => format!(".coding/plans/{plan_id}.md"),
            _ => ".coding/checks.toml".to_string(),
        }
    }

    /// Prove the plan's regression test red at the worker's current tip.
    async fn run_regression(&self, worker_id: &str) -> String {
        match self.prove_red(worker_id).await {
            Ok(notice) => notice,
            Err(error) => {
                self.fail(worker_id, &error).await;
                format!("regression proof for {worker_id} failed: {error}")
            }
        }
    }

    /// The red check itself: resolve the inputs, run the proof, record what it
    /// proved. `Err` means the checkout could not be restored.
    async fn prove_red(&self, worker_id: &str) -> Result<String, String> {
        let worker = self.worker(worker_id).await?;
        let test = match self.worker_plan(&worker).await {
            Some((PlanKind::BugFixing, Some(test))) => test,
            _ => {
                return Ok(format!(
                    "regression proof for {worker_id} skipped: the plan records no regression test"
                ));
            }
        };
        let Some(cfg) = proof::load_proof_config(&self.workspace_root).regression else {
            return Ok(format!(
                "regression proof for {worker_id} skipped: add a [regression] table to \
                 .coding/checks.toml"
            ));
        };
        let tip = phase3::tip(&worker.worktree).await?;
        let base = phase3::git_value(&self.workspace_root, &["merge-base", "HEAD", &tip]).await?;
        let request = ProveRequest {
            checkout: worker.worktree.clone(),
            base: base.clone(),
            tip: tip.clone(),
            cfg,
            test: test.clone(),
        };
        let outcome = self.runner.prove(request).await?;
        if !checkout_at(&worker.worktree, &tip).await {
            return Ok(format!(
                "regression proof for {worker_id} discarded: the tip moved while it ran"
            ));
        }
        let file = match diff_text(&self.workspace_root, &base, &tip)
            .await
            .ok()
            .and_then(|diff| added_line_file(&diff, &test))
        {
            Some(file) => file,
            None => self.fallback_file(&worker).await,
        };
        match outcome {
            RedOutcome::Red { output } => {
                let detail = format!("{test}\n{}", output_tail(&output));
                self.record_check(worker_id, RED_CHECK, Some(detail), &tip)
                    .await?;
                Ok(format!(
                    "regression proof for {worker_id}: red (fails without the fix)"
                ))
            }
            RedOutcome::NotRed { output } => {
                let finding = Finding {
                    id: NOT_RED_ID.to_string(),
                    severity: "high".to_string(),
                    title: "The regression test passes without the fix".to_string(),
                    detail: format!(
                        "{}\nthe test does not reproduce the defect",
                        output_tail(&output)
                    ),
                    file,
                    line: 0,
                    status: "open".to_string(),
                    fix_commit: None,
                    evidence: None,
                    justification: None,
                    category: None,
                    enforce: None,
                };
                self.open_findings(worker_id, vec![finding]).await?;
                Ok(format!(
                    "regression proof for {worker_id}: NOT red, finding {NOT_RED_ID} opened"
                ))
            }
            RedOutcome::Inconclusive { reason } => {
                let finding = Finding {
                    id: INCONCLUSIVE_ID.to_string(),
                    severity: "low".to_string(),
                    title: "The regression test could not be proven red".to_string(),
                    detail: reason.clone(),
                    file,
                    line: 0,
                    status: "open".to_string(),
                    fix_commit: None,
                    evidence: None,
                    justification: None,
                    category: None,
                    enforce: None,
                };
                self.open_findings(worker_id, vec![finding]).await?;
                Ok(format!(
                    "regression proof for {worker_id}: inconclusive: {reason}"
                ))
            }
        }
    }

    /// Run the mutation tool over the change's diff and report what no test
    /// caught.
    async fn run_mutation(&self, worker_id: &str) -> String {
        match self.mutate(worker_id).await {
            Ok(notice) => notice,
            Err(error) => {
                self.fail(worker_id, &error).await;
                format!("mutation check for {worker_id} failed: {error}")
            }
        }
    }

    /// The mutation check itself. `Err` means the run left the checkout dirty.
    async fn mutate(&self, worker_id: &str) -> Result<String, String> {
        let worker = self.worker(worker_id).await?;
        let Some(cfg) = proof::load_proof_config(&self.workspace_root).mutation else {
            return Ok(format!(
                "mutation check for {worker_id} skipped: configure [mutation] in \
                 .coding/checks.toml"
            ));
        };
        let tip = phase3::tip(&worker.worktree).await?;
        let base = phase3::git_value(&self.workspace_root, &["merge-base", "HEAD", &tip]).await?;
        let request = MutateRequest {
            main_root: self.workspace_root.clone(),
            checkout: worker.worktree.clone(),
            base,
            tip: tip.clone(),
            cfg,
            worker_id: worker_id.to_string(),
        };
        let mutants = self.runner.mutate(request).await?;
        if !checkout_at(&worker.worktree, &tip).await {
            return Ok(format!(
                "mutation check for {worker_id} discarded: the tip moved while it ran"
            ));
        }
        if mutants.is_empty() {
            self.record_check(worker_id, MUTATION_CLEAN_CHECK, None, &tip)
                .await?;
            return Ok(format!(
                "mutation check for {worker_id}: every changed line is caught by a test"
            ));
        }
        let fallback = self.fallback_file(&worker).await;
        let findings = mutant_findings(&mutants, &fallback);
        self.open_findings(worker_id, findings).await?;
        Ok(format!(
            "mutation check for {worker_id}: {} uncaught changes, findings opened",
            mutants.len()
        ))
    }

    /// Whether this worker's tip still needs its regression test proven red:
    /// the plan is a bug fix with a regression test, the worker's diff adds a
    /// line carrying that test's name, and the tip carries no proof result yet.
    pub(super) async fn regression_due(&self, worker: &WorkerRecord) -> bool {
        let Some((PlanKind::BugFixing, Some(test))) = self.worker_plan(worker).await else {
            return false;
        };
        let Ok(tip) = phase3::tip(&worker.worktree).await else {
            return false;
        };
        if self.tip_proven(worker, &tip) {
            return false;
        }
        self.diff_adds(&tip, &test).await
    }

    /// Whether the worker already carries a proof result for this tip.
    fn tip_proven(&self, worker: &WorkerRecord, tip: &str) -> bool {
        if worker.checks.iter().any(|check| {
            check.name == RED_CHECK && check.status == "passed" && check.tip == tip
        }) {
            return true;
        }
        // An open proof finding is the result for this tip until the lead
        // resolves it; a resolved one no longer blocks the gate, so a later
        // turn proves again.
        worker.findings.iter().any(|finding| {
            matches!(finding.id.as_str(), NOT_RED_ID | INCONCLUSIVE_ID)
                && finding.status == "open"
        })
    }

    /// Whether the worker's diff from its base adds a line carrying `test`.
    async fn diff_adds(&self, tip: &str, test: &str) -> bool {
        let Ok(base) = phase3::git_value(&self.workspace_root, &["merge-base", "HEAD", tip]).await
        else {
            return false;
        };
        match diff_text(&self.workspace_root, &base, tip).await {
            Ok(diff) => added_line_file(&diff, test).is_some(),
            Err(_) => false,
        }
    }

    /// What the merge gate needs: whether a proof is running, and whether this
    /// tip still owes a red check.
    pub(super) async fn merge_inputs(&self, worker: &WorkerRecord, tip: &str) -> MergeProof {
        let running = self.proving.is_active(&worker.id);
        let mut proof = MergeProof {
            running,
            ..MergeProof::default()
        };
        if running {
            return proof;
        }
        let Some((PlanKind::BugFixing, Some(test))) = self.worker_plan(worker).await else {
            return proof;
        };
        proof.regression_required = self.diff_adds(tip, &test).await;
        proof.regression_proven = worker.checks.iter().any(|check| {
            check.name == RED_CHECK && check.status == "passed" && check.tip == tip
        });
        proof.regression_waived = worker.findings.iter().any(|finding| {
            matches!(finding.id.as_str(), NOT_RED_ID | INCONCLUSIVE_ID)
                && finding.status != "open"
        });
        proof
    }
}

/// The proofs a finished turn earns.
///
/// The red check runs for every bug fix whose diff adds the plan's regression
/// test; the mutation check runs for a high-risk worker when the project
/// configured it. Both run in the background: the pump must keep folding the
/// worker's events.
pub(super) async fn spawn_turn_proofs(ctx: &ProofCtx, worker_id: String) {
    let Ok(worker) = ctx.worker(&worker_id).await else {
        return;
    };
    if worker.kind != "implementation" {
        return;
    }
    let red_due = ctx.regression_due(&worker).await;
    let mutation_due = worker.review_risk == "high"
        && proof::load_proof_config(&ctx.workspace_root).mutation.is_some();
    if !red_due && !mutation_due {
        return;
    }
    if red_due {
        let _ = ctx.spawn(&worker_id, ProofJob::Regression);
    }
    if mutation_due {
        let _ = ctx.spawn(&worker_id, ProofJob::Mutation);
    }
}

/// The findings a mutation run opens: one per missed mutant, capped, with one
/// extra finding listing the rest.
fn mutant_findings(mutants: &[Mutant], fallback: &str) -> Vec<Finding> {
    let mut findings: Vec<Finding> = mutants
        .iter()
        .take(MUTANT_CAP)
        .enumerate()
        .map(|(index, mutant)| Finding {
            id: format!("mutant-{}", index + 1),
            severity: "low".to_string(),
            title: format!("No test catches: {}", mutant.text),
            detail: "Mnemo2 changed this code and every test still passed. Add a test that \
                     fails for this change, or resolve wontfix with why it does not matter."
                .to_string(),
            file: if mutant.file.is_empty() {
                fallback.to_string()
            } else {
                mutant.file.clone()
            },
            line: mutant.line,
            status: "open".to_string(),
            fix_commit: None,
            evidence: None,
            justification: None,
            category: None,
            enforce: None,
        })
        .collect();
    if mutants.len() > MUTANT_CAP {
        let rest: Vec<String> = mutants
            .iter()
            .skip(MUTANT_CAP)
            .map(|mutant| format!("{}:{}: {}", mutant.file, mutant.line, mutant.text))
            .collect();
        findings.push(Finding {
            id: "mutant-more".to_string(),
            severity: "low".to_string(),
            title: format!(
                "{} more uncaught changes",
                mutants.len().saturating_sub(MUTANT_CAP)
            ),
            detail: rest.join("\n"),
            file: fallback.to_string(),
            line: 0,
            status: "open".to_string(),
            fix_commit: None,
            evidence: None,
            justification: None,
            category: None,
            enforce: None,
        });
    }
    findings
}

/// The `git diff --no-color --no-ext-diff -U0 <base> <tip>` text of a change.
async fn diff_text(root: &Path, base: &str, tip: &str) -> Result<String, String> {
    let args = ["diff", "--no-color", "--no-ext-diff", "-U0", base, tip];
    let output = phase3::git(root, &args).await?;
    if !output.status.success() {
        return Err(phase3::git_error(&args, &output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The file whose added lines carry `needle`: the first `+++ b/<path>` section
/// with an added line that contains it.
fn added_line_file(diff: &str, needle: &str) -> Option<String> {
    let mut file = String::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            file = path.to_string();
            continue;
        }
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            if !file.is_empty() && added.contains(needle) {
                return Some(file);
            }
        }
    }
    None
}

/// Whether the checkout sits exactly at `tip`.
async fn checkout_at(checkout: &Path, tip: &str) -> bool {
    phase3::git_value(checkout, &["rev-parse", "HEAD"])
        .await
        .map(|head| head == tip)
        .unwrap_or(false)
}

/// The last lines of a proof command's output: enough to read the failure, not
/// the whole test log.
fn output_tail(output: &str) -> String {
    const KEPT: usize = 40;
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(KEPT);
    lines[start..].join("\n")
}

impl TeamRuntime {
    /// Run the red check now, on the lead's request: the worker's plan may have
    /// gained its `regression_test` after the worker's last turn, so no turn
    /// end ever triggered it.
    pub(super) async fn prove_regression_tool(&self, worker_id: &str) -> Result<String, String> {
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merging | WorkerStatus::Merged) {
            return Err(format!(
                "worker '{worker_id}' is {:?}; its tip can no longer be proven",
                worker.status
            ));
        }
        if self.worker_is_busy(worker_id).await {
            return Err(format!(
                "worker '{worker_id}' is busy; the regression proof runs in its checkout while it is idle"
            ));
        }
        let ctx = self.proof_ctx();
        if !matches!(
            ctx.worker_plan(&worker).await,
            Some((PlanKind::BugFixing, Some(_)))
        ) {
            return Err(format!(
                "worker '{worker_id}' has no bug_fixing plan with a regression test; record one with plan_update, then prove it"
            ));
        }
        ctx.spawn(worker_id, ProofJob::Regression)?;
        Ok(format!(
            "regression proof started for {worker_id}; it finishes with a notice and records a check or finding"
        ))
    }

    /// Run the mutation check now, on the lead's request.
    pub(super) async fn mutation_check_tool(&self, worker_id: &str) -> Result<String, String> {
        if proof::load_proof_config(&self.workspace_root)
            .mutation
            .is_none()
        {
            return Err("configure [mutation] in .coding/checks.toml".to_string());
        }
        let worker = self.worker(worker_id).await?;
        if matches!(worker.status, WorkerStatus::Merging | WorkerStatus::Merged) {
            return Err(format!(
                "worker '{worker_id}' is {:?}; its tip can no longer be mutated",
                worker.status
            ));
        }
        if self.worker_is_busy(worker_id).await {
            return Err(format!(
                "worker '{worker_id}' is busy; the mutation check copies its checkout while it is idle"
            ));
        }
        self.proof_ctx().spawn(worker_id, ProofJob::Mutation)?;
        Ok(format!(
            "mutation check started for {worker_id}; it finishes with a notice and opens a finding per uncaught change"
        ))
    }
}
