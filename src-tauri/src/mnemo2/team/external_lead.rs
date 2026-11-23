// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! External lead state: the claim an outside Claude Code session makes on
//! the team over MCP, its notice queue and waiters, and the lead's status.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use mnemo::app::plan_board::BoardStatus;
use mnemo::headless::SessionId;

use crate::mnemo2::ledger;
use crate::mnemo2::notice_tag::{NoticeState, TurnTag};
use crate::mnemo2::runtime::{finished_notice, TEAM_NOTICE_PREFIX};

use super::agent_log;
use super::agent_watch::{self, FinishedRun, NO_FINAL_MESSAGE};
use super::lead_tools::TeamLeadStatus;
use super::pump::{save_workers, save_workers_now_locked};
use super::record::{audit_finished, WorkerRecord, WorkerStatus};
use super::{unix_ms, TeamRuntime};

/// How long an attached external lead may go without calling in before it is
/// detached and the in-app lead takes over again.
pub const EXTERNAL_LEAD_IDLE_MS: u64 = 10 * 60 * 1000;

/// The durable-learning nudge appended to external-lead merge and close results.
pub const EXTERNAL_LEAD_MEMORY_REMINDER: &str = "Record durable SPEC:/DECISION:/HOW: learnings with memory_write and supersede stale records with memory_supersede.";

/// The most notices an unread external queue keeps; the oldest are dropped.
const EXTERNAL_QUEUE_CAP: usize = 256;

/// The state of an external lead (Claude Code leading over MCP) on one team.
#[derive(Debug, Default)]
pub(super) struct ExternalLead {
    /// When it attached (Unix epoch milliseconds); `None` while detached.
    since_ms: Option<u64>,
    /// When it was released for silence (Unix epoch milliseconds). It has
    /// not left, so the UI still names it; cleared when it attaches again or
    /// really detaches.
    idle_since_ms: Option<u64>,
    /// Notices queued for it, oldest first. The text is exactly what the
    /// in-app lead would have been sent.
    queue: VecDeque<String>,
    /// Notices an external lead never read and the in-app lead could not take
    /// (no session to receive them). They wait here, oldest first, for the
    /// in-app lead's next notice or the next external lead.
    undelivered: VecDeque<String>,
    /// Helper process ids that made a tool call while the lead was attached.
    /// A named close releases the lead only after the last id has left.
    bridges: std::collections::BTreeSet<String>,
    /// Its last call (MCP, notices or wait), in Unix epoch milliseconds.
    last_activity_ms: u64,
    /// Successful memory searches in the current plan-creation window.
    memory_search_since_plan_create: bool,
    bug_search_since_plan_create: bool,
    /// Waiters currently blocked in [`TeamRuntime::wait_external_notices`].
    waiters: usize,
    /// The Claude Code session that staked this lead, when it named itself
    /// (the Stop hook suggests `wait --session <id>`; Claude Code tells its
    /// MCP servers no session id, so a bare tool call cannot). Kept through
    /// the idle release, so that session's own hooks can re-attach it; a
    /// fresh anonymous claim or a forgotten idle lead clears it.
    session_id: Option<String>,
}

/// How a [`TeamRuntime::wait_external`] ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalWaitStatus {
    /// At least one notice was queued.
    Notices,
    /// Nothing arrived before the timeout.
    Timeout,
    /// The external lead detached while waiting (or was not attached).
    Detached,
}

impl ExternalWaitStatus {
    /// The wire name `/lead/wait` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notices => "notices",
            Self::Timeout => "timeout",
            Self::Detached => "detached",
        }
    }
}

/// The result of a [`TeamRuntime::wait_external`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalWait {
    /// The drained notices, oldest first (empty unless the status is
    /// [`ExternalWaitStatus::Notices`]).
    pub notices: Vec<String>,
    /// Why the wait ended.
    pub status: ExternalWaitStatus,
}

/// Counts one blocked waiter for as long as it lives, and counts the end of
/// the wait as activity.
struct ExternalWaiter<'a>(&'a TeamRuntime);

