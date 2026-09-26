// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Event forwarder — Rust → frontend bridge.
//!
//! Owns the fan-in receiver directly (no manager lock while waiting on
//! `recv()`), reads agent events, and emits Tauri events. For
//! `ApprovalRequest`, the oneshot sender is stored in the pending-approvals
//! map before emitting the serializable event.
//!
//! It also tracks agent running state: on `Started` it marks the agent
//! running, and on `Finished` **or a final (non-retrying) `Error`** it marks
//! it not running (the agent is idle but still alive — it can accept new
//! prompts). The agent is only removed from the manager on `Exited`, which
//! fires when the agent task truly terminates (inbox closed or cancelled).
//! Removing on `Finished` would kill the agent after every turn, blocking
//! follow-up prompts (e.g. starting a new task after the workflow reaches
//! Complete). For a tool-spawned child, the not-running mark happens
//! synchronously BEFORE the completion notification is sent to the parent
//! (backlog 2c406d72 — see [`notify_parent_on_completion`]): the parent's
//! descendant-gate consult must never see a verifiably-finished child as
//! still running.
//!
//! ## Run-All terminal exclusivity
//!
//! A turn must resolve the backlog **at most once**. Some failure paths
//! historically emitted a final `Error { retrying: false }` *and then*
//! `Finished` — without a latch both resolutions would run (the old
//! contract rolled back on Error and then commit-successed on Finished;
//! today the failure resolution would take the non-closure disposition
//! and the success resolution would then act on stale state).
//! [`TurnResolveLatch`] ensures only the first terminal outcome wins.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

use mnemo::runtime::channels::SerializableAgentEvent;
use mnemo::runtime::turn_resolve::{
    completion_suggestion_text, reviewer_failure_suggestion_text, workflow_transition_counts,
    ResolveAction, TurnResolveLatch,
};
use mnemo::runtime::{AgentCommand, AgentEvent, AgentId, AgentManager, SerializedEvent};
use mnemo::workflow::WorkflowState;

use crate::ipc::approval::PendingApprovals;
use crate::ipc::state::IpcState;

/// The Tauri event channel name for agent events.
pub const AGENT_EVENT_CHANNEL: &str = "agent://event";

/// A short activity note for the hang watchdog's ring, for state-changing
/// events only. Per-chunk streaming events (`TextDelta`, `ToolCallArgDelta`,
/// `Usage`, …) are skipped — a note per chunk would drown the 64-entry ring
/// and add a mutex push per token on the hot path.
fn watchdog_note(agent_id: AgentId, event: &SerializableAgentEvent) -> Option<String> {
    let note = match event {
        SerializableAgentEvent::Started => format!("agent{agent_id} Started"),
        SerializableAgentEvent::Finished { .. } => format!("agent{agent_id} Finished"),
        SerializableAgentEvent::Error { retrying, .. } if *retrying => {
            format!("agent{agent_id} Error(retrying)")
        }
        SerializableAgentEvent::Error { .. } => format!("agent{agent_id} Error(final)"),
        SerializableAgentEvent::WorkflowStateChanged { state, .. } => {
            format!("agent{agent_id} workflow → {state}")
        }
        SerializableAgentEvent::ApprovalRequest { tool_name, .. } => {
            format!("agent{agent_id} approval: {tool_name}")
        }
        SerializableAgentEvent::UserQuestion { .. } => format!("agent{agent_id} UserQuestion"),
        SerializableAgentEvent::PromptDispatched { .. } => {
            format!("agent{agent_id} PromptDispatched")
        }
        SerializableAgentEvent::SkillStarted { name, .. } => {
            format!("agent{agent_id} skill: {name}")
        }
        SerializableAgentEvent::VisionDescribe { index, total, .. } => {
            format!("agent{agent_id} VisionDescribe image {index}/{total}")
        }
        SerializableAgentEvent::VisionDescribed {
            index,
            total,
            success,
            ..
        } => {
            format!(
                "agent{agent_id} VisionDescribed image {index}/{total} ({})",
                if *success { "ok" } else { "failed" }
            )
        }
        SerializableAgentEvent::Exited => format!("agent{agent_id} Exited"),
        // Pre-stall evidence (backlog 5c33e945, 2027-01-07): the ring must
        // show WHY the agent parked — an interrupt (user stop) must be
        // distinguishable from a budget-exhausted stall when diagnosing a
        // live "needed manual c" incident.
        SerializableAgentEvent::Parked {
            reason,
            workflow_state,
            descendants_running,
            auto_continue_streak,
        } => format!(
            "agent{agent_id} parked: {reason:?} (wf={workflow_state}, desc={descendants_running}, streak={auto_continue_streak})"
        ),
        _ => return None,
    };
    Some(note)
}

/// How long a delta batch may sit before it is flushed even if no structural
/// event arrives. 16 ms ≈ one 60 Hz frame — the frontend already rAF-buffers
/// deltas (see `useAgentEvents.ts`), so this adds no perceptible latency while
/// cutting the per-token IPC/WebView2 chatter that drives the inbound SEND
/// behind the tao keyboard deadlock (AppHangB1, 2026-08-20).
///
/// This is a MAXIMUM batch age, enforced by a deadline armed when a batch's
/// first delta lands and never reset by later deltas (see
/// [`recv_until_deadline`]) — a sustained stream flushes every interval too.
const DELTA_FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// Per-bucket byte cap: a bucket that grows past this flushes immediately
/// (mirrors the frontend's 64 KiB synchronous-flush cap, so a long hidden
/// period can never accumulate an unbounded batch).
const DELTA_BUCKET_CAP: usize = 64 * 1024;

/// A per-(agent, kind) delta bucket. `ToolCallArgDelta` buckets are keyed by
/// the tool-call index so fragments for different calls never interleave.
#[derive(Debug, Default)]
struct DeltaBucket {
    text: String,
    bytes: usize,
}

/// Rust-side coalescing for the agent event forwarder.
///
/// The agent loop emits one `TextDelta` / `ReasoningDelta` / `ToolCallArgDelta`
/// per token; the forwarder previously forwarded each as its own Tauri event,
/// so a streaming turn produced hundreds of IPC + WebView2 round-trips per
/// second. The frontend already rAF-buffers these deltas (hybrid flush policy,
/// `useAgentEvents.ts`), so batching them on the Rust side is invisible to the
/// UI while cutting the emit volume that drives the inbound SEND behind the
/// tao keyboard self-deadlock (AppHangB1, 2026-08-20) and the posted-queue
/// backlog symptom.
///
/// Semantics:
/// - Deltas accumulate per `(agent_id, kind)` — and per tool-call index for
///   `ToolCallArgDelta` — and are emitted as a single concatenated event per
///   bucket on flush.
/// - A flush is triggered by (a) a non-delta event for the same agent
///   (structural flush — order is preserved: the deltas are emitted before the
///   structural event), (b) a 16 ms timer, or (c) a per-bucket 64 KiB cap.
/// - `ToolOutputDelta` (a running tool's live output) is deliberately NOT
///   bucketed: the tool that owns the child already throttles its own stream
///   (>= 60 ms between chunks, ~16/s), so it rides the structural pass-through
///   below — in order, behind any pending text deltas for that agent.
/// - Deltas for OTHER agents are never flushed by an unrelated event — each
///   agent's stream stays independent.
/// - `flush` returns the events to emit; it never emits itself (pure, so the
///   batching logic is unit-testable without a Tauri handle).
#[derive(Debug, Default)]
struct DeltaBatcher {
    buckets: HashMap<(AgentId, DeltaKind, u32), DeltaBucket>,
}

/// Which delta stream a bucket belongs to. `index` is the tool-call index for
/// `ToolCallArgDelta` and 0 otherwise. Variant order = natural stream order
/// (reasoning phase precedes the answer text, tool-call args stream with it),
/// used to sort buckets at flush time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum DeltaKind {
    Reasoning,
    Text,
    ToolCallArg,
}

impl DeltaBatcher {
    /// Feed one event. Returns `Some(events)` when the event itself must be
    /// emitted (non-delta events, possibly preceded by a structural flush of
    /// this agent's pending deltas), or `None` when the event was absorbed
    /// into a bucket.
    fn push(
        &mut self,
        agent_id: AgentId,
        event: SerializableAgentEvent,
    ) -> Option<Vec<SerializableAgentEvent>> {
        match event {
            SerializableAgentEvent::TextDelta { text } => {
                self.push_delta(agent_id, DeltaKind::Text, 0, text)
            }
            SerializableAgentEvent::ReasoningDelta { text } => {
                self.push_delta(agent_id, DeltaKind::Reasoning, 0, text)
            }
            SerializableAgentEvent::ToolCallArgDelta { index, fragment } => {
                self.push_delta(agent_id, DeltaKind::ToolCallArg, index, fragment)
            }
            other => {
                // Structural event: flush this agent's pending deltas first so
                // ordering is preserved, then pass the event through.
                let mut out = self.flush_agent(agent_id);
                out.push(other);
                Some(out)
            }
        }
    }

    fn push_delta(
        &mut self,
        agent_id: AgentId,
        kind: DeltaKind,
        index: u32,
        fragment: String,
    ) -> Option<Vec<SerializableAgentEvent>> {
        let bucket = self
            .buckets
            .entry((agent_id, kind, index))
            .or_insert_with(DeltaBucket::default);
        bucket.bytes += fragment.len();
        bucket.text.push_str(&fragment);
        if bucket.bytes >= DELTA_BUCKET_CAP {
            Some(self.flush_agent(agent_id))
        } else {
            None
        }
    }

    /// Flush every pending bucket for `agent_id`, oldest bucket first, as
    /// concatenated delta events. Returns the events to emit (empty when
    /// nothing was pending).
    fn flush_agent(&mut self, agent_id: AgentId) -> Vec<SerializableAgentEvent> {
        let mut out = Vec::new();
        let mut keys: Vec<(AgentId, DeltaKind, u32)> = self
            .buckets
            .keys()
            .copied()
            .filter(|(id, _, _)| *id == agent_id)
            .collect();
        keys.sort_by_key(|(_, kind, index)| (*kind as u8, *index));
        for key in keys {
            if let Some(bucket) = self.buckets.remove(&key) {
                if bucket.text.is_empty() {
                    continue;
                }
                let event = match key.1 {
                    DeltaKind::Text => SerializableAgentEvent::TextDelta { text: bucket.text },
                    DeltaKind::Reasoning => {
                        SerializableAgentEvent::ReasoningDelta { text: bucket.text }
                    }
                    DeltaKind::ToolCallArg => SerializableAgentEvent::ToolCallArgDelta {
                        index: key.2,
                        fragment: bucket.text,
                    },
                };
                out.push(event);
            }
        }
        out
    }

    /// Flush every pending bucket (timer tick). Returns `(agent_id, event)`
    /// pairs to emit.
    fn flush_all(&mut self) -> Vec<(AgentId, SerializableAgentEvent)> {
        let mut out = Vec::new();
        let mut agents: Vec<AgentId> = self.buckets.keys().map(|(id, _, _)| *id).collect();
        agents.sort_unstable();
        agents.dedup();
        for agent in agents {
            for ev in self.flush_agent(agent) {
                out.push((agent, ev));
            }
        }
        out
    }

    /// Whether any bucket is pending (drives the flush timer).
    fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
}

/// Outcome of one [`recv_until_deadline`] step.
#[derive(Debug)]
enum RecvOutcome<T> {
    /// An event arrived on the channel.
    Event(T),
    /// The fixed flush deadline passed while deltas were pending.
    Deadline,
    /// All channel senders dropped.
    Closed,
}

/// Receive the next fan-in event, racing it against the pending delta
/// batch's FIXED flush deadline.
///
/// The deadline is an argument (not an internally constructed
/// `sleep(interval)`) so the caller arms it once when a batch's first delta
/// lands and then passes the SAME `Instant` on every subsequent call until
/// it fires. Re-creating `sleep(now + interval)` per call would reset the
/// deadline on every received event — a sustained stream faster than one
/// event per interval would starve the flush indefinitely (review HIGH-1,
/// 2026-09-05). Re-creating `sleep_until(deadline)` with an unchanged
/// deadline is equivalent to pinning one sleep across calls: it fires at
/// the original instant no matter how many events arrive in between.
async fn recv_until_deadline<T>(
    rx: &mut mpsc::Receiver<T>,
    deadline: Option<tokio::time::Instant>,
) -> RecvOutcome<T> {
    let Some(deadline) = deadline else {
        // Nothing pending — plain receive; the caller keeps the deadline
        // disarmed while the batcher is empty.
        return rx
            .recv()
            .await
            .map_or(RecvOutcome::Closed, RecvOutcome::Event);
    };
    let sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(sleep);
    tokio::select! {
        r = rx.recv() => r.map_or(RecvOutcome::Closed, RecvOutcome::Event),
        _ = &mut sleep => RecvOutcome::Deadline,
    }
}

