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
//!
//! Two race findings shaped the claim: [`ProofCtx::claim_idle`] holds the merge
//! lock while it decides, so a worker whose turn just started can never be
//! proved underneath itself (claimRace), and one claim runs every job the turn
//! owes, in sequence, so the second job is never dropped (secondJobDropped).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use tokio::sync::{broadcast, Mutex};

use mnemo::app::plan_board::PlanBoard;
use mnemo::machine_slots::HeavySlots;
use mnemo::memory::plan_format::{PlanFile, PlanKind};

use crate::mnemo2::phase3;
use crate::mnemo2::phase3::Finding;
use crate::mnemo2::usage::ProjectUsage;

use super::findings_ledger;
use super::persistence::TeamPersistence;
use super::proof::{self, Mutant, MutationCfg, RedOutcome, RegressionCfg};
use super::pump::save_workers;
use super::record::{RemoteCheckout, WorkerCheck, WorkerRecord, WorkerStatus};
use super::remote::RemoteWorkers;
use super::review::CheckRecorder;
use super::{now_key, BoardSignal, LiveWorker, TeamRuntime};

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
/// The folder a running proof's claim marker lives in (finding
/// restartWhileProving): `.coding/mnemo2/` is per-instance state, gitignored,
/// so a hard kill mid-proof leaves the marker for the next open to repair from.
const MARKER_DIR: &str = ".coding/mnemo2/proving";

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
    /// claims through [`ProofCtx::claim_idle`], which releases on drop.
    #[cfg(test)]
    pub(super) fn hold_for_test(&self, worker_id: &str) -> bool {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(worker_id.to_string())
    }

    /// Claim `worker_id`, or `None` when it is already claimed. The claim is
    /// released when the guard drops.
    fn claim(&self, worker_id: &str) -> Option<ProvingGuard> {
        let mut ids = self.ids.lock().unwrap_or_else(|e| e.into_inner());
        if !ids.insert(worker_id.to_string()) {
            return None;
        }
        Some(ProvingGuard {
            proving: self.clone(),
            worker_id: worker_id.to_string(),
            marker: None,
        })
    }

    fn release(&self, worker_id: &str) {
        self.ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(worker_id);
    }
}

/// The claim one running proof holds; dropping it frees the worker and removes
/// the claim marker.
struct ProvingGuard {
    proving: Proving,
    worker_id: String,
    /// The claim marker this proof wrote; removed here, whatever ended the
    /// proof (finding restartWhileProving).
    marker: Option<PathBuf>,
}

impl Drop for ProvingGuard {
    fn drop(&mut self) {
        if let Some(marker) = &self.marker {
            let _ = std::fs::remove_file(marker);
        }
        self.proving.release(&self.worker_id);
    }
}

/// The refusal every entry point that would move a proved checkout returns.
pub(super) fn proving_error(worker_id: &str) -> String {
    format!("Mnemo2 is proving {worker_id}'s tests; wait for its notice")
}