impl<'a> ExternalWaiter<'a> {
    fn new(team: &'a TeamRuntime) -> Self {
        let mut state = team.external_state();
        state.waiters += 1;
        state.last_activity_ms = unix_ms();
        drop(state);
        Self(team)
    }
}

impl Drop for ExternalWaiter<'_> {
    fn drop(&mut self) {
        let mut state = self.0.external_state();
        state.waiters = state.waiters.saturating_sub(1);
        state.last_activity_ms = unix_ms();
    }
}

impl TeamRuntime {
    /// Mark the lead non-idle: the merge gate's busy hook, re-armed at
    /// every turn boundary so mid-turn merges stay refused.
    pub fn begin_lead_turn(&self) {
        self.lead_busy.store(true, Ordering::SeqCst);
    }

    /// Mirror the lead session's busy state into the merge gate.
    pub fn set_lead_busy(&self, busy: bool) {
        self.lead_busy.store(busy, Ordering::SeqCst);
    }

    pub(crate) fn lead_is_busy(&self) -> bool {
        self.lead_busy.load(Ordering::SeqCst)
    }

    pub(crate) async fn attach_lead(&self, session_id: SessionId) {
        *self.lead_session.lock().await = Some(session_id);
    }

    pub(crate) async fn lead_is_current(&self, session_id: SessionId) -> bool {
        *self.lead_session.lock().await == Some(session_id)
    }

    fn external_state(&self) -> std::sync::MutexGuard<'_, ExternalLead> {
        self.external.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Make an external lead (Claude Code over MCP) this team's lead: from
    /// now on lead notices queue for it instead of reaching the in-app lead.
    /// Notices a previous external lead never read (and the in-app lead could
    /// not take) are handed to this one. Returns `true` when it was not
    /// attached before. Counts as activity. The claim stays anonymous: a bare
    /// MCP tool call cannot know Claude Code's session id.
    pub fn attach_external_lead(&self) -> bool {
        self.attach_external_lead_as(None)
    }

    /// [`TeamRuntime::attach_external_lead`] naming the Claude Code session
    /// that staked the claim, when the caller knows it: the waiter runs with
    /// `--session <id>` (the Stop hook names it — only a hook knows a session
    /// id). A named claim also names an already-attached lead, so that
    /// session's own hooks can re-attach it after an idle release
    /// ([`TeamRuntime::reattach_idle_external_lead`]); a fresh anonymous
    /// claim clears the name instead, so no session can reach through a hook
    /// for a lead it never staked.
    pub fn attach_external_lead_as(&self, session: Option<&str>) -> bool {
        let named = session.map(str::trim).filter(|session| !session.is_empty());
        let mut state = self.external_state();
        state.last_activity_ms = unix_ms();
        if let Some(named) = named {
            state.session_id = Some(named.to_string());
        }
        if state.since_ms.is_some() {
            return false;
        }
        if named.is_none() {
            state.session_id = None;
        }
        state.since_ms = Some(state.last_activity_ms);
        state.idle_since_ms = None;
        state.queue = std::mem::take(&mut state.undelivered);
        state.memory_search_since_plan_create = false;
        state.bug_search_since_plan_create = false;
        drop(state);
        // A new lead reads the tier setup in its fresh tool list, so nothing
        // has changed since it last read it.
        self.tier_changes().mark_seen();
        if let Some(board) = self.board() {
            board.set_external_lead_attached(true);
        }
        true
    }

    /// Bring an idle-released lead back through its own hook. Only when the
    /// lead was released for silence (it has not really detached) and
    /// `session` names the Claude Code session that staked the claim: a hook
    /// from another session must never take the lead, and an anonymous lead
    /// has no hooks to come back with. Returns `true` when the lead attached
    /// again — which also clears the idle mark and keeps the claim staked,
    /// so it can come back the same way the next time it idles out.
    pub fn reattach_idle_external_lead(&self, session: Option<&str>) -> bool {
        let Some(session) = session.map(str::trim).filter(|session| !session.is_empty()) else {
            return false;
        };
        let staked = {
            let state = self.external_state();
            state.idle_since_ms.is_some() && state.session_id.as_deref() == Some(session)
        };
        if !staked {
            return false;
        }
        self.attach_external_lead_as(Some(session))
    }