/// Spawn the event forwarder: takes ownership of the fan-in receiver so it
/// can read events without holding the manager lock (which would deadlock
/// Tauri commands while waiting for the next event).
///
/// `manager_arc` is cloned in so the forwarder can briefly lock it to update
/// running state + remove dead agents on `Started`/`Finished` events.
/// `agent_loops` is cloned in so the forwarder can remove a dead agent's loop
/// from the per-agent map on `Exited` (keeping the map in sync with the
/// manager).
pub fn spawn(
    app: AppHandle,
    fanin_rx: mpsc::Receiver<(AgentId, AgentEvent)>,
    manager_arc: Arc<Mutex<AgentManager>>,
    pending_approvals: Arc<PendingApprovals>,
    pending_questions: Arc<crate::ipc::questions::PendingQuestions>,
    agent_loops: crate::ipc::state::AgentLoopMap,
) {
    let mut rx = fanin_rx;
    // Background agents we've already sent a completion notification for. A
    // tool-spawned agent that runs multiple turns would otherwise notify its
    // parent on every `Finished`; we only want the first (its task is done).
    let mut notified_children: std::collections::HashSet<AgentId> =
        std::collections::HashSet::new();
    // Each agent's workflow state the last time we saw a WorkflowStateChanged
    // event. Used to detect a fresh plan started from Complete (Complete →
    // Executing), which triggers cleanup of inactive subagents.
    let mut prev_workflow_state: std::collections::HashMap<AgentId, WorkflowState> =
        std::collections::HashMap::new();
    // Each agent's top plan id the last time we saw a WorkflowStateChanged
    // event (backlog bba2c82d). At a root-plan abandonment the event's own
    // top_plan_id is the POST-pop top (None) and the live
    // main_agent_top_plan_id no longer names the abandoned plan either —
    // so the PREVIOUS tracked top is the abandoned plan's id, the linkage
    // evidence the Failed-transition guard needs.
    let mut prev_top_plan_id: std::collections::HashMap<AgentId, Option<String>> =
        std::collections::HashMap::new();

    // Run-All / auto-feed must not double-resolve when a turn emits both a
    // final Error and a Finished (or two terminal events of any kind).
    let mut turn_resolve = TurnResolveLatch::default();

    // Agents with a compaction in flight (CompactStarted seen, not yet
    // paired). The pairing contract (agent.rs compact_context): every
    // CompactStarted is followed by exactly one Compacted (completion —
    // even a no-op) or Error (failure). Used to resolve the Run-All
    // between-items auto-compact wait on BOTH outcomes — a failed
    // compaction must not burn the wait timeout.
    let mut compact_in_flight: std::collections::HashSet<AgentId> =
        std::collections::HashSet::new();

    // Rust-side delta coalescing: per-token TextDelta/ReasoningDelta/
    // ToolCallArgDelta are batched and emitted as concatenated events on a
    // 16 ms max-age deadline / structural event / 64 KiB cap (see
    // DeltaBatcher). Cuts the per-token IPC/WebView2 chatter behind the tao
    // keyboard deadlock (AppHangB1, 2026-08-20) and the posted-queue backlog
    // symptom.
    let mut delta_batcher = DeltaBatcher::default();

    // Flush deadline for the pending delta batch. Armed ONCE when the first
    // delta of a batch lands and never reset by later events, so even a
    // sustained stream (>1 event per interval) still hits the deadline and
    // flushes within one interval. (An inline `sleep(DELTA_FLUSH_INTERVAL)`
    // in the select re-armed the timer on every iteration — i.e. every
    // received event — starving the flush indefinitely under sustained
    // streams; review HIGH-1, 2026-09-05.)
    let mut flush_deadline: Option<tokio::time::Instant> = None;

    diag_line("mnemo[fwd] event forwarder task spawning");
    tauri::async_runtime::spawn(async move {
        diag_line("mnemo[fwd] event forwarder task RUNNING");
        let mut recv_count: usize = 0;
        loop {
            // Arm the flush deadline when the first delta of a batch lands;
            // disarm when nothing is pending (idle forwarder stays
            // zero-overhead — no armed timer, no tick).
            if delta_batcher.is_empty() {
                flush_deadline = None;
            } else if flush_deadline.is_none() {
                flush_deadline = Some(tokio::time::Instant::now() + DELTA_FLUSH_INTERVAL);
            }
            let recv_result = match recv_until_deadline(&mut rx, flush_deadline).await {
                RecvOutcome::Deadline => {
                    // The batch's fixed deadline passed — emit every pending
                    // bucket and go back to waiting.
                    for (aid, ev) in delta_batcher.flush_all() {
                        emit_payload(&app, aid, ev);
                    }
                    flush_deadline = None;
                    continue;
                }
                RecvOutcome::Event(ev) => Some(ev),
                RecvOutcome::Closed => None,
            };
            let Some((agent_id, event)) = recv_result else {
                // All agent senders dropped — flush any trailing batch
                // (abrupt teardown without a terminal structural event must
                // not drop the final tokens) and exit. Review LOW-1,
                // 2026-09-05.
                for (aid, ev) in delta_batcher.flush_all() {
                    emit_payload(&app, aid, ev);
                }
                break;
            };

            let SerializedEvent {
                event: serial,
                approval_sender,
                question_sender,
            } = event.into_serializable();

            // Chain marker: events reaching the forwarder at all (see
            // `diag_line`). Capped like the emit diagnostics.
            recv_count += 1;
            if recv_count <= EMIT_DIAG_LOG_LIMIT {
                let mut kind = format!("{serial:?}");
                kind.truncate(70);
                diag_line(&format!(
                    "mnemo[fwd] recv #{recv_count} agent={agent_id} {kind}"
                ));
            }

            // Feed the hang watchdog's activity ring on state-changing
            // events (cheap — the ring caps itself at 64 entries). The ring
            // is the watchdog's "what was the agent doing right before the
            // stall" evidence.
            if let Some(note) = watchdog_note(agent_id, &serial) {
                if let Some(watchdog) = &app.state::<IpcState>().watchdog {
                    watchdog.note(note);
                }
            }

            // Detect a fresh plan started from Complete: the agent's workflow
            // transitioned Complete → Executing (create_plan clears a finished
            // stack and enters Executing). On that transition, clean up
            // INACTIVE subagents — send each non-running subagent a Cancel so
            // it exits and its tab disappears. RUNNING subagents are never
            // touched (they finish on their own and are removed via Exited).
            if let SerializableAgentEvent::WorkflowStateChanged { state, top_plan_id } = &serial {
                let new_state = *state;
                let prev_state = prev_workflow_state.get(&agent_id).copied();
                let was_complete = prev_state == Some(WorkflowState::Complete);
                // (backlog bba2c82d) The abandoned plan's id comes from
                // the PREVIOUS top — read BEFORE the map update: after
                // the abandonment pop the event's own top_plan_id is the
                // post-pop top, and the live main_agent_top_plan_id no
                // longer names the abandoned plan either.
                let prev_top = prev_top_plan_id.get(&agent_id).cloned().flatten();
                prev_workflow_state.insert(agent_id, new_state);
                prev_top_plan_id.insert(agent_id, top_plan_id.clone());
                // Plan-loop gate evidence: only a REAL state transition
                // counts (a failed workflow tool can emit its unchanged
                // state — belt-and-braces behind the emission-side
                // result.success gating; see workflow_transition_counts).
                if workflow_transition_counts(prev_state, new_state) {
                    turn_resolve.note_workflow_changed(agent_id);
                }
                // Root-plan abandonment (backlog 45dcf577): latched
                // per-turn; the resolution paths turn it into the
                // dispatched item's `Failed` (failure means exactly "the
                // plan was abandoned"). The captured plan id (bba2c82d)
                // is the linkage evidence for the Failed guard.
                if crate::ipc::run_all::is_root_plan_abandonment(prev_state, new_state) {
                    turn_resolve.note_plan_abandoned(agent_id, prev_top);
                }
                // Stamp the in-flight backlog item `InFlight` when the MAIN
                // agent's workflow actually enters `Executing`: items leave
                // `Pending` on execution entry — never at dispatch — so a
                // pre-planning steer/interrupt leaves the item queued. Main
                // agent only: `run_all.current_item` / `single_in_flight`
                // are main-agent bookkeeping, and a child entering
                // `Executing` must not stamp the main's item while it is
                // still pre-planning.
                if crate::ipc::run_all::should_stamp_in_flight(prev_state, new_state) {
                    // Parallel run-all (plan ffd7a86f, review R1): route by
                    // OWNERSHIP, not by a live main_agent_id() — a spawned
                    // run-all agent is parentless, and after the main
                    // agent's exit the smallest parentless id is a spawned
                    // lane; routing its Executing entries through the main
                    // stamp would flip the requeued main item back to
                    // InFlight and link it to the lane's plan.
                    if crate::ipc::run_all::owns_spawned_run(&app, agent_id).await {
                        crate::ipc::run_all::stamp_spawned_in_flight(
                            &app,
                            agent_id,
                            top_plan_id.as_deref(),
                        )
                        .await;
                    } else {
                        let is_main = {
                            let mgr = manager_arc.lock().await;
                            mgr.main_agent_id() == Some(agent_id)
                        };
                        if is_main {
                            crate::ipc::run_all::stamp_backlog_in_flight(
                                &app,
                                top_plan_id.as_deref(),
                            )
                            .await;
                        }
                    }
                }
                if was_complete && new_state == WorkflowState::Executing {
                    cleanup_inactive_subagents(&manager_arc, agent_id).await;
                }
            }

            // Run-All auto-compact signal (`auto_compact_on_plan_complete`):
            // the between-items compaction task (run_all::compact_then_dispatch_next)
            // waits on the shared counter. Increment it when the MAIN agent's
            // compaction finishes — on Compacted (completion, even a no-op) or
            // on the Error paired with its CompactStarted (failure) — so both
            // outcomes resolve the wait immediately.
            match &serial {
                SerializableAgentEvent::CompactStarted => {
                    compact_in_flight.insert(agent_id);
                }
                SerializableAgentEvent::Compacted { .. } => {
                    if compact_in_flight.remove(&agent_id) {
                        let is_main =
                            manager_arc.lock().await.main_agent_id() == Some(agent_id);
                        if is_main {
                            app.state::<IpcState>()
                                .backlog
                                .compact_signal
                                .send_if_modified(|c| {
                            *c += 1;
                            true
                        });
                        }
                    }
                }
                SerializableAgentEvent::Error { .. } => {
                    if compact_in_flight.remove(&agent_id) {
                        let is_main =
                            manager_arc.lock().await.main_agent_id() == Some(agent_id);
                        if is_main {
                            app.state::<IpcState>()
                                .backlog
                                .compact_signal
                                .send_if_modified(|c| {
                            *c += 1;
                            true
                        });
                        }
                    }
                }
                _ => {}
            }

            // If a tool-spawned background agent (one with a recorded parent)
            // just finished its task — either cleanly (`Finished`) or with a
            // final, non-retrying `Error` — notify its parent agent so the
            // parent can combine the child's results into its plan. This is
            // the completion-notification feedback loop that lets an
            // orchestrating agent know when its spawned agents are done.
            match &serial {
                SerializableAgentEvent::Finished { .. } => {
                    // Only notify once per background agent (on its first
                    // finish) — a multi-turn child shouldn't spam its parent.
                    if notified_children.insert(agent_id) {
                        if let Some(ev) =
                            notify_parent_on_completion(&manager_arc, &agent_loops, agent_id, true)
                                .await
                        {
                            emit_child_finished(&app, ev);
                        }
                    }
                }
                SerializableAgentEvent::Error { retrying, .. } if !retrying => {
                    if notified_children.insert(agent_id) {
                        if let Some(ev) =
                            notify_parent_on_completion(&manager_arc, &agent_loops, agent_id, false)
                                .await
                        {
                            emit_child_finished(&app, ev);
                        }
                    }
                }
                _ => {}
            }

            // Track running state + clean up dead agents. We lock the manager
            // only briefly here (not while waiting on recv()), so Tauri
            // commands can still acquire it. Each arm takes the lock AT MOST
            // ONCE; delta events (TextDelta / ToolCallArgDelta / etc.) take it
            // ZERO times — they fall through to the emit with no lock.
            //
            // NOTE on the running-flag fast path: `AgentHandle.running` is a
            // bare `AtomicBool` owned inside the manager's map, not an
            // `Arc<AtomicBool>`, so the forwarder cannot hold a shared ref to
            // it without changing `channels.rs` (out of scope here). We
            // therefore keep a single lock per Started/Finished but never more.
            match &serial {
                SerializableAgentEvent::Started => {
                    // New turn — clear any stale failure latch and mark running.
                    turn_resolve.on_started(agent_id);
                    let mgr = manager_arc.lock().await;
                    mgr.set_running(agent_id, true);
                }
                SerializableAgentEvent::Error { retrying, error } if !retrying => {
                    // Final error ends the turn: agent is idle (same as
                    // Finished). Must flip running here — provider-exhaustion
                    // paths emit Error without Finished, and without this the
                    // manager would leave the agent "running" forever.
                    let mgr = manager_arc.lock().await;
                    mgr.set_running(agent_id, false);
                    let is_main = mgr.main_agent_id() == Some(agent_id);
                    // Match Finished: do not resolve Run-All while descendants
                    // are still running (parent may still be consolidating).
                    let descendants_running = mgr.has_running_descendants(agent_id);
                    drop(mgr);
                    pending_approvals.cleanup_for_agent(agent_id);
                    pending_questions.cleanup_for_agent(agent_id);
                    // Parallel run-all (plan ffd7a86f, review R1): route by
                    // OWNERSHIP, not by a live main_agent_id() — spawned
                    // run-all agents are parentless, and after the main
                    // agent's exit the smallest parentless id is a spawned
                    // lane; routing its events through the main path
                    // cross-stamps the requeued main item and strands the
                    // lane. A spawned-run owner is never the main lane.
                    if crate::ipc::run_all::owns_spawned_run(&app, agent_id).await {
                        let note = if error.is_empty() {
                            "agent turn ended in an error".to_string()
                        } else {
                            error.clone()
                        };
                        match turn_resolve.on_final_error(agent_id, note, descendants_running) {
                            ResolveAction::Failure(n) => {
                                // Off the forwarder's critical path (review
                                // L2): evidence is read BEFORE the spawn
                                // (the next Started clears it); the
                                // continuation runs in its own task.
                                let loop_evidence = turn_resolve.workflow_changed(agent_id);
                                let plan_abandoned = turn_resolve.plan_abandoned(agent_id);
                                let abandoned_plan_id = turn_resolve
                                    .abandoned_plan_id(agent_id)
                                    .map(str::to_string);
                                let app = app.clone();
                                spawn_resolution_continuation(async move {
                                    crate::ipc::run_all::on_spawned_turn_resolved(
                                        &app,
                                        agent_id,
                                        false,
                                        Some(n),
                                        loop_evidence,
                                        plan_abandoned,
                                        abandoned_plan_id.as_deref(),
                                    )
                                    .await;
                                });
                            }
                            ResolveAction::Success | ResolveAction::None => {}
                        }
                    } else if is_main {
                        let note = if error.is_empty() {
                            "agent turn ended in an error".to_string()
                        } else {
                            error.clone()
                        };
                        match turn_resolve.on_final_error(agent_id, note, descendants_running) {
                            ResolveAction::Failure(n) => {
                                // Off the forwarder's critical path (review
                                // L2): evidence is read BEFORE the spawn
                                // (the next Started clears it); the
                                // continuation runs in its own task.
                                let loop_evidence = turn_resolve.workflow_changed(agent_id);
                                let plan_abandoned = turn_resolve.plan_abandoned(agent_id);
                                let abandoned_plan_id = turn_resolve
                                    .abandoned_plan_id(agent_id)
                                    .map(str::to_string);
                                let app = app.clone();
                                spawn_resolution_continuation(async move {
                                    crate::ipc::run_all::on_main_turn_resolved(
                                        &app,
                                        false,
                                        Some(n),
                                        loop_evidence,
                                        plan_abandoned,
                                        abandoned_plan_id.as_deref(),
                                    )
                                    .await;
                                });
                            }
                            ResolveAction::Success | ResolveAction::None => {}
                        }
                    }
                }
                SerializableAgentEvent::Finished { .. } => {
                    // The turn ended — the agent is idle but still alive and
                    // ready for the next prompt. Do NOT remove it; that would
                    // close the command channel and kill the task. A SINGLE
                    // lock covers the flag flip + both structural reads.
                    let mgr = manager_arc.lock().await;
                    mgr.set_running(agent_id, false);
                    let is_main = mgr.main_agent_id() == Some(agent_id);
                    let descendants_running = mgr.has_running_descendants(agent_id);
                    drop(mgr);
                    // Drop stale pending approvals for this agent (none should
                    // remain after a clean finish, but clean up defensively).
                    pending_approvals.cleanup_for_agent(agent_id);
                    pending_questions.cleanup_for_agent(agent_id);
                    // Backlog: resolve once when MAIN is idle and no descendants.
                    // Prior final Error (possibly deferred) wins over success.
                    // Parallel run-all (plan ffd7a86f, review R1): route by
                    // OWNERSHIP, not by a live main_agent_id() — spawned
                    // run-all agents are parentless, and after the main
                    // agent's exit the smallest parentless id is a spawned
                    // lane; routing its events through the main path
                    // cross-stamps the requeued main item and strands the
                    // lane. A spawned-run owner is never the main lane.
                    if crate::ipc::run_all::owns_spawned_run(&app, agent_id).await {
                        // A spawned worktree agent's turn resolved — resolve
                        // ITS item under the same once-when-idle latch (never
                        // the main agent's). Spawned agents are parentless, so
                        // the deferred-main-failure flush does not apply.
                        match turn_resolve.on_finished(agent_id, descendants_running) {
                            ResolveAction::Success => {
                                // Off the forwarder's critical path (review
                                // L2): evidence is read BEFORE the spawn
                                // (the next Started clears it); the
                                // continuation runs in its own task.
                                let loop_evidence = turn_resolve.workflow_changed(agent_id);
                                let plan_abandoned = turn_resolve.plan_abandoned(agent_id);
                                let abandoned_plan_id = turn_resolve
                                    .abandoned_plan_id(agent_id)
                                    .map(str::to_string);
                                let app = app.clone();
                                spawn_resolution_continuation(async move {
                                    crate::ipc::run_all::on_spawned_turn_resolved(
                                        &app,
                                        agent_id,
                                        true,
                                        None,
                                        loop_evidence,
                                        plan_abandoned,
                                        abandoned_plan_id.as_deref(),
                                    )
                                    .await;
                                });
                            }
                            ResolveAction::Failure(n) => {
                                // Off the forwarder's critical path (review
                                // L2): evidence is read BEFORE the spawn
                                // (the next Started clears it); the
                                // continuation runs in its own task.
                                let loop_evidence = turn_resolve.workflow_changed(agent_id);
                                let plan_abandoned = turn_resolve.plan_abandoned(agent_id);
                                let abandoned_plan_id = turn_resolve
                                    .abandoned_plan_id(agent_id)
                                    .map(str::to_string);
                                let app = app.clone();
                                spawn_resolution_continuation(async move {
                                    crate::ipc::run_all::on_spawned_turn_resolved(
                                        &app,
                                        agent_id,
                                        false,
                                        Some(n),
                                        loop_evidence,
                                        plan_abandoned,
                                        abandoned_plan_id.as_deref(),
                                    )
                                    .await;
                                });
                            }
                            ResolveAction::None => {}
                        }
                    } else if is_main {
                        match turn_resolve.on_finished(agent_id, descendants_running) {
                            ResolveAction::Success => {
                                // Off the forwarder's critical path (review
                                // L2): evidence is read BEFORE the spawn
                                // (the next Started clears it); the
                                // continuation runs in its own task.
                                let loop_evidence = turn_resolve.workflow_changed(agent_id);
                                let plan_abandoned = turn_resolve.plan_abandoned(agent_id);
                                let abandoned_plan_id = turn_resolve
                                    .abandoned_plan_id(agent_id)
                                    .map(str::to_string);
                                let app = app.clone();
                                spawn_resolution_continuation(async move {
                                    crate::ipc::run_all::on_main_turn_resolved(
                                        &app,
                                        true,
                                        None,
                                        loop_evidence,
                                        plan_abandoned,
                                        abandoned_plan_id.as_deref(),
                                    )
                                    .await;
                                });
                            }
                            ResolveAction::Failure(n) => {
                                // Off the forwarder's critical path (review
                                // L2): evidence is read BEFORE the spawn
                                // (the next Started clears it); the
                                // continuation runs in its own task.
                                let loop_evidence = turn_resolve.workflow_changed(agent_id);
                                let plan_abandoned = turn_resolve.plan_abandoned(agent_id);
                                let abandoned_plan_id = turn_resolve
                                    .abandoned_plan_id(agent_id)
                                    .map(str::to_string);
                                let app = app.clone();
                                spawn_resolution_continuation(async move {
                                    crate::ipc::run_all::on_main_turn_resolved(
                                        &app,
                                        false,
                                        Some(n),
                                        loop_evidence,
                                        plan_abandoned,
                                        abandoned_plan_id.as_deref(),
                                    )
                                    .await;
                                });
                            }
                            ResolveAction::None => {}
                        }
                    } else {
                        // A child finished — if main already failed while we
                        // were still out, deliver the deferred failure now.
                        // The spawned-lane flush runs FIRST (review R3-L1):
                        // post-main-exit main_agent_id() resolves to a lane,
                        // and the main flush would mis-consume a lane's
                        // latch and silently discard its disposition.
                        try_flush_deferred_spawned_resolutions(
                            &app,
                            &manager_arc,
                            &mut turn_resolve,
                        )
                        .await;
                        try_flush_deferred_main_resolution(&app, &manager_arc, &mut turn_resolve)
                            .await;
                    }
                }
                SerializableAgentEvent::Exited => {
                    // The agent task truly terminated (inbox closed or
                    // cancelled). Remove it from the manager + the per-agent
                    // loops map so both stay in sync and don't leak. The old
                    // redundant `set_running(false)` before `remove` is dropped
                    // (one fewer atomic write + it was unobservable: the handle
                    // is destroyed by `remove`). ONE lock for the removal.
                    // Parallel run-all (plan ffd7a86f, review R1): route by
                    // OWNERSHIP — a spawned lane is never the main lane,
                    // even when it now holds the smallest parentless id
                    // (after the main agent's exit, main_agent_id()
                    // resolves to a spawned lane; routing its exit through
                    // the main drain would re-drain the main's
                    // already-cleared pointer).
                    let owns_spawned =
                        crate::ipc::run_all::owns_spawned_run(&app, agent_id).await;
                    let mut mgr = manager_arc.lock().await;
                    let was_main =
                        !owns_spawned && mgr.main_agent_id() == Some(agent_id);
                    mgr.remove(agent_id);
                    drop(mgr);
                    agent_loops.lock().await.remove(&agent_id);
                    pending_approvals.cleanup_for_agent(agent_id);
                    pending_questions.cleanup_for_agent(agent_id);
                    // Drop the dedup + prev-workflow-state entries so a long
                    // session with many spawns doesn't grow them unboundedly.
                    notified_children.remove(&agent_id);
                    prev_workflow_state.remove(&agent_id);
                    prev_top_plan_id.remove(&agent_id);
                    compact_in_flight.remove(&agent_id);
                    // Backlog ffd4bac3: drop the tracked usage with the
                    // agent so the map doesn't leak across a long session.
                    app.state::<IpcState>()
                        .runtime
                        .context_usage
                        .lock()
                        .await
                        .remove(&agent_id);
                    turn_resolve.on_exited(agent_id);
                    // Child exit may clear the last running descendant.
                    // The spawned-lane flush runs FIRST (review R3-L1):
                    // post-main-exit main_agent_id() resolves to a lane,
                    // and the main flush would mis-consume a lane's latch
                    // and silently discard its disposition.
                    try_flush_deferred_spawned_resolutions(
                        &app,
                        &manager_arc,
                        &mut turn_resolve,
                    )
                    .await;
                    try_flush_deferred_main_resolution(&app, &manager_arc, &mut turn_resolve).await;
                    // Backlog 45dcf577 (review LOW-2): a MAIN-agent exit while
                    // a Run-All is active (cancel/crash — e.g. while blocked on
                    // an approval halt, whose kept run state expects a later
                    // turn resolution) bypasses every designed drain path and
                    // would strand the run state ("run-all is already active",
                    // a stuck UI, an InFlight item with no UI recovery). Drain
                    // it: the item is requeued to Pending — unless the work
                    // already landed (the done-orphan guard, 6c6966b9: plan
                    // complete + commits after the pre-item checkpoint →
                    // auto-resolves Done) — (the queue is its
                    // recovery — run-all orphans, 2027-01-07). Also clear the
                    // single-dispatch pointer: a stale single_in_flight
                    // misattributes the next agent's first turn resolution
                    // (the plan gate would resolve the dead item against the
                    // new agent's state) and blocks adoption of single-dispatch
                    // orphans.
                    if was_main {
                        crate::ipc::run_all::drain_run_all_on_main_exit(&app).await;
                        *app.state::<IpcState>()
                            .backlog
                            .single_in_flight
                            .lock()
                            .expect("single_in_flight lock poisoned") = None;
                    }
                    // Parallel run-all (plan ffd7a86f): a spawned worktree
                    // agent that exited WITHOUT a turn resolution (cancel or
                    // crash mid-item) — drain its item (requeue, or land +
                    // auto-Done when the work already landed). No-op when
                    // the agent owned no spawned run.
                    crate::ipc::run_all::drain_spawned_on_exit(&app, agent_id).await;
                }
                SerializableAgentEvent::ContextUsage { used, max, .. } => {
                    // Backlog ffd4bac3: track the last usage per agent —
                    // the app's only context-fill source. The run-all
                    // between-items auto-compact gate reads the main
                    // agent's entry to decide whether the post-item
                    // context is at/above the effective fill-rate
                    // threshold before compacting.
                    app.state::<IpcState>()
                        .runtime
                        .context_usage
                        .lock()
                        .await
                        .insert(agent_id, (u64::from(*used), u64::from(*max)));
                }
                _ => {}
            }

            // If this is an approval request, store the oneshot sender.
            if let Some(sender) = approval_sender {
                if let SerializableAgentEvent::ApprovalRequest {
                    ref tool_call_id,
                    ref tool_name,
                    ref args,
                    core_operation,
                    ..
                } = serial
                {
                    pending_approvals.insert(
                        agent_id,
                        tool_call_id.clone(),
                        sender,
                        tool_name.clone(),
                        args.clone(),
                        core_operation,
                    );
                    // Backlog Run-All: an approval request from the MAIN agent
                    // halts the loop (the user's safety choice is never
                    // auto-resolved — the run stops and waits for the user).
                    let is_main = {
                        let mgr = manager_arc.lock().await;
                        mgr.main_agent_id() == Some(agent_id)
                    };
                    if is_main {
                        crate::ipc::run_all::halt_run_all(
                            &app,
                            "approval requested during unattended run",
                            true,
                        )
                        .await;
                    }
                }
            }

            // If this is a user question, store the oneshot sender so the
            // `answer_question` command can resolve it. Mirrors the approval
            // path above.
            if let Some(sender) = question_sender {
                if let SerializableAgentEvent::UserQuestion {
                    ref question_id, ..
                } = serial
                {
                    pending_questions.insert(agent_id, question_id.clone(), sender);
                }
            }

            // Emit the serializable event to the frontend. Delta events are
            // absorbed into the batcher (emitted concatenated on the next
            // flush); structural events flush this agent's pending deltas
            // first (order preserved) and then pass through.
            if let Some(events) = delta_batcher.push(agent_id, serial) {
                for ev in events {
                    emit_payload(&app, agent_id, ev);
                }
            }
        }
    });
}