/// The claim marker of one running proof: `<workspace_root>/<MARKER_DIR>/
/// <worker_id>.json`.
fn marker_path(workspace_root: &Path, worker_id: &str) -> PathBuf {
    workspace_root
        .join(MARKER_DIR)
        .join(format!("{worker_id}.json"))
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

/// The real runner: [`super::proof`]'s two entry points, carrying the runtime's
/// RESOLVED config dir so a proof's machine slot lives in the same pool as the
/// instance's own heavy commands (finding proofConfigDir). Re-resolving
/// `~/.mnemo` here built a SECOND pool in production and wrote to the real home
/// directory in tests, which must only ever touch a temp dir.
pub(super) struct RealProofs {
    config_dir: PathBuf,
}

impl RealProofs {
    /// The runner over the runtime's config dir (the dir `Setup` opened:
    /// `$MNEMO_HOME` or `~/.mnemo` in production, a temp dir in tests).
    pub(super) fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    /// The machine's heavy slots as THIS instance sees them: the slot files
    /// under `<config_dir>/run/heavy` and the saved `[general.machine]` limit.
    /// `None` when the config cannot be read — the proof then runs unslotted,
    /// because the limit is an optimisation, never a failure mode.
    fn heavy_slots(&self) -> Option<HeavySlots> {
        let config = mnemo::config::Config::load(&self.config_dir).ok()?;
        Some(HeavySlots::new(
            mnemo::machine_slots::default_dir(&self.config_dir),
            config.general.general.machine.heavy_commands,
        ))
    }
}

#[async_trait]
impl ProofRunner for RealProofs {
    async fn prove(&self, request: ProveRequest) -> Result<RedOutcome, String> {
        let slots = self.heavy_slots();
        proof::prove_regression(
            request.checkout,
            request.base,
            request.tip,
            request.cfg,
            request.test,
            slots.as_ref(),
        )
        .await
    }

    async fn mutate(&self, request: MutateRequest) -> Result<Vec<Mutant>, String> {
        let slots = self.heavy_slots();
        proof::run_mutation(
            request.main_root,
            request.checkout,
            request.base,
            request.tip,
            request.cfg,
            request.worker_id,
            slots.as_ref(),
        )
        .await
    }
}

/// The remote runner: the two proofs executed on a worker node, in the
/// worker's own checkout there (plan 9a2f4b67, decision 4).
///
/// The hub keeps an ordinary shadow checkout of the same branch, so review,
/// diff and merge read it locally, but the proofs run where the warm build
/// cache and the right operating system are.
pub(super) struct RemoteProofRunner {
    /// The remote-worker seam: the node calls.
    pub(super) remote: Arc<RemoteWorkers>,
    /// The worker's checkout on the node.
    pub(super) checkout: RemoteCheckout,
    /// The project key the node names the project by.
    pub(super) project: String,
}

#[async_trait]
impl ProofRunner for RemoteProofRunner {
    async fn prove(&self, request: ProveRequest) -> Result<RedOutcome, String> {
        let params = serde_json::json!({
            "checkout": self.checkout.checkout,
            "base": request.base,
            "tip": request.tip,
            "cfg": request.cfg,
            "test": request.test,
        });
        let answer = self
            .remote
            .call_node(
                &self.checkout,
                proof::PROOF_PROVE,
                params,
                proof::PROOF_TIMEOUT,
            )
            .await?;
        serde_json::from_value(answer).map_err(|error| {
            format!(
                "the node's {} answer did not parse: {error}",
                proof::PROOF_PROVE
            )
        })
    }

    async fn mutate(&self, request: MutateRequest) -> Result<Vec<Mutant>, String> {
        let params = serde_json::json!({
            "checkout": self.checkout.checkout,
            "project": self.project,
            "base": request.base,
            "tip": request.tip,
            "cfg": request.cfg,
            "worker_id": request.worker_id,
        });
        let answer = self
            .remote
            .call_node(
                &self.checkout,
                proof::PROOF_MUTATE,
                params,
                proof::MUTATE_TIMEOUT,
            )
            .await?;
        serde_json::from_value(answer).map_err(|error| {
            format!(
                "the node's {} answer did not parse: {error}",
                proof::PROOF_MUTATE
            )
        })
    }
}

/// The refusal a remote worker's uncommitted node changes earn: a proof or a
/// merge would own a checkout the worker still edits.
pub(super) fn dirty_remote_error(worker_id: &str, host: &str, dirty: &[String]) -> String {
    let first: Vec<&str> = dirty.iter().take(5).map(String::as_str).collect();
    format!(
        "worker '{worker_id}' has uncommitted changes on host '{host}': {}; ask it to commit them",
        first.join(", ")
    )
}

/// Push `sha` to the node's repo, so a proof there can diff against the base.
async fn push_base(
    remote: &RemoteWorkers,
    checkout: &RemoteCheckout,
    sha: &str,
) -> Result<(), String> {
    let Some(server) = remote.server() else {
        return Err(format!(
            "worker host '{}' is not connected",
            checkout.node_name
        ));
    };
    remote.push_commit(&server.node(&checkout.node_id), sha).await
}

/// Which proof a worker is being asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ProofJob {
    /// The regression red check.
    Regression,
    /// The mutation check over the change's diff.
    Mutation,
    /// The same check, asked for by the lead: the automatic skip rules and the
    /// missing-tool deduplication do not apply, because the lead asked.
    MutationRequested,
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
    /// The lead resolved the proof finding as wontfix, with a written reason.
    /// Only that waives the gate: a finding closed `fixed` must be backed by
    /// the red check at the tip being merged (finding proofPerTip).
    pub(super) regression_waived: bool,
}

/// What one session remembers between automatic mutation runs: the tip each
/// worker's last attempt reached the runner at, and the `[mutation]` commands
/// whose missing tool it already reported (decision 2026-11-19).
///
/// It lives on the runtime, not on [`ProofCtx`], which is rebuilt per call, and
/// in memory only: a restart repeats one run or one notice, which is fine.
#[derive(Debug, Default)]
pub(super) struct MutationMemory {
    /// Worker id to the tip its last attempt reached the runner at.
    last_tip: HashMap<String, String>,
    /// The `[mutation]` commands whose missing tool was already reported.
    missing_noticed: HashSet<String>,
}

impl MutationMemory {
    /// The tip `worker_id`'s last mutation attempt reached the runner at.
    fn last_tip(&self, worker_id: &str) -> Option<&str> {
        self.last_tip.get(worker_id).map(String::as_str)
    }

    /// Record that `worker_id`'s attempt reached the runner at `tip`.
    fn record_tip(&mut self, worker_id: &str, tip: &str) {
        self.last_tip
            .insert(worker_id.to_string(), tip.to_string());
    }

    /// Remember that `command`'s tool was reported missing; `true` when this is
    /// the first report in this session.
    fn notice_missing(&mut self, command: &str) -> bool {
        self.missing_noticed.insert(command.to_string())
    }

    /// Forget `command`'s missing report: a run that worked proves the tool is
    /// installed, so a later failure is a new situation.
    fn clear_missing(&mut self, command: &str) {
        self.missing_noticed.remove(command);
    }
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
    /// Mnemo's own check runs: a proof must not edit a checkout they measure.
    pub(super) check_runs: super::check_run::CheckRuns,
    /// The plan board, when the host attached one.
    pub(super) board: Option<Arc<PlanBoard>>,
    /// The proof commands.
    pub(super) runner: Arc<dyn ProofRunner>,
    /// What this session already mutated and already reported: the automatic
    /// skip rules and the missing-tool deduplication read it (decision
    /// 2026-11-19). It lives on the runtime, because [`ProofCtx`] is rebuilt per
    /// call.
    pub(super) mutation_memory: Arc<StdMutex<MutationMemory>>,
    /// The merge lock: the same lock every entry point that starts a turn or
    /// moves a checkout takes (finding claimRace).
    pub(super) merge_lock: Arc<Mutex<()>>,
    /// The live sessions, for the busy test [`ProofCtx::worker_busy`].
    pub(super) live: Arc<Mutex<HashMap<String, LiveWorker>>>,
    /// The remote-worker seam: the shadow sync and the node calls a remote
    /// worker's proofs need. A local worker never touches it.
    pub(super) remote: Arc<RemoteWorkers>,
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