    /// Remember a helper process that made a tool call while this external
    /// lead was attached. Does nothing while no external lead is attached.
    pub fn note_external_bridge(&self, bridge: &str) {
        let mut state = self.external_state();
        if state.since_ms.is_some() {
            state.bridges.insert(bridge.to_string());
        }
    }

    /// Forget a closing helper process and return how many leading helpers
    /// remain. The caller releases the lead only when this returns zero.
    pub fn forget_external_bridge(&self, bridge: &str) -> usize {
        let mut state = self.external_state();
        state.bridges.remove(bridge);
        state.bridges.len()
    }

    /// Hand the team back to the in-app lead and wake blocked waiters.
    /// Returns `true` when an external lead was attached.
    ///
    /// Notices it never read are not dropped: they move to the undelivered
    /// list, which [`TeamRuntime::take_undelivered_notices`] hands over (the
    /// caller gives them to the in-app lead, see
    /// `LeadRuntime::release_external_lead`).
    ///
    /// The staking session is kept: the idle release runs through here too,
    /// and that session's own hooks must still be able to re-attach it
    /// ([`TeamRuntime::reattach_idle_external_lead`]); a fresh claim or a
    /// forgotten idle lead clears it.
    pub fn detach_external_lead(&self) -> bool {
        let was_attached = {
            let mut state = self.external_state();
            let was_attached = state.since_ms.take().is_some();
            state.idle_since_ms = None;
            state.bridges.clear();
            // They are newer than anything already held back.
            let unread = std::mem::take(&mut state.queue);
            state.undelivered.extend(unread);
            while state.undelivered.len() > EXTERNAL_QUEUE_CAP {
                state.undelivered.pop_front();
            }
            state.memory_search_since_plan_create = false;
            state.bug_search_since_plan_create = false;
            was_attached
        };
        if let Some(board) = self.board() {
            board.set_external_lead_attached(false);
        }
        self.external_wake.notify_waiters();
        was_attached
    }

    /// The external lead leaves (its session ended, it idled out, or the
    /// project closes): detach it as [`TeamRuntime::detach_external_lead`]
    /// does, then hand back the agent workers it can no longer run
    /// ([`TeamRuntime::hand_back_agent_workers`]). Returns `true` when an
    /// external lead was attached; nothing is handed back otherwise.
    pub async fn leave_external_lead(&self) -> bool {
        let was_attached = self.detach_external_lead();
        if was_attached {
            if let Err(error) = self.hand_back_agent_workers().await {
                eprintln!("team: could not hand back agent workers after the lead left: {error}");
            }
        }
        was_attached
    }

    /// Hand back every agent worker no Claude Code lead runs any more: one
    /// that waits for its agent to be launched, or whose agent stalled. Each
    /// becomes an ordinary interrupted Mnemo worker with the reason (record,
    /// branch, checkout and commits kept), and the next lead gets one notice
    /// per worker. A running agent is left alone: its transcript may still be
    /// written. Does nothing while an external lead is attached (it came back
    /// before this ran). Returns the ids handed back.
    pub(super) async fn hand_back_agent_workers(&self) -> Result<Vec<String>, String> {
        let _guard = self.merge_lock.lock().await;
        if self.external_lead_attached() {
            return Ok(Vec::new());
        }
        let candidates: Vec<WorkerRecord> = self
            .records
            .lock()
            .await
            .iter()
            .filter(|worker| {
                worker.claude_agent.is_some()
                    && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
            })
            .cloned()
            .collect();
        let mut due = Vec::new();
        for worker in candidates {
            let view = self.agent_view(&worker).await;
            if agent_watch::hand_back_due(&worker, view.as_ref()) {
                due.push(worker);
            }
        }
        if due.is_empty() {
            return Ok(Vec::new());
        }
        let mut handed = Vec::new();
        {
            let mut records = self.records.lock().await;
            for seen in &due {
                // Re-checked: the agent may have been registered meanwhile.
                let Some(worker) = records.iter_mut().find(|worker| worker.id == seen.id) else {
                    continue;
                };
                if worker.claude_agent.is_none()
                    || worker.claude_agent_id != seen.claude_agent_id
                    || worker.status != seen.status
                {
                    continue;
                }
                agent_watch::hand_back(worker);
                handed.push(worker.id.clone());
            }
        }
        for id in &handed {
            self.agent_watch.forget(id);
        }
        if handed.is_empty() {
            return Ok(handed);
        }
        // The records changed either way: a failed save is reported, and the
        // next save writes them.
        let saved = save_workers(
            &self.records,
            &self.usage,
            &self.save_lock,
            &self.persistence,
        )
        .await;
        let held: Vec<String> = handed
            .iter()
            .map(|id| agent_watch::handed_back_notice(id))
            .filter(|notice| !self.push_external_notice(notice.clone()))
            .collect();
        self.restore_undelivered_notices(held);
        let _ = self.signals.send(super::BoardSignal::Changed);
        saved.map(|()| handed)
    }