/// Emit one agent event payload on `AGENT_EVENT_CHANNEL`. Best-effort: a
/// failed emit is logged, not fatal.
///
/// # Delivery diagnostics
///
/// `Manager::emit` is a SILENT no-op for any webview that has no JS listener
/// registered for the channel: `emit_js_filter` looks the channel up in
/// `js_event_listeners[webview_label]` and simply skips the webview when the
/// entry is missing — returning `Ok(())`. So "the UI never updates while
/// `invoke` keeps working" produces no error anywhere: the forwarder emits
/// happily into a void (live 2026-09-08 — chat, reasoning and token counters
/// frozen from process start while polled panels stayed current).
///
/// The first few emits therefore log the webview labels the app knows about
/// plus the emit result, so one line of stderr distinguishes the three
/// candidates: the forwarder never emitting, `emit` returning `Err`, or
/// `emit` returning `Ok` with nobody listening. Capped at
/// `EMIT_DIAG_LOG_LIMIT` lines so a streaming turn never floods the console;
/// errors are always logged.
fn emit_payload(app: &AppHandle, agent_id: AgentId, event: SerializableAgentEvent) {
    let payload = AgentEventPayload { agent_id, event };
    let result = app.emit(AGENT_EVENT_CHANNEL, &payload);
    let n = EMITS_LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n < EMIT_DIAG_LOG_LIMIT {
        let labels: Vec<String> = app
            .webviews()
            .keys()
            .map(|label| label.to_string())
            .collect();
        diag_line(&format!(
            "mnemo[emit] #{} channel={} agent={} webviews={:?} result={}",
            n + 1,
            AGENT_EVENT_CHANNEL,
            agent_id,
            labels,
            match &result {
                Ok(()) => "ok".to_string(),
                Err(e) => format!("ERR({e})"),
            }
        ));
        if n == 0 {
            // First event: probe every webview through the same eval path
            // event delivery uses (see WEBVIEW_PROBE_JS).
            for (label, webview) in app.webviews() {
                match webview.eval(WEBVIEW_PROBE_JS) {
                    Ok(()) => diag_line(&format!("mnemo[probe] eval into '{label}' queued ok")),
                    Err(e) => {
                        diag_line(&format!("mnemo[probe] eval into '{label}' FAILED: {e}"))
                    }
                }
            }
        }
    }
    if let Err(e) = result {
        eprintln!("failed to emit agent event: {e}");
    }
}