    /// The same busy test [`TeamRuntime::worker_is_busy`] uses: the live
    /// session's own flag, which the pump holds while a turn runs.
    async fn worker_busy(&self, worker_id: &str) -> bool {
        self.live
            .lock()
            .await
            .get(worker_id)
            .is_some_and(|live| live.busy.load(Ordering::SeqCst))
    }

    /// Claim `worker_id` for one proof run, or refuse: the claim holds the
    /// merge lock while it decides, so a turn that just started can never be
    /// proved underneath itself — every entry point that starts a turn takes
    /// the same lock before it moves the checkout, so it either sees this
    /// claim and refused, or this claim sees its turn and refuses (finding
    /// claimRace).
    async fn claim_idle(&self, worker_id: &str) -> Result<ProvingGuard, String> {
        let _lock = self.merge_lock.lock().await;
        if self.proving.is_active(worker_id) {
            return Err(proving_error(worker_id));
        }
        if self.check_runs.is_active(worker_id) {
            return Err(super::check_run::check_run_error(worker_id));
        }
        let (worktree, claimable) = {
            let records = self.records.lock().await;
            let Some(worker) = records.iter().find(|worker| worker.id == worker_id) else {
                return Err(format!("unknown worker '{worker_id}'"));
            };
            (worker.worktree.clone(), claimable_status(&worker.status))
        };
        if !claimable {
            return Err(format!(
                "worker '{worker_id}' is busy; the proof runs in its checkout while it is idle"
            ));
        }
        if self.worker_busy(worker_id).await {
            return Err(format!(
                "worker '{worker_id}' is busy; the proof runs in its checkout while it is idle"
            ));
        }
        let Some(mut guard) = self.proving.claim(worker_id) else {
            return Err(proving_error(worker_id));
        };
        // The marker names the tip to repair this checkout back to if a hard
        // kill interrupts the proof (finding restartWhileProving). A claim
        // that cannot write it is refused: without the marker the repair hook
        // below has nothing to repair from.
        let tip = match phase3::git_value(&worktree, &["rev-parse", "HEAD"]).await {
            Ok(tip) => tip,
            Err(error) => {
                return Err(format!("worker '{worker_id}': {error}"));
            }
        };
        let marker = marker_path(&self.workspace_root, worker_id);
        let payload = serde_json::json!({
            "tip": tip,
            "checkout": worktree.to_string_lossy(),
        });
        let written = std::fs::create_dir_all(marker.parent().unwrap_or(marker.as_path()))
            .and_then(|()| std::fs::write(&marker, payload.to_string()));
        if let Err(error) = written {
            return Err(format!(
                "worker '{worker_id}' could not write its proof marker: {error}"
            ));
        }
        guard.marker = Some(marker);
        Ok(guard)
    }

    /// Start the owed `jobs` for `worker_id` in the background, under one
    /// claim, in order: the red check first, then the mutation check (finding
    /// secondJobDropped — two claims ran the first job and silently dropped
    /// the second). Each job ends with its own notice.
    pub(super) async fn spawn(&self, worker_id: &str, jobs: &[ProofJob]) -> Result<(), String> {
        if jobs.is_empty() {
            return Ok(());
        }
        let guard = self.claim_idle(worker_id).await?;
        let ctx = self.clone();
        let id = worker_id.to_string();
        let jobs = jobs.to_vec();
        // Tracked in the team's task register: the jobs run Git and check
        // commands in the worker's checkout, so a shutdown waits for them.
        let tasks = self.persistence.tasks.clone();
        tasks.register(tokio::spawn(async move {
            // One claim runs every owed job in sequence: the claim is held
            // for both, and the busy guard keeps the checkout still.
            let _guard = guard;
            for job in jobs {
                // `None` is a result with nothing to report — a missing tool
                // this session already named — so no empty notice is sent.
                let notice = match job {
                    ProofJob::Regression => Some(ctx.run_regression(&id).await),
                    ProofJob::Mutation => ctx.run_mutation(&id, false).await,
                    ProofJob::MutationRequested => ctx.run_mutation(&id, true).await,
                };
                if let Some(text) = notice.filter(|text| !text.is_empty()) {
                    ctx.notice(format!("proof:{id}:{}", now_key()), text);
                }
            }
        }));
        Ok(())
    }