    /// Whether an external lead currently leads this team.
    pub fn external_lead_attached(&self) -> bool {
        self.external_state().since_ms.is_some()
    }

    /// Refuse an external-lead plan until its attachment searched memory.
    /// Bug-fixing plans additionally require a search narrowed to bug records.
    pub fn external_plan_create_refusal(&self, bug_fixing: bool) -> Option<String> {
        let state = self.external_state();
        state.since_ms?;
        if !state.memory_search_since_plan_create {
            return Some(
                "plan_create refused: call memory_search first, then retry plan_create."
                    .to_string(),
            );
        }
        if bug_fixing && !state.bug_search_since_plan_create {
            return Some(
                "plan_create refused for kind bug_fixing: call memory_search with record_type \"bug\", then retry plan_create."
                    .to_string(),
            );
        }
        None
    }

    /// Record a successful external-lead memory search for the current
    /// attachment and current plan-creation window.
    pub fn record_external_memory_search(&self, bug_search: bool) {
        let mut state = self.external_state();
        if state.since_ms.is_some() {
            state.memory_search_since_plan_create = true;
            state.bug_search_since_plan_create |= bug_search;
        }
    }

    /// Start a fresh required-search window after a successful plan_create.
    pub fn record_external_plan_create(&self) {
        let mut state = self.external_state();
        if state.since_ms.is_some() {
            state.memory_search_since_plan_create = false;
            state.bug_search_since_plan_create = false;
        }
    }

    /// When the external lead attached (Unix epoch milliseconds), if it is.
    pub fn external_lead_since_ms(&self) -> Option<u64> {
        self.external_state().since_ms
    }

    /// Remember that the external lead was released for silence at `at_ms`
    /// rather than leaving. No-op while it is attached.
    pub fn mark_external_lead_idle(&self, at_ms: u64) {
        let mut state = self.external_state();
        if state.since_ms.is_none() {
            state.idle_since_ms = Some(at_ms);
        }
    }

    /// When the external lead was released for silence, if it has not
    /// attached again or detached since.
    pub fn external_lead_idle_since_ms(&self) -> Option<u64> {
        self.external_state().idle_since_ms
    }

    /// The lead state a `team_status` answer reports, read under one lock so
    /// an attach or idle release cannot land between the two timestamps.
    pub(super) fn lead_status(&self) -> TeamLeadStatus {
        let (since_ms, idle_since_ms) = {
            let state = self.external_state();
            (state.since_ms, state.idle_since_ms)
        };
        TeamLeadStatus {
            kind: if since_ms.is_some() || idle_since_ms.is_some() {
                "external"
            } else {
                "in_app"
            },
            attached: since_ms.is_some(),
            since_ms,
            idle_since_ms,
        }
    }

    /// Forget an idle external lead, together with the Claude Code session
    /// that staked it (after a real detach even that session's hooks must not
    /// bring it back). Returns `true` when one was idle.
    pub fn clear_external_lead_idle(&self) -> bool {
        let mut state = self.external_state();
        let was_idle = state.idle_since_ms.take().is_some();
        state.session_id = None;
        was_idle
    }