/// A one-shot probe eval'd into every webview on the first agent event, to
/// test the EXACT mechanism Tauri event delivery uses.
///
/// `Webview::emit_js` delivers an event by eval'ing
/// `(function(){ const fn = window['<name>']; fn && fn({...}, ids) })()`
/// (tauri src/event/mod.rs:194). The `fn &&` guard means a MISSING global is
/// swallowed: no error, `emit` still returns `Ok(())`, and the UI simply
/// never updates. This probe reports back through `invoke` — which is known
/// to work when events are dead, since app-defined commands travel over the
/// custom IPC protocol rather than an eval — so one line of stderr says
/// whether eval-into-JS works at all and which `__TAURI*` globals exist.
const WEBVIEW_PROBE_JS: &str = r#"(function(){
  try {
    var g = Object.keys(window).filter(function(k){return k.indexOf('__TAURI')===0;});
    var rep = JSON.stringify({
      globals: g,
      hasInternals: !!window.__TAURI_INTERNALS__,
      hasEventInternals: !!window.__TAURI_EVENT_PLUGIN_INTERNALS__,
      href: String(location.href)
    });
    if (window.__TAURI_INTERNALS__ && window.__TAURI_INTERNALS__.invoke) {
      window.__TAURI_INTERNALS__.invoke('ui_diag', { msg: 'probe ' + rep });
    }
  } catch (e) { /* nothing we can do from here */ }
})()"#;

/// Append one diagnostic line to the emit-diag log (and stderr).
///
/// A release build is a GUI-subsystem binary and only attaches a console in
/// `--console` mode, so `eprintln!` diagnostics can vanish entirely — a
/// terminal launch with `2> file` produced an empty file (live 2026-09-08).
/// The file sink is therefore the primary channel for delivery diagnostics;
/// stderr is kept as a bonus for the console/dev paths. Best-effort: a
/// diagnostic that cannot be written must never affect the app.
///
/// The sink is resolved once by [`diag_sink`]: the project-local
/// `.coding/logs/emit-diag.log` when a `.coding` side-car exists under the
/// current working directory (dev launches), else
/// `std::env::temp_dir().join("mnemo-emit-diag.log")` — packaged builds run
/// with the exe dir (or `/`) as CWD, where a relative `.coding/logs/...`
/// either is not writable or lands where nobody looks; the temp dir is
/// the same anchor the hang watchdog uses for its reports (quality review
/// Q2, 2026-09-08).
pub(crate) fn diag_line(line: &str) {
    // NEVER `eprintln!` here: it PANICS when the stderr write fails
    // ("failed printing to stderr"), and a release build is a GUI-subsystem
    // binary whose std handles can be missing or invalid (`attach_console`
    // only runs in `--console` mode). A panic on the forwarder task would
    // kill event delivery outright — the very blackout this line exists to
    // diagnose. Write and swallow instead.
    {
        use std::io::Write;
        let _ = writeln!(std::io::stderr(), "{line}");
    }
    let path = diag_sink();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// The resolved file sink for [`diag_line`] (see there for the rationale).
static DIAG_SINK: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Resolve the emit-diag log path once: `.coding/logs/emit-diag.log` when a
/// `.coding` side-car exists under the current working directory (dev
/// launches — every existing reference keeps working), else
/// `std::env::temp_dir().join("mnemo-emit-diag.log")` for packaged builds
/// (the hang watchdog's anchoring pattern).
fn diag_sink() -> &'static std::path::Path {
    DIAG_SINK.get_or_init(|| {
        if std::path::Path::new(".coding").exists() {
            std::path::PathBuf::from(".coding/logs/emit-diag.log")
        } else {
            std::env::temp_dir().join("mnemo-emit-diag.log")
        }
    })
}

/// How many emits log a delivery-diagnostic line (see [`emit_payload`]).
const EMIT_DIAG_LOG_LIMIT: usize = 5;

/// Emits logged so far — drives the [`EMIT_DIAG_LOG_LIMIT`] cap.
static EMITS_LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// If the agent that just finished (`agent_id`) is a tool-spawned background
/// agent (it has a recorded `parent_id`), send a completion `Suggestion` to
/// its parent so the parent reads the child's report and combines the results
/// into its plan, and return a `ChildFinished` event for the frontend to mark
/// the child "done" in the sidebar. Best-effort: the manager lock is held only
/// briefly, and if the parent is gone (already removed) the notification is
/// silently dropped. Returns `None` when the agent has no parent (a normal
/// main/UI-agent turn), so ordinary turns don't emit spurious events.
/// `success` reflects whether the child finished cleanly or with a final error.
///
/// # Tracker invariant (backlog 2c406d72)
///
/// This function deregisters the finishing agent from the descendant tracker
/// (`set_running(agent_id, false)`) BEFORE the notification is sent: the
/// completion `Suggestion` is the parent's signal that the child is done, and
/// the tracker must already agree by the time the parent can observe that
/// signal. The old order — notify here, clear the flag later in the
/// forwarder's separate running-state match — left a window (the
/// `ChildFinished` webview emit plus a manager-lock acquisition) in which the
/// parent could wake on the `Suggestion`, run a whole turn, and hit the
/// descendant gate (`has_running_descendants`) while the child was still
/// marked running: live-observed as `complete_step` being refused for a full
/// turn after every child had verifiably ended (2026-12-30 session, plan
/// 72329f2c). Clearing first makes the notification and the tracker agree by
/// construction. The running-state match still clears the flag afterwards —
/// idempotent, and it keeps that arm's single-lock structure intact.
///
/// When the child recorded a review-report path (via `write_review_report`),
/// the path is included in the Suggestion text so the parent can read the
/// report directly — instead of a generic "read its report" message that
/// forces the parent to search for the file (unreliable with multiple
/// concurrent reviewers). `agent_loops` is consulted to read the child's
/// `last_review_report`; the tokio lock is held only briefly (the read clones
/// a `String` off the child's `AgentLoop` and never awaits while held).
async fn notify_parent_on_completion(
    manager_arc: &Arc<Mutex<AgentManager>>,
    agent_loops: &crate::ipc::state::AgentLoopMap,
    agent_id: AgentId,
    success: bool,
) -> Option<SerializableAgentEvent> {
    // Deregister from the descendant tracker FIRST (backlog 2c406d72): the
    // Suggestion below wakes the parent immediately, and its next
    // dispatch-cycle gate consult must not see this child as running. This
    // also covers parentless agents (main/UI turns) harmlessly — the
    // forwarder's running-state match clears them in the same iteration
    // anyway.
    manager_arc.lock().await.set_running(agent_id, false);
    // Read the parent id + child name + child role without holding the lock
    // across the send.
    let (parent_id, child_name, child_role) = {
        let mgr = manager_arc.lock().await;
        match mgr.parent_id(agent_id) {
            // Only notify for tool-spawned agents (those with a parent). A
            // normal turn by the main/UI agent has no parent, so this is None
            // and we skip — we don't want every finished turn to spam a parent.
            Some(pid) => {
                let name = mgr
                    .get(agent_id)
                    .map(|h| h.name.clone())
                    .unwrap_or_else(|| format!("agent-{agent_id}"));
                let role = mgr.role(agent_id);
                (pid, name, role)
            }
            None => return None,
        }
    };

    // Read the child's last review-report path (if it wrote one) so the
    // parent can read the report directly. The tokio lock is held only for
    // the read (which clones a String off the child's AgentLoop); it is never
    // held across an await, and `set_last_review_report` (the only other
    // acquirer of the child's std mutex, from turn.rs) does not take this
    // tokio lock — so there is no reverse-order acquisition / deadlock.
    let report_path: Option<String> = {
        let loops = agent_loops.lock().await;
        loops.get(&agent_id).map(|l| l.last_review_report())
    }
    .flatten();

    // Failed-reviewer protocol: a `role: "reviewer"` child that ended (failed
    // OR finished cleanly) with NO report did not review anything — the
    // parent must NOT assume the review is done and must NOT blindly respawn
    // the same reviewer (same model, same task → the same failure). Set the
    // parent's reviewer_failure_pending latch (denies further reviewer
    // spawns at dispatch until the parent asks the user) and use distinct
    // failure text that tells the parent to ask the user (retry on another
    // model / abandon the review — the main agent can never self-review).
    // Backlog 5b46674d: the !success gate is gone — a reviewer that FINISHES
    // report-less (live-observed 2026-12-30: two of five reviewers) is just
    // as report-less as a failed one.
    let reviewer_failed_no_report =
        child_role.as_deref() == Some("reviewer") && report_path.is_none();
    if reviewer_failed_no_report {
        if let Some(parent_loop) = {
            let loops = agent_loops.lock().await;
            loops.get(&parent_id).cloned()
        } {
            parent_loop.set_reviewer_failure_pending(true);
        }
    }

    let text = if reviewer_failed_no_report {
        reviewer_failure_suggestion_text(&child_name)
    } else {
        let outcome = if success { "finished" } else { "failed" };
        completion_suggestion_text(&child_name, outcome, report_path.as_deref())
    };

    // Best-effort send. The manager's `send` uses try_send; if the parent's
    // inbox is full or the parent is gone, `send` returns the command back —
    // we just drop it (the notification is a nicety, not a guarantee).
    let mgr = manager_arc.lock().await;
    let _ = mgr.send(parent_id, AgentCommand::Suggestion(text.into()));
    drop(mgr);

    Some(SerializableAgentEvent::ChildFinished {
        child_id: agent_id,
        name: child_name,
        success,
    })
}

