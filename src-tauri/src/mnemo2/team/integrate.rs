// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `team_integrate` (plan ca7f2177 decision 6): merge an existing local
//! branch into a worker's branch, in the worker's own checkout, done by
//! TeamManager. A step that needs another branch's work gets it this way, and
//! integrating the lead branch is the deliberate catch-up for a stale base.
//!
//! A conflict aborts the merge and changes nothing. A clean merge is recorded
//! in the worker's `integrations`; its checks and review are bound to the old
//! tip, so the new merge commit invalidates them on its own. A Mnemo worker
//! hears about the merge at the start of its next message; an agent worker's
//! note goes back to the lead, who passes it on.

use mnemo::app::worktrees::merge_branch_into_checkout;

use super::dispatch::short_sha;
use super::record::{Integration, WorkerRecord, WorkerStatus};
use super::{unix_ms, TeamRuntime};

impl TeamRuntime {
    /// Merge `branch` into `worker_id`'s branch. Returns the updated record
    /// and, for a worker that runs as a Claude Code agent, the note for the
    /// lead to pass to its agent.
    ///
    /// Refused unless the worker is an implementation worker that is not
    /// merged, merging or cleaned; has no busy session, proof or check run;
    /// is not an agent worker that is still Working or Fixing; and `branch`
    /// is an existing local branch other than the worker's own. The helper
    /// refuses a dirty checkout and a branch the worker already contains.
    pub(super) async fn integrate(
        &self,
        worker_id: &str,
        branch: &str,
    ) -> Result<(WorkerRecord, Option<String>), String> {
        let _guard = self.merge_lock.lock().await;
        self.proving_guard(worker_id)?;
        let worker = self.worker(worker_id).await?;
        if worker.kind != "implementation" {
            return Err(format!(
                "team_integrate merges into implementation workers; {worker_id} is an {}",
                worker.kind
            ));
        }
        if matches!(
            worker.status,
            WorkerStatus::Merged | WorkerStatus::Merging | WorkerStatus::Cancelled
        ) {
            return Err(format!(
                "{worker_id} is merged or cleaned: there is no branch to integrate into"
            ));
        }
        if worker.claude_agent.is_some()
            && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
        {
            return Err(format!(
                "{worker_id}'s Claude Code agent is still working: wait for it to end; Mnemo then finishes the worker"
            ));
        }
        if self.worker_is_busy(worker_id).await {
            return Err(format!(
                "{worker_id} is busy: integrate once its turn has ended"
            ));
        }
        let branch = branch.trim();
        if branch.is_empty() {
            return Err("team_integrate requires a 'branch' name".to_string());
        }
        if branch == worker.branch {
            return Err(format!("{branch} is {worker_id}'s own branch"));
        }
        let reference = format!("refs/heads/{branch}");
        if crate::mnemo2::phase3::git_value(
            &self.workspace_root,
            &["rev-parse", "--verify", "--quiet", &reference],
        )
        .await
        .is_err()
        {
            return Err(format!("there is no local branch named {branch}"));
        }
        let message = format!(
            "integrate {branch} into {} (team_integrate)",
            worker.branch
        );
        let commit = match merge_branch_into_checkout(
            self.workspace_root.clone(),
            worker.worktree.clone(),
            branch.to_string(),
            message,
        )
        .await
        .map_err(|error| format!("could not merge {branch} into {worker_id}: {error}"))?
        {
            Ok(commit) => commit,
            Err(paths) => {
                return Err(format!(
                    "merging {branch} into {worker_id} conflicts in: {}; nothing changed.",
                    paths.join(", ")
                ));
            }
        };
        let note = format!(
            "[From Mnemo2] Mnemo merged {branch} into your branch at {}; read git log -1 --stat to see what came in.",
            short_sha(&commit)
        );
        let agent = worker.claude_agent.is_some();
        self.update_worker(worker_id, |worker| {
            worker.integrations.push(Integration {
                branch: branch.to_string(),
                commit: commit.clone(),
                at_ms: unix_ms(),
            });
            if !agent {
                worker.integration_note = Some(match worker.integration_note.take() {
                    Some(earlier) => format!("{earlier}\n{note}"),
                    None => note.clone(),
                });
            }
        })
        .await;
        self.persist().await?;
        let worker = self.worker(worker_id).await?;
        Ok((worker, agent.then_some(note)))
    }
}