    /// Record a call from the external lead (keeps it from idling out).
    pub fn touch_external_lead(&self) {
        self.external_state().last_activity_ms = unix_ms();
    }

    /// Whether the attached external lead has been silent for `idle_ms` as of
    /// `now_ms`. A blocked waiter counts as presence, so a long-poll never
    /// idles out.
    pub fn external_idle_expired(&self, now_ms: u64, idle_ms: u64) -> bool {
        let state = self.external_state();
        state.since_ms.is_some()
            && state.waiters == 0
            && now_ms.saturating_sub(state.last_activity_ms) >= idle_ms
    }

    /// Whether a `wait` long-poll is blocked right now.
    pub fn external_waiter_alive(&self) -> bool {
        self.external_state().waiters > 0
    }

    /// Queue a notice for the external lead and wake any waiter. The text is
    /// the exact string the in-app lead would have been sent.
    ///
    /// The check and the push happen under one lock: with no external lead
    /// attached nothing is queued and this returns `false`, so the caller
    /// gives the notice to the in-app lead instead (a detach can never
    /// strand a notice in a queue nobody reads).
    pub fn push_external_notice(&self, text: String) -> bool {
        {
            let mut state = self.external_state();
            if state.since_ms.is_none() {
                return false;
            }
            if state.queue.len() >= EXTERNAL_QUEUE_CAP {
                state.queue.pop_front();
            }
            state.queue.push_back(text);
        }
        self.external_wake.notify_waiters();
        true
    }

    /// Take every queued notice, oldest first.
    pub fn drain_external_notices(&self) -> Vec<String> {
        self.external_state().queue.drain(..).collect()
    }

    /// Keep the notices that still describe their worker: a tagged notice
    /// whose worker moved on, or no longer exists, is dropped. Untagged
    /// notices always stay.
    pub(crate) async fn current_notices(&self, notices: Vec<String>) -> Vec<String> {
        let records = self.records.lock().await;
        notices
            .into_iter()
            .filter(
                |notice| match crate::mnemo2::notice_tag::TurnTag::parse(notice) {
                    Some(tag) => records
                        .iter()
                        .any(|worker| worker.id == tag.worker_id && tag.still_current(worker)),
                    None => true,
                },
            )
            .collect()
    }

    /// Put notices that were drained but never reached the external lead
    /// (the response write failed) back at the front of its queue, in their
    /// original order. If the lead detached meanwhile they join the
    /// undelivered list instead, ahead of anything newer.
    pub fn requeue_external_notices(&self, notices: Vec<String>) {
        if notices.is_empty() {
            return;
        }
        {
            let mut state = self.external_state();
            let attached = state.since_ms.is_some();
            let target = if attached {
                &mut state.queue
            } else {
                &mut state.undelivered
            };
            for notice in notices.into_iter().rev() {
                target.push_front(notice);
            }
            while target.len() > EXTERNAL_QUEUE_CAP {
                target.pop_front();
            }
        }
        self.external_wake.notify_waiters();
    }

    /// Take the notices an external lead never read and the in-app lead has
    /// not yet received, oldest first.
    pub fn take_undelivered_notices(&self) -> Vec<String> {
        self.external_state().undelivered.drain(..).collect()
    }

    /// Put notices the in-app lead could not take back on the undelivered
    /// list, ahead of anything newer.
    pub fn restore_undelivered_notices(&self, notices: Vec<String>) {
        let mut state = self.external_state();
        for notice in notices.into_iter().rev() {
            state.undelivered.push_front(notice);
        }
        while state.undelivered.len() > EXTERNAL_QUEUE_CAP {
            state.undelivered.pop_front();
        }
    }

    /// A working Claude Code agent worker, for the agent gate: its id and
    /// the settled tier it runs on, so the gate names what runs while the
    /// lead's own Agent does the step.
    pub async fn claude_agent_working(&self) -> Option<(String, String)> {
        let records = self.records.lock().await;
        records
            .iter()
            .find(|worker| {
                worker.claude_agent.is_some()
                    && matches!(worker.status, WorkerStatus::Working | WorkerStatus::Fixing)
            })
            .map(|worker| (worker.id.clone(), worker.tier.clone().unwrap_or_default()))
    }