/// Emit a `ChildFinished` event to the frontend, tagged with the child's id so
/// the sidebar can mark that background agent "done". Separate from the main
/// payload because this event is synthesized by the forwarder, not the agent.
fn emit_child_finished(app: &AppHandle, event: SerializableAgentEvent) {
    let child_id = match &event {
        SerializableAgentEvent::ChildFinished { child_id, .. } => *child_id,
        _ => return,
    };
    let payload = AgentEventPayload {
        agent_id: child_id,
        event,
    };
    if let Err(e) = app.emit(AGENT_EVENT_CHANNEL, &payload) {
        eprintln!("failed to emit child_finished event: {e}");
    }
}

/// Cancel every INACTIVE subagent registered with the manager, leaving the
/// given `trigger_agent` and any RUNNING subagent untouched.
///
/// Called when an agent starts a fresh plan from Complete (Complete →
/// Executing): stale background agents from the previous plan should be
/// cleaned up so their tabs disappear. A subagent is "inactive" when it has
/// started at least one turn and is not currently running — cancelling a
/// running subagent would kill a task still in flight, so those are left alone
/// (they exit on their own and are removed by the existing `Exited` forwarder
/// path), and a subagent that has never started is *pending*, not completed,
/// so it is left alone too.
///
/// Also called from `spawn_agent_shared` right before a new agent is spawned:
/// per the user's rule, completed (inactive) subagents are cleaned up only when
/// a new agent is spawned (or the user closes a tab) — never proactively. This
/// keeps finished subagent tabs around for review until the user moves on.
///
/// Each cancelled subagent emits `Finished` then `Exited`; the `Exited`
/// handler in the forwarder removes it from the manager + the per-agent loop
/// map and emits the event so the frontend drops its tab.
///
/// The snapshot + all the `Cancel` sends happen under a **single** manager
/// lock acquisition. `mgr.send` uses `try_send` (non-blocking), so this never
/// awaits while holding the lock. Holding the lock across the whole loop
/// prevents a concurrent `send_prompt` IPC command from enqueuing a `Prompt`
/// *between* the snapshot and the `Cancel` — the `Prompt` send needs the same
/// lock, so it blocks until every `Cancel` is already in the channel, landing
/// the `Cancel` ahead of any later `Prompt` in the FIFO. The agent task then
/// reads the `Cancel` first and exits without running an extra turn.
pub(crate) async fn cleanup_inactive_subagents(
    manager_arc: &Arc<Mutex<AgentManager>>,
    trigger_agent: AgentId,
) {
    let mgr = manager_arc.lock().await;
    let to_cancel: Vec<AgentId> = mgr
        .list()
        .iter()
        .filter(|h| {
            h.id != trigger_agent
                && h.parent_id.is_some()
                // "Inactive" means ran-and-went-idle, NOT never-started. A
                // subagent registered by an earlier `spawn_agent` in the SAME
                // assistant message has `running == false` until its first
                // `Started` event reaches the forwarder — without the
                // `has_ever_started` half of this test, each spawn's cleanup
                // cancelled its not-yet-started siblings, so only the
                // last-spawned agent of a batch survived (live-observed
                // 2026-09-08, plan ccff0138: four reviewers spawned, one ran).
                // Latch first (Acquire load): observing `has_ever_started`
                // guarantees the `running = true` store that preceded the
                // latch store in `set_running` is visible to the
                // `is_running()` load below — on the first latch transition
                // (a freshly spawned agent) the two can never present
                // "started + idle" mid-`set_running(true)` (quality review
                // Q1, 2026-09-08). On re-starts (turn 2+) a
                // nanosecond-scale residual remains, equivalent to the
                // cleanup winning the race during the agent's designed
                // idle gap between turns.
                && h.has_ever_started()
                && !h.is_running()
        })
        .map(|h| h.id)
        .collect();
    // Send each Cancel while still holding the lock (try_send is non-blocking,
    // so this is brief and never awaits). Keeping the lock until all Cancels
    // are enqueued guarantees correct FIFO ordering vs. a concurrent Prompt.
    for id in to_cancel {
        let _ = mgr.send(id, AgentCommand::Cancel);
    }
}

/// The payload emitted to the frontend: the agent id + the serializable event.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentEventPayload {
    pub agent_id: AgentId,
    pub event: SerializableAgentEvent,
}

/// Fire a resolved turn's dispatch continuation OFF the forwarder's
/// critical path (review L2, 2027-01-09 perf round).
///
/// The forwarder consumed the turn-resolve latch and read the turn's
/// evidence BEFORE calling this — it needs the latch result, not the
/// dispatch completion. The continuation (git checkpoint commit, embedder
/// recall, worktree checkout/branch-fork provisioning, serialized
/// landings — seconds, tens of seconds when several lanes provision)
/// runs in its own task, so the forwarder returns to recv() immediately
/// and every agent's events keep flowing; awaiting it inline stopped the
/// single forwarder task for the dispatch duration, filled the bounded
/// fan-in channel (capacity 256), and froze every agent's event delivery
/// app-wide.
///
/// Ordering is unchanged: dispatch decisions were already serialized by
/// DISPATCH_LOCK and landings by LANDING_LOCK — the locks, not the
/// forwarder, were the serialization points (the between-items compact
/// path already runs spawned on this same contract).
///
/// The caller MUST read the latch evidence (workflow_changed /
/// plan_abandoned / abandoned_plan_id) BEFORE this call and move the owned
/// values into the future: the next `Started` event clears them, and the
/// forwarder processes further events the moment this returns.
fn spawn_resolution_continuation<F>(fut: F) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(fut)
}

/// If the main agent has a deferred resolution (failure or success) and no
/// descendants are running, resolve Run-All / auto-feed once.
///
/// **Failures** are always flushed (a failure is terminal). **Successes** are
/// flushed only when the main agent's workflow is `Complete` — a deferred
/// success while the workflow is still `Reviewing` (the main agent spawned a
/// reviewer and ended its turn) is NOT delivered prematurely, because the main
/// agent will be resumed to fix findings and call `finish`. Delivering it
/// then would close the turn as a non-closure — the item is left in_flight
/// with a premature "plan loop did not close (workflow: Reviewing)" note
/// even though the main agent hasn't finished its work yet. When
/// the workflow is not Complete, the pending entry is left in place; the main
/// agent's next turn (`on_started` clears it) will handle resolution when the
/// plan actually reaches Complete.
async fn try_flush_deferred_main_resolution(
    app: &AppHandle,
    manager_arc: &Arc<Mutex<AgentManager>>,
    turn_resolve: &mut TurnResolveLatch,
) {
    let (main_id, descendants_running) = {
        let mgr = manager_arc.lock().await;
        match mgr.main_agent_id() {
            Some(id) => (id, mgr.has_running_descendants(id)),
            None => return,
        }
    };
    match turn_resolve.flush_deferred_main_failure(main_id, descendants_running) {
        ResolveAction::Failure(n) => {
            // Off the forwarder's critical path (review L2): evidence is
            // read BEFORE the spawn (the next Started clears it); the
            // continuation runs in its own task.
            let loop_evidence = turn_resolve.workflow_changed(main_id);
            let plan_abandoned = turn_resolve.plan_abandoned(main_id);
            let abandoned_plan_id = turn_resolve
                .abandoned_plan_id(main_id)
                .map(str::to_string);
            let app = app.clone();
            spawn_resolution_continuation(async move {
                crate::ipc::run_all::on_main_turn_resolved(
                    &app,
                    false,
                    Some(n),
                    loop_evidence,
                    plan_abandoned,
                    abandoned_plan_id.as_deref(),
                )
                .await;
            });
        }
        ResolveAction::Success => {
            // Gate: only deliver the deferred success when the main agent's
            // plan actually reached Complete. If the workflow is still
            // Reviewing (the main agent spawned a reviewer and ended its
            // turn), do NOT resolve — the main agent will be resumed to
            // finish its plan. Re-mark the pending entry so a later drain
            // (or the next turn) can still resolve it.
            let state = app.state::<IpcState>();
            let wf_complete = crate::ipc::run_all::main_agent_workflow_state(&state)
                .await
                .map_or(false, |s| s == WorkflowState::Complete);
            if wf_complete {
                // Off the forwarder's critical path (review L2): evidence
                // is read BEFORE the spawn (the next Started clears it);
                // the continuation runs in its own task.
                let loop_evidence = turn_resolve.workflow_changed(main_id);
                let plan_abandoned = turn_resolve.plan_abandoned(main_id);
                let abandoned_plan_id = turn_resolve
                    .abandoned_plan_id(main_id)
                    .map(str::to_string);
                let app = app.clone();
                spawn_resolution_continuation(async move {
                    crate::ipc::run_all::on_main_turn_resolved(
                        &app,
                        true,
                        None,
                        loop_evidence,
                        plan_abandoned,
                        abandoned_plan_id.as_deref(),
                    )
                    .await;
                });
            } else {
                // Not Complete yet — re-mark the pending entry (it was
                // consumed by flush_deferred_main_failure) so a later
                // descendant-drain or the next turn can still resolve.
                turn_resolve.remark_pending_finished(main_id);
            }
        }
        ResolveAction::None => {}
    }
}