    /// Send the lead one finished-proof notice. A job with nothing to report
    /// returns no notice at all, so empty text is never sent.
    fn notice(&self, key: String, text: String) {
        if text.trim().is_empty() {
            return;
        }
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
    /// A proof finding whose id already exists is updated in place when it is
    /// open at an older tip (finding proofPerTip): its detail's tip line
    /// names the tip its proof ran at, so a later tip reports again instead of
    /// being silently skipped. A closed finding is left alone — its
    /// resolution is the lead's, not the proof's.
    async fn open_findings(
        &self,
        worker_id: &str,
        findings: Vec<Finding>,
    ) -> Result<(), String> {
        if findings.is_empty() {
            return Ok(());
        }
        let mut changed = Vec::new();
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
                if let Some(existing) = worker
                    .findings
                    .iter_mut()
                    .find(|existing| existing.id == finding.id)
                {
                    if existing.status == "open" && existing.detail != finding.detail {
                        existing.detail = finding.detail;
                        existing.title = finding.title;
                        existing.severity = finding.severity;
                        existing.file = finding.file;
                        existing.line = finding.line;
                        changed.push(existing.clone());
                    } else if existing.status != "open" && reopen_closed(existing, &finding) {
                        // A proof at a new tip re-opens what an earlier tip
                        // closed (finding staleWaiver): the closure described
                        // that tip; the new detail names this one.
                        existing.status = "open".to_string();
                        existing.detail = finding.detail;
                        existing.title = finding.title;
                        existing.severity = finding.severity;
                        existing.file = finding.file;
                        existing.line = finding.line;
                        existing.fix_commit = None;
                        existing.evidence = None;
                        existing.justification = None;
                        changed.push(existing.clone());
                    }
                    continue;
                }
                changed.push(finding.clone());
                worker.findings.push(finding);
            }
        }
        if changed.is_empty() {
            return Ok(());
        }
        if let Ok(worker) = self.worker(worker_id).await {
            let _ = findings_ledger::append_new(&self.workspace_root, &worker, &changed);
        }
        save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await
    }

    /// Close every open proof finding as fixed: a red check at `tip` proves
    /// what the open finding said was unproven (finding proofPerTip). The
    /// evidence names the tip, like the finding's own tip line.
    async fn close_proof_findings(&self, worker_id: &str, tip: &str) -> Result<(), String> {
        let evidence = format!("proven red at tip {tip}");
        let mut closed = false;
        {
            let mut records = self.records.lock().await;
            let Some(worker) = records
                .iter_mut()
                .find(|worker| worker.id == worker_id)
            else {
                return Ok(());
            };
            if matches!(worker.status, WorkerStatus::Merging | WorkerStatus::Merged) {
                return Ok(());
            }
            for finding in worker.findings.iter_mut() {
                if matches!(finding.id.as_str(), NOT_RED_ID | INCONCLUSIVE_ID)
                    && finding.status == "open"
                {
                    finding.status = "fixed".to_string();
                    finding.fix_commit = Some(tip.to_string());
                    finding.evidence = Some(evidence.clone());
                    closed = true;
                }
            }
        }
        if closed {
            save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await?;
        }
        Ok(())
    }

    /// Close the mutant findings this run did not report again (findings
    /// mutantIds, mutantMoreStale): a mutant that is caught now — or that
    /// left the diff — is fixed, with the tip that stopped reporting it. The
    /// overflow marker `mutant-more` is swept the same way: a run that no
    /// longer overflows reports it no longer, so it closes; a run that still
    /// overflows lists it in `reported` and it is updated instead. A finding
    /// the lead resolved as wontfix is not touched: only an open one closes.
    async fn close_absent_mutants(
        &self,
        worker_id: &str,
        reported: &[String],
        tip: &str,
    ) -> Result<(), String> {
        let evidence = format!("not reported at tip {tip}");
        let mut closed = false;
        {
            let mut records = self.records.lock().await;
            let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) else {
                return Ok(());
            };
            if matches!(worker.status, WorkerStatus::Merging | WorkerStatus::Merged) {
                return Ok(());
            }
            for finding in worker.findings.iter_mut() {
                if finding.status == "open"
                    && finding.id.starts_with("mutant-")
                    && !reported.iter().any(|id| id == &finding.id)
                {
                    finding.status = "fixed".to_string();
                    finding.fix_commit = Some(tip.to_string());
                    finding.evidence = Some(evidence.clone());
                    closed = true;
                }
            }
        }
        if closed {
            save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await?;
        }
        Ok(())
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
        let _ = save_workers(&self.records, &self.usage, &self.save_lock, &self.persistence).await;
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

    /// The claim's second look (finding engineTurnUnderClaim): the engine can
    /// start a steer follow-up on its own while the claim is held, so each
    /// job re-checks — under the merge lock, immediately before its long run
    /// — that the worker is still idle and claimable. The engine's own
    /// clean-tree precondition remains the second line of defence.
    async fn still_claimable(&self, worker_id: &str) -> bool {
        let _lock = self.merge_lock.lock().await;
        {
            let records = self.records.lock().await;
            let Some(worker) = records.iter().find(|worker| worker.id == worker_id) else {
                return false;
            };
            if !claimable_status(&worker.status) {
                return false;
            }
        }
        !self.worker_busy(worker_id).await
    }

    /// Prove the plan's regression test red at the worker's current tip.
    pub(super) async fn run_regression(&self, worker_id: &str) -> String {
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
        // A remote worker's commits live on its node: bring the hub shadow onto
        // the node tip first, refuse a node checkout the worker still edits, and
        // give the node the base commit its diff needs. A local worker keeps the
        // warm checkout it built in.
        let remote_sync = match worker.remote.as_ref() {
            Some(_) => {
                let sync = self.remote.sync_shadow(&worker, true).await?;
                if !sync.dirty.is_empty() {
                    return Err(dirty_remote_error(
                        worker_id,
                        worker.host_label(),
                        &sync.dirty,
                    ));
                }
                Some(sync)
            }
            None => None,
        };
        let tip = match &remote_sync {
            Some(sync) => sync.tip.clone(),
            None => phase3::tip(&worker.worktree).await?,
        };
        let base = phase3::git_value(&self.workspace_root, &["merge-base", "HEAD", &tip]).await?;
        if let Some(remote) = worker.remote.as_ref() {
            push_base(&self.remote, remote, &base).await?;
        }
        let request = ProveRequest {
            checkout: worker.worktree.clone(),
            base: base.clone(),
            tip: tip.clone(),
            cfg,
            test: test.clone(),
        };
        let outcome = if !self.still_claimable(worker_id).await {
            // The claim never saw this turn: the job ends here, records
            // nothing, and that turn's own end triggers the proofs again
            // (finding engineTurnUnderClaim).
            return Ok(format!(
                "regression proof for {worker_id} skipped: the worker started a turn"
            ));
        } else if let Some(remote) = worker.remote.as_ref() {
            let runner = RemoteProofRunner {
                remote: Arc::clone(&self.remote),
                checkout: remote.clone(),
                project: remote.project.clone(),
            };
            runner.prove(request).await?
        } else {
            self.runner.prove(request).await?
        };
        // The tip the proof ran at is the NODE's for a remote worker, so the
        // freshness check must ask the node, not the shadow the hub happens to
        // hold.
        let on_tip = match worker.remote.as_ref() {
            Some(remote) => self
                .remote
                .node_tip(remote)
                .await
                .map(|answer| answer.tip == tip)
                .unwrap_or(false),
            None => checkout_at(&worker.worktree, &tip).await,
        };
        if !on_tip {
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
                // A red check proves what an open proof finding said was
                // unproven: close it as fixed, at this tip.
                self.close_proof_findings(worker_id, &tip).await?;
                Ok(format!(
                    "regression proof for {worker_id}: red (fails without the fix)"
                ))
            }
            RedOutcome::NotRed { output } => {
                // The detail's first line is the tip (finding proofPerTip):
                // the finding describes this tip's proof, and a later tip
                // re-reports instead of being skipped.
                let finding = Finding {
                    id: NOT_RED_ID.to_string(),
                    severity: "high".to_string(),
                    title: "The regression test passes without the fix".to_string(),
                    detail: format!(
                        "tip {tip}\n{}\nthe test does not reproduce the defect",
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
                    detail: format!("tip {tip}\n{reason}"),
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
    ///
    /// `None` means the run has nothing to tell the lead: a missing tool this
    /// session already named, and the lead did not ask for this run.
    async fn run_mutation(&self, worker_id: &str, requested: bool) -> Option<String> {
        match self.mutate(worker_id, requested).await {
            Ok(notice) => notice,
            Err(error) => {
                // Only a checkout the run left off its tip or dirty is a worker
                // error; a run that could not run is a notice (decision
                // 2026-11-19).
                self.fail(worker_id, &error).await;
                Some(format!("mutation check for {worker_id} failed: {error}"))
            }
        }
    }

    /// The mutation check itself. `Ok(None)` means there is nothing to report;
    /// `Err` means the run left the checkout off its tip or dirty.
    async fn mutate(&self, worker_id: &str, requested: bool) -> Result<Option<String>, String> {
        let worker = self.worker(worker_id).await?;
        let Some(cfg) = proof::load_proof_config(&self.workspace_root).mutation else {
            return Ok(Some(format!(
                "mutation check for {worker_id} skipped: configure [mutation] in \
                 .coding/checks.toml"
            )));
        };
        // The configured command keys the missing-tool deduplication: the
        // substituted command carries this worker's own `{out}` path, so it
        // never repeats.
        let command = cfg.command.clone();
        // A remote worker's commits live on its node: bring the hub shadow onto
        // the node tip first, refuse a node checkout the worker still edits, and
        // give the node the base commit its diff needs.
        let remote_sync = match worker.remote.as_ref() {
            Some(_) => {
                let sync = self.remote.sync_shadow(&worker, true).await?;
                if !sync.dirty.is_empty() {
                    return Err(dirty_remote_error(
                        worker_id,
                        worker.host_label(),
                        &sync.dirty,
                    ));
                }
                Some(sync)
            }
            None => None,
        };
        let tip = match &remote_sync {
            Some(sync) => sync.tip.clone(),
            None => phase3::tip(&worker.worktree).await?,
        };
        let base = phase3::git_value(&self.workspace_root, &["merge-base", "HEAD", &tip]).await?;
        if let Some(remote) = worker.remote.as_ref() {
            push_base(&self.remote, remote, &base).await?;
        }
        let request = MutateRequest {
            main_root: self.workspace_root.clone(),
            checkout: worker.worktree.clone(),
            base,
            tip: tip.clone(),
            cfg,
            worker_id: worker_id.to_string(),
        };
        if !self.still_claimable(worker_id).await {
            // The run never reached the runner, so it owes no memory either.
            return Ok(Some(format!(
                "mutation check for {worker_id} skipped: the worker started a turn"
            )));
        }
        // Recorded before the run: every attempt that reached the runner counts
        // for this worker at this tip, whatever its outcome, so a turn end at
        // the same tip starts no second run (decision 2026-11-19).
        self.remember_mutation_tip(worker_id, &tip);
        let mutants = match match worker.remote.as_ref() {
            Some(remote) => {
                RemoteProofRunner {
                    remote: Arc::clone(&self.remote),
                    checkout: remote.clone(),
                    project: remote.project.clone(),
                }
                .mutate(request)
                .await
            }
            None => self.runner.mutate(request).await,
        } {
            Ok(mutants) => {
                // A run that worked proves the tool is installed: a later
                // failure is a new situation, not a repeat.
                self.forget_missing_tool(&command);
                mutants
            }
            Err(error) => {
                return self
                    .mutation_failure(&worker, &tip, &command, &error, requested)
                    .await;
            }
        };
        let on_tip = match worker.remote.as_ref() {
            Some(remote) => self
                .remote
                .node_tip(remote)
                .await
                .map(|answer| answer.tip == tip)
                .unwrap_or(false),
            None => checkout_at(&worker.worktree, &tip).await,
        };
        if !on_tip {
            return Ok(Some(format!(
                "mutation check for {worker_id} discarded: the tip moved while it ran"
            )));
        }
        if mutants.is_empty() {
            // Nothing is uncaught now: every open mutant finding — the
            // overflow marker included — is fixed at this tip (findings
            // mutantCloseOnEmpty and mutantMoreStale).
            self.close_absent_mutants(worker_id, &[], &tip).await?;
            self.record_check(worker_id, MUTATION_CLEAN_CHECK, None, &tip)
                .await?;
            return Ok(Some(format!(
                "mutation check for {worker_id}: every changed line is caught by a test"
            )));
        }
        let fallback = self.fallback_file(&worker).await;
        let findings = mutant_findings(&mutants, &tip, &fallback);
        // The reported set is every mutant of the run, not the capped
        // findings list: a mutant past the cap is still uncaught, and
        // closing it would claim a fix that did not happen (finding
        // mutantCapClose). The overflow marker counts as reported while the
        // run overflows, so it is updated rather than closed.
        let mut reported: Vec<String> = mutants
            .iter()
            .map(|mutant| mutant_id(&mutant.file, &mutant.text))
            .collect();
        if mutants.len() > MUTANT_CAP {
            reported.push("mutant-more".to_string());
        }
        self.open_findings(worker_id, findings).await?;
        self.close_absent_mutants(worker_id, &reported, &tip).await?;
        Ok(Some(format!(
            "mutation check for {worker_id}: {} uncaught changes, findings opened",
            mutants.len()
        )))
    }

    /// What a mutation run that could not run reports. A failed run that left
    /// the checkout exactly at its tip and clean is not a worker error — a
    /// missing tool or a failing command says nothing about the worker — so it
    /// is a notice; only a checkout the run left off its tip or dirty stays an
    /// error, which the caller turns into the worker's error (decision
    /// 2026-11-19).
    async fn mutation_failure(
        &self,
        worker: &WorkerRecord,
        tip: &str,
        command: &str,
        error: &str,
        requested: bool,
    ) -> Result<Option<String>, String> {
        // The real checkout is the node's for a remote worker: ask the node
        // whether its tip is still the one the run started at and its tree is
        // still clean. A shadow can only be stale against that.
        let clean = match worker.remote.as_ref() {
            Some(remote) => self
                .remote
                .node_tip(remote)
                .await
                .map(|answer| answer.tip == tip && answer.dirty.is_empty())
                .unwrap_or(false),
            None => {
                checkout_at(&worker.worktree, tip).await
                    && matches!(
                        phase3::git_value(&worker.worktree, &["status", "--porcelain", "-uall"])
                            .await,
                        Ok(status) if status.is_empty()
                    )
            }
        };
        let worker_id = worker.id.as_str();
        if !clean {
            return Err(error.to_string());
        }
        let Some(line) = missing_tool(error) else {
            return Ok(Some(format!(
                "mutation check for {worker_id} could not run: {error}"
            )));
        };
        let first = self.notice_missing_tool(command);
        // The lead's own request is always answered, even about a tool already
        // reported: only the automatic run repeats nothing.
        if !requested && !first {
            return Ok(None);
        }
        Ok(Some(format!(
            "mutation check for {worker_id} skipped: the mutation tool is not installed \
             ({line}). Install it (for Rust: cargo install cargo-mutants) or remove \
             [mutation] from .coding/checks.toml."
        )))
    }

    /// Record that `worker_id`'s mutation attempt reached the runner at `tip`.
    fn remember_mutation_tip(&self, worker_id: &str, tip: &str) {
        self.lock_mutation_memory().record_tip(worker_id, tip);
    }

    /// The tip `worker_id`'s last mutation attempt in this session reached the
    /// runner at.
    fn last_mutated_tip(&self, worker_id: &str) -> Option<String> {
        self.lock_mutation_memory()
            .last_tip(worker_id)
            .map(str::to_string)
    }

    /// Remember that `command`'s mutation tool is missing; `true` when this is
    /// the first such report in this session.
    fn notice_missing_tool(&self, command: &str) -> bool {
        self.lock_mutation_memory().notice_missing(command)
    }

    /// Forget a missing-tool report: a run that worked proves the tool exists.
    fn forget_missing_tool(&self, command: &str) {
        self.lock_mutation_memory().clear_missing(command);
    }

    /// The session's mutation memory. A poisoned lock is tolerated: the data is
    /// a plain map and set of strings, so a panic elsewhere leaves it
    /// consistent.
    fn lock_mutation_memory(&self) -> std::sync::MutexGuard<'_, MutationMemory> {
        self.mutation_memory
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Whether this worker's current tip still needs its regression test proven
    /// red: the plan is a bug fix with a regression test, the worker's diff
    /// adds a line carrying that test's name, and the tip carries no proof
    /// result yet.
    pub(super) async fn regression_due(&self, worker: &WorkerRecord) -> bool {
        let Some((PlanKind::BugFixing, Some(test))) = self.worker_plan(worker).await else {
            return false;
        };
        let Ok(tip) = phase3::tip(&worker.worktree).await else {
            return false;
        };
        if Self::tip_proven(worker, &tip) {
            return false;
        }
        self.diff_adds(&tip, &test).await
    }

    /// Whether this turn owes an automatic mutation run: the project configured
    /// `[mutation]`, no attempt of this session already reached the runner at
    /// this tip, and the change touches a file that is not a test. A test-only
    /// change has no new code for the mutation tool to change (decision
    /// 2026-11-19).
    pub(super) async fn mutation_due(&self, worker: &WorkerRecord) -> bool {
        if proof::load_proof_config(&self.workspace_root)
            .mutation
            .is_none()
        {
            return false;
        }
        let Ok(tip) = phase3::tip(&worker.worktree).await else {
            return false;
        };
        if self.last_mutated_tip(&worker.id).as_deref() == Some(tip.as_str()) {
            return false;
        }
        // A probe that fails means the run is not due: no runner call, no
        // notice, no worker error. A checkout this broken cannot be mutated,
        // and the lead can still ask for the run with team_mutation_check.
        self.change_touches_source(&worker.worktree, &tip).await
    }

    /// Whether the change from the worker's merge base to `tip` touches a file
    /// that is not a test file ([`proof::is_test_path`]).
    async fn change_touches_source(&self, checkout: &Path, tip: &str) -> bool {
        let Ok(base) = phase3::git_value(&self.workspace_root, &["merge-base", "HEAD", tip]).await
        else {
            return false;
        };
        let args = ["diff", "--name-only", &base, tip];
        match phase3::git(checkout, &args).await {
            Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .any(|path| !path.is_empty() && !proof::is_test_path(path)),
            _ => false,
        }
    }

    /// Whether the worker already carries a proof result for this tip: the
    /// passed red check, or an open proof finding whose tip line names this
    /// tip (finding proofPerTip).
    fn tip_proven(worker: &WorkerRecord, tip: &str) -> bool {
        if worker
            .checks
            .iter()
            .any(|check| check.name == RED_CHECK && check.status == "passed" && check.tip == tip)
        {
            return true;
        }
        worker.findings.iter().any(|finding| {
            matches!(finding.id.as_str(), NOT_RED_ID | INCONCLUSIVE_ID)
                && finding.status == "open"
                && proof_finding_tip(finding) == Some(tip)
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
        proof.regression_proven = worker
            .checks
            .iter()
            .any(|check| check.name == RED_CHECK && check.status == "passed" && check.tip == tip);
        // Only a wontfix whose tip line names the tip being merged waives
        // the gate (finding staleWaiver): a wontfix written for an earlier
        // tip does not cover a later one.
        proof.regression_waived = worker.findings.iter().any(|finding| {
            matches!(finding.id.as_str(), NOT_RED_ID | INCONCLUSIVE_ID)
                && finding.status == "wontfix"
                && proof_finding_tip(finding) == Some(tip)
        });
        proof
    }
}

impl TeamRuntime {
    /// Run the red check now, on the lead's request: the worker's plan may have
    /// gained its `regression_test` after the worker's last turn, so no turn
    /// end ever triggered it.
    pub(super) async fn prove_regression_tool(&self, worker_id: &str) -> Result<String, String> {
        let worker = self.worker(worker_id).await?;
        if worker.kind != "implementation" {
            return Err("only implementation workers are proven".to_string());
        }
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
        ctx.spawn(worker_id, &[ProofJob::Regression]).await?;
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
        if worker.kind != "implementation" {
            return Err("only implementation workers are proven".to_string());
        }
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
        self.proof_ctx()
            .spawn(worker_id, &[ProofJob::MutationRequested])
            .await?;
        Ok(format!(
            "mutation check started for {worker_id}; it finishes with a notice and opens a finding per uncaught change"
        ))
    }
}

/// The proofs a finished turn earns.
///
/// The red check runs for every bug fix whose diff adds the plan's regression
/// test; the mutation check runs for a high-risk worker when the project
/// configured it. Both run in the background: the pump must keep folding the
/// worker's events.
///
/// The dues are computed first and the claim comes last (finding claimRace): a
/// turn that started in between makes the idle claim fail — and that turn's
/// own end triggers the proofs again. Both owed jobs run under one claim, in
/// sequence (finding secondJobDropped).
pub(super) async fn spawn_turn_proofs(ctx: &ProofCtx, worker_id: String) {
    let Ok(worker) = ctx.worker(&worker_id).await else {
        return;
    };
    if worker.kind != "implementation" {
        return;
    }
    let red_due = ctx.regression_due(&worker).await;
    // Only a high-risk worker owes a mutation run, and only when the session
    // has not already mutated this tip and the change carries new code, not
    // just tests (decision 2026-11-19).
    let mutation_due = worker.review_risk == "high" && ctx.mutation_due(&worker).await;
    let mut jobs = Vec::new();
    if red_due {
        jobs.push(ProofJob::Regression);
    }
    if mutation_due {
        jobs.push(ProofJob::Mutation);
    }
    if jobs.is_empty() {
        return;
    }
    let _ = ctx.spawn(&worker_id, &jobs).await;
}

/// Repair checkouts a hard kill left reverse-applied (finding
/// restartWhileProving): every claim writes a marker naming the tip it repairs
/// its checkout back to, and deletes it when the proof ends. A marker that
/// survived a kill is processed here, at open:
///
/// - a checkout its worker record no longer owns is never touched;
/// - a checkout whose HEAD moved past the marker's tip is never touched;
/// - a checkout still at the marker's tip and dirty is restored with the same
///   restore the proof itself uses (`git reset --hard <tip>`, `git clean -ffd`,
///   never `-x`), and the repair logs one line.
///
/// The marker names the worker's HUB checkout, so a remote worker's proof —
/// which ran in the node's checkout — cannot be repaired here: that checkout
/// stays reverse-applied until the node's next `checkout.prepare` resets it
/// onto its branch tip, which is the only state a later run accepts.
///
/// The marker is deleted once processed.
pub(super) async fn repair_interrupted_proofs(workspace_root: &Path, workers: &[WorkerRecord]) {
    let Ok(entries) = std::fs::read_dir(workspace_root.join(MARKER_DIR)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let Ok(marker) = serde_json::from_str::<serde_json::Value>(&text) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let (Some(tip), Some(checkout)) = (
            marker.get("tip").and_then(serde_json::Value::as_str),
            marker.get("checkout").and_then(serde_json::Value::as_str),
        ) else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let checkout = PathBuf::from(checkout);
        let Some(worker) = workers
            .iter()
            .find(|worker| same_path(&worker.worktree, &checkout))
        else {
            // No worker record owns this checkout: the worker was deleted, so
            // its proof contract ended with it. The marker goes; the checkout
            // is never touched.
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let head = phase3::git_value(&checkout, &["rev-parse", "HEAD"])
            .await
            .unwrap_or_default();
        let dirty = phase3::git_value(&checkout, &["status", "--porcelain", "-uall"])
            .await
            .unwrap_or_default();
        if head == tip && !dirty.is_empty() {
            let id = &worker.id;
            match proof::restore_checkout(&checkout, tip).await {
                Ok(()) => eprintln!(
                    "mnemo2: repaired the checkout of {id}, left reverse-applied by a proof a hard kill interrupted (back to {tip})"
                ),
                Err(error) => eprintln!(
                    "mnemo2: the checkout of {id} could not be repaired after an interrupted proof: {error}"
                ),
            }
        }
        let _ = std::fs::remove_file(&path);
    }
}

/// Whether two paths name the same place, canonicalized when they both exist
/// so separator and symlink differences do not split a match.
fn same_path(left: &Path, right: &Path) -> bool {
    match (dunce::canonicalize(left), dunce::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// The tip a proof finding's detail names, as its first line (finding
/// proofPerTip): a proof finding describes the tip whose proof produced it.
fn proof_finding_tip(finding: &Finding) -> Option<&str> {
    finding.detail.lines().next()?.strip_prefix("tip ")
}

/// Whether a status the claim accepts: settled, never mid-turn (finding
/// claimRace).
fn claimable_status(status: &WorkerStatus) -> bool {
    matches!(
        status,
        WorkerStatus::Completed
            | WorkerStatus::InReview
            | WorkerStatus::Idle
            | WorkerStatus::Interrupted
            | WorkerStatus::Stopped
            | WorkerStatus::Failed
            | WorkerStatus::Conflict
    )
}

/// Whether a closed finding re-opens for this run's result.
///
/// A proof finding (finding staleWaiver) re-opens only when the tip its
/// detail names differs from the tip the new result names — a wontfix written
/// for one tip must survive a tool re-run at that same tip.
///
/// A mutant finding (finding mutantWontfixReopen) re-opens when it was closed
/// `fixed` and is reported again: the fix did not hold. A mutant finding the
/// lead resolved `wontfix` is never reopened — the justification is about that
/// specific code change, which its content id names, so a later run reporting
/// the same change again reports nothing new.
fn reopen_closed(existing: &Finding, finding: &Finding) -> bool {
    if matches!(finding.id.as_str(), NOT_RED_ID | INCONCLUSIVE_ID) {
        return proof_finding_tip(existing)
            .is_some_and(|closed_tip| Some(closed_tip) != proof_finding_tip(finding));
    }
    existing.status != "wontfix"
}

/// A mutant finding's stable id: the first 8 hex digits of an FNV-1a 64-bit
/// hash of `file` + newline + `text` (finding mutantIds) — the same content
/// is the same uncaught change across runs, and a run that reports it again
/// finds its finding instead of opening a twin. `DefaultHasher` is not
/// stable across releases, so the hash is inline.
pub(super) fn mutant_id(file: &str, text: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in format!("{file}\n{text}").as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let hex = format!("{hash:016x}");
    format!("mutant-{}", &hex[..8])
}

/// The findings a mutation run reports: one per missed mutant, identified by
/// its content, capped, with one overflow finding listing the rest. The
/// detail's first line is the tip (finding mutantIds), so a later run's
/// update and close decisions can tell runs apart.
fn mutant_findings(mutants: &[Mutant], tip: &str, fallback: &str) -> Vec<Finding> {
    let mut findings: Vec<Finding> = mutants
        .iter()
        .take(MUTANT_CAP)
        .map(|mutant| Finding {
            id: mutant_id(&mutant.file, &mutant.text),
            severity: "low".to_string(),
            title: format!("No test catches: {}", mutant.text),
            detail: format!(
                "tip {tip}\nMnemo2 changed this code and every test still passed. Add a test \
                 that fails for this change, or resolve wontfix with why it does not matter."
            ),
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
            detail: format!("tip {tip}
{}", rest.join("
")),
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

/// Whether a failed proof command says its tool is not installed: the first
/// line of `error` carrying one of the markers a shell uses for a missing
/// command.
fn missing_tool(error: &str) -> Option<&str> {
    const MARKERS: [&str; 4] = [
        "no such command",
        "is not recognized as",
        "command not found",
        "not found in path",
    ];
    error.lines().find(|line| {
        let lower = line.to_lowercase();
        MARKERS.iter().any(|marker| lower.contains(marker))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan d3336a8a (finding proofSlotsUntested): the real runner's machine
    /// slots come from the config dir it was BUILT with. Re-pointing
    /// [`RealProofs::heavy_slots`] at a re-resolved `~/.mnemo` would (a) give
    /// proofs a second slot pool and (b) write into the real home directory in
    /// this very test — so the exact-dir equality below is the red line, and the
    /// Busy check proves the pool object really points at the temp dir's slot.
    #[test]
    fn real_proofs_takes_its_slots_from_its_config_dir() {
        let dir = tempfile::tempdir().expect("temp config dir");
        std::fs::write(
            dir.path().join("config.toml"),
            "[general.machine]\nheavy_commands = 1\n",
        )
        .expect("temp config.toml");
        let proofs = RealProofs::new(dir.path().to_path_buf());
        let slots = proofs.heavy_slots().expect("the temp config loads");
        assert_eq!(
            slots.dir(),
            mnemo::machine_slots::default_dir(dir.path()),
            "the proof's slot pool must be the RUNTIME config dir, not a re-resolved \
             ~/.mnemo (finding proofConfigDir)"
        );
        assert_eq!(
            slots.slots(),
            1,
            "the limit comes from THAT dir's config.toml"
        );
        // Hold the temp dir's only slot: the runner's own HeavySlots must see it
        // as taken. A re-resolved home pool would report a free slot here.
        let held = slots
            .try_take()
            .guard()
            .expect("the temp dir's only slot is free");
        assert!(
            matches!(
                proofs.heavy_slots().expect("slots").try_take(),
                mnemo::machine_slots::SlotAttempt::Busy
            ),
            "the runner's slots are the temp dir's slots"
        );
        drop(held);
    }
}