    /// Finish a Claude Code agent worker (`team_agent_done`): record its
    /// final report, mark it completed or failed, and bump the turn as the
    /// pump's Finished handling does. Only a worker that runs as a Claude
    /// Code agent, in status Working, Fixing or Interrupted, can be closed this
    /// way; anything else is refused with the reason. The finished (or
    /// failed) notice then reaches the lead through
    /// [`TeamRuntime::push_agent_done_notice`].
    pub async fn agent_done(
        &self,
        worker_id: &str,
        report: String,
        failed: bool,
    ) -> Result<WorkerRecord, String> {
        self.agent_done_with_snapshot(worker_id, report, failed, None)
            .await
    }

    /// `agent_done` with an optional saved-worker override for the
    /// persistence regression seam.
    ///
    /// On a run Mnemo already finished itself (the agent's transcript said
    /// it ended, [`TeamRuntime::auto_finish_agent`]) the call succeeds
    /// without a second notice: a non-empty report replaces the agent's last
    /// message, and `failed` marks the worker Failed with the failed notice.
    pub(super) async fn agent_done_with_snapshot(
        &self,
        worker_id: &str,
        report: String,
        failed: bool,
        snapshot_override: Option<WorkerRecord>,
    ) -> Result<WorkerRecord, String> {
        let _guard = self.merge_lock.lock().await;
        let record = self.worker(worker_id).await?;
        if record.claude_agent.is_none() {
            return Err(format!(
                "{worker_id} does not run as a Claude Code agent: team_agent_done only finishes agent workers"
            ));
        }
        if self.agent_watch.auto_finished(&record) {
            let worker = self
                .close_agent_run_locked(&record, report, failed, true, snapshot_override)
                .await?;
            if failed {
                self.agent_watch.clear_auto_finished(worker_id);
                self.push_agent_done_notice(&worker, true, false);
            }
            return Ok(worker);
        }
        // Called once per agent run: the first run, a continuation after
        // team_message, or a fix round after team_route_fix.
        if !matches!(
            record.status,
            WorkerStatus::Working | WorkerStatus::Interrupted | WorkerStatus::Fixing
        ) {
            let status = serde_json::to_value(record.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "not working".to_string());
            return Err(format!(
                "{worker_id} is {status}: team_agent_done needs a Working, Fixing or Interrupted agent worker"
            ));
        }
        let worker = self
            .close_agent_run_locked(&record, report, failed, false, snapshot_override)
            .await?;
        self.push_agent_done_notice(&worker, failed, false);
        Ok(worker)
    }

    /// Finish the worker of an agent run whose transcript says it ended, as
    /// `team_agent_done` with `failed=false` would: the agent's last message
    /// (or [`NO_FINAL_MESSAGE`]) is the report, the worker is Completed, a
    /// fix round closes, and the lead gets the ordinary finished notice with
    /// the report. Only while the worker still runs that same run (same
    /// agent, same turn, Working or Fixing), re-checked under the merge lock,
    /// so a lead's `team_agent_done` that got there first is not doubled.
    /// Returns whether it finished the worker.
    pub(super) async fn auto_finish_agent(&self, run: &FinishedRun) -> Result<bool, String> {
        // Read before the lock: the transcript can be long.
        let report = run
            .transcript
            .as_deref()
            .and_then(|path| agent_log::last_assistant_text_of(std::path::Path::new(path)))
            .unwrap_or_else(|| NO_FINAL_MESSAGE.to_string());
        let _guard = self.merge_lock.lock().await;
        let Ok(record) = self.worker(&run.worker_id).await else {
            return Ok(false);
        };
        let same_run = record.claude_agent.is_some()
            && record.claude_agent_id.as_deref() == Some(run.agent_id.as_str())
            && record.turns == run.turns
            && matches!(record.status, WorkerStatus::Working | WorkerStatus::Fixing);
        if !same_run {
            return Ok(false);
        }
        let worker = self
            .close_agent_run_locked(&record, report, false, false, None)
            .await?;
        // Marked before the merge lock is released, so a team_agent_done
        // waiting on it finds the run finished.
        self.agent_watch.mark_auto_finished(&worker);
        self.push_agent_done_notice(&worker, false, true);
        Ok(true)
    }