/// Flush a deferred resolution for every spawned run-all lane (plan
/// ffd7a86f, review R3-L1): a lane's turn that ended while its reviewer
/// subagent still ran is deferred under the LANE's id — and unlike the
/// main agent, no descendant-cleared flush existed for it (the recovery
/// rode on the reviewer's completion notification, a best-effort
/// try_send; a dropped send stalled the lane's item InFlight with the
/// run waiting). Called from the same two sites as the main flush (a
/// child's Finished, any Exited), BEFORE it: post-main-exit
/// `main_agent_id()` resolves to a lane, and the main flush would
/// mis-consume a lane's latch and silently discard its disposition.
/// Best-effort, no-op when no lane latch is deferred.
async fn try_flush_deferred_spawned_resolutions(
    app: &AppHandle,
    manager_arc: &Arc<Mutex<AgentManager>>,
    turn_resolve: &mut TurnResolveLatch,
) {
    let state = app.state::<IpcState>();
    let lanes: Vec<mnemo::runtime::AgentId> = {
        let guard = state.backlog.run_all.lock().await;
        guard.as_ref().map_or(Vec::new(), |r| {
            r.spawned
                .lock()
                .expect("spawned lock poisoned")
                .iter()
                .map(|s| s.agent_id)
                .collect()
        })
    };
    for lane_id in lanes {
        let descendants_running = {
            let mgr = manager_arc.lock().await;
            mgr.has_running_descendants(lane_id)
        };
        match turn_resolve.flush_deferred_main_failure(lane_id, descendants_running) {
            ResolveAction::Failure(n) => {
                // Off the forwarder's critical path (review L2): evidence
                // is read BEFORE the spawn (the next Started clears it);
                // the continuation runs in its own task.
                let loop_evidence = turn_resolve.workflow_changed(lane_id);
                let plan_abandoned = turn_resolve.plan_abandoned(lane_id);
                let abandoned_plan_id = turn_resolve
                    .abandoned_plan_id(lane_id)
                    .map(str::to_string);
                let app = app.clone();
                spawn_resolution_continuation(async move {
                    crate::ipc::run_all::on_spawned_turn_resolved(
                        &app,
                        lane_id,
                        false,
                        Some(n),
                        loop_evidence,
                        plan_abandoned,
                        abandoned_plan_id.as_deref(),
                    )
                    .await;
                });
            }
            ResolveAction::Success => {
                // The same Complete gate as the main flush: only deliver
                // when the lane's plan actually reached Complete (a
                // Reviewing lane will be resumed by its reviewer's
                // completion notification).
                let wf_complete =
                    crate::ipc::run_all::agent_workflow_state(&state, lane_id)
                        .await
                        .map_or(false, |s| s == WorkflowState::Complete);
                if wf_complete {
                    // Off the forwarder's critical path (review L2):
                    // evidence is read BEFORE the spawn (the next Started
                    // clears it); the continuation runs in its own task.
                    let loop_evidence = turn_resolve.workflow_changed(lane_id);
                    let plan_abandoned = turn_resolve.plan_abandoned(lane_id);
                    let abandoned_plan_id = turn_resolve
                        .abandoned_plan_id(lane_id)
                        .map(str::to_string);
                    let app = app.clone();
                    spawn_resolution_continuation(async move {
                        crate::ipc::run_all::on_spawned_turn_resolved(
                            &app,
                            lane_id,
                            true,
                            None,
                            loop_evidence,
                            plan_abandoned,
                            abandoned_plan_id.as_deref(),
                        )
                        .await;
                    });
                } else {
                    // Not Complete yet — re-mark the pending entry (it
                    // was consumed by flush_deferred_main_failure) so a
                    // later descendant-drain or the next turn can still
                    // resolve.
                    turn_resolve.remark_pending_finished(lane_id);
                }
            }
            ResolveAction::None => {}
        }
    }
}

#[cfg(test)]
mod cleanup_tests {
    //! Regression guard for the back-to-back-spawn cancellation (live
    //! 2026-09-08, plan ccff0138): four reviewers spawned from four
    //! `spawn_agent` tool calls in ONE assistant message, and only the
    //! last-spawned survived. Each spawn runs `cleanup_inactive_subagents`
    //! BEFORE registering its own agent, and the filter used to cancel every
    //! subagent with `!is_running()` — which a sibling registered moments
    //! earlier by the previous spawn also is, until its first `Started`
    //! event reaches the forwarder. The cleanup therefore killed the
    //! pending siblings before their first turn (no reports, no
    //! `ChildFinished`, tabs vanished).
    //!
    //! These tests drive `cleanup_inactive_subagents` against the exact
    //! manager state a second spawn sees, holding the children's command
    //! receivers so "was a `Cancel` enqueued" is an exact assertion rather
    //! than an inference.

    use super::*;
    use mnemo::runtime::AgentHandle;

    /// A manager holding one parent plus one subagent in the given state.
    /// `started` replays the forwarder's `Started`/`Finished` arms.
    async fn manager_with_child(
        started: bool,
        running: bool,
    ) -> (Arc<Mutex<AgentManager>>, mpsc::Receiver<AgentCommand>) {
        let mut mgr = AgentManager::new(16);
        drop(mgr.take_fanin_rx());
        let (parent_tx, _parent_rx) = mpsc::channel::<AgentCommand>(8);
        let (child_tx, child_rx) = mpsc::channel::<AgentCommand>(8);
        mgr.register(AgentHandle::new(1, "parent".into(), parent_tx));
        mgr.register(AgentHandle::new(2, "child".into(), child_tx).with_parent(1));
        if started {
            mgr.set_running(2, true);
            if !running {
                mgr.set_running(2, false);
            }
        }
        (Arc::new(Mutex::new(mgr)), child_rx)
    }

    #[tokio::test]
    async fn cleanup_spares_a_subagent_that_has_never_started() {
        // The bug: agent 2 was registered by the PREVIOUS `spawn_agent` in
        // the same assistant message and its first turn has not begun, so
        // `is_running()` is false. The next spawn's cleanup (trigger = the
        // id it just allocated, 3) must leave it alone — it is pending, not
        // completed.
        let (manager, mut child_rx) = manager_with_child(false, false).await;

        cleanup_inactive_subagents(&manager, 3).await;

        assert!(
            child_rx.try_recv().is_err(),
            "a never-started subagent must NEVER be cancelled — it is the agent the previous spawn_agent call just registered"
        );
        assert_eq!(
            manager.lock().await.len(),
            2,
            "both handles stay registered"
        );
    }

    #[tokio::test]
    async fn cleanup_still_cancels_a_started_then_idle_subagent() {
        // The intended behavior is preserved: a subagent that ran a turn and
        // went idle is genuinely completed, so a fresh spawn retires it (its
        // tab disappears as the new agent appears).
        let (manager, mut child_rx) = manager_with_child(true, false).await;

        cleanup_inactive_subagents(&manager, 3).await;

        assert!(
            matches!(child_rx.try_recv(), Ok(AgentCommand::Cancel)),
            "a completed (started, now idle) subagent is still cancelled"
        );
    }

    #[tokio::test]
    async fn cleanup_never_cancels_a_running_subagent() {
        // Unchanged invariant: cancelling a running subagent would kill a
        // task still in flight.
        let (manager, mut child_rx) = manager_with_child(true, true).await;

        cleanup_inactive_subagents(&manager, 3).await;

        assert!(
            child_rx.try_recv().is_err(),
            "a RUNNING subagent must never be cancelled"
        );
    }

    #[tokio::test]
    async fn three_pending_siblings_all_survive_a_fourth_spawn() {
        // The reported shape: three subagents registered by three earlier
        // `spawn_agent` calls in the same message, none started yet, and a
        // fourth spawn runs cleanup. Before the fix every one of them was
        // cancelled; all three must survive.
        let mut mgr = AgentManager::new(16);
        drop(mgr.take_fanin_rx());
        let (parent_tx, _parent_rx) = mpsc::channel::<AgentCommand>(8);
        mgr.register(AgentHandle::new(1, "parent".into(), parent_tx));
        let mut receivers = Vec::new();
        for id in 2..=4 {
            let (tx, rx) = mpsc::channel::<AgentCommand>(8);
            mgr.register(AgentHandle::new(id, format!("reviewer{id}"), tx).with_parent(1));
            receivers.push((id, rx));
        }
        let manager = Arc::new(Mutex::new(mgr));

        cleanup_inactive_subagents(&manager, 5).await;

        for (id, rx) in receivers.iter_mut() {
            assert!(
                rx.try_recv().is_err(),
                "pending sibling {id} must not be cancelled by the next spawn"
            );
        }
        assert_eq!(manager.lock().await.len(), 4, "all handles stay registered");
    }
}

#[cfg(test)]
mod delta_batcher_tests {
    use super::{DeltaBatcher, DeltaKind, DELTA_BUCKET_CAP};
    use mnemo::runtime::channels::SerializableAgentEvent;

    fn text(agent: u64, s: &str) -> (u64, SerializableAgentEvent) {
        (agent, SerializableAgentEvent::TextDelta { text: s.into() })
    }

    fn reasoning(agent: u64, s: &str) -> (u64, SerializableAgentEvent) {
        (
            agent,
            SerializableAgentEvent::ReasoningDelta { text: s.into() },
        )
    }

    fn arg(agent: u64, index: u32, s: &str) -> (u64, SerializableAgentEvent) {
        (
            agent,
            SerializableAgentEvent::ToolCallArgDelta {
                index,
                fragment: s.into(),
            },
        )
    }

    fn finished(agent: u64) -> (u64, SerializableAgentEvent) {
        (
            agent,
            SerializableAgentEvent::Finished {
                reason: mnemo::provider::FinishReason::Stop,
            },
        )
    }

    /// Deltas are absorbed (None) and concatenated per (agent, kind) on flush.
    #[test]
    fn deltas_concatenate_per_agent_and_kind() {
        let mut b = DeltaBatcher::default();
        assert!(b.push(1, text(1, "hel").1).is_none());
        assert!(b.push(1, text(1, "lo").1).is_none());
        assert!(b.push(2, text(2, "other").1).is_none());
        assert!(b.push(1, reasoning(1, "think").1).is_none());

        let flushed = b.flush_agent(1);
        assert_eq!(flushed.len(), 2, "one event per kind bucket");
        assert!(matches!(
            &flushed[0],
            SerializableAgentEvent::ReasoningDelta { text } if text == "think"
        ));
        assert!(matches!(
            &flushed[1],
            SerializableAgentEvent::TextDelta { text } if text == "hello"
        ));
        // Agent 2's bucket is untouched by agent 1's flush.
        assert!(!b.is_empty());
        let rest = b.flush_all();
        assert_eq!(rest.len(), 1);
        assert!(matches!(
            &rest[0].1,
            SerializableAgentEvent::TextDelta { text } if text == "other"
        ));
        assert_eq!(rest[0].0, 2);
        assert!(b.is_empty());
    }

    /// Tool-call arg fragments for different indices never interleave.
    #[test]
    fn arg_deltas_are_keyed_by_tool_call_index() {
        let mut b = DeltaBatcher::default();
        assert!(b.push(1, arg(1, 0, "a").1).is_none());
        assert!(b.push(1, arg(1, 1, "x").1).is_none());
        assert!(b.push(1, arg(1, 0, "b").1).is_none());

        let flushed = b.flush_agent(1);
        assert_eq!(flushed.len(), 2);
        assert!(matches!(
            &flushed[0],
            SerializableAgentEvent::ToolCallArgDelta { index: 0, fragment } if fragment == "ab"
        ));
        assert!(matches!(
            &flushed[1],
            SerializableAgentEvent::ToolCallArgDelta { index: 1, fragment } if fragment == "x"
        ));
    }

    /// A structural event flushes the agent's pending deltas FIRST (order
    /// preserved) and then passes through.
    #[test]
    fn structural_event_flushes_pending_deltas_first() {
        let mut b = DeltaBatcher::default();
        assert!(b.push(1, text(1, "abc").1).is_none());
        let (aid, ev) = finished(1);
        let out = b.push(aid, ev).unwrap();
        assert_eq!(out.len(), 2, "delta flush + the structural event");
        assert!(matches!(
            &out[0],
            SerializableAgentEvent::TextDelta { text } if text == "abc"
        ));
        assert!(matches!(&out[1], SerializableAgentEvent::Finished { .. }));
        assert!(b.is_empty());
    }

    /// A structural event for agent 2 must NOT flush agent 1's pending deltas.
    #[test]
    fn structural_event_only_flushes_its_own_agent() {
        let mut b = DeltaBatcher::default();
        assert!(b.push(1, text(1, "abc").1).is_none());
        let (aid, ev) = finished(2);
        let out = b.push(aid, ev).unwrap();
        assert_eq!(out.len(), 1, "only the structural event passes through");
        assert!(matches!(&out[0], SerializableAgentEvent::Finished { .. }));
        assert!(!b.is_empty(), "agent 1's deltas stay pending");
        let rest = b.flush_all();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].0, 1);
    }

    /// The per-bucket cap flushes immediately (mirrors the frontend's 64 KiB
    /// synchronous-flush cap).
    #[test]
    fn cap_triggers_an_immediate_flush() {
        let mut b = DeltaBatcher::default();
        let big = "x".repeat(DELTA_BUCKET_CAP - 1);
        assert!(b.push(1, text(1, &big).1).is_none());
        // One more byte crosses the cap → immediate flush of agent 1.
        let out = b.push(1, text(1, "y").1).unwrap();
        assert_eq!(out.len(), 1);
        assert!(matches!(
            &out[0],
            SerializableAgentEvent::TextDelta { text } if text.len() == DELTA_BUCKET_CAP
        ));
        assert!(b.is_empty());
    }

    /// flush_all emits every agent's buckets, oldest kind first.
    #[test]
    fn flush_all_covers_every_agent() {
        let mut b = DeltaBatcher::default();
        assert!(b.push(1, text(1, "a").1).is_none());
        assert!(b.push(2, reasoning(2, "r").1).is_none());
        let out = b.flush_all();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, 1);
        assert_eq!(out[1].0, 2);
        assert!(b.is_empty());
    }

    /// Empty buckets are never emitted (no-op flush).
    #[test]
    fn empty_flush_is_a_noop() {
        let mut b = DeltaBatcher::default();
        assert!(b.flush_all().is_empty());
        assert!(b.flush_agent(1).is_empty());
        assert!(b.is_empty());
    }

    /// DeltaKind variant order = natural stream order (reasoning → text →
    /// tool-call args) — the flush sort relies on it.
    #[test]
    fn delta_kind_order_is_reasoning_then_text_then_args() {
        assert!((DeltaKind::Reasoning as u8) < (DeltaKind::Text as u8));
        assert!((DeltaKind::Text as u8) < (DeltaKind::ToolCallArg as u8));
    }
}

#[cfg(test)]
mod recv_until_deadline_tests {
    use super::{recv_until_deadline, RecvOutcome, DELTA_FLUSH_INTERVAL};
    use std::time::Duration;
    use tokio::sync::mpsc;

    /// The flush deadline is FIXED at arm time: events arriving before it
    /// are returned as `Event`, and the deadline fires at its ORIGINAL
    /// instant even while events keep arriving — never one interval after
    /// the last event. Regression for review HIGH-1 (2026-09-05): an inline
    /// `sleep(DELTA_FLUSH_INTERVAL)` re-created per loop iteration reset
    /// the deadline on every received event, so a sustained stream (>1
    /// event per interval) starved the flush indefinitely.
    #[tokio::test(start_paused = true)]
    async fn deadline_is_fixed_at_arm_time() {
        let (tx, mut rx) = mpsc::channel::<u8>(16);
        let armed = tokio::time::Instant::now() + DELTA_FLUSH_INTERVAL;

        // Event 5 ms after arming → Event.
        tokio::time::advance(Duration::from_millis(5)).await;
        tx.send(1).await.unwrap();
        assert!(matches!(
            recv_until_deadline(&mut rx, Some(armed)).await,
            RecvOutcome::Event(1)
        ));

        // Event 10 ms after arming → still Event; the deadline must NOT
        // have moved to 10 ms + 16 ms.
        tokio::time::advance(Duration::from_millis(5)).await;
        tx.send(2).await.unwrap();
        assert!(matches!(
            recv_until_deadline(&mut rx, Some(armed)).await,
            RecvOutcome::Event(2)
        ));

        // Silence afterwards: the paused clock auto-advances and the
        // ORIGINAL deadline (t+16 ms) fires — proving the timer was not
        // re-armed by the intervening events. The instant assertion is the
        // actual pin: a regressed helper that ignored the passed deadline
        // and slept its own interval (armed at the last event, t+10 ms)
        // would land at t+26 ms ≠ armed. (Review N-1, 2026-09-05.)
        assert!(matches!(
            recv_until_deadline(&mut rx, Some(armed)).await,
            RecvOutcome::Deadline
        ));
        assert_eq!(
            tokio::time::Instant::now(),
            armed,
            "deadline must fire at the ORIGINAL arm instant, not now + interval"
        );
    }

    /// Without an armed deadline the step is a plain receive, and a closed
    /// channel surfaces as `Closed` (the forwarder then flushes the
    /// trailing batch before exiting — the contract behind the review
    /// LOW-1, 2026-09-05 fix).
    #[tokio::test(start_paused = true)]
    async fn no_deadline_is_a_plain_receive_and_closed_surfaces() {
        let (tx, mut rx) = mpsc::channel::<u8>(4);
        tx.send(7).await.unwrap();
        assert!(matches!(
            recv_until_deadline(&mut rx, None).await,
            RecvOutcome::Event(7)
        ));
        drop(tx);
        assert!(matches!(
            recv_until_deadline(&mut rx, None).await,
            RecvOutcome::Closed
        ));
    }
}

#[cfg(test)]
mod watchdog_note_tests {
    use super::watchdog_note;
    use mnemo::provider::FinishReason;
    use mnemo::runtime::channels::{ParkReason, SerializableAgentEvent};
    use mnemo::runtime::AgentId;
    use mnemo::workflow::WorkflowState;

    /// Review finding 4 (2026-08-20): every state-changing event arm must
    /// produce its note; streaming/display-only events must be skipped.
    #[test]
    fn state_changing_events_produce_notes() {
        let cases: Vec<(AgentId, SerializableAgentEvent, &str)> = vec![
            (7, SerializableAgentEvent::Started, "agent7 Started"),
            (
                7,
                SerializableAgentEvent::Finished {
                    reason: FinishReason::Stop,
                },
                "agent7 Finished",
            ),
            (
                7,
                SerializableAgentEvent::Error {
                    error: "boom".into(),
                    retrying: true,
                },
                "agent7 Error(retrying)",
            ),
            (
                7,
                SerializableAgentEvent::Error {
                    error: "boom".into(),
                    retrying: false,
                },
                "agent7 Error(final)",
            ),
            (
                7,
                SerializableAgentEvent::WorkflowStateChanged {
                    state: WorkflowState::Reviewing,
                    top_plan_id: None,
                },
                "agent7 workflow → Reviewing",
            ),
            (
                7,
                SerializableAgentEvent::ApprovalRequest {
                    tool_call_id: "t1".into(),
                    tool_name: "file_edit".into(),
                    args: serde_json::json!({}),
                    preview: None,
                    core_operation: false,
                },
                "agent7 approval: file_edit",
            ),
            (
                7,
                SerializableAgentEvent::UserQuestion {
                    question_id: "q1".into(),
                    question: "which?".into(),
                    options: vec![],
                },
                "agent7 UserQuestion",
            ),
            (
                7,
                SerializableAgentEvent::PromptDispatched {
                    text: "run the backlog".into(),
                    images: vec![],
                },
                "agent7 PromptDispatched",
            ),
            (
                7,
                SerializableAgentEvent::SkillStarted {
                    name: "merge_to_main".into(),
                    prompt: "merge".into(),
                },
                "agent7 skill: merge_to_main",
            ),
            (7, SerializableAgentEvent::Exited, "agent7 Exited"),
            (
                7,
                SerializableAgentEvent::Parked {
                    reason: ParkReason::BudgetExhausted,
                    workflow_state: "Reviewing".into(),
                    descendants_running: false,
                    auto_continue_streak: 12,
                },
                "agent7 parked: BudgetExhausted (wf=Reviewing, desc=false, streak=12)",
            ),
        ];
        for (id, event, expected) in cases {
            assert_eq!(
                watchdog_note(id, &event).as_deref(),
                Some(expected),
                "note for {expected}"
            );
        }
    }

    /// Streaming and display-only events must NOT drown the 64-entry ring —
    /// one note per token would evict every real state change.
    #[test]
    fn streaming_and_display_events_are_skipped() {
        let skipped = vec![
            SerializableAgentEvent::TextDelta { text: "hi".into() },
            SerializableAgentEvent::ReasoningDelta { text: "hmm".into() },
            SerializableAgentEvent::ToolCallStart {
                index: 0,
                id: "t".into(),
                name: "shell".into(),
            },
            SerializableAgentEvent::ToolCallArgDelta {
                index: 0,
                fragment: "{".into(),
            },
            SerializableAgentEvent::ToolResult {
                tool_call_id: "t".into(),
                result: mnemo::tool::ToolResult::success("ok"),
            },
            SerializableAgentEvent::Usage {
                prompt_tokens: 1,
                completion_tokens: 2,
                reasoning_tokens: 0,
                cached_tokens: 0,
                ttft_ms: None,
                generation_ms: None,
            },
            SerializableAgentEvent::ContextUsage {
                used: 1,
                max: 2,
                breakdown: Default::default(),
                // Lever 6 is off in this vector, so the pre-lever JSON shape is
                // what gets pinned (no `quality` key on the wire).
                quality: None,
            },
            SerializableAgentEvent::StepCompleted { step_index: 1 },
            SerializableAgentEvent::SuggestionInjected {
                text: "s".into(),
                images: vec![],
            },
            SerializableAgentEvent::ModelChanged {
                model: "gpt-4o".into(),
                provider: None,
                reasoning_effort: Some("low".into()),
            },
            SerializableAgentEvent::MemoryRecalled { hits: vec![] },
            SerializableAgentEvent::Phase {
                phase: mnemo::runtime::channels::PhaseKind::Streaming,
            },
            SerializableAgentEvent::ChildFinished {
                child_id: 2,
                name: "child".into(),
                success: true,
            },
            SerializableAgentEvent::Compacted {
                before: 1,
                after: 1,
            },
        ];
        for event in skipped {
            assert_eq!(
                watchdog_note(7, &event),
                None,
                "no note expected for {event:?}"
            );
        }
    }

    /// The Parked event carries the pre-stall evidence (backlog 5c33e945,
    /// 2027-01-07): the ring must show WHY the agent parked — an interrupt
    /// (user stop) must be distinguishable from a designed wait when
    /// diagnosing a live "needed manual c" incident.
    #[test]
    fn parked_event_notes_the_pre_stall_evidence() {
        let interrupted = SerializableAgentEvent::Parked {
            reason: ParkReason::Interrupted,
            workflow_state: "Executing".into(),
            descendants_running: false,
            auto_continue_streak: 3,
        };
        assert_eq!(
            watchdog_note(7, &interrupted).as_deref(),
            Some("agent7 parked: Interrupted (wf=Executing, desc=false, streak=3)"),
        );
        let waiting = SerializableAgentEvent::Parked {
            reason: ParkReason::WaitingForDescendants,
            workflow_state: "Reviewing".into(),
            descendants_running: true,
            auto_continue_streak: 0,
        };
        assert_eq!(
            watchdog_note(7, &waiting).as_deref(),
            Some(
                "agent7 parked: WaitingForDescendants (wf=Reviewing, desc=true, streak=0)"
            ),
        );
    }
}

/// Emit a backend-synthesized agent event to the frontend on the shared
/// `AGENT_EVENT_CHANNEL`, tagged with the agent it concerns. This is the
/// single emit path for display events injected from outside the normal
/// agent→UI stream — e.g. a backlog-dispatched prompt (`PromptDispatched`)
/// or a toolbar-started skill (`SkillStarted`). Both mirror how the main
/// input optimistically appends a user message: the event lands in the
/// transcript so the user sees what was sent to the agent, before the
/// agent's own `Started` event arrives.
///
/// Best-effort: a failed emit is logged, not fatal.
pub(crate) fn emit_agent_event(app: &AppHandle, agent_id: AgentId, event: SerializableAgentEvent) {
    let payload = AgentEventPayload { agent_id, event };
    if let Err(e) = app.emit(AGENT_EVENT_CHANNEL, &payload) {
        eprintln!("failed to emit agent event: {e}");
    }
}

/// Emit a `PromptDispatched` agent event to the frontend, tagged with the
/// agent the prompt was dispatched to. Lets the frontend show a
/// backlog-dispatched prompt as the goal at the top of the transcript —
/// mirroring how the main input optimistically appends the user message.
pub(crate) fn emit_prompt_dispatched(
    app: &AppHandle,
    agent_id: AgentId,
    text: &str,
    images: &[String],
) {
    emit_agent_event(
        app,
        agent_id,
        SerializableAgentEvent::PromptDispatched {
            text: text.to_string(),
            images: images.to_vec(),
        },
    );
}

#[cfg(test)]
mod tests {
    //! Regression tests for the descendant-tracker / finish-notification
    //! invariant (backlog 2c406d72): after a spawned child's session ends
    //! (finish notification emitted), `has_running_descendants(parent)`
    //! must return false immediately — the parent's next dispatch-cycle
    //! gate consult must never see a verifiably-finished child as still
    //! running. Before the fix, the notification path
    //! (`notify_parent_on_completion`) sent the completion `Suggestion`
    //! without clearing the child's running flag; the clear happened only
    //! later, in the forwarder's separate running-state match — leaving a
    //! window (the `ChildFinished` webview emit plus a manager-lock
    //! acquisition) in which the parent, woken by the `Suggestion`, hit the
    //! descendant gate while the child was still marked running
    //! (live-observed 2026-12-30, plan 72329f2c: `complete_step` refused
    //! for a full turn after every child had verifiably ended).

    use super::*;
    use mnemo::runtime::AgentHandle;