    /// Close one agent run on `previous` (the record the caller checked
    /// under the merge lock it holds) and save it. A run closes Completed or
    /// Failed with `report`, a counted turn, and its fix round closed. With
    /// `after_auto` the run was already closed by Mnemo: only a non-empty
    /// `report` replaces the stored one and `failed` marks it Failed. A
    /// failed save restores what this changed and returns the error.
    async fn close_agent_run_locked(
        &self,
        previous: &WorkerRecord,
        report: String,
        failed: bool,
        after_auto: bool,
        snapshot_override: Option<WorkerRecord>,
    ) -> Result<WorkerRecord, String> {
        let worker_id = previous.id.as_str();
        // Serialize the whole mutation/write/rollback section. Other savers
        // take this guard before cloning records, so none can observe or
        // persist the transient Completed state.
        let save_guard = self.save_lock.lock().await;
        self.update_worker(worker_id, |worker| {
            if after_auto {
                if failed {
                    worker.status = WorkerStatus::Failed;
                }
                if !report.trim().is_empty() {
                    worker.report = Some(report);
                }
                return;
            }
            worker.status = if failed {
                WorkerStatus::Failed
            } else {
                WorkerStatus::Completed
            };
            worker.turns = worker.turns.saturating_add(1);
            worker.pending = None;
            worker.compacting = false;
            worker.report = Some(report);
            // A finished fix round closes, as a Mnemo worker's does when its
            // fix turn ends.
            worker.fixer_finding_id = None;
        })
        .await;
        if let Err(error) = save_workers_now_locked(
            &self.records,
            &self.usage,
            &self.persistence,
            snapshot_override,
            &save_guard,
        )
        .await
        {
            // No notice has been queued yet. Restore only the fields this
            // completion changed; a concurrent routing classification or
            // other independent update must survive the rollback.
            let mut records = self.records.lock().await;
            if let Some(worker) = records.iter_mut().find(|worker| worker.id == worker_id) {
                worker.status = previous.status;
                worker.turns = previous.turns;
                worker.pending = previous.pending.clone();
                worker.compacting = previous.compacting;
                worker.report = previous.report.clone();
                worker.fixer_finding_id = previous.fixer_finding_id.clone();
            }
            return Err(error);
        }
        drop(save_guard);
        self.worker(worker_id).await
    }

    /// Deliver the finished (or failed) notice for a Claude Code agent
    /// worker `team_agent_done` closed: the pump's exact notice shape, so
    /// the review skill applies to it unchanged. The tag is built from the
    /// completed record — an agent worker's generation is 0 (it never had
    /// a session) and its turn was just bumped, so `current_notices` keeps
    /// the notice. It rides the external-lead queue when one is attached
    /// (the tool answer drains it); otherwise it parks on the undelivered
    /// list for whoever leads next.
    /// `with_report` quotes the stored report after the finished line: the
    /// lead did not write it (Mnemo finished the run from the agent's
    /// transcript).
    fn push_agent_done_notice(&self, worker: &WorkerRecord, failed: bool, with_report: bool) {
        let step = match (&worker.plan_id, worker.plan_step) {
            (Some(plan), Some(step)) => format!(" on plan {plan} step {}", step + 1),
            _ => String::new(),
        };
        let detail = if failed {
            // A failed agent worker is never reviewed: name the failure,
            // quote the agent's report, and offer the ways forward.
            let report = worker
                .report
                .as_deref()
                .map(|report| format!("\n\nIts report: {report}"))
                .unwrap_or_default();
            format!(
                "{} could not finish its turn{step}: the Claude Code agent failed.{report}\nTo try again, dispatch the step again (a fresh worker id), or team_stop this worker to clear it.",
                worker.name
            )
        } else if with_report {
            let report = worker.report.as_deref().unwrap_or(NO_FINAL_MESSAGE);
            format!(
                "{}\nIts Claude Code agent ended; its last message is the report:\n{}",
                finished_notice(worker),
                agent_watch::clip_report(report)
            )
        } else {
            finished_notice(worker)
        };
        let tag = TurnTag::of(
            worker,
            worker.generation,
            if failed {
                NoticeState::Failed
            } else {
                NoticeState::Finished
            },
        )
        .suffix();
        let text = {
            use std::fmt::Write as _;
            let mut text = String::new();
            let _ = writeln!(text, "{TEAM_NOTICE_PREFIX} worker_id={}{}", worker.id, tag);
            let _ = writeln!(text, "{detail}");
            let _ = write!(
                text,
                "Use team_status with this exact worker_id. Fix findings through your own agent (SendMessage to it); team_stop clears the worker. Never infer lifecycle from Git, files, timestamps, or team.json."
            );
            text
        };
        if !self.push_external_notice(text.clone()) {
            self.restore_undelivered_notices(vec![text]);
        }
    }

    /// The queued notices, left in place.
    pub fn peek_external_notices(&self) -> Vec<String> {
        self.external_state().queue.iter().cloned().collect()
    }

    /// Block until a notice is queued (then drain and return all of them), the
    /// external lead detaches, or `timeout` passes (then return `[]`).
    pub async fn wait_external_notices(&self, timeout: std::time::Duration) -> Vec<String> {
        self.wait_external(timeout).await.notices
    }

    /// Like [`TeamRuntime::wait_external_notices`], and says how the wait
    /// ended.
    pub async fn wait_external(&self, timeout: std::time::Duration) -> ExternalWait {
        let _waiter = ExternalWaiter::new(self);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Arm the wake-up before looking, so a push in between is not lost.
            let woken = self.external_wake.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            let notices = self.current_notices(self.drain_external_notices()).await;
            if !notices.is_empty() {
                return ExternalWait {
                    notices,
                    status: ExternalWaitStatus::Notices,
                };
            }
            if !self.external_lead_attached() {
                return ExternalWait {
                    notices,
                    status: ExternalWaitStatus::Detached,
                };
            }
            if tokio::time::timeout_at(deadline, woken).await.is_err() {
                let notices = self.current_notices(self.drain_external_notices()).await;
                let status = if !notices.is_empty() {
                    ExternalWaitStatus::Notices
                } else if self.external_lead_attached() {
                    ExternalWaitStatus::Timeout
                } else {
                    ExternalWaitStatus::Detached
                };
                return ExternalWait { notices, status };
            }
        }
    }

    /// A short board, workers and tiers line for the external lead's session
    /// start.
    pub async fn external_summary(&self) -> String {
        let (running, waiting, merged, total) = {
            let workers = self.records.lock().await;
            (
                ledger::live_turns(&workers),
                workers
                    .iter()
                    .filter(|w| {
                        !audit_finished(w)
                            && matches!(
                                w.status,
                                WorkerStatus::Completed
                                    | WorkerStatus::InReview
                                    | WorkerStatus::NeedsYou
                                    | WorkerStatus::Failed
                                    | WorkerStatus::Conflict
                                    | WorkerStatus::Interrupted
                            )
                    })
                    .count(),
                workers.iter().filter(|w| ledger::merged(w)).count(),
                workers.len(),
            )
        };
        let mut summary = format!(
            "Mnemo2 team: {running} worker(s) running, {waiting} waiting on you (review, blocked or failed), {merged} merged, {total} total."
        );
        let plans: Vec<String> = self
            .board()
            .and_then(|board| board.list().ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|plan| plan.status == BoardStatus::Open)
            .map(|plan| {
                let done = plan.steps.iter().filter(|step| step.done).count();
                format!("{} \"{}\" ({done}/{} steps done)", plan.id, plan.title, plan.steps.len())
            })
            .collect();
        if plans.is_empty() {
            summary.push_str(" No open plans.");
        } else {
            summary.push_str(&format!(" Open plans: {}.", plans.join("; ")));
        }
        if let Some(tiers) = self.tier_guidance() {
            summary.push_str(&format!(" Tiers - {}", tiers.replace('\n', "; ")));
        }
        summary
    }
}