    /// Register a parent (whose command inbox the test holds) plus a
    /// running child spawned by it — the manager state the forwarder sees
    /// at the moment a child's `Finished` event is processed.
    async fn parent_with_running_child()
    -> (Arc<Mutex<AgentManager>>, mpsc::Receiver<AgentCommand>) {
        let manager = Arc::new(Mutex::new(AgentManager::new(16)));
        let (parent_tx, parent_rx) = mpsc::channel::<AgentCommand>(8);
        let (child_tx, _child_rx) = mpsc::channel::<AgentCommand>(8);
        {
            let mut mgr = manager.lock().await;
            mgr.register(AgentHandle::new(1, "parent".into(), parent_tx));
            mgr.register(AgentHandle::new(2, "child".into(), child_tx).with_parent(1));
            // Simulate the forwarder's `Started` arm: the child is mid-task.
            mgr.set_running(2, true);
            assert!(
                mgr.has_running_descendants(1),
                "precondition: the running child counts as a running descendant"
            );
        }
        (manager, parent_rx)
    }

    /// An empty per-agent loop map: no recorded review-report path, so the
    /// notification uses the generic completion text.
    fn empty_loop_map() -> crate::ipc::state::AgentLoopMap {
        Arc::new(Mutex::new(HashMap::new()))
    }

    // --- Parent-loop factory (review LOW 4, backlog 5b46674d) -------------

    use mnemo::agent::context::ContextManager;
    use mnemo::agent::factory::AgentLoopFactory;
    use mnemo::project::ConstitutionSource;
    use mnemo::provider::{Capabilities, LlmEvent, ProviderKind, ToolSchema};
    use mnemo::tool::agent::sandbox::Sandbox;

    /// A provider whose `complete` always errors — never called in these
    /// tests (the loop is only registered so the notification path can
    /// read and latch it).
    struct NeverProvider;
    #[async_trait::async_trait]
    impl mnemo::provider::LlmClient for NeverProvider {
        fn capabilities(&self) -> &Capabilities {
            use std::sync::OnceLock;
            static CAPS: OnceLock<Capabilities> = OnceLock::new();
            CAPS.get_or_init(Capabilities::openai)
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "never"
        }
        async fn complete(
            &self,
            _: &[mnemo::provider::Message],
            _: &[ToolSchema],
            _: Option<mnemo::provider::ToolChoice>,
        ) -> mnemo::error::Result<futures::stream::BoxStream<'_, LlmEvent>> {
            Err(mnemo::error::Error::Provider(
                "never provider never completes".into(),
            ))
        }
    }

    /// The same factory pattern the console tests use: a real `AgentLoop`
    /// for the parent, so the notification path can set (and the test
    /// assert) the reviewer-failure latch.
    fn make_factory(dir: &std::path::Path) -> Arc<AgentLoopFactory> {
        let provider: Arc<dyn mnemo::provider::LlmClient> = Arc::new(NeverProvider);
        let sandbox = Arc::new(Sandbox::new(dir).unwrap());
        let gpath = dir.join("global.md");
        let ppath = dir.join("project.md");
        std::fs::write(&gpath, "").unwrap();
        std::fs::write(&ppath, "").unwrap();
        Arc::new(AgentLoopFactory::new(
            provider,
            ConstitutionSource::new(&gpath, &ppath).unwrap(),
            None,
            sandbox,
            dir.to_path_buf(),
            None,
            Arc::new(std::sync::RwLock::new(
                mnemo::config::SafetyMode::Autonomous,
            )),
            ContextManager::new(128_000, 0.5),
            dir.join("plans"),
            None,
        ))
    }

    #[tokio::test]
    async fn child_finish_notification_leaves_no_running_descendant() {
        let (manager, mut parent_rx) = parent_with_running_child().await;
        let loops = empty_loop_map();

        // The child's `Finished` event reaches the notification path.
        let ev = notify_parent_on_completion(&manager, &loops, 2, true).await;
        assert!(matches!(
            ev,
            Some(SerializableAgentEvent::ChildFinished { success: true, .. })
        ));

        // The finish notification reached the parent...
        match parent_rx.try_recv().expect("parent must receive the completion Suggestion") {
            AgentCommand::Suggestion(payload) => {
                let text = &payload.text;
                assert!(text.contains("child"), "notification names the child: {text}");
            }
            other => panic!("expected Suggestion, got {other:?}"),
        }
        // ...and the tracker already agrees: no running descendant — the
        // very next dispatch-cycle gate consult sees the child as done.
        assert!(
            !manager.lock().await.has_running_descendants(1),
            "tracker must report no running descendants once the notification is out"
        );
    }

    #[tokio::test]
    async fn child_final_error_notification_leaves_no_running_descendant() {
        // The final (non-retrying) `Error` arm shares
        // `notify_parent_on_completion`, so a child that fails its task is
        // equally "done" for gate purposes.
        let (manager, mut parent_rx) = parent_with_running_child().await;
        let loops = empty_loop_map();

        let ev = notify_parent_on_completion(&manager, &loops, 2, false).await;
        assert!(matches!(
            ev,
            Some(SerializableAgentEvent::ChildFinished { success: false, .. })
        ));
        assert!(matches!(
            parent_rx.try_recv(),
            Ok(AgentCommand::Suggestion(_))
        ));
        assert!(
            !manager.lock().await.has_running_descendants(1),
            "tracker must report no running descendants once the notification is out"
        );
    }

    #[tokio::test]
    async fn tracker_agrees_at_notification_receipt() {
        // Pins the happens-before: the running flag is cleared BEFORE the
        // completion `Suggestion` is sent, so the moment the parent
        // observes the notification, the tracker already reports no
        // running descendants — the gate's exact view at that instant.
        let (manager, mut parent_rx) = parent_with_running_child().await;
        let loops = empty_loop_map();

        let notify = {
            let manager = Arc::clone(&manager);
            let loops = Arc::clone(&loops);
            tokio::spawn(async move { notify_parent_on_completion(&manager, &loops, 2, true).await })
        };
        match parent_rx.recv().await.expect("Suggestion must reach the parent") {
            AgentCommand::Suggestion(_) => {}
            other => panic!("expected Suggestion, got {other:?}"),
        }
        // The parent has woken on the notification — check the tracker at
        // this exact moment (this is what the descendant gate would see).
        assert!(
            !manager.lock().await.has_running_descendants(1),
            "tracker must agree with the notification at receipt time"
        );
        assert!(matches!(
            notify.await,
            Ok(Some(SerializableAgentEvent::ChildFinished { .. }))
        ));
    }

    #[tokio::test]
    async fn reportless_reviewer_gets_failure_text_even_on_success() {
        // Backlog 5b46674d: a `role: "reviewer"` child that FINISHES cleanly
        // without writing a report is just as report-less as a failed one —
        // the review did not happen either way. The failed-reviewer protocol
        // (distinct failure text + parent latch) must fire for ANY report-less
        // reviewer, not just !success ones (live-observed 2026-12-30: two
        // reviewers finished report-less and the parent got the generic
        // "read its report" text, then needed shell forensics to learn no
        // report existed).
        let manager = Arc::new(Mutex::new(AgentManager::new(16)));
        let (parent_tx, mut parent_rx) = mpsc::channel::<AgentCommand>(8);
        let (child_tx, _child_rx) = mpsc::channel::<AgentCommand>(8);
        {
            let mut mgr = manager.lock().await;
            mgr.register(AgentHandle::new(1, "parent".into(), parent_tx));
            mgr.register(
                AgentHandle::new(2, "quality-reviewer".into(), child_tx)
                    .with_parent(1)
                    .with_role(Some("reviewer".into())),
            );
            mgr.set_running(2, true);
        }
        // A real parent loop, so the notification path can latch it (review
        // LOW 4): the child's loop stays absent — no report path recorded.
        let dir = tempfile::tempdir().unwrap();
        let parent_loop = make_factory(dir.path()).build_with_id(1);
        let loops = empty_loop_map();
        loops.lock().await.insert(1, parent_loop.clone());
        assert!(
            !parent_loop.reviewer_failure_pending(),
            "latch starts clear"
        );

        let ev = notify_parent_on_completion(&manager, &loops, 2, true).await;
        assert!(matches!(
            ev,
            Some(SerializableAgentEvent::ChildFinished { success: true, .. })
        ));
        match parent_rx
            .try_recv()
            .expect("parent must receive the failure Suggestion")
        {
            AgentCommand::Suggestion(payload) => {
                let text = &payload.text;
                assert!(
                    text.contains("no report"),
                    "the notification must say no report was written: {text}"
                );
                assert!(
                    text.contains("Do NOT respawn"),
                    "the failed-reviewer protocol text must fire: {text}"
                );
                assert!(
                    !text.contains("read its report"),
                    "must not send the parent hunting for a nonexistent report: {text}"
                );
            }
            other => panic!("expected Suggestion, got {other:?}"),
        }
        assert!(
            parent_loop.reviewer_failure_pending(),
            "a report-less reviewer must latch the parent even on success"
        );
    }
}

#[cfg(test)]
mod resolution_continuation_tests {
    //! Review L2 (2027-01-09 perf round): the forwarder must return to
    //! recv() while a resolution→dispatch continuation is in flight.
    //! Awaiting the continuation inline stopped the single forwarder task
    //! for the dispatch duration (git checkpoint, embedder recall, worktree
    //! provisioning, landings — seconds to tens of seconds during a
    //! parallel run-all), filled the bounded fan-in channel (capacity
    //! 256), and froze every agent's event delivery app-wide.
    //!
    //! The forwarder loop needs a Tauri AppHandle, so the handoff is
    //! pinned at its seam: every production invocation of the continuations
    //! must be fired through the spawn helper, never awaited inline.

    use super::spawn_resolution_continuation;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[tokio::test(start_paused = true)]
    async fn continuation_in_flight_does_not_block_event_flow() {
        // Paused clock (the recv_until_deadline_tests seam): an inline
        // await of the continuation would advance the clock by the
        // dispatch's duration before the caller resumes; a spawn returns
        // with the clock untouched.
        let t0 = tokio::time::Instant::now();
        // The fan-in stand-in: events the forwarder must keep draining.
        let (fanin_tx, mut fanin_rx) = mpsc::channel::<u8>(8);
        // Signals the continuation actually started (it IS in flight).
        let (started_tx, mut started_rx) = mpsc::channel::<()>(1);

        let handle = spawn_resolution_continuation(async move {
            started_tx.send(()).await.unwrap();
            // The "dispatch": checkpoint + recall + provisioning — tens
            // of seconds live; 30s of virtual time here.
            tokio::time::sleep(Duration::from_secs(30)).await;
        });

        // The continuation is in flight...
        started_rx.recv().await.unwrap();
        // ...and the caller (the forwarder) is already back at recv():
        // events flow with the clock untouched.
        fanin_tx.send(1).await.unwrap();
        assert_eq!(fanin_rx.recv().await, Some(1));
        assert_eq!(
            tokio::time::Instant::now(),
            t0,
            "the handoff must not await the continuation — no clock \
             advance while the dispatch is in flight"
        );

        // Cleanup: cancel the in-flight continuation.
        handle.abort();
    }

    #[test]
    fn every_continuation_invocation_is_spawned_not_awaited_inline() {
        // Source contract: the production region of this file (before the
        // first test module) must fire every resolution→dispatch
        // continuation through the spawn helper — an unwrapped call is the
        // L2 inline-await regression. Evidence reads must stay OUTSIDE the
        // spawned block: the next Started clears them.
        let src = include_str!("events.rs");
        let prod = &src[..src.find("#[cfg(test)]").expect("test modules exist")];
        let wrappers = prod
            .matches("spawn_resolution_continuation(async move {")
            .count();
        let continuations = prod
            .matches("crate::ipc::run_all::on_spawned_turn_resolved(")
            .count()
            + prod
                .matches("crate::ipc::run_all::on_main_turn_resolved(")
                .count();
        assert!(
            wrappers > 0 && wrappers == continuations,
            "every continuation invocation must be fired via the spawn \
             helper — an unwrapped call is the inline-await regression \
             (wrappers={wrappers}, continuations={continuations})"
        );
        for (i, _) in prod.match_indices("spawn_resolution_continuation(async move {") {
            // Depth-count to the block's CLOSING brace — a nested brace
            // (match/if/closure) must not end the scan early (review
            // round-1 LOW-2) — and require the block to carry the
            // continuation call it exists to fire.
            let open = i + "spawn_resolution_continuation(async move ".len();
            let mut depth = 0usize;
            let mut end = prod.len();
            for (off, ch) in prod[open..].char_indices() {
                match ch {
                    '{' => depth += 1,
                    '}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            end = open + off;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let block = &prod[i..end];
            assert!(
                block.contains("crate::ipc::run_all::on_"),
                "each spawned block must fire a continuation — a wrapper \
                 without its call is a lost dispatch"
            );
            assert!(
                !block.contains("turn_resolve."),
                "latch evidence must be read BEFORE spawning, not inside \
                 the spawned block — the next Started clears it"
            );
        }
    }
}
