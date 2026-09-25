// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The agent task — owns its inbox + conversation, drives the loop.
//!
//! Each agent is a tokio task with its own `mpsc::Receiver<AgentCommand>` inbox
//! and a cloned fan-in sender. It drives the `AgentLoop`, processing commands
//! and emitting events. On shutdown, it triggers memory consolidation.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::agent::failure_triage::{
    self, FailureClass, FailureSite, FailureTriage, TriageAction, TriageDisposition,
};
use crate::agent::AgentLoop;
use crate::memory::consolidation::{consolidate_session, corpus_digest};
use crate::provider::{Message, MessageContent};
use crate::runtime::channels::{
    AgentCommand, AgentEvent, AgentId, ParkReason, UNATTENDED_PROMPT_PREFIX,
};
use crate::runtime::correction::detect_correction;

/// Maximum attempts for a single turn when the *provider* fails (network /
/// stream error), retried with jittered exponential backoff (0.5–1s, 1–2s,
/// 2–4s — see [`crate::error::retry_backoff_ms`]). Distinct from
/// `agent::MAX_RETRIES`, which caps consecutive *tool-execution* errors within
/// a turn. See [`AgentTask::run_turn_with_retry`].
const MAX_PROVIDER_TURN_ATTEMPTS: u32 = 3;

/// The maximum length (in chars) of a provider-error string that is pushed
/// into the conversation as an in-context harness note on terminal failure.
/// Provider error bodies can be huge (HTML error pages, echoed request
/// bodies); capping keeps the note from bloating the next request's context.
const MAX_PROVIDER_ERROR_NOTE_CHARS: usize = 2000;

/// The agent task's state: its conversation history + the agent loop.
pub struct AgentTask {
    pub id: AgentId,
    pub name: String,
    messages: Vec<Message>,
    agent_loop: Arc<AgentLoop>,
    /// The memory session id, started on the first prompt. Used for
    /// consolidation on shutdown.
    session_id: Option<String>,
    /// Consecutive auto-continue turns (reset to 0 on external input: a
    /// Prompt, or a between-turn Suggestion — a user steer or a finished
    /// descendant's completion notification, both real progress that
    /// deserves a fresh budget). When the model finishes a turn
    /// (Finish::Stop) but the workflow still expects progress (Executing
    /// or Reviewing), a synthetic "continue" message is pushed and another
    /// turn runs — up to [`MAX_AUTO_CONTINUE`] times before
    /// parking for the user (prevents infinite loops on genuine deadlocks).
    /// A turn also parks WITHOUT consuming the streak while spawned
    /// descendants are running — see the None arm in `run_turn_with_retry`.
    auto_continue_streak: u32,
    /// Whether this agent was last dispatched unattended (run-all): the
    /// most recent Prompt embedded the unattended preamble
    /// ([`UNATTENDED_PROMPT_PREFIX`]) — the runtime's only harness-visible
    /// dispatch-context signal. Re-derived on EVERY Prompt (assignment, not
    /// OR): a run-all dispatched prompt sets it, a plain user prompt (the
    /// user taking over after a halt or intervention) clears it. A
    /// between-turn Suggestion must NOT clear it — the child-completion
    /// Suggestion (a finished reviewer) is machine progress, not user
    /// presence; the run is still unattended. While set, the auto-continue
    /// gate also covers Planning (see
    /// [`AgentLoop::workflow_expects_progress`]) and the synthetic
    /// continue note says no user input is coming. A user interactively
    /// typing the prefix verbatim gets unattended semantics too —
    /// accepted: the text itself says unattended, and the effect is
    /// bounded (the 12-turn plateau) and bannered (BudgetExhausted parks
    /// in the InputBar).
    unattended: bool,
    /// Dedup guard for `spawn_consolidation`: when `true`, a consolidation
    /// task is already in flight for this agent — a second call returns
    /// early instead of double-spending LLM tokens on overlapping
    /// consolidations. Cleared by the spawned task on completion (success
    /// or failure).
    consolidation_in_flight: Arc<std::sync::atomic::AtomicBool>,
}

/// Maximum consecutive auto-continue turns before the agent parks for user
/// input. Prevents infinite loops when the model keeps finishing without
/// completing the plan (a genuine deadlock the user must break).
const MAX_AUTO_CONTINUE: u32 = 12;

/// Fire a background memory consolidation every N auto-continue turns so
/// working-memory events are compressed + pruned proactively during long
/// auto-continue chains, instead of accumulating unboundedly until exit.
const CONSOLIDATE_EVERY_N_TURNS: u32 = 8;

/// What `run_turn_with_retry` signals back to `run` after a turn (or chain of
/// turns) completes. Determines what `run` does next: continue listening,
/// terminate the agent, compact the context, or clear the conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AfterTurn {
    /// Normal turn end (or interrupt, or final provider failure). The agent
    /// stays alive and returns to `cmd_rx.recv()`.
    Continue,
    /// The turn was cancelled (`StopReason::Cancel`). `run` breaks so `Exited`
    /// fires after the loop.
    Cancelled,
    /// The user requested a context compaction (`/compact`) mid-turn.
    /// `drive_turns` summarizes old messages after the turn ends, then RESUMES
    /// the interrupted work on the compacted conversation: the carried
    /// commands (mid-turn-queued steers from `StopReason::CompactWithSteers`,
    /// never dropped; empty for a plain Compact) are pushed as user messages
    /// and drive the follow-up turn — or, when empty, a harness-continuation
    /// note nudges the model to continue from where it left off.
    Compact(Vec<crate::runtime::SteerPayload>),
    /// The user requested a fresh conversation (`/new`). `run` clears the
    /// message history after the turn ends.
    Clear,
}

/// The outcome of a manual compaction ([`AgentTask::compact_context`]).
/// Distinct from the summarization call's bare `Option<StopReason>` so
/// callers can tell a COMPLETED compaction from a failed one — `drive_turns`
/// resumes the interrupted work only on `Completed` (a failed compact must
/// not push a false "context was compacted" harness note nor resume on the
/// uncompacted conversation).
#[derive(Debug, Clone, PartialEq, Eq)]
enum CompactOutcome {
    /// The summarization completed and the summary was applied (with or
    /// without a token reduction).
    Completed,
    /// The summarization LLM call failed — an Error event was emitted and
    /// the conversation is unchanged.
    Failed,
    /// The user stopped the summarization mid-call (Interrupt = go idle,
    /// Cancel = terminate the agent).
    Stopped(crate::agent::StopReason),
}

/// RAII guard that clears the `consolidation_in_flight` flag when dropped.
///
/// Moved into the spawned consolidation task so the flag is cleared on ALL
/// exit paths — normal completion, error, OR panic (Drop runs during
/// unwinding). Without this, a panic in the consolidation task would leave
/// the flag stuck `true`, permanently blocking future consolidations for
/// that agent.
struct ConsolidationFlagGuard {
    flag: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for ConsolidationFlagGuard {
    fn drop(&mut self) {
        self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl AgentTask {
    pub fn new(id: AgentId, name: String, agent_loop: Arc<AgentLoop>) -> Self {
        Self {
            id,
            name,
            messages: Vec::new(),
            agent_loop,
            session_id: None,
            auto_continue_streak: 0,
            unattended: false,
            consolidation_in_flight: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Build the user-message content for a text + images payload —
    /// delegates to [`crate::agent::AgentLoop::build_user_content`], the ONE
    /// image-handling path shared by the normal prompt arm, every steer
    /// injection site, and the summarization re-injection paths (multimodal
    /// multipart / vision-model fallback / provider-side strip).
    async fn build_user_content(
        &self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        text: String,
        images: &[String],
    ) -> MessageContent {
        self.agent_loop
            .build_user_content(self.id, fanin_tx, text, images)
            .await
    }

    /// Run a turn with the standard retry-on-provider-error logic (up to
    /// [`MAX_PROVIDER_TURN_ATTEMPTS`] attempts with exponential backoff).
    /// Shared by the `Prompt` and `Suggestion` arms so a between-turn steer
    /// (converted to a user message) gets the same error recovery as a regular
    /// prompt.
    ///
    /// Distinct from `agent::MAX_RETRIES` — that constant caps *consecutive
    /// tool-execution errors within a turn*; this one caps *provider-level
    /// turn attempts* (network/stream failures) across a whole turn.
    ///
    /// Returns an [`AfterTurn`] signaling what `run` should do next: `Continue`
    /// for a normal turn end / interrupt / final provider failure, `Cancelled`
    /// when the turn was cancelled (so `run` breaks → `Exited`), `Compact` when
    /// the user requested a context compaction, or `Clear` when the user requested
    /// a fresh conversation.
    ///
    /// If the turn soft-stopped on a mid-turn steer (`StopReason::Steer`), the
    /// steer is pushed as a regular user message and a follow-up turn runs —
    /// repeating until a turn ends without a steer (so a chain of steers each
    /// soft-stops + resumes). Mirrors the between-turn Suggestion path: the
    /// steer becomes a user message that drives the next turn.
    async fn run_turn_with_retry(
        &mut self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
    ) -> AfterTurn {
        loop {
            let outcome = self.run_turn_attempt(fanin_tx, cmd_rx).await;
            let Some(outcome) = outcome else {
                return AfterTurn::Continue;
            }; // final provider failure
            let mut stop_reason = outcome.stop_reason;
            // Drain any commands queued in the window between the turn ending and
            // now (e.g. a steer cancellation that arrived just after the turn
            // soft-stopped) so a cancelled steer is dropped BEFORE it's injected.
            // `fold` applies the same accumulation rule as the turn's select!
            // (steers accumulate; a CancelSuggestion removes a matching steer and
            // may collapse it to None; Interrupt/Compact preserve queued steers;
            // Cancel/Clear override). This also folds any other command that
            // arrived in that window, so nothing queued is silently dropped.
            while let Ok(cmd) = cmd_rx.try_recv() {
                crate::agent::StopReason::fold(&mut stop_reason, cmd);
            }
            match stop_reason {
                Some(crate::agent::StopReason::Steer(steers))
                | Some(crate::agent::StopReason::InterruptWithSteers(steers)) => {
                    // The turn stopped (soft-stop on a steer, or the user pressed
                    // Stop with commands already queued). Push EVERY queued steer
                    // as a regular user message + emit SuggestionInjected for
                    // each (so the UI marks them landed), then run a follow-up
                    // turn over all of them — in arrival order, none dropped.
                    // InterruptWithSteers is the "Stop flushes pending commands"
                    // case: the interrupt already ended the turn; the queued
                    // commands start immediately.
                    for steer in steers {
                        let crate::runtime::SteerPayload { text, images } = steer;
                        let _ = fanin_tx
                            .send((
                                self.id,
                                AgentEvent::SuggestionInjected {
                                    text: text.clone(),
                                    images: images.clone(),
                                },
                            ))
                            .await;
                        // Same image handling as a normal prompt (multimodal
                        // multipart / vision fallback) — a steered image must
                        // reach the model exactly like a prompt's.
                        let content = self
                            .build_user_content(fanin_tx, text, &images)
                            .await;
                        self.messages.push(Message {
                            content,
                            ..Message::user_text("")
                        });
                    }
                    // Loop: run the follow-up turn (which may itself soft-stop).
                }
                Some(crate::agent::StopReason::Cancel) => return AfterTurn::Cancelled,
                Some(crate::agent::StopReason::Compact) => return AfterTurn::Compact(vec![]),
                // `/compact` with commands queued mid-turn: compact on the
                // quiesced conversation, then run the queued commands (they must
                // not be dropped — the same bug class as Stop-with-pending).
                Some(crate::agent::StopReason::CompactWithSteers(steers)) => {
                    return AfterTurn::Compact(steers)
                }
                Some(crate::agent::StopReason::Clear) => return AfterTurn::Clear,
                // User pressed Stop — never auto-resume, always park. The
                // turn is over; the next user prompt (or steer) starts a
                // fresh turn. Emit the park evidence so the UI can
                // distinguish an interrupted turn from a hang (2027-01-07
                // live: a mid-Executing stop during a shell batch looked
                // like a hang and needed a manual "c" to resume).
                Some(crate::agent::StopReason::Interrupt) => {
                    let _ = fanin_tx
                        .send((
                            self.id,
                            AgentEvent::Parked {
                                reason: ParkReason::Interrupted,
                                workflow_state: self
                                    .agent_loop
                                    .workflow_state_label()
                                    .await,
                                descendants_running: self
                                    .agent_loop
                                    .has_running_descendants()
                                    .await,
                                auto_continue_streak: self.auto_continue_streak,
                            },
                        ))
                        .await;
                    return AfterTurn::Continue;
                }
                // Normal turn end (Finish::Stop, no steer/compact/cancel). If the
                // workflow is in Executing or Reviewing state (an in-progress plan
                // or the closing sequence), push a synthetic "continue" message and
                // run another turn — so the agent doesn't park and wait for the user
                // to type "continue" after every tool round. Bounded by
                // MAX_AUTO_CONTINUE so a genuine deadlock (model keeps finishing
                // without progress) parks for the user after 12 consecutive
                // auto-continues.
                // EXCEPTION: while spawned descendants (reviewers, parallel
                // background agents) are still running, the parent PARKS instead —
                // the dispatch state-transition gate refuses every workflow
                // transition while subagents run, so a synthetic continue turn
                // could make no plan progress; the child-completion Suggestion
                // resumes the parent exactly once (Suggestion arm, below).
                // EXCEPTION 2: subagents ALWAYS park. They are single-task (the
                // spawner stamps set_is_subagent for every parented loop), and
                // historically their workflow mirrored the main plan's derived
                // state (Workflow::load_latest → Executing/Reviewing), so
                // without this guard a finished reviewer self-continued up to
                // MAX_AUTO_CONTINUE synthetic turns — token burn, deliverable-
                // mutation risk (write_review_report), and a held descendant-park
                // that blocked the dispatch state-transition gate (High 1 of the
                // 2026-12-31 auto-continue-reviewing review, observed live). The
                // child-completion Suggestion resumes the parent instead.
                // Since 2026-01-03 the spawn path stamps the dedicated
                // WorkflowState::Subagent (no derived lifecycle state), so
                // workflow_expects_progress is already false for subagents and
                // this guard is belt-and-braces: it still catches a future
                // spawn path that forgets the stamp.
                //
                // EXCEPTION 3: an UNATTENDED agent (run-all dispatched — its
                // Prompt embedded the unattended preamble) also auto-continues
                // in Planning: a premature turn end mid-exploration would
                // otherwise park with nobody watching and halt the whole run
                // (backlog a6a7727a, live 2027-01-07). Interactive Planning
                // still parks — a deliberate turn end there is usually a
                // question-wait for the user, and a synthetic continue would
                // self-answer it.
                None => {
                    let expects_progress = self
                        .agent_loop
                        .workflow_expects_progress(self.unattended)
                        .await;
                    let descendants_running =
                        self.agent_loop.has_running_descendants().await;
                    let subagent = self.agent_loop.is_subagent();
                    if self.auto_continue_streak < MAX_AUTO_CONTINUE
                        && expects_progress
                        && !descendants_running
                        && !subagent
                    {
                        self.auto_continue_streak += 1;
                        // Mid-session consolidation checkpoint (the third
                        // auto-continuation behavior): every 8 auto-continue
                        // turns, fire a background consolidation so working-memory
                        // events are compressed + pruned proactively — instead of
                        // accumulating unboundedly until session exit. Fire-and-
                        // forget (spawns a task holding Arc clones); doesn't block
                        // the turn. Only fires during auto-continue chains, which
                        // is exactly when context pressure builds up.
                        if self.auto_continue_streak % CONSOLIDATE_EVERY_N_TURNS == 0 {
                            self.spawn_consolidation();
                        }
                        // Unattended variant: the model may have ended its
                        // turn with a question despite the preamble's
                        // "do not ask" rule — say explicitly that no user
                        // input is coming so it proceeds on best judgment
                        // instead of waiting for an answer.
                        let note = if self.unattended {
                            "[harness note] continue from where you left off. \
                             (You are running unattended — no user input is \
                             coming; use your best judgment.)"
                        } else {
                            "[harness note] continue from where you left off."
                        };
                        self.messages.push(Message::user_text(note));
                        // Loop: run the follow-up turn.
                        continue;
                    }
                    // Parked evidence: every main-agent park reports the
                    // pre-stall state (workflow state, descendants,
                    // streak) to the watchdog ring via the forwarder — and
                    // the two manual-input reasons (budget exhausted while
                    // work is expected; user interrupt above) surface in
                    // the UI so a parked turn never looks like a hang.
                    // Subagents are excluded: their routine turn-end parks
                    // would flood the ring. (2027-01-07 live: two parks
                    // needed a manual "c" and the cause had to be inferred
                    // from static reading.)
                    if !subagent {
                        let reason = if descendants_running {
                            ParkReason::WaitingForDescendants
                        } else if !expects_progress {
                            ParkReason::NoWorkExpected
                        } else {
                            ParkReason::BudgetExhausted
                        };
                        let _ = fanin_tx
                            .send((
                                self.id,
                                AgentEvent::Parked {
                                    reason,
                                    workflow_state: self
                                        .agent_loop
                                        .workflow_state_label()
                                        .await,
                                    descendants_running,
                                    auto_continue_streak: self.auto_continue_streak,
                                },
                            ))
                            .await;
                    }
                    return AfterTurn::Continue;
                }
            }
        }
    }

    /// Drive turns and their aftermath until the agent should go idle
    /// (returns `false`) or terminate (returns `true`, on `Cancel`).
    ///
    /// Handles the [`AfterTurn`] follow-ups in a loop so a mid-turn `/compact`
    /// RESUMES the interrupted work instead of leaving the agent idle (user
    /// report 2026-08-22: "when you compact mid-flight, it works but it stops
    /// execution — it should continue afterwards"): after compaction
    /// completes, queued steers are pushed as user messages (never dropped)
    /// and a follow-up turn runs on the compacted conversation; with no queued
    /// steers a harness note nudges the model to continue from where it left
    /// off. A FAILED compaction (provider error) goes idle instead of resuming
    /// — no false "context was compacted" note, and no immediate follow-up
    /// turn burning provider-error retries when the provider may be down. An
    /// `Interrupt` during the summarization call is honored as a stop
    /// (the agent goes idle; pushed steers stay in history as context for the
    /// next prompt). `Clear` wipes the conversation and goes idle — there is
    /// nothing to resume after a fresh start.
    async fn drive_turns(
        &mut self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
    ) -> bool {
        loop {
            match self.run_turn_with_retry(fanin_tx, cmd_rx).await {
                AfterTurn::Cancelled => return true,
                AfterTurn::Continue => return false,
                AfterTurn::Clear => {
                    self.clear_context(fanin_tx).await;
                    return false;
                }
                AfterTurn::Compact(steers) => {
                    let outcome = self.compact_context(fanin_tx, cmd_rx).await;
                    if matches!(
                        outcome,
                        CompactOutcome::Stopped(crate::agent::StopReason::Cancel)
                    ) {
                        return true;
                    }
                    // Commands queued mid-turn run on the compacted
                    // conversation — pushed as user messages so the follow-up
                    // turn below drives them (never dropped).
                    for steer in &steers {
                        let crate::runtime::SteerPayload { text, images } = steer;
                        let _ = fanin_tx
                            .send((
                                self.id,
                                AgentEvent::SuggestionInjected {
                                    text: text.clone(),
                                    images: images.clone(),
                                },
                            ))
                            .await;
                        // Same image handling as a normal prompt (multimodal
                        // multipart / vision fallback) — a steered image must
                        // reach the model exactly like a prompt's.
                        let content = self
                            .build_user_content(fanin_tx, text.clone(), images)
                            .await;
                        self.messages.push(Message {
                            content,
                            ..Message::user_text("")
                        });
                    }
                    match outcome {
                        CompactOutcome::Completed => {
                            if steers.is_empty() {
                                // No queued commands — nudge the model to
                                // resume the work the compacted turn was
                                // interrupted from.
                                self.messages.push(Message::user_text(
                                    "[harness note] Context was compacted mid-task — \
                                     continue from where you left off.",
                                ));
                            }
                            // Loop: run the follow-up turn on the compacted
                            // conversation (which may itself end in another
                            // AfterTurn, handled by the next iteration).
                        }
                        CompactOutcome::Failed => {
                            // The compaction failed (the Error event was
                            // already emitted) — go idle instead of pushing a
                            // false "context was compacted" note and resuming
                            // on the uncompacted conversation, which would
                            // also burn the follow-up turn's provider-error
                            // retries when the provider is down (review
                            // finding). Pushed steers stay in history as
                            // context for the next prompt.
                            return false;
                        }
                        CompactOutcome::Stopped(_) => {
                            // Interrupt during summarization — the user
                            // pressed Stop; honor it and go idle.
                            return false;
                        }
                    }
                }
            }
        }
    }

    /// A single turn attempt with provider-error retry. Returns the
    /// [`TurnOutcome`] on success, or `None` on a final provider failure.
    async fn run_turn_attempt(
        &mut self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
    ) -> Option<crate::agent::TurnOutcome> {
        let mut attempt = 0u32;
        let mut tried_fallback = false;
        // Failure triage (Laya, opt-in; plan 02deea7c): the logged row from
        // the most recent classified attempt — resolved when its outcome is
        // known (a later attempt succeeding = `retry_succeeded`; the
        // final-failure path = `retry_failed`).
        let mut pending_triage: Option<failure_triage::PendingFailureRow> = None;
        // The class that SKIPPED the backoff ladder (needs-user / permanent).
        // Named in the final-failure note + event so neither ever claims
        // "retries exhausted" for a skip.
        let mut skip_triage: Option<FailureClass> = None;
        loop {
            match self
                .agent_loop
                .run_turn(
                    &mut self.messages,
                    fanin_tx,
                    self.id,
                    cmd_rx,
                    self.session_id.as_deref(),
                )
                .await
            {
                Ok(outcome) => {
                    if let Some(pending) = pending_triage.take() {
                        failure_triage::resolve_failure(
                            pending,
                            TriageDisposition::RetrySucceeded,
                        );
                    }
                    return Some(outcome);
                }
                Err(e) => {
                    // 429 fallback: a rate limit means the provider is out of
                    // quota — retrying the SAME provider never helps ("stop
                    // after a single 429"). Instead, try to switch to the same
                    // model on a DIFFERENT endpoint (the automatic cross-
                    // provider fallback). At most one switch per turn: if the
                    // fallback also 429s, or no alternate exists, go straight
                    // to final failure rather than cascading through every
                    // endpoint or burning backoff sleeps against a rate-limited
                    // provider.
                    if e.is_rate_limited() {
                        if !tried_fallback {
                            tried_fallback = true;
                            if let Some(info) = self.agent_loop.try_429_fallback() {
                                let _ = fanin_tx
                                    .send((
                                        self.id,
                                        AgentEvent::Error {
                                            error: format!(
                                                "Rate limited (429) on {}@{} — \
                                                 switching to alternate provider {}",
                                                info.model, info.from_endpoint, info.to_endpoint
                                            ),
                                            retrying: true,
                                        },
                                    ))
                                    .await;
                                continue; // retry run_turn with the fallback provider
                            }
                        }
                        // No alternate found, or the fallback also 429'd —
                        // don't retry the same rate-limited provider with
                        // backoff. Go straight to final failure.
                        attempt = MAX_PROVIDER_TURN_ATTEMPTS;
                    } else if e.is_non_retryable() {
                        // Non-retryable errors (context overflow, auth, model
                        // not-found) skip the turn-level retry stack too —
                        // jumping straight to the final-failure path below
                        // (in-context note + non-retrying Error + return None)
                        // instead of burning 3×~1s/2s jittered backoff sleeps
                        // on a condition that can never succeed.
                        attempt = MAX_PROVIDER_TURN_ATTEMPTS;
                    } else {
                        // Failure triage (Laya, opt-in; plan 02deea7c):
                        // classify the provider error ONCE per attempt
                        // sequence. The classifier only ADDS — a confident
                        // needs-user / permanent reading skips the useless
                        // backoff ladder (the same "set attempt to the cap"
                        // trick the non-retryable arm uses), while a
                        // transient / flaky-test reading — or no usable
                        // answer at all — keeps today's ladder
                        // byte-for-byte. A 429 never arrives here (the
                        // fallback guard above owns it) and is never
                        // classified.
                        let mut classified = None;
                        if pending_triage.is_none() && skip_triage.is_none() {
                            if let Some(gate) = self.agent_loop.failure_triage() {
                                let error_text = e.to_string();
                                if let FailureTriage::Classified { class, confidence } =
                                    gate.triage(&error_text).await
                                {
                                    classified = Some((gate, class, confidence, error_text));
                                }
                            }
                        }
                        match classified {
                            Some((gate, class, confidence, error_text)) => match class {
                                FailureClass::NeedsUser | FailureClass::Permanent => {
                                    // The skip: logged now, disposition
                                    // `escalated` — the ladder was
                                    // deliberately not attempted, so the
                                    // counterfactual is unobserved.
                                    let pending = gate.log_failure(
                                        FailureSite::ProviderTurn,
                                        None,
                                        &error_text,
                                        class,
                                        confidence,
                                        TriageAction::SkipRetry,
                                    );
                                    failure_triage::resolve_failure(
                                        pending,
                                        TriageDisposition::Escalated,
                                    );
                                    skip_triage = Some(class);
                                    attempt = MAX_PROVIDER_TURN_ATTEMPTS;
                                }
                                FailureClass::Transient | FailureClass::FlakyTest => {
                                    // No behavior change — the existing
                                    // ladder decides — but the row is logged
                                    // now and resolved on its outcome.
                                    pending_triage = Some(gate.log_failure(
                                        FailureSite::ProviderTurn,
                                        None,
                                        &error_text,
                                        class,
                                        confidence,
                                        TriageAction::Ladder,
                                    ));
                                    attempt += 1;
                                }
                            },
                            None => attempt += 1,
                        }
                    }
                    if attempt < MAX_PROVIDER_TURN_ATTEMPTS {
                        // Surface a retry note and retry after backoff.
                        // `retrying: true` tells the frontend to keep
                        // `running = true` (no string-matching needed).
                        let _ = fanin_tx
                            .send((
                                self.id,
                                AgentEvent::Error {
                                    error: format!(
                                        "{e} — retrying (attempt {attempt}/{MAX_PROVIDER_TURN_ATTEMPTS})"
                                    ),
                                    retrying: true,
                                },
                            ))
                            .await;
                        // Jittered exponential backoff (equal jitter: half
                        // fixed + half random) so concurrent agents retrying
                        // against the same recovering endpoint don't stay in
                        // lockstep — see `retry_backoff_ms`.
                        let delay_ms = crate::error::retry_backoff_ms(attempt);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        // Attribute the backoff sleep (the actual jittered
                        // duration) to the NEXT turn attempt's first trace
                        // record: parked on the provider here, stamped when
                        // the retry's record is created — the same pattern
                        // complete_with_retry uses — so the wait between turn
                        // attempts shows as `backoff_ms` in the graph instead
                        // of an invisible gap between per-attempt records.
                        self.agent_loop.provider().record_backoff_ms(delay_ms as u32);
                    } else {
                        // Final failure — surface and stop. Also record the
                        // error in-context so the NEXT turn (a re-prompt, a
                        // backlog auto-feed item, or a retry of the same goal)
                        // shows the model WHY its last request failed, letting
                        // it self-correct (fix malformed args, shrink an
                        // oversized request) instead of repeating the same
                        // failing call. Without this the model is blind to
                        // provider errors (they only ever went to the UI).
                        // Truncated to cap context growth; char-safe via
                        // chars().take (no mid-UTF8 split). Pushed once per
                        // terminal failure (this branch runs once per
                        // run_turn_attempt).
                        let err_text = e.to_string();
                        let truncated: String = err_text
                            .chars()
                            .take(MAX_PROVIDER_ERROR_NOTE_CHARS)
                            .collect();
                        // Non-retryable errors (context overflow, auth, model
                        // not-found) skip the retry stack entirely — 0 retries
                        // occurred — so the note must not say "retries
                        // exhausted". A 429 with no alternate provider (or
                        // after the fallback also 429'd) likewise failed
                        // without retries. The model still gets the actionable
                        // error text either way.
                        // Failure triage: a classified attempt whose retry
                        // never landed resolves `retry_failed` here.
                        if let Some(pending) = pending_triage.take() {
                            failure_triage::resolve_failure(
                                pending,
                                TriageDisposition::RetryFailed,
                            );
                        }
                        let qualifier = if e.is_rate_limited() {
                            "rate limited (429), no alternate provider found"
                        } else if e.is_classified_skip() {
                            "classified failure-triage skip (retrying cannot help)"
                        } else if let Some(class) = skip_triage {
                            match class {
                                FailureClass::NeedsUser => {
                                    "classified needs_user — retrying cannot help \
                                     (credentials, permissions, or a quota need the user)"
                                }
                                _ => {
                                    "classified permanent — retrying cannot help \
                                     (the request itself must change)"
                                }
                            }
                        } else if e.is_non_retryable() {
                            "non-retryable provider error"
                        } else {
                            "provider error, retries exhausted"
                        };
                        self.messages.push(Message::user_text(format!(
                            "[harness note] The previous LLM request failed terminally \
                             ({qualifier}): {truncated}"
                        )));
                        // The user-facing event: for a 429 with no alternate,
                        // the raw "HTTP 429" alone doesn't tell the user what
                        // to do — augment it with the qualifier + an actionable
                        // hint (switch models / configure another endpoint).
                        // Other error kinds surface the raw text unchanged.
                        let event_text = if e.is_rate_limited() {
                            format!(
                                "{e} — {qualifier}. Switch models via the status-bar picker, \
                                 or configure another endpoint serving this model."
                            )
                        } else if skip_triage.is_some() {
                            format!("{e} — {qualifier}.")
                        } else {
                            err_text
                        };
                        let _ = fanin_tx
                            .send((
                                self.id,
                                AgentEvent::Error {
                                    error: event_text,
                                    retrying: false,
                                },
                            ))
                            .await;
                        return None;
                    }
                }
            }
        }
    }

    /// Run the agent task: process commands from the inbox, drive turns.
    pub async fn run(
        mut self,
        mut cmd_rx: mpsc::Receiver<AgentCommand>,
        fanin_tx: mpsc::Sender<(AgentId, AgentEvent)>,
    ) {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                AgentCommand::Prompt { text, images } => {
                    // Real user input — reset the auto-continue streak so the
                    // next 12 auto-continues are available (the user is engaged
                    // again, so a fresh budget is appropriate). The Suggestion
                    // arm below resets too: a between-turn Suggestion is
                    // external progress (a user steer or a finished
                    // descendant's completion notification), not mere guidance.
                    self.auto_continue_streak = 0;
                    // Re-derive the unattended (run-all dispatch) mode from
                    // the prompt marker: a run-all dispatched prompt embeds
                    // the unattended preamble as its prefix — the runtime's
                    // only harness-visible dispatch-context signal.
                    // Assignment, not OR: a plain user prompt (the user
                    // taking over after a halt or intervention) CLEARS it,
                    // so interactive Planning question-waits park as
                    // before. A Suggestion must not touch it (see the
                    // field doc).
                    self.unattended = text.starts_with(UNATTENDED_PROMPT_PREFIX);
                    // Start a memory session on the first prompt (if a memory
                    // store is available).
                    if self.session_id.is_none() {
                        if let Some(store) = self.agent_loop.memory_handle() {
                            if let Ok(session) = store.start_session(&self.name).await {
                                self.agent_loop.set_session_id(session.id.clone());
                                self.session_id = Some(session.id);
                            }
                        }
                    }

                    // Correction-detection hook: if the prompt looks like the
                    // user is correcting the agent (rule violation, "that's
                    // wrong", "did that land?", brevity nudges, ...), record it
                    // as a Working-tier memory NOW — before the turn runs, so
                    // the lesson is captured even if the agent then fails to
                    // `memory_write` it itself. This is deterministic and does
                    // not rely on the model's in-the-moment judgment. Silent
                    // (no approval, no event), mirroring the tool-event capture.
                    //
                    // Run on the raw `text` (before any image handling) so image
                    // attachments don't interfere. Guarded on a memory store
                    // being present (tests may not have one).
                    if let Some(trigger) = detect_correction(&text) {
                        if let Some(store) = self.agent_loop.memory_handle() {
                            let note = format!(
                                "User correction detected (trigger: \"{trigger}\"). \
                                 Review this exchange at consolidation time and distill \
                                 a procedural/semantic rule so the mistake is not repeated. \
                                 Prompt: {text}"
                            );
                            // Fire-and-forget stays (the capture must never
                            // block or break the turn), but a failed durable
                            // write must not vanish silently — log it
                            // (quality review LOW 6, class-closure).
                            if let Err(e) = store
                                .record_tool_event(
                                    self.session_id.as_deref(),
                                    "user_correction",
                                    serde_json::json!({
                                        "prompt": text,
                                        "trigger": trigger,
                                    }),
                                    &note,
                                    // Corrections are a form of negative feedback —
                                    // surface the trigger as the "error" channel so
                                    // it's easy to filter for them later.
                                    Some(trigger),
                                )
                                .await
                            {
                                eprintln!("mnemo: failed to record user-correction event: {e}");
                            }
                        }
                    }

                    // Add the user message. Image handling (multimodal
                    // multipart / vision fallback / provider-side strip)
                    // lives in build_user_content — the ONE path, shared
                    // with every steer injection site so a steered image
                    // rides the exact same handling as a prompt's.
                    let content = self
                        .build_user_content(&fanin_tx, text, &images)
                        .await;
                    self.messages.push(Message {
                        content,
                        ..Message::user_text("")
                    });
                    // Drive the turn and its aftermath: a mid-turn `/compact`
                    // resumes the work on the compacted conversation (or runs
                    // queued commands); Cancel breaks the command loop.
                    if self.drive_turns(&fanin_tx, &mut cmd_rx).await {
                        break;
                    }
                }
                AgentCommand::Suggestion(payload) => {
                    // A steer that lands between turns (the agent is idle) is
                    // converted to a regular user message and triggers a new
                    // turn — there's no in-flight work to steer, so it becomes
                    // a follow-up prompt the agent responds to. Mid-work
                    // steers (handled in run_turn's select!) stay as System
                    // messages — guidance the model sees on the next iteration
                    // without starting a new turn.

                    // External progress, not mere guidance: a between-turn
                    // Suggestion is either a user steer or a finished
                    // descendant's completion notification — the closing
                    // sequence's ONLY resume mechanism. Reset the
                    // auto-continue streak like a Prompt (2027-01-07 live
                    // regression: with the budget exhausted, the
                    // Suggestion-driven turn ended, the None-arm gate failed
                    // on `streak < MAX`, and the closing sequence parked
                    // until the user typed "c").
                    self.auto_continue_streak = 0;

                    // Start a memory session on the first message (if not
                    // already started — a steer can arrive before any prompt).
                    if self.session_id.is_none() {
                        if let Some(store) = self.agent_loop.memory_handle() {
                            if let Ok(session) = store.start_session(&self.name).await {
                                self.agent_loop.set_session_id(session.id.clone());
                                self.session_id = Some(session.id);
                            }
                        }
                    }

                    let crate::runtime::SteerPayload { text, images } = payload;
                    // Notify the UI that the steer landed (was injected into
                    // the conversation) so it can highlight + clear the backlog
                    // entry. Emitted before the turn so the steer bubble
                    // appears above the streaming response.
                    let _ = fanin_tx
                        .send((
                            self.id,
                            AgentEvent::SuggestionInjected {
                                text: text.clone(),
                                images: images.clone(),
                            },
                        ))
                        .await;

                    // Convert the steer to a user message (same image handling
                    // as a normal prompt — multimodal multipart / vision
                    // fallback) and run a turn.
                    let content = self
                        .build_user_content(&fanin_tx, text, &images)
                        .await;
                    self.messages.push(Message {
                        content,
                        ..Message::user_text("")
                    });
                    // Drive the turn and its aftermath: a mid-turn `/compact`
                    // resumes the work on the compacted conversation (or runs
                    // queued commands); Cancel breaks the command loop.
                    if self.drive_turns(&fanin_tx, &mut cmd_rx).await {
                        break;
                    }
                }
                AgentCommand::CancelSuggestion(_) => {
                    // Between turns there's nothing queued to cancel — a
                    // between-turn Suggestion is consumed immediately as a
                    // turn (above). Mid-turn cancels are handled by
                    // `StopReason::fold` and the pre-inject drain in
                    // `run_turn_with_retry`. No-op here.
                }
                AgentCommand::Interrupt => {
                    // The interrupt is handled in the approval gate / stream.
                    // If we're between turns, it's a no-op.
                }
                AgentCommand::Compact => {
                    // Between-turns compaction: summarize old messages. A
                    // Cancel arriving mid-summarization terminates the agent
                    // (mirrors the post-turn Compact path in drive_turns).
                    if matches!(
                        self.compact_context(&fanin_tx, &mut cmd_rx).await,
                        CompactOutcome::Stopped(crate::agent::StopReason::Cancel)
                    ) {
                        break;
                    }
                }
                AgentCommand::Clear => {
                    // Between-turns clear: wipe the conversation history.
                    self.clear_context(&fanin_tx).await;
                }
                AgentCommand::Cancel => {
                    // Emit Finished + break immediately so the Exited event
                    // fires + the agent's tab disappears right away. The
                    // session's working-memory consolidation is best-effort
                    // and runs in the background AFTER the loop (see the
                    // post-loop `spawn_consolidation` call below), which covers
                    // every exit path — Cancel `break` and natural loop exit —
                    // so it is NOT spawned here (doing so would double-fire).
                    let _ = fanin_tx
                        .send((
                            self.id,
                            AgentEvent::Finished {
                                reason: crate::provider::FinishReason::Stop,
                            },
                        ))
                        .await;
                    break;
                }
            }
        }

        // The task is truly terminating (inbox closed or cancelled). Emit
        // `Exited` so the forwarder removes the agent from the manager. This
        // is distinct from `Finished`, which fires at the end of every turn
        // while the agent stays alive and ready for the next prompt.
        //
        // Consolidate the session's working memory on EVERY termination —
        // the single best-effort background spawn below covers both the
        // inbox-closed exit and the Cancel break above (there is no separate
        // Cancel-path spawn), so working-tier events are compressed + pruned
        // whenever a session ends.
        self.spawn_consolidation();
        let _ = fanin_tx.send((self.id, AgentEvent::Exited)).await;
    }

    /// Best-effort background consolidation of the current session's working
    /// memory into an episodic summary (+ semantic/procedural extraction when
    /// an LLM is available), then prune the raw working-tier events.
    ///
    /// Fire-and-forget: spawns a task holding `Arc` clones of the store +
    /// provider + session id, so it runs safely after this `AgentTask` is
    /// dropped. Never blocks the caller (used on both the Cancel and the
    /// natural-termination paths so the agent exits promptly). No-op when
    /// there is no memory store or no active session.
    fn spawn_consolidation(&self) {
        if let Some(store) = self.agent_loop.memory_handle() {
            if let Some(sid) = &self.session_id {
                // Dedup guard: if a consolidation is already in flight for
                // this agent, skip — overlapping consolidations double-spend
                // LLM tokens and can write duplicate episodic rows. The flag
                // is cleared by the spawned task on completion (success or
                // failure), so a later call after this one finishes runs.
                if self
                    .consolidation_in_flight
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    return;
                }
                let store = store.clone();
                let provider = self.agent_loop.provider();
                let sid = sid.clone();
                // RAII guard: clears the dedup flag when the spawned task
                // ends (success, failure, OR panic — Drop runs during
                // unwinding). Moved into the task below.
                let guard = ConsolidationFlagGuard {
                    flag: self.consolidation_in_flight.clone(),
                };
                // Digest the accumulated plan/review corpus so extraction
                // merges this session's knowledge with what earlier
                // plans/reviews established (learning across sessions).
                // N5 (2026-06-14): the digest reads up to ~20 files sync —
                // compute it INSIDE the spawned task (owned paths moved in)
                // so the agent's exit path never blocks on disk I/O, keeping
                // this function's "never blocks the caller" promise true.
                let plans_dir = self.agent_loop.plans_dir().to_path_buf();
                let reviews_dir = plans_dir
                    .parent()
                    .map(|p| p.join("reviews"))
                    .unwrap_or_else(|| std::path::PathBuf::from(".coding/reviews"));
                tokio::spawn(async move {
                    // Move the RAII guard into the task so the dedup flag is
                    // cleared when this task ends (success, failure, OR
                    // panic — Drop runs during unwinding).
                    let _guard = guard;
                    // The digest reads up to ~20 files synchronously — run it
                    // on the blocking pool so it never occupies a tokio
                    // worker either (review I2, 2026-08-18). Paths are owned;
                    // a join error (unreachable short of a panic) degrades to
                    // an empty corpus — consolidation still runs.
                    let corpus = match tokio::task::spawn_blocking(move || {
                        corpus_digest(&plans_dir, &reviews_dir)
                    })
                    .await
                    {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!(
                                "memory: corpus digest task failed during consolidation: {e}"
                            );
                            String::new()
                        }
                    };
                    if let Err(e) = store.end_session(&sid).await {
                        eprintln!("memory: end_session({sid}) failed during consolidation: {e}");
                    }
                    if let Err(e) =
                        consolidate_session(store.as_ref(), &sid, Some(provider.as_ref()), &corpus)
                            .await
                    {
                        eprintln!("memory: consolidate_session({sid}) failed: {e}");
                    }
                    // The dedup flag is cleared by `_guard`'s Drop impl when
                    // this task ends (success, failure, OR panic).
                });
            }
        }
    }

    /// Manually compact the context: summarize old messages into a summary
    /// system message, keeping the system prompt + recent messages. Honors
    /// interrupts during the summarization LLM call (abandoning the summary on
    /// Interrupt/Cancel — no data loss). Emits a `ContextUsage` event with the
    /// new token count so the UI updates, and announces the compaction in the
    /// transcript: `CompactStarted` up front, then a paired end — `Compacted`
    /// (before → after, or the no-reduction note) on completion, `Error` on
    /// failure, and an "interrupted" note when the user stops the
    /// summarization (never a dangling "Compacting context…"). Backed by
    /// `/compact` + the context-popup Compact button.
    ///
    /// Returns the [`CompactOutcome`]: `Stopped(Cancel)` means the user
    /// dismissed the agent mid-compaction — the caller must `break` so
    /// `Exited` fires; `Stopped(Interrupt)` means the user pressed Stop — the
    /// agent stays alive and returns to idle; `Failed` means the summarization
    /// call errored (conversation unchanged, Error event emitted); `Completed`
    /// means the summary was applied. Non-interrupt commands
    /// (Suggestion/Prompt) buffered during the call are re-injected as
    /// messages (mirrors turn.rs) so no user input is lost.
    async fn compact_context(
        &mut self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
    ) -> CompactOutcome {
        // Announce the start on every manual path (slash command, popup
        // button, mid-turn compact) — paired with a Compacted (completion) or
        // Error (failure) event at the end so the transcript never shows a
        // dangling "Compacting context…".
        let _ = fanin_tx.send((self.id, AgentEvent::CompactStarted)).await;
        // [models.summarize] routes the summary to the dedicated slot when
        // set; unset rides the turn's provider (today's behavior). The
        // run-all between-items compact funnels through here too.
        let turn_provider = self.agent_loop.provider();
        let provider = self.agent_loop.summarize_provider(&turn_provider);
        let context_manager = self.agent_loop.context_manager();
        // Token count before compaction — used for the Compacted confirmation
        // event (before → after) so the user sees the reduction.
        let before = crate::agent::context::ContextManager::count_tokens(&self.messages) as u32;
        // Token-optimizer lever 4 (backlog e4a50d22): the manual/slash path
        // gets the same compaction-survival treatment as the auto path —
        // archive the dropping region first, hand the summarizer the
        // must-preserve decisions, and note the digest afterwards. Fail-open
        // throughout; with the flag off this is byte-identical to today.
        let survival = self.agent_loop.optimizer_config().compaction_survival;
        let decisions = if survival {
            crate::agent::optimizer::extract_decisions(&self.messages)
        } else {
            Vec::new()
        };
        // Lever 6 counters: the manual compaction path feeds the score's
        // decision-density signal exactly like the auto path in turn.rs.
        self.agent_loop.optimizer_state.note_decisions(decisions.len());
        let preserve_block = crate::agent::context::must_preserve_block(&decisions);
        let dropped_digest = if survival {
            crate::agent::optimizer::build_digest(crate::agent::optimizer::dropped_region(
                &self.messages,
                6,
            ))
        } else {
            String::new()
        };
        let checkpoint_id = if survival {
            self.agent_loop
                .checkpoint_before_compaction(self.session_id.as_deref(), &self.messages, 6)
                .await
        } else {
            None
        };
        let (summarized, mut buffered, stop, summarize_usage) = match context_manager
            .summarize_with_interrupt_opts(
                &self.messages,
                6,
                // The TURN provider is what sends the compacted history next, so
                // the result is bounded by its window, not the summarizer's.
                crate::agent::context::sendable_budget(turn_provider.capabilities()),
                provider.as_ref(),
                (!preserve_block.is_empty()).then_some(preserve_block.as_str()),
                cmd_rx,
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                // Surface the failure instead of silently swallowing it — the
                // user asked to compact and should see why nothing happened
                // (previously `.unwrap_or_else` hid the error entirely).
                // `retrying: true` = a transient note, not a fatal agent
                // failure: the agent stays alive and idle (the frontend keeps
                // `running` unchanged for retrying errors).
                let _ = fanin_tx
                    .send((
                        self.id,
                        AgentEvent::Error {
                            error: format!("context compaction failed: {e}"),
                            retrying: true,
                        },
                    ))
                    .await;
                // R21 (round-1 review LOW 5): the summarizer's own failure is
                // invisible to the main loop's Usage arm — record it tagged
                // purpose='summarize' so the mega-prompt's cost shows even
                // when it fails. The estimate uses the full conversation (the
                // summary prompt embeds it serialized).
                self.agent_loop.record_stats_row(
                    self.session_id.as_deref(),
                    provider.as_ref(),
                    crate::agent::StatsRow {
                        prompt_tokens: crate::provider::estimate_prompt_tokens(
                            &self.messages,
                            &[],
                        ) as u32,
                        completion_tokens: 0,
                        reasoning_tokens: 0,
                        cached_tokens: None,
                        ttft_ms: None,
                        generation_ms: None,
                        outcome: Some("error".into()),
                        purpose: Some("summarize".into()),
                    },
                );
                return CompactOutcome::Failed;
            }
        };
        // R21: compaction's own mega-prompt is invisible to the main loop's
        // Usage arm — record it tagged purpose='summarize' so its cost (up to
        // ~300K tokens, guaranteed 0% cache) shows in the aggregates. The
        // manual/slash-command and run-all between-items compacts funnel
        // through here too.
        if let Some(usage) = summarize_usage {
            self.agent_loop.record_stats_row(
                self.session_id.as_deref(),
                provider.as_ref(),
                crate::agent::StatsRow {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                    cached_tokens: Some(usage.cached_tokens),
                    ttft_ms: usage.ttft_ms,
                    generation_ms: usage.generation_ms,
                    outcome: None,
                    purpose: Some("summarize".into()),
                },
            );
        } else if stop.is_some() {
            // D1 (round-1 review LOW 4): the summarizer's stream was dropped
            // mid-flight by a user interrupt — the provider billed the tokens
            // generated so far. Record a cancelled summarize row (mirroring
            // the turn-loop arm) so the most expensive aborted-request class
            // stays countable. The estimate uses the full conversation (the
            // summary prompt embeds it serialized).
            self.agent_loop.record_stats_row(
                self.session_id.as_deref(),
                provider.as_ref(),
                crate::agent::StatsRow {
                    prompt_tokens: crate::provider::estimate_prompt_tokens(
                        &self.messages,
                        &[],
                    ) as u32,
                    completion_tokens: 0,
                    reasoning_tokens: 0,
                    cached_tokens: None,
                    ttft_ms: None,
                    generation_ms: None,
                    outcome: Some("cancelled".into()),
                    purpose: Some("summarize".into()),
                },
            );
        }
        // Only apply the summary if the summarization wasn't interrupted.
        if stop.is_none() {
            self.messages = summarized;
            // Token-optimizer lever 4 (backlog e4a50d22): the digest note plus
            // the checkpoint pointer, mirroring the auto path in turn.rs.
            if survival {
                let pointer = match &checkpoint_id {
                    Some(id) => format!(
                        "\npre-compaction context archived — expand_result id={id} to \
                         retrieve dropped detail"
                    ),
                    None => String::new(),
                };
                self.messages.push(crate::provider::Message::system(format!(
                    "{dropped_digest}{pointer}"
                )));
            }
        }
        // Re-inject any commands buffered during the summarization LLM call
        // (mirrors the turn.rs summarization path at turn.rs:197-224). A
        // Suggestion is pushed as a system message + emits SuggestionInjected;
        // a Prompt is pushed as a user message. Other commands (Compact/Clear)
        // are logged — they're rare edge cases (the user sent another
        // compact/clear while one was already in progress).
        // Drop any steers the user dismissed (the "x" on a pending steer)
        // before re-injecting buffered commands.
        crate::agent::drop_cancelled_steers(&mut buffered);
        for cmd in buffered {
            match cmd {
                crate::runtime::AgentCommand::Suggestion(payload) => {
                    let crate::runtime::SteerPayload { text, images } = payload;
                    let _ = fanin_tx
                        .send((
                            self.id,
                            AgentEvent::SuggestionInjected {
                                text: text.clone(),
                                images: images.clone(),
                            },
                        ))
                        .await;
                    // Text-only steer → guidance message (unchanged mid-work
                    // semantics; user-role on `tail_as_user_messages` vendors
                    // so no trailing system block can reach DeepSeek/Ollama —
                    // see AgentLoop::push_suggestion_message). An image-bearing
                    // steer → user message with image blocks (image content
                    // belongs in user messages; the same multimodal /
                    // vision-fallback handling as a normal prompt).
                    if images.is_empty() {
                        crate::agent::AgentLoop::push_suggestion_message(
                            &mut self.messages,
                            &turn_provider,
                            &text,
                        );
                    } else {
                        let content =
                            self.build_user_content(fanin_tx, text, &images).await;
                        self.messages.push(Message {
                            content,
                            ..Message::user_text("")
                        });
                    }
                }
                crate::runtime::AgentCommand::Prompt { text, images } => {
                    // Same image handling as the normal prompt path —
                    // previously the images were silently dropped here.
                    // A Prompt buffered during the summarization never
                    // passed through the run() Prompt arm — apply the
                    // same re-derivations here (review LOW 2,
                    // 2026-09-07-unattended-planning-auto-continue):
                    // real user input resets the auto-continue streak,
                    // and the unattended flag is re-derived from the
                    // prompt marker (a user takeover during a mid-turn
                    // compaction must clear unattended, or a later
                    // Planning question-wait would be self-answered).
                    self.auto_continue_streak = 0;
                    self.unattended = text.starts_with(UNATTENDED_PROMPT_PREFIX);
                    let content = self.build_user_content(fanin_tx, text, &images).await;
                    self.messages.push(Message {
                        content,
                        ..Message::user_text("")
                    });
                }
                other => {
                    eprintln!(
                        "unexpected command buffered during /compact summarization: {other:?}"
                    );
                }
            }
        }
        // Emit the new context usage so the UI's ctx bar updates.
        let used = crate::agent::context::ContextManager::count_tokens(&self.messages) as u32;
        let max = context_manager.max_tokens() as u32;
        let breakdown = crate::agent::context::ContextManager::count_tokens_by_role(&self.messages);
        let _ = fanin_tx
            .send((
                self.id,
                AgentEvent::ContextUsage {
                    used,
                    max,
                    breakdown,
                    quality: self
                        .agent_loop
                        .quality_report(used, max, self.messages.len()),
                },
            ))
            .await;
        // Confirm the outcome in the transcript. Fires whenever the
        // summarization completed — even with no reduction (the frontend
        // shows a "nothing to compact" note) so every CompactStarted has a
        // paired end. The interrupted path gets an "interrupted" end note
        // (never a dangling "Compacting context…"); on failure the Error
        // event above already paired with CompactStarted.
        if stop.is_none() {
            let _ = fanin_tx
                .send((
                    self.id,
                    AgentEvent::Compacted {
                        before,
                        after: used,
                    },
                ))
                .await;
        } else {
            let _ = fanin_tx
                .send((
                    self.id,
                    AgentEvent::Error {
                        error: "Compaction interrupted — original conversation kept.".into(),
                        retrying: true,
                    },
                ))
                .await;
        }
        match stop {
            Some(reason) => CompactOutcome::Stopped(reason),
            None => CompactOutcome::Completed,
        }
    }

    /// Clear the conversation history entirely — a fresh start. Emits a
    /// `ContextUsage` event with `used: 0` so the UI's ctx bar resets. Backed
    /// by `/new`.
    async fn clear_context(&mut self, fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>) {
        self.messages.clear();
        let max = self.agent_loop.context_manager().max_tokens() as u32;
        let _ = fanin_tx
            .send((
                self.id,
                AgentEvent::ContextUsage {
                    used: 0,
                    max,
                    breakdown: crate::runtime::ContextBreakdown::default(),
                    quality: self.agent_loop.quality_report(0, max, 0),
                },
            ))
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::classifier::{Answer, Classifier, Question};
    use crate::provider::Role;
    use crate::agent::context::ContextManager;
    use crate::agent::AgentLoopConfig;
    use crate::config::SafetyMode;
    use crate::error::Result;
    use crate::memory::embedder::HashEmbedder;
    use crate::memory::MemoryStore;
    use crate::provider::{
        Capabilities, FinishReason, LlmClient, LlmEvent, ProviderKind, ToolSchema,
    };
    use crate::tool::agent::sandbox::Sandbox;
    use crate::tool::agent::{file_read::FileReadTool, file_write::FileWriteTool};
    use crate::tool::memory::{MemoryRecallTool, MemoryWriteTool};
    use crate::tool::workflow::plan::{CompleteStepTool, CreatePlanTool};
    use crate::tool::ToolRegistry;
    use crate::workflow::Workflow;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use tempfile::tempdir;

    /// A mock provider that returns a simple text response.
    struct MockProvider {
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for MockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            let events = vec![
                LlmEvent::TextDelta {
                    text: "Done!".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn agent_task_processes_prompt() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // Cancel after a moment.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();

        // Collect events.
        let mut events = Vec::new();
        while let Ok(Some((_id, event))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
        {
            events.push(event);
            if events.len() > 20 {
                break;
            }
        }

        // Should have seen Started + TextDelta("Done!") + Finished.
        assert!(events.iter().any(|e| matches!(e, AgentEvent::Started)));
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::TextDelta(t) if t == "Done!")));

        let _ = handle.await;
    }

    #[tokio::test]
    async fn clear_context_grades_the_report_only_when_the_quality_flag_is_on() {
        // Lever 6 (backlog e4a50d22): `/new` (clear_context) is a THIRD graded
        // emission site beyond top-of-loop and post-compaction. It grades the
        // freshly-cleared window so the ctx popup badge resets honestly — the
        // frontend reads an absent grade as "unchanged", so emitting `None`
        // here would leave a stale grade on show at 0% fill. Pin both halves.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        let build = |quality_score: bool| {
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(MockProvider {
                        caps: Capabilities::openai(),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow: workflow.clone(),
                    sandbox: sandbox.clone(),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_optimizer(Arc::new(std::sync::RwLock::new(crate::config::OptimizerConfig {
                quality_score,
                ..Default::default()
            })))
        };

        let max = 128_000;
        let graded = build(true)
            .quality_report(0, max, 0)
            .expect("a cleared window is still graded when the flag is on");
        assert_eq!(graded.fill_pct, 0, "a cleared window is 0% full");
        assert_eq!(graded.grade, crate::agent::optimizer::QualityGrade::S);

        assert!(
            build(false).quality_report(0, max, 0).is_none(),
            "the flag off must not put a grade on the wire"
        );
    }

    /// A provider that always returns a non-retryable context-overflow error.
    struct ContextOverflowProvider {
        caps: Capabilities,
        calls: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait]
    impl LlmClient for ContextOverflowProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-overflow"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(crate::error::Error::Provider(
                "This model's maximum context length is 262144 tokens. However, you \
                 requested 131072 output tokens and your prompt contains at least \
                 131073 input tokens"
                    .into(),
            ))
        }
    }

    #[tokio::test]
    async fn run_turn_attempt_skips_retry_for_non_retryable_error() {
        // Regression (L3): the turn-level retry layer (run_turn_attempt)
        // must skip retries for non-retryable errors. Without the
        // `attempt = MAX_PROVIDER_TURN_ATTEMPTS` line, run_turn would be
        // called 3× (3 backoff sleeps + 3 retrying notes). With it, the
        // error fails immediately — 0 retrying notes, 1 provider call.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(ContextOverflowProvider {
                    caps: Capabilities::openai(),
                    calls: Arc::clone(&calls),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Collect events until the agent goes idle (Finished/Exited/Error).
        let mut events = Vec::new();
        while let Ok(Some((_id, event))) =
            tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
        {
            events.push(event);
            if events.len() > 20 {
                break;
            }
        }
        // Drop the command sender to close the channel — the agent task
        // exits its `while let Some(cmd) = cmd_rx.recv()` loop and `run`
        // returns, so `handle.await` completes instead of hanging.
        drop(cmd_tx);
        let _ = handle.await;

        // The provider must be called exactly ONCE — no turn-level retries.
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "non-retryable error must skip the turn-level retry stack (1 call, not 3)"
        );
        // No retrying notes should have been emitted (the retry path was
        // skipped entirely).
        let retrying_notes = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Error { retrying: true, .. }))
            .count();
        assert_eq!(
            retrying_notes, 0,
            "non-retryable error must not emit any retrying notes"
        );
    }

    // --- 429 cross-provider fallback -------------------------------------

    /// A test provider that either returns a 429 error (rate_limited=true) or
    /// a simple text+finish stream (rate_limited=false). Reports a fixed model
    /// id + provider (endpoint) name so the fallback logic can identify which
    /// endpoint served the request.
    struct FallbackTestProvider {
        model: &'static str,
        provider: &'static str,
        rate_limited: bool,
        calls: Arc<std::sync::atomic::AtomicU32>,
        caps: Capabilities,
    }

    /// The [`Capabilities`] every [`FallbackTestProvider`] reports — the
    /// standard test shape with a per-endpoint context window (the
    /// mixed-window 429 tests serve the same model id from endpoints with
    /// different windows; 128_000 everywhere else).
    fn fallback_test_caps(max_context: usize) -> Capabilities {
        Capabilities {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context,
            max_output_tokens: 4096,
            multimodal: false,
        }
    }

    #[async_trait]
    impl LlmClient for FallbackTestProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            self.model
        }
        fn provider_name(&self) -> &str {
            self.provider
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.rate_limited {
                return Err(crate::error::Error::Provider(
                    "stream request: HTTP 429 from https://primary.example.com/v1/ — \
                     Too Many Requests"
                        .into(),
                ));
            }
            Ok(Box::pin(futures::stream::iter(vec![
                LlmEvent::TextDelta {
                    text: "recovered".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ])))
        }
    }

    /// A test [`ModelResolver`] double for the 429-fallback tests. Resolves
    /// every context to the "primary" endpoint; builds the primary or fallback
    /// provider based on the endpoint name; reports an alternate endpoint
    /// when `has_alternate` is set.
    struct FallbackResolver {
        primary: Arc<dyn LlmClient>,
        fallback: Arc<dyn LlmClient>,
        has_alternate: bool,
    }

    impl crate::model_resolver::ModelResolver for FallbackResolver {
        fn resolve(
            &self,
            _ctx: crate::model_resolver::ModelContext<'_>,
        ) -> Option<crate::config::ModelRef> {
            Some(crate::config::ModelRef {
                endpoint: "primary".into(),
                model: "test-model".into(),
                reasoning_effort: None,
            })
        }
        fn build_turn_provider(
            &self,
            model: &crate::config::ModelRef,
            fill_rate: f64,
        ) -> Option<(Arc<dyn LlmClient>, ContextManager)> {
            let p = if model.endpoint == "primary" {
                Arc::clone(&self.primary)
            } else {
                Arc::clone(&self.fallback)
            };
            let cm = ContextManager::new(p.capabilities().max_context, fill_rate);
            Some((p, cm))
        }
        fn find_alternate_endpoint(
            &self,
            model_id: &str,
            exclude: &str,
        ) -> Option<crate::config::ModelRef> {
            // Faithful to the real ConfigModelResolver: return the OTHER
            // endpoint serving the model (excluding the one that just
            // failed), for either exclude value. This makes the mock ping-pong
            // (primary→fallback→primary→…) when both providers keep 429ing —
            // so the `tried_fallback` guard's anti-cascade effect is actually
            // exercised: without the guard, the turn would ping-pong forever
            // (the test's 5s deadline would expire with no terminal error →
            // panic). A mock that only returned "fallback" for exclude==
            // "primary" (and None otherwise) could NOT catch a removal of the
            // guard, since the second switch would return None → final failure
            // either way.
            if self.has_alternate && model_id == "test-model" {
                let other = if exclude == "primary" {
                    "fallback"
                } else {
                    "primary"
                };
                return Some(crate::config::ModelRef {
                    endpoint: other.into(),
                    model: "test-model".into(),
                    reasoning_effort: None,
                });
            }
            None
        }
    }

    #[tokio::test]
    async fn rate_limit_429_falls_back_to_alternate_endpoint() {
        // On a 429, run_turn_attempt must switch to the same model on a
        // different endpoint and retry — the turn succeeds on the fallback
        // provider. Verifies: (1) the primary is called once (429, no
        // same-provider retry), (2) a switching note is emitted, (3) the
        // fallback provider is used, (4) the turn finishes successfully.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: true,
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: fallback.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Collect the marker events.
        let mut saw_switching = false;
        let mut saw_finished = false;
        let mut saw_terminal_error = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error { error, retrying } => {
                        if retrying && error.contains("429") && error.contains("switching") {
                            saw_switching = true;
                        }
                        if !retrying {
                            saw_terminal_error = true;
                        }
                    }
                    AgentEvent::Finished { .. } => {
                        saw_finished = true;
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        // The primary was called exactly once (429 — no same-provider retry).
        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once (429, no same-provider retry)"
        );
        // The fallback was called (recovery).
        assert!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "fallback provider must be called (recovery)"
        );
        // A switching note was emitted.
        assert!(
            saw_switching,
            "a 429 switching note (retrying:true, mentions 429 + switching) must be emitted"
        );
        // The turn succeeded — no terminal error.
        assert!(
            saw_finished,
            "the turn should finish successfully on the fallback"
        );
        assert!(
            !saw_terminal_error,
            "no terminal error — the fallback recovered the turn"
        );
    }

    #[tokio::test]
    async fn rate_limit_429_with_no_alternate_fails_with_clear_message() {
        // When no alternate endpoint serves the model, the 429 must produce a
        // terminal error with a clear "no alternate provider found" message
        // (not "retries exhausted" — 0 retries occurred). The primary is
        // called once (no same-provider retry, no fallback).
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(128_000),
        });
        // A fallback provider that would succeed — but the resolver reports NO
        // alternate endpoint, so it must never be called.
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: false,
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: fallback.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        let mut terminal_error_text: Option<String> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error {
                        error,
                        retrying: false,
                    } => {
                        terminal_error_text = Some(error);
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        // The primary was called once (429 — no retry, no fallback).
        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once (429, no same-provider retry)"
        );
        // The fallback was never called (no alternate was found).
        assert_eq!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "fallback must not be called when no alternate endpoint exists"
        );
        // A terminal error with the "no alternate" message was emitted.
        let text = terminal_error_text.expect("a terminal error should be emitted");
        assert!(
            text.contains("429"),
            "terminal error should mention 429, got: {text}"
        );
        assert!(
            text.contains("no alternate provider"),
            "terminal error should say 'no alternate provider found', got: {text}"
        );
    }

    #[tokio::test]
    async fn rate_limit_429_falls_back_to_smaller_window_alternate_when_live_conversation_fits() {
        // Mixed-window failover (backlog a117e827): the primary serves
        // test-model with a 200k window, the alternate with 128k. The OLD
        // viability check compared context WINDOWS (128k < 200k → reject),
        // so a 429 never failed over even when the live conversation was a
        // handful of tokens. The live-count check must accept the alternate
        // when its window holds the live conversation plus headroom (its own
        // summarize threshold: 128k × 0.5 fill = 64k; a tiny conversation +
        // 64k ≪ 128k). Verifies: switching note, the turn finishes on the
        // alternate, no terminal error.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(200_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: true,
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: fallback.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(200_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Collect the marker events.
        let mut saw_switching = false;
        let mut saw_finished = false;
        let mut saw_terminal_error = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error { error, retrying } => {
                        if retrying && error.contains("429") && error.contains("switching") {
                            saw_switching = true;
                        }
                        if !retrying {
                            saw_terminal_error = true;
                        }
                    }
                    AgentEvent::Finished { .. } => {
                        saw_finished = true;
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        // The primary was called exactly once (429 — no same-provider retry).
        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once (429, no same-provider retry)"
        );
        // The smaller-window alternate served the recovery — the live
        // conversation fits its window with headroom.
        assert!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the smaller-window alternate must serve the recovery (live conversation fits)"
        );
        // A switching note was emitted.
        assert!(
            saw_switching,
            "a 429 switching note (retrying:true, mentions 429 + switching) must be emitted"
        );
        // The turn succeeded — no terminal error.
        assert!(
            saw_finished,
            "the turn should finish successfully on the smaller-window alternate"
        );
        assert!(
            !saw_terminal_error,
            "no terminal error — the live-count viability check accepted the alternate"
        );
    }

    #[tokio::test]
    async fn rate_limit_429_rejects_alternate_when_live_conversation_genuinely_exceeds_it() {
        // The flip side of the live-count check (backlog a117e827): the same
        // mixed-window pair (primary 200k, alternate 128k) but the live
        // conversation is ~80k tokens — beyond the alternate's viability
        // line (128k window − 64k headroom = 64k). The alternate must be
        // rejected as not viable: terminal error, no switching note, the
        // fallback endpoint never called. The current endpoint's own
        // summarize threshold is 100k (200k × 0.5), so no compaction
        // interferes before the 429.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(200_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: true,
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: fallback.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(200_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // ~80k tokens of prompt: "a b " repeated 40k times (each
        // letter+space pair is one BPE token — 2 tokens per repeat).
        let big_prompt = "a b ".repeat(40_000);
        cmd_tx
            .send(AgentCommand::Prompt {
                text: big_prompt,
                images: vec![],
            })
            .await
            .unwrap();

        let mut terminal_error_text: Option<String> = None;
        let mut saw_switching = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error { error, retrying } => {
                        if retrying && error.contains("429") && error.contains("switching") {
                            saw_switching = true;
                        }
                        if !retrying {
                            terminal_error_text = Some(error);
                            break;
                        }
                    }
                    AgentEvent::Finished { .. } | AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        // The primary was called once (429 — no same-provider retry).
        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once (429, no same-provider retry)"
        );
        // The smaller-window alternate was never called — the live
        // conversation exceeds its viability line.
        assert_eq!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the alternate must not be called when the live conversation exceeds its viability line"
        );
        // No switching note — the alternate was rejected as not viable.
        assert!(
            !saw_switching,
            "no switching note — the alternate is not viable for this conversation"
        );
        // A terminal error with the "no alternate" message was emitted.
        let text = terminal_error_text.expect("a terminal error should be emitted");
        assert!(
            text.contains("429"),
            "terminal error should mention 429, got: {text}"
        );
        assert!(
            text.contains("no alternate provider"),
            "terminal error should say 'no alternate provider found', got: {text}"
        );
    }

    #[test]
    fn try_429_fallback_viability_uses_live_count_with_window_fallback() {
        // Direct unit test of the viability rule (backlog a117e827), without
        // the full agent harness. The loop's default provider is the
        // "fallback" endpoint (200k window), so from try_429_fallback's point
        // of view the alternate candidate is the "primary" endpoint (128k
        // window; its own summarize threshold at 0.5 fill = 64k headroom).
        // With a live count recorded, the alternate is viable when its window
        // holds live + headroom (20k + 64k = 84k ≤ 128k) even though the
        // window comparison alone would reject it (128k < 200k). With NO live
        // count recorded (0 — the 429 escaped before any request was built),
        // the conservative window comparison still guards.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(200_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: true,
        });

        let agent_loop = AgentLoop::new(
            AgentLoopConfig {
                provider: fallback.clone(),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(200_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        )
        .with_model_resolver(resolver);

        // No live count recorded yet (0): the window comparison rejects the
        // 128k alternate against the 200k current window.
        assert!(
            agent_loop.try_429_fallback().is_none(),
            "live=0 must fall back to the window comparison (128k < 200k → reject)"
        );

        // Live count recorded (20k): the alternate's window holds live +
        // its own summarize threshold (20k + 64k = 84k ≤ 128k) — viable even
        // though the window comparison alone would reject it.
        agent_loop
            .live_token_count
            .store(20_000, std::sync::atomic::Ordering::Relaxed);
        let info = agent_loop
            .try_429_fallback()
            .expect("live count within the alternate's viability line must accept it");
        assert_eq!(info.to_endpoint, "primary");
        assert_eq!(info.from_endpoint, "fallback");
        assert_eq!(info.model, "test-model");
    }

    #[tokio::test]
    async fn rate_limit_429_high_fill_rate_does_not_regress_window_viability() {
        // HIGH-1 regression (review 2026-09-08-429-fallback-live-token-count):
        // the headroom rule alone — live ≤ alt×(1−fill) — is stricter than
        // the incumbent's own operating band whenever fill > 0.5. Here:
        // fill 0.75, current window 128k (operating band to 96k), alternate
        // 200k (a 1.56× LARGER window the old window comparison accepted),
        // live conversation ~60k. The headroom rule alone rejects it
        // (60k + 150k = 210k > 200k) — silently disabling failover the old
        // code provided. The OR'd window comparison must keep it viable:
        // switching note, turn finishes on the alternate, no terminal error.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(200_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: true,
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: fallback.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.75),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver)
            .with_fill_rate(0.75),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // ~60k tokens of prompt ("a b " × 30k ≈ 2 BPE tokens per repeat) —
        // inside the current endpoint's operating band (96k threshold at
        // 0.75 fill), so no compaction interferes before the 429.
        let big_prompt = "a b ".repeat(30_000);
        cmd_tx
            .send(AgentCommand::Prompt {
                text: big_prompt,
                images: vec![],
            })
            .await
            .unwrap();

        let mut saw_switching = false;
        let mut saw_finished = false;
        let mut saw_terminal_error = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error { error, retrying } => {
                        if retrying && error.contains("429") && error.contains("switching") {
                            saw_switching = true;
                        }
                        if !retrying {
                            saw_terminal_error = true;
                        }
                    }
                    AgentEvent::Finished { .. } => {
                        saw_finished = true;
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once (429, no same-provider retry)"
        );
        assert!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the larger-window alternate must serve the recovery — the window \
             comparison keeps it viable at high fill rates"
        );
        assert!(
            saw_switching,
            "a 429 switching note (retrying:true, mentions 429 + switching) must be emitted"
        );
        assert!(
            saw_finished,
            "the turn should finish successfully on the larger-window alternate"
        );
        assert!(
            !saw_terminal_error,
            "no terminal error — the OR'd window comparison accepted the alternate"
        );
    }

    #[tokio::test]
    async fn rate_limit_429_fallback_also_429s_fails_without_cascade() {
        // The `tried_fallback` guard: when the primary 429s, the fallback is
        // found + pinned, and the retry on the fallback ALSO 429s, the turn
        // must go straight to final failure — no third endpoint, no backoff
        // retry of a rate-limited provider, no cascade. Exactly 2 provider
        // calls (1 primary + 1 fallback), and a terminal error carrying the
        // 429 qualifier.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        // BOTH providers are rate-limited — the fallback also 429s.
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: true,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });
        let resolver: Arc<dyn ModelResolver> = Arc::new(FallbackResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
            has_alternate: true,
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: fallback.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        let mut terminal_error_text: Option<String> = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error {
                        error,
                        retrying: false,
                    } => {
                        terminal_error_text = Some(error);
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        // Exactly 2 provider calls: 1 primary (429) + 1 fallback (also 429).
        // No third endpoint, no backoff retry of either rate-limited provider.
        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once (429, no same-provider retry)"
        );
        assert_eq!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "fallback must be called once (also 429) — no cascade to a third endpoint"
        );
        // A terminal error with the 429 qualifier was emitted.
        let text = terminal_error_text.expect("a terminal error should be emitted");
        assert!(
            text.contains("429"),
            "terminal error should mention 429, got: {text}"
        );
        assert!(
            text.contains("no alternate provider"),
            "terminal error should carry the 429 'no alternate' qualifier, got: {text}"
        );
    }

    #[tokio::test]
    async fn rate_limit_429_falls_back_on_default_provider_path() {
        // MEDIUM 1 regression: when the turn ran on the DEFAULT provider (no
        // per-context override resolved → resolved_model() is None), a 429
        // must STILL fall back to the alternate endpoint. try_429_fallback
        // falls back to provider().model() when resolved_model() is None,
        // mirroring effective_provider_name(). Without the fix, the fallback
        // silently no-ops (returns None) even when a backup endpoint exists.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "test-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });
        // A resolver whose resolve() returns None — the default-provider path
        // (no [models.*] override). The fallback must still fire.
        struct NoOverrideResolver {
            primary: Arc<dyn LlmClient>,
            fallback: Arc<dyn LlmClient>,
        }
        impl ModelResolver for NoOverrideResolver {
            fn resolve(
                &self,
                _ctx: crate::model_resolver::ModelContext<'_>,
            ) -> Option<crate::config::ModelRef> {
                None // no override → default provider
            }
            fn build_turn_provider(
                &self,
                model: &crate::config::ModelRef,
                fill_rate: f64,
            ) -> Option<(Arc<dyn LlmClient>, ContextManager)> {
                let p = if model.endpoint == "primary" {
                    Arc::clone(&self.primary)
                } else {
                    Arc::clone(&self.fallback)
                };
                let cm = ContextManager::new(p.capabilities().max_context, fill_rate);
                Some((p, cm))
            }
            fn find_alternate_endpoint(
                &self,
                model_id: &str,
                exclude: &str,
            ) -> Option<crate::config::ModelRef> {
                if model_id == "test-model" && exclude == "primary" {
                    Some(crate::config::ModelRef {
                        endpoint: "fallback".into(),
                        model: "test-model".into(),
                        reasoning_effort: None,
                    })
                } else {
                    None
                }
            }
        }
        let resolver: Arc<dyn ModelResolver> = Arc::new(NoOverrideResolver {
            primary: Arc::clone(&primary),
            fallback: Arc::clone(&fallback),
        });

        // The loop's DEFAULT provider is the primary (rate-limited). With no
        // override resolved, resolve_turn_provider returns None → run_turn
        // uses the default snapshot (primary). On 429, try_429_fallback must
        // read provider().model() ("test-model") + effective_provider_name()
        // ("primary") and switch to the fallback.
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: primary.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow,
                    sandbox: sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        let mut saw_switching = false;
        let mut saw_finished = false;
        let mut saw_terminal_error = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error { error, retrying } => {
                        if retrying && error.contains("429") && error.contains("switching") {
                            saw_switching = true;
                        }
                        if !retrying {
                            saw_terminal_error = true;
                        }
                    }
                    AgentEvent::Finished { .. } => {
                        saw_finished = true;
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        drop(cmd_tx);
        let _ = handle.await;

        // The primary (default provider) was called once (429).
        assert_eq!(
            primary_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "default provider (primary) must be called once (429)"
        );
        // The fallback was called — the default-provider path recovered.
        assert!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "fallback must be called even on the default-provider path"
        );
        assert!(
            saw_switching,
            "a 429 switching note must be emitted on the default-provider path"
        );
        assert!(
            saw_finished,
            "the turn should finish successfully on the fallback"
        );
        assert!(
            !saw_terminal_error,
            "no terminal error — the default-provider fallback recovered the turn"
        );
    }

    /// Wait for one turn to finish (or fail) on the fan-in channel. Returns
    /// `(saw_finished, saw_terminal_error, saw_switching_note)` for the turn
    /// whose events arrive while waiting.
    async fn wait_for_turn_outcome(
        fanin_rx: &mut mpsc::Receiver<(u64, AgentEvent)>,
    ) -> (bool, bool, bool) {
        let mut saw_switching = false;
        let mut saw_finished = false;
        let mut saw_terminal_error = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(500), fanin_rx.recv()).await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::Error { error, retrying } => {
                        if retrying && error.contains("429") && error.contains("switching") {
                            saw_switching = true;
                        }
                        if !retrying {
                            saw_terminal_error = true;
                        }
                    }
                    AgentEvent::Finished { .. } => {
                        saw_finished = true;
                        break;
                    }
                    AgentEvent::Exited => break,
                    _ => {}
                },
                _ => break,
            }
        }
        (saw_finished, saw_terminal_error, saw_switching)
    }

    /// Close the command channel and wait for the agent loop to exit, draining
    /// fan-in while it winds down.
    ///
    /// The drain is not optional. While the workflow is Executing the loop
    /// auto-continues (up to `MAX_AUTO_CONTINUE`) and every event is a blocking
    /// `send` on a bounded channel, so a test that stops reading fills the
    /// channel and parks the loop mid-turn — before it can observe the closed
    /// command channel. `handle.await` then never returns. The timeout keeps a
    /// future regression a test failure rather than a hung suite.
    async fn shutdown_agent(
        cmd_tx: mpsc::Sender<AgentCommand>,
        mut fanin_rx: mpsc::Receiver<(u64, AgentEvent)>,
        handle: tokio::task::JoinHandle<()>,
    ) {
        drop(cmd_tx);
        let drain = tokio::spawn(async move { while fanin_rx.recv().await.is_some() {} });
        tokio::time::timeout(std::time::Duration::from_secs(10), handle)
            .await
            .expect("agent task must exit once the command channel closes")
            .expect("agent task must not panic");
        drain.abort();
    }

    #[tokio::test]
    async fn state_override_survives_429_fallback() {
        // Regression (2026-12-05): after a 429-triggered fallback, the next
        // workflow-state transition MUST still switch models. The buggy
        // try_429_fallback pinned the fallback provider via
        // set_explicit_provider — that pin outranks the entire
        // [models.planning]/[models.executing] resolver chain and is never
        // cleared, so one 429 permanently froze the agent on the fallback
        // model. Here: Planning runs plan-model@primary (429s) → fallback to
        // plan-model@fallback; then the workflow enters Executing and the
        // next turn must run exec-model@primary.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_plan_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_plan_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary_exec_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary_plan: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "plan-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_plan_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback_plan: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "plan-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_plan_calls),
            caps: fallback_test_caps(128_000),
        });
        let primary_exec: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "exec-model",
            provider: "primary",
            rate_limited: false,
            calls: Arc::clone(&primary_exec_calls),
            caps: fallback_test_caps(128_000),
        });

        // Resolves a per-state override: Planning → plan-model@primary,
        // Executing → exec-model@primary.
        struct StatefulResolver {
            primary_plan: Arc<dyn LlmClient>,
            fallback_plan: Arc<dyn LlmClient>,
            primary_exec: Arc<dyn LlmClient>,
        }
        impl ModelResolver for StatefulResolver {
            fn resolve(
                &self,
                ctx: crate::model_resolver::ModelContext<'_>,
            ) -> Option<crate::config::ModelRef> {
                match ctx.workflow_state {
                    crate::workflow::WorkflowState::Planning => Some(crate::config::ModelRef {
                        endpoint: "primary".into(),
                        model: "plan-model".into(),
                        reasoning_effort: None,
                    }),
                    crate::workflow::WorkflowState::Executing => Some(crate::config::ModelRef {
                        endpoint: "primary".into(),
                        model: "exec-model".into(),
                        reasoning_effort: None,
                    }),
                    _ => None,
                }
            }
            fn build_turn_provider(
                &self,
                model: &crate::config::ModelRef,
                fill_rate: f64,
            ) -> Option<(Arc<dyn LlmClient>, ContextManager)> {
                let p = match (model.endpoint.as_str(), model.model.as_str()) {
                    ("primary", "plan-model") => Arc::clone(&self.primary_plan),
                    ("fallback", "plan-model") => Arc::clone(&self.fallback_plan),
                    ("primary", "exec-model") => Arc::clone(&self.primary_exec),
                    _ => return None,
                };
                let cm = ContextManager::new(p.capabilities().max_context, fill_rate);
                Some((p, cm))
            }
            fn find_alternate_endpoint(
                &self,
                model_id: &str,
                exclude: &str,
            ) -> Option<crate::config::ModelRef> {
                if model_id == "plan-model" && exclude == "primary" {
                    Some(crate::config::ModelRef {
                        endpoint: "fallback".into(),
                        model: "plan-model".into(),
                        reasoning_effort: None,
                    })
                } else {
                    None
                }
            }
        }
        let resolver: Arc<dyn ModelResolver> = Arc::new(StatefulResolver {
            primary_plan: Arc::clone(&primary_plan),
            fallback_plan: Arc::clone(&fallback_plan),
            primary_exec: Arc::clone(&primary_exec),
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: primary_plan.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow.clone(),
                    sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // --- Turn 1: Planning on plan-model@primary (429) → fallback. ---
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let (finished, terminal, switching) = wait_for_turn_outcome(&mut fanin_rx).await;
        assert!(finished, "turn 1 should finish on the fallback endpoint");
        assert!(!terminal, "turn 1 must recover via the 429 fallback");
        assert!(switching, "turn 1 must emit a 429 switching note");
        assert_eq!(
            primary_plan_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "primary must be called once in Planning (429, no same-provider retry)"
        );
        assert!(
            fallback_plan_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the fallback endpoint must serve the Planning turn"
        );

        // --- Transition: create a plan → workflow enters Executing. ---
        workflow
            .lock()
            .await
            .create_plan_with_kind(
                "t",
                "g",
                "c",
                vec!["step one".into()],
                crate::workflow::PlanKind::Research,
                None,
            )
            .unwrap();

        // --- Turn 2: Executing MUST switch to exec-model@primary. ---
        let fallback_after_turn1 = fallback_plan_calls.load(std::sync::atomic::Ordering::SeqCst);
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "execute now".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let (finished2, terminal2, _switching2) = wait_for_turn_outcome(&mut fanin_rx).await;
        assert!(finished2, "turn 2 should finish");
        assert!(
            !terminal2,
            "turn 2 should not error (exec-model is healthy)"
        );
        // >= 1, not == 1: since auto-continuation (2891c05), a turn that
        // ends with Stop while the workflow is still Executing drives up to
        // MAX_AUTO_CONTINUE synthetic "continue" turns on the same
        // (Executing) resolver branch — exec-model@primary. What this
        // regression pins is WHICH provider serves the turn: exec-model
        // must (>= 1 call) and the fallback must not (frozen below).
        assert!(
            primary_exec_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the Executing turn must run exec-model@primary — the 429 fallback \
             must not permanently pin the Planning model"
        );
        assert_eq!(
            fallback_plan_calls.load(std::sync::atomic::Ordering::SeqCst),
            fallback_after_turn1,
            "the fallback endpoint must not serve the Executing turn"
        );

        shutdown_agent(cmd_tx, fanin_rx, handle).await;
    }

    #[tokio::test]
    async fn default_path_429_fallback_does_not_pin() {
        // Regression (2026-12-05), default-provider twin of the above: a 429
        // on the DEFAULT provider (no [models.*] override active) must fall
        // back for that turn, but must NOT pin the agent to the fallback —
        // a later per-state override must still take effect.
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let primary_default_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_default_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary_exec_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let primary_default: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "default-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&primary_default_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback_default: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "default-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_default_calls),
            caps: fallback_test_caps(128_000),
        });
        let primary_exec: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "exec-model",
            provider: "primary",
            rate_limited: false,
            calls: Arc::clone(&primary_exec_calls),
            caps: fallback_test_caps(128_000),
        });

        // No override in Planning (default-provider path); Executing resolves
        // exec-model@primary.
        struct LazyResolver {
            fallback_default: Arc<dyn LlmClient>,
            primary_exec: Arc<dyn LlmClient>,
        }
        impl ModelResolver for LazyResolver {
            fn resolve(
                &self,
                ctx: crate::model_resolver::ModelContext<'_>,
            ) -> Option<crate::config::ModelRef> {
                match ctx.workflow_state {
                    crate::workflow::WorkflowState::Executing => Some(crate::config::ModelRef {
                        endpoint: "primary".into(),
                        model: "exec-model".into(),
                        reasoning_effort: None,
                    }),
                    _ => None,
                }
            }
            fn build_turn_provider(
                &self,
                model: &crate::config::ModelRef,
                fill_rate: f64,
            ) -> Option<(Arc<dyn LlmClient>, ContextManager)> {
                let p = match (model.endpoint.as_str(), model.model.as_str()) {
                    ("fallback", "default-model") => Arc::clone(&self.fallback_default),
                    ("primary", "exec-model") => Arc::clone(&self.primary_exec),
                    _ => return None,
                };
                let cm = ContextManager::new(p.capabilities().max_context, fill_rate);
                Some((p, cm))
            }
            fn find_alternate_endpoint(
                &self,
                model_id: &str,
                exclude: &str,
            ) -> Option<crate::config::ModelRef> {
                if model_id == "default-model" && exclude == "primary" {
                    Some(crate::config::ModelRef {
                        endpoint: "fallback".into(),
                        model: "default-model".into(),
                        reasoning_effort: None,
                    })
                } else {
                    None
                }
            }
        }
        let resolver: Arc<dyn ModelResolver> = Arc::new(LazyResolver {
            fallback_default: Arc::clone(&fallback_default),
            primary_exec: Arc::clone(&primary_exec),
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: primary_default.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow.clone(),
                    sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // --- Turn 1: default provider 429s → fallback recovers the turn. ---
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let (finished, terminal, switching) = wait_for_turn_outcome(&mut fanin_rx).await;
        assert!(finished, "turn 1 should finish on the fallback endpoint");
        assert!(!terminal, "turn 1 must recover via the 429 fallback");
        assert!(switching, "turn 1 must emit a 429 switching note");
        assert!(
            fallback_default_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the fallback endpoint must serve the default-path 429 turn"
        );

        // --- Transition: create a plan → workflow enters Executing. ---
        workflow
            .lock()
            .await
            .create_plan_with_kind(
                "t",
                "g",
                "c",
                vec!["step one".into()],
                crate::workflow::PlanKind::Research,
                None,
            )
            .unwrap();

        // --- Turn 2: the Executing override must win over the fallback. ---
        let fallback_after_turn1 = fallback_default_calls.load(std::sync::atomic::Ordering::SeqCst);
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "execute now".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let (finished2, terminal2, _switching2) = wait_for_turn_outcome(&mut fanin_rx).await;
        assert!(finished2, "turn 2 should finish");
        assert!(
            !terminal2,
            "turn 2 should not error (exec-model is healthy)"
        );
        // >= 1, not == 1: since auto-continuation (2891c05), a turn that
        // ends with Stop while the workflow is still Executing drives up to
        // MAX_AUTO_CONTINUE synthetic "continue" turns on the same
        // (Executing) resolver branch — exec-model@primary. What this
        // regression pins is WHICH provider serves the turn: exec-model
        // must (>= 1 call) and the fallback must not (frozen below).
        assert!(
            primary_exec_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the Executing override must serve turn 2 — the default-path 429 \
             fallback must not pin the agent"
        );
        assert_eq!(
            fallback_default_calls.load(std::sync::atomic::Ordering::SeqCst),
            fallback_after_turn1,
            "the fallback endpoint must not serve the Executing turn"
        );

        shutdown_agent(cmd_tx, fanin_rx, handle).await;
    }

    #[tokio::test]
    async fn picker_pin_429_fallback_reroutes_endpoint() {
        // Regression (2026-12-06, review follow-up): the EXPLICIT PICKER PIN
        // path of resolve_turn_provider must honor the recorded 429 endpoint
        // stickiness too. The pin branch used to return the pinned provider
        // directly without consulting sticky_endpoint, so a 429 on the pinned
        // endpoint recorded an entry that was never consulted: the retry
        // re-entered the pin branch, hit the same 429 again, and the turn
        // failed with a contradictory "no alternate provider found" error one
        // note after announcing the switch. Here the user has pinned
        // pin-model@primary via the per-agent picker; it 429s and the SAME
        // turn must be served by pin-model@fallback (endpoint routing only —
        // the pin's model choice stays intact).
        use crate::model_resolver::ModelResolver;

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));

        let pinned_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let fallback_calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let pinned: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "pin-model",
            provider: "primary",
            rate_limited: true,
            calls: Arc::clone(&pinned_calls),
            caps: fallback_test_caps(128_000),
        });
        let fallback: Arc<dyn LlmClient> = Arc::new(FallbackTestProvider {
            model: "pin-model",
            provider: "fallback",
            rate_limited: false,
            calls: Arc::clone(&fallback_calls),
            caps: fallback_test_caps(128_000),
        });

        // No [models.*] overrides at all — resolution falls through to the
        // picker pin on every turn; the resolver only serves builds and the
        // alternate endpoint.
        struct PinResolver {
            pinned: Arc<dyn LlmClient>,
            fallback: Arc<dyn LlmClient>,
        }
        impl ModelResolver for PinResolver {
            fn resolve(
                &self,
                _ctx: crate::model_resolver::ModelContext<'_>,
            ) -> Option<crate::config::ModelRef> {
                None
            }
            fn build_turn_provider(
                &self,
                model: &crate::config::ModelRef,
                fill_rate: f64,
            ) -> Option<(Arc<dyn LlmClient>, ContextManager)> {
                let p = match (model.endpoint.as_str(), model.model.as_str()) {
                    ("primary", "pin-model") => Arc::clone(&self.pinned),
                    ("fallback", "pin-model") => Arc::clone(&self.fallback),
                    _ => return None,
                };
                let cm = ContextManager::new(p.capabilities().max_context, fill_rate);
                Some((p, cm))
            }
            fn find_alternate_endpoint(
                &self,
                model_id: &str,
                exclude: &str,
            ) -> Option<crate::config::ModelRef> {
                if model_id == "pin-model" && exclude == "primary" {
                    Some(crate::config::ModelRef {
                        endpoint: "fallback".into(),
                        model: "pin-model".into(),
                        reasoning_effort: None,
                    })
                } else {
                    None
                }
            }
        }
        let resolver: Arc<dyn ModelResolver> = Arc::new(PinResolver {
            pinned: Arc::clone(&pinned),
            fallback: Arc::clone(&fallback),
        });

        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: pinned.clone(),
                    tools: Arc::new(registry),
                    workflow: workflow.clone(),
                    sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_model_resolver(resolver),
        );
        // The user's picker action: pin pin-model@primary for this agent.
        agent_loop.set_explicit_provider(Arc::clone(&pinned), ContextManager::new(128_000, 0.5));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let (finished, terminal, switching) = wait_for_turn_outcome(&mut fanin_rx).await;
        assert!(finished, "the turn should finish on the alternate endpoint");
        assert!(
            !terminal,
            "the pin path must recover via the recorded 429 stickiness"
        );
        assert!(switching, "the turn must emit a 429 switching note");
        assert_eq!(
            pinned_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the pinned endpoint must serve exactly one attempt (the 429) — \
             the retry must be rerouted to the alternate"
        );
        assert!(
            fallback_calls.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the alternate endpoint must serve the retried turn"
        );

        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn correction_prompt_writes_user_correction_working_memory() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: Some(store.clone()),
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // A correction prompt — the ≤3-line-summary nudge that motivated this.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "your summary is too long — keep it to 3 lines".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Poll for the write (bounded at 5s) instead of a fixed 150ms sleep:
        // under full-suite parallel load the sleep raced the async agent task
        // (flake observed 2026-08-20) — the assertion fired while the memory
        // write was still in flight. Assert BEFORE Cancel: shutdown runs
        // consolidation, which deletes the session's working-tier events
        // (they're distilled into episodic), so the working memory is only
        // observable while the task is still alive.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut correction = None;
        while correction.is_none() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            let working = store
                .list_by_tier(crate::memory::MemoryTier::Working)
                .await
                .unwrap();
            correction = working
                .into_iter()
                .find(|m| m.title == "tool: user_correction");
        }
        let Some(correction) = correction else {
            let working = store
                .list_by_tier(crate::memory::MemoryTier::Working)
                .await
                .unwrap();
            let titles: Vec<&str> = working.iter().map(|m| m.title.as_str()).collect();
            panic!("a correction prompt must record a user_correction working memory; got titles: {titles:?}");
        };
        assert!(correction.content.contains("too long"));

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
        drop(fanin_rx);
    }

    #[tokio::test]
    async fn normal_prompt_writes_no_correction_memory() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: Some(store.clone()),
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // A normal, non-correction prompt.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "add a spawn_agent tool".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Assert BEFORE Cancel (shutdown consolidation would delete working
        // events anyway, making the absence check meaningless afterward).
        let working = store
            .list_by_tier(crate::memory::MemoryTier::Working)
            .await
            .unwrap();
        assert!(
            !working.iter().any(|m| m.title == "tool: user_correction"),
            "a normal prompt must NOT record a user_correction memory"
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
        drop(fanin_rx);
    }

    #[tokio::test]
    async fn agent_task_stays_alive_after_turn_and_processes_second_prompt() {
        // Regression: the agent task must NOT exit after a turn completes. It
        // emits `Finished` (turn done) but stays alive, so a second prompt is
        // processed. `Exited` must only fire when the task truly terminates.
        // (Previously the forwarder removed the agent on every `Finished`,
        // closing the command channel and killing the task — blocking
        // follow-up prompts, e.g. after the workflow reached Complete.)
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // First prompt.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "first".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Drain events until we see the first Finished. The task must still be
        // alive (no Exited) at this point.
        let mut saw_first_finished = false;
        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        saw_first_finished = true;
                        break;
                    }
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                    }
                }
                _ => break,
            }
        }
        assert!(saw_first_finished, "first turn should emit Finished");
        assert!(
            !saw_exited,
            "agent must NOT emit Exited after a turn — it should stay alive"
        );

        // The task handle should still be running (not yet joined).
        // Give the runtime a tick to ensure it hasn't exited.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "agent task must still be alive after a turn completes"
        );

        // Send a second prompt — it must be processed (another Finished).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "second".into(),
                images: vec![],
            })
            .await
            .unwrap();

        let mut saw_second_finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        saw_second_finished = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_second_finished,
            "second prompt must be processed — the agent stayed alive after the first turn"
        );

        // Now cancel — the task should exit and emit Exited.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();

        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_exited,
            "agent must emit Exited only when the task truly terminates (after Cancel)"
        );

        let _ = handle.await;
    }

    #[tokio::test]
    async fn cancel_emits_exited_promptly_without_blocking_on_consolidation() {
        // The close-x dismisses the agent immediately: Cancel must emit
        // Finished + Exited right away, NOT block on the session's
        // working-memory consolidation (an LLM call that can take seconds).
        // The consolidation is spawned in the background. Verify by starting a
        // session with working-memory events (so consolidation would run),
        // then Cancel + assert Exited arrives within a tight deadline.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt so a session starts + working memory is captured.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // Drain until the first Finished (the turn ran, session is active).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Now Cancel. Exited must arrive promptly — the consolidation is
        // spawned (not awaited), so it can't block the exit. A 2s deadline
        // is generous; the real guarantee is "not blocked on an LLM call."
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let mut saw_exited = false;
        let prompt_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < prompt_deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_exited,
            "Cancel must emit Exited promptly (consolidation is spawned, not awaited)"
        );

        let _ = handle.await;
    }

    #[tokio::test]
    async fn between_turn_steer_is_converted_to_user_message_and_runs_turn() {
        // A steer that lands while the agent is idle (between turns) must be
        // converted to a regular user message and trigger a new turn — there's
        // no in-flight work to steer, so it becomes a follow-up prompt the
        // agent responds to. Verify: (1) a SuggestionInjected event fires,
        // (2) a second Finished fires (the turn ran), and (3) the steer text
        // is in the conversation as a User message (not a System message).
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // First prompt — run a turn to completion.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Now send a steer while the agent is idle (between turns).
        cmd_tx
            .send(AgentCommand::Suggestion("now do this".into()))
            .await
            .unwrap();

        // We should see SuggestionInjected + a second Finished (the turn ran).
        let mut saw_suggestion_injected = false;
        let mut saw_second_finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::SuggestionInjected { .. }) {
                        saw_suggestion_injected = true;
                    }
                    if matches!(event, AgentEvent::Finished { .. }) {
                        saw_second_finished = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_suggestion_injected,
            "a between-turn steer must emit SuggestionInjected"
        );
        assert!(
            saw_second_finished,
            "a between-turn steer must trigger a new turn (Finished)"
        );

        // Cancel to shut down the task.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn idle_suggestion_then_prompt_runs_two_sequential_turns() {
        // CONTRACT PIN (run-all defect 2026-08-20): a Suggestion sent to an
        // IDLE agent runs as its OWN complete turn before any Prompt queued
        // behind it is processed — the Prompt never merges into the steer's
        // turn. Run-All used to send its unattended preamble as a Suggestion
        // before the item's Prompt; every item then resolved on a steer-only
        // turn (plan-loop gate → CantResolve) and the NEXT item was
        // dispatched before the real prompt ran. The fix embeds the preamble
        // in the Prompt (ipc::run_all::run_all_prompt); this test pins the
        // runtime behavior the fix depends on — if the idle-Suggestion arm
        // ever changes to buffer the steer instead of running a turn, this
        // fails and the run-all rationale must be re-examined.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // The agent is IDLE. Send a steer and a prompt back-to-back — exactly
        // what run_all_dispatch_next used to do.
        cmd_tx
            .send(AgentCommand::Suggestion("steer text".into()))
            .await
            .unwrap();
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "task text".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Collect the marker-event sequence until TWO turns have finished.
        #[derive(PartialEq, Debug)]
        enum Marker {
            Injected,
            Started,
            Finished,
        }
        let mut seq: Vec<Marker> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::SuggestionInjected { .. } => seq.push(Marker::Injected),
                    AgentEvent::Started => seq.push(Marker::Started),
                    AgentEvent::Finished { .. } => {
                        seq.push(Marker::Finished);
                        if seq.iter().filter(|m| **m == Marker::Finished).count() == 2 {
                            break;
                        }
                    }
                    _ => {}
                },
                _ => break,
            }
        }

        // The steer runs its own turn FIRST; the queued Prompt's turn runs
        // strictly AFTER it — never merged into the steer's turn. Note the
        // Prompt can reach its turn by two timing-dependent paths (both
        // valid): it queues for the task loop's Prompt arm, OR run_turn's
        // mid-turn steer mechanism absorbs it (soft-stop + follow-up turn,
        // which adds a second SuggestionInjected marker). So assert the
        // contract, not a literal sequence.
        let started_count = seq.iter().filter(|m| **m == Marker::Started).count();
        let finished_count = seq.iter().filter(|m| **m == Marker::Finished).count();
        assert_eq!(started_count, 2, "exactly two turns must run: {seq:?}");
        assert_eq!(finished_count, 2, "both turns must finish: {seq:?}");
        assert_eq!(
            seq.first(),
            Some(&Marker::Injected),
            "the steer is injected before any turn — its turn runs first: {seq:?}"
        );
        let second_started =
            seq.iter()
                .position(|m| *m == Marker::Started)
                .and_then(|first_started| {
                    seq.iter()
                        .skip(first_started + 1)
                        .position(|m| *m == Marker::Started)
                        .map(|p| p + first_started + 1)
                });
        let first_finished = seq.iter().position(|m| *m == Marker::Finished);
        assert!(
            match (second_started, first_finished) {
                (Some(s2), Some(f1)) => s2 > f1,
                _ => false,
            },
            "the second turn must start only after the first turn finished (sequential): {seq:?}"
        );

        // Cancel to shut down the task.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    /// A provider that yields one text chunk, then blocks on a Notify until
    /// released, then yields a Finish. On the second call it returns a plain
    /// stop. Used to test the mid-turn steer soft-stop → follow-up turn path
    /// at the AgentTask level.
    struct PausingMockProvider {
        gate: Arc<tokio::sync::Notify>,
        call_count: Arc<std::sync::atomic::AtomicU32>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for PausingMockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n > 0 {
                // Follow-up turn — just stop.
                return Ok(Box::pin(futures::stream::iter(vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }])));
            }
            // First turn: yield a chunk, then pause until released.
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![LlmEvent::TextDelta {
                text: "partial".into(),
            }])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    /// A provider that pauses mid-stream on the first call (like
    /// [`PausingMockProvider`]) AND captures the messages each `complete()`
    /// call receives, so tests can assert on the exact conversation the
    /// model sees (e.g. an injected steer message carrying image blocks).
    struct CapturingPausingProvider {
        gate: Arc<tokio::sync::Notify>,
        calls: Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for CapturingPausingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock"
        }
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let n = self.calls.lock().unwrap().len();
            self.calls.lock().unwrap().push(messages.to_vec());
            if n > 0 {
                // Follow-up turn — just stop.
                return Ok(Box::pin(futures::stream::iter(vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }])));
            }
            // First turn: yield a chunk, then pause until released.
            let gate = self.gate.clone();
            let stream = futures::stream::iter(vec![LlmEvent::TextDelta {
                text: "partial".into(),
            }])
            .chain(futures::stream::once(async move {
                gate.notified().await;
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }
            }));
            Ok(Box::pin(stream))
        }
    }

    #[tokio::test]
    async fn mid_turn_steer_soft_stops_and_runs_follow_up_turn() {
        // T1: a steer arriving mid-stream must soft-stop the turn (keeping
        // partial output) and run a follow-up turn with the steer as a user
        // message. Verified end-to-end through AgentTask::run: (1) the first
        // turn soft-stops (Finished), (2) SuggestionInjected fires, (3) a
        // second Finished fires (the follow-up turn ran).
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let gate = Arc::new(tokio::sync::Notify::new());
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(PausingMockProvider {
                    gate: gate.clone(),
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send the first prompt — the turn starts streaming "partial" then
        // pauses on the gate.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // Give the stream a moment to emit "partial" + reach the pause.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Send a steer mid-stream (while paused).
        cmd_tx
            .send(AgentCommand::Suggestion("do this instead".into()))
            .await
            .unwrap();
        // Give the select! a moment to process the steer (set soft_stop).
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Release the pause so the stream can finish — the soft-stop fires.
        gate.notify_one();

        // Collect events: expect (1) a first Finished (soft-stop), (2)
        // SuggestionInjected, (3) a second Finished (follow-up turn).
        let mut saw_first_finished = false;
        let mut saw_suggestion_injected = false;
        let mut saw_second_finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::SuggestionInjected { .. }) {
                        saw_suggestion_injected = true;
                    }
                    if matches!(event, AgentEvent::Finished { .. }) {
                        if !saw_first_finished {
                            saw_first_finished = true;
                        } else {
                            saw_second_finished = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_first_finished,
            "the first turn must soft-stop (Finished)"
        );
        assert!(
            saw_suggestion_injected,
            "the mid-turn steer must emit SuggestionInjected"
        );
        assert!(
            saw_second_finished,
            "the follow-up turn must run (second Finished)"
        );

        // Cancel to shut down the task.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn midturn_image_input_injects_user_message_with_image_block() {
        // REGRESSION (steered images dropped, 2027-01-07): an image-bearing
        // input sent while the agent runs — the steer path (the frontend's
        // steer branch; any Prompt folded mid-stream rides the same
        // fold → StopReason::Steer → injection pipeline) — must land as a
        // user message WITH image blocks, exactly like a normal prompt's
        // images. The fold used to accumulate bare Strings (images
        // discarded), so the injected message was Message::user_text and
        // the model never saw the image.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));

        let gate = Arc::new(tokio::sync::Notify::new());
        let calls: Arc<std::sync::Mutex<Vec<Vec<Message>>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(CapturingPausingProvider {
                    gate: gate.clone(),
                    calls: calls.clone(),
                    // Multimodal: image blocks are sent directly — the path
                    // a steered image must ride.
                    caps: Capabilities {
                        multimodal: true,
                        ..Capabilities::openai()
                    },
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // First prompt — the turn starts streaming "partial" then pauses on
        // the gate.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // An image-bearing input arrives mid-stream (while the agent runs)
        // — the steer path. It must soft-stop the turn and be injected WITH
        // the image.
        let data_url = "data:image/png;base64,iVBORw0KGgo=".to_string();
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "look at this screenshot".into(),
                images: vec![data_url.clone()],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Release the pause so the stream can finish — the soft-stop fires.
        gate.notify_one();

        // Collect events: (1) a first Finished (soft-stop), (2) a second
        // Finished (the follow-up turn ran).
        let mut saw_first_finished = false;
        let mut saw_second_finished = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        if !saw_first_finished {
                            saw_first_finished = true;
                        } else {
                            saw_second_finished = true;
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_second_finished,
            "the follow-up turn must run (second Finished)"
        );

        // The follow-up turn's request must carry the injected steer as a
        // user message with an image block (multimodal path) — not bare
        // text with the image silently dropped.
        let calls = calls.lock().unwrap();
        assert!(
            calls.len() >= 2,
            "the follow-up turn must have issued a request: {calls:?}"
        );
        let follow_up = &calls[1];
        let carries_image = follow_up.iter().any(|m| {
            m.role == Role::User
                && matches!(
                    &m.content,
                    MessageContent::Parts(parts) if parts.iter().any(|p| {
                        matches!(
                            p,
                            crate::provider::ContentPart::ImageUrl { image_url }
                                if image_url.url == data_url
                        )
                    })
                )
        });
        assert!(
            carries_image,
            "the injected steer must carry its image as an image block: {follow_up:?}"
        );

        // Cancel to shut down the task.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn mid_turn_multiple_steers_are_never_dropped() {
        // Regression: previously only the FIRST mid-turn steer soft-stopped and
        // later ones were silently discarded. Three steers sent mid-stream must
        // ALL be carried (SuggestionInjected × 3) and ALL become user messages
        // driving the follow-up turn — none dropped.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let gate = Arc::new(tokio::sync::Notify::new());
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(PausingMockProvider {
                    gate: gate.clone(),
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Start the first turn (streams "partial", then pauses on the gate).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Send THREE steers while the stream is paused.
        for text in ["first queued", "second queued", "third queued"] {
            cmd_tx
                .send(AgentCommand::Suggestion(text.into()))
                .await
                .unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        gate.notify_one();

        // Expect: first Finished (soft-stop), 3 SuggestionInjected (one per
        // queued steer — NONE dropped), then a second Finished (follow-up turn).
        let mut injected: Vec<String> = Vec::new();
        let mut finished_count = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < deadline && finished_count < 2 {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::SuggestionInjected { text, .. } => injected.push(text),
                    AgentEvent::Finished { .. } => finished_count += 1,
                    _ => {}
                },
                _ => break,
            }
        }
        assert_eq!(
            injected,
            vec![
                "first queued".to_string(),
                "second queued".to_string(),
                "third queued".to_string()
            ],
            "all three mid-turn steers must be carried in arrival order — none dropped"
        );
        assert_eq!(
            finished_count, 2,
            "soft-stop turn + follow-up turn must both finish"
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn cancelled_steer_is_not_injected() {
        // The "x" on a pending steer: a steer sent mid-stream is cancelled
        // (CancelSuggestion with the same text) before the turn ends, so it
        // must NOT be injected — no SuggestionInjected event, no follow-up
        // turn (only one Finished). The cancel folds the Steer to None in the
        // streaming select!, so the turn ends normally instead of soft-stopping.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let gate = Arc::new(tokio::sync::Notify::new());
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(PausingMockProvider {
                    gate: gate.clone(),
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Start the first turn (streams "partial", then pauses on the gate).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Send a steer, then cancel it — both while the stream is paused.
        cmd_tx
            .send(AgentCommand::Suggestion("do this instead".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cmd_tx
            .send(AgentCommand::CancelSuggestion("do this instead".into()))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        gate.notify_one();

        // Expect: ONE Finished (the turn ended normally — the steer was
        // cancelled so no soft-stop, no follow-up turn). NO SuggestionInjected.
        let mut injected: Vec<String> = Vec::new();
        let mut finished_count = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < deadline && finished_count < 1 {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::SuggestionInjected { text, .. } => injected.push(text),
                    AgentEvent::Finished { .. } => finished_count += 1,
                    _ => {}
                },
                _ => break,
            }
        }
        assert!(
            injected.is_empty(),
            "a cancelled steer must not be injected; got {injected:?}"
        );
        assert_eq!(
            finished_count, 1,
            "the turn must end normally (no follow-up turn) when the only steer is cancelled"
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn stop_with_queued_steers_runs_them_immediately() {
        // Regression: pressing Stop with commands queued mid-turn used to drop
        // the queued commands (the Interrupt overwrote the buffered steer).
        // Now the interrupt carries them (InterruptWithSteers): the turn stops
        // AND every queued command runs immediately as the follow-up turn.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let gate = Arc::new(tokio::sync::Notify::new());
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(PausingMockProvider {
                    gate: gate.clone(),
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Start the first turn (streams "partial", then pauses on the gate).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Queue two commands, THEN press Stop — the classic "stop, now do this"
        // flow. FIFO order guarantees the steers are processed before the
        // Interrupt.
        cmd_tx
            .send(AgentCommand::Suggestion("queued command one".into()))
            .await
            .unwrap();
        cmd_tx
            .send(AgentCommand::Suggestion("queued command two".into()))
            .await
            .unwrap();
        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        gate.notify_one();

        // Expect: first Finished (the stop), then SuggestionInjected × 2 (the
        // queued commands preserved + started immediately), then a second
        // Finished (the follow-up turn ran). The agent must stay ALIVE (an
        // Interrupt, not a Cancel — no Exited).
        let mut injected: Vec<String> = Vec::new();
        let mut finished_count = 0;
        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
        while std::time::Instant::now() < deadline && finished_count < 2 {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => match event {
                    AgentEvent::SuggestionInjected { text, .. } => injected.push(text),
                    AgentEvent::Finished { .. } => finished_count += 1,
                    AgentEvent::Exited => saw_exited = true,
                    _ => {}
                },
                _ => break,
            }
        }
        assert_eq!(
            injected,
            vec![
                "queued command one".to_string(),
                "queued command two".to_string()
            ],
            "Stop with queued commands must preserve + start every queued command"
        );
        assert_eq!(
            finished_count, 2,
            "stopped turn + immediate follow-up turn must both finish"
        );
        assert!(
            !saw_exited,
            "Stop must keep the agent alive (Interrupt, not Cancel)"
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn cancel_during_streaming_exits_agent() {
        // Bug 2 fix: Cancel during streaming must terminate the agent task
        // (Exited fires, tab closes). Previously Cancel ended the turn but the
        // agent stayed alive — contradicting the "Stop the agent entirely" doc.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let gate = Arc::new(tokio::sync::Notify::new());
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(PausingMockProvider {
                    gate: gate.clone(),
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt — the turn starts streaming "partial" then pauses.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // Give the stream a moment to emit "partial" + reach the pause.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Send Cancel mid-stream (while paused).
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();

        // Exited must fire — Cancel terminates the agent task.
        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_exited,
            "Cancel during streaming must emit Exited (agent task terminates)"
        );

        let _ = handle.await;
    }

    #[tokio::test]
    async fn interrupt_during_streaming_keeps_agent_alive() {
        // Bug 1 fix (streaming half): Interrupt during streaming must end the
        // turn (Finished) but NOT terminate the agent (no Exited). The agent
        // stays alive and returns to idle, ready for the next prompt. This is
        // the distinction from Cancel (which exits).
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let gate = Arc::new(tokio::sync::Notify::new());
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(PausingMockProvider {
                    gate: gate.clone(),
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt — the turn starts streaming "partial" then pauses.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        // Send Interrupt mid-stream (while paused).
        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();

        // Finished must fire (turn ended). Exited must NOT fire (agent alive).
        let mut saw_finished = false;
        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        saw_finished = true;
                        break;
                    }
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                    }
                }
                _ => break,
            }
        }
        assert!(saw_finished, "Interrupt must end the turn (Finished)");
        assert!(
            !saw_exited,
            "Interrupt must NOT exit the agent (no Exited — agent stays alive)"
        );
        // The task must still be running.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "agent task must still be alive after Interrupt"
        );

        // Clean up.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    /// A provider that emits a `create_plan` tool call on the first call (to
    /// transition the workflow to Executing, where file_write is allowed), then
    /// a `file_write` tool call on the second call (which triggers an
    /// ApprovalRequest under ApproveEachAction mode). Used to test the
    /// approval-interrupt/cancel paths.
    struct ToolCallProvider {
        call_count: Arc<std::sync::atomic::AtomicU32>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for ToolCallProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-toolcall"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let events = if n == 0 {
                // First call: create_plan (AutoRun — no approval needed even
                // under ApproveEachAction). Transitions Planning → Executing.
                vec![
                    LlmEvent::ToolCallStart {
                        index: 0,
                        id: "call_plan".into(),
                        name: "create_plan".into(),
                    },
                    LlmEvent::ToolCallArgumentDelta {
                        index: 0,
                        fragment: r#"{"title":"T","goal":"G","context":"Approval-flow fixture: edit src/widget.rs then verify with cargo test.","steps":["edit src/widget.rs"]}"#.into(),
                    },
                    LlmEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ]
            } else {
                // Second call: file_write (NeedsApproval under
                // ApproveEachAction). Now in Executing state, so the ToolFilter
                // allows it.
                vec![
                    LlmEvent::ToolCallStart {
                        index: 0,
                        id: "call_1".into(),
                        name: "file_write".into(),
                    },
                    LlmEvent::ToolCallArgumentDelta {
                        index: 0,
                        fragment: r#"{"path":"x.txt","content":"hi"}"#.into(),
                    },
                    LlmEvent::Finish {
                        reason: FinishReason::ToolCalls,
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn cancel_during_approval_exits_agent() {
        // Bug 1 fix (Cancel): Cancel while awaiting approval must terminate the
        // agent (Exited). Previously Cancel was consumed and mapped to a tool
        // error — the turn continued with the next tool call.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        // file_write is NeedsApproval; ApproveEachAction mode forces the prompt.
        // The provider first emits create_plan (→ Executing), then file_write.
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(ToolCallProvider {
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::ApproveEachAction,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt — the provider emits create_plan (→ Executing), then
        // file_write, which triggers an ApprovalRequest.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "write a file".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Wait for the ApprovalRequest (after create_plan completes), then
        // send Cancel.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::ApprovalRequest { .. }) {
                        // Send Cancel while awaiting approval.
                        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
                        break;
                    }
                }
                _ => break,
            }
        }

        // Exited must fire — Cancel terminates the agent.
        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_exited,
            "Cancel during approval must emit Exited (agent terminates)"
        );
        // The file must NOT have been written (the tool was never approved).
        assert!(
            !dir.path().join("x.txt").exists(),
            "file must not be written when Cancel arrives during approval"
        );

        let _ = handle.await;
    }

    #[tokio::test]
    async fn interrupt_during_approval_stops_turn_and_keeps_agent_alive() {
        // Bug 1 fix (Interrupt): Interrupt while awaiting approval must end the
        // turn (Finished) but NOT terminate the agent (no Exited). The tool is
        // not executed. Previously Interrupt was consumed and the turn continued
        // with the next tool call.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let call_count = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(ToolCallProvider {
                    call_count: call_count.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::ApproveEachAction,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt — the provider emits create_plan (→ Executing), then
        // file_write → approval.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "write a file".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Wait for the ApprovalRequest (after create_plan completes), then
        // send Interrupt.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::ApprovalRequest { .. }) {
                        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
                        break;
                    }
                }
                _ => break,
            }
        }

        // Finished must fire (turn ended). Exited must NOT fire (agent alive).
        let mut saw_finished = false;
        let mut saw_exited = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        saw_finished = true;
                        break;
                    }
                    if matches!(event, AgentEvent::Exited) {
                        saw_exited = true;
                    }
                }
                _ => break,
            }
        }
        assert!(
            saw_finished,
            "Interrupt during approval must end the turn (Finished)"
        );
        assert!(
            !saw_exited,
            "Interrupt during approval must NOT exit the agent (no Exited)"
        );
        // The file must NOT have been written.
        assert!(
            !dir.path().join("x.txt").exists(),
            "file must not be written when Interrupt arrives during approval"
        );
        // The task must still be running.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !handle.is_finished(),
            "agent task must still be alive after Interrupt during approval"
        );

        // Clean up.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn prompt_with_images_builds_multipart_message() {
        // When a Prompt carries images (base64 data URLs), the agent task
        // must build a multipart user message (text + image_url blocks),
        // not plain text. We verify by capturing the messages the provider
        // receives.
        use std::sync::Mutex as StdMutex;

        struct CapturingProvider {
            captured: Arc<StdMutex<Vec<Message>>>,
            caps: Capabilities,
        }

        #[async_trait]
        impl LlmClient for CapturingProvider {
            fn capabilities(&self) -> &Capabilities {
                &self.caps
            }
            fn kind(&self) -> ProviderKind {
                ProviderKind::OpenAI
            }
            fn model(&self) -> &str {
                "mock-capture"
            }
            async fn complete(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _tool_choice: Option<crate::provider::ToolChoice>,
            ) -> Result<BoxStream<'_, LlmEvent>> {
                // Capture the user message (the last user-role message).
                if let Some(user_msg) = messages.iter().rev().find(|m| m.role == Role::User) {
                    self.captured.lock().unwrap().push(user_msg.clone());
                }
                Ok(Box::pin(futures::stream::iter(vec![LlmEvent::Finish {
                    reason: FinishReason::Stop,
                }])))
            }
        }

        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));
        let embedder: Arc<dyn crate::memory::Embedder> = Arc::new(HashEmbedder::new());
        let store: Arc<dyn crate::memory::MemoryStoreTrait> =
            Arc::new(MemoryStore::open_in_memory(embedder).unwrap());
        registry.register(Box::new(MemoryWriteTool::new(store.clone())));
        registry.register(Box::new(MemoryRecallTool::new(store)));

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(CapturingProvider {
                    captured: captured.clone(),
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);

        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Send a prompt with an image attachment (a tiny base64 data URL).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "What's in this image?".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            })
            .await
            .unwrap();

        // Wait for the turn to finish.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(2000), fanin_rx.recv())
                .await
            {
                Ok(Some((_id, event))) => {
                    if matches!(event, AgentEvent::Finished { .. }) {
                        break;
                    }
                }
                _ => break,
            }
        }

        // The captured user message must be multipart (Parts), not plain text.
        let messages = captured.lock().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "provider should have received one user message"
        );
        match &messages[0].content {
            crate::provider::MessageContent::Parts(parts) => {
                // text block + image_url block.
                assert_eq!(parts.len(), 2, "expected 2 parts (text + image)");
                assert!(matches!(
                    parts[0],
                    crate::provider::ContentPart::Text { .. }
                ));
                assert!(matches!(
                    parts[1],
                    crate::provider::ContentPart::ImageUrl { .. }
                ));
            }
            other => panic!("expected Parts, got {other:?}"),
        }

        // Cancel to shut down.
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    /// Shared harness: a provider that captures the last user message it
    /// receives (so tests can inspect what the agent actually sent).
    struct CapturingProvider {
        captured: std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for CapturingProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-capture"
        }
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            if let Some(user_msg) = messages.iter().rev().find(|m| m.role == Role::User) {
                self.captured.lock().unwrap().push(user_msg.clone());
            }
            Ok(Box::pin(futures::stream::iter(vec![LlmEvent::Finish {
                reason: FinishReason::Stop,
            }])))
        }
    }

    /// Build an agent task with a capturing provider + optional vision client,
    /// spawn it, and return (captured messages, cmd sender, event-fanin
    /// receiver, task handle). The fanin receiver lets tests assert the
    /// announcement events (Started, VisionDescribe/…Described, …) the task
    /// emits around a turn.
    fn spawn_capturing_task(
        caps: Capabilities,
        vision: Option<std::sync::Arc<dyn crate::provider::vision::ImageDescriber>>,
    ) -> (
        std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
        mpsc::Sender<AgentCommand>,
        mpsc::Receiver<(AgentId, AgentEvent)>,
        tokio::task::JoinHandle<()>,
    ) {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(FileWriteTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(CapturingProvider {
                    captured: captured.clone(),
                    caps,
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: vision,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        (captured, cmd_tx, fanin_rx, handle)
    }

    /// Wait until the provider has captured at least one user message (or
    /// time out), so the test doesn't race the turn.
    async fn wait_for_capture(
        captured: &std::sync::Arc<std::sync::Mutex<Vec<Message>>>,
    ) -> Vec<Message> {
        // Generous deadline: the vision-fallback test awaits a failed HTTP
        // connect to an unreachable host before the provider is ever called,
        // and a refused/blackholed connect can take a couple of seconds.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            {
                let msgs = captured.lock().unwrap();
                if !msgs.is_empty() {
                    return msgs.clone();
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for the provider to receive a user message"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A mock vision model returning a canned description — no network, so
    /// the fallback path is exercised without a live connection.
    struct CannedDescriber {
        description: String,
    }

    #[async_trait]
    impl crate::provider::vision::ImageDescriber for CannedDescriber {
        async fn describe_image(&self, _image_url: &str, _prompt: &str) -> Result<String> {
            Ok(self.description.clone())
        }
    }

    /// A mock vision model that always fails — exercises the graceful
    /// per-image degradation without a real connection error.
    struct FailingDescriber;

    #[async_trait]
    impl crate::provider::vision::ImageDescriber for FailingDescriber {
        async fn describe_image(&self, _image_url: &str, _prompt: &str) -> Result<String> {
            Err(crate::error::Error::Provider("vision is down".into()))
        }
    }

    #[tokio::test]
    async fn non_multimodal_with_vision_injects_image_description_text() {
        // A non-multimodal provider + a vision model: the image attachment
        // must NOT be sent as an image block — it's described via the vision
        // model and the description is injected as text.
        let mut caps = Capabilities::openai();
        caps.multimodal = false;
        let vision = std::sync::Arc::new(CannedDescriber {
            description: "a photo of a cat".into(),
        });

        let (captured, cmd_tx, mut fanin_rx, handle) = spawn_capturing_task(caps, Some(vision));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "What's in this image?".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            })
            .await
            .unwrap();

        // The vision round-trip must be announced BEFORE the turn: a
        // VisionDescribe (index/total + the query sent to the vision model)
        // followed by a VisionDescribed carrying the description — this is
        // what the transcript's "image parsing" card renders (the pause
        // before this fix was silent because Started only fires in run_turn).
        let mut saw_describe = None;
        let mut saw_described = None;
        while saw_described.is_none() {
            let (_, event) =
                tokio::time::timeout(std::time::Duration::from_secs(20), fanin_rx.recv())
                    .await
                    .expect("timed out waiting for events")
                    .expect("event channel closed");
            match event {
                AgentEvent::VisionDescribe {
                    index,
                    total,
                    query,
                } => {
                    saw_describe = Some((index, total, query));
                }
                AgentEvent::VisionDescribed {
                    index,
                    total,
                    success,
                    description,
                } => {
                    saw_described = Some((index, total, success, description));
                }
                _ => {}
            }
        }
        let (d_index, d_total, d_query) =
            saw_describe.expect("VisionDescribe must precede VisionDescribed");
        assert_eq!(d_index, 1, "1-based image index");
        assert_eq!(d_total, 1, "one attachment");
        assert!(
            d_query.contains("Describe this image"),
            "the query must be the prompt sent to the vision model, got: {d_query}"
        );
        let (index, total, success, description) = saw_described.unwrap();
        assert_eq!(index, 1);
        assert_eq!(total, 1);
        assert!(success, "canned describer must succeed");
        assert_eq!(description, "a photo of a cat");

        let messages = wait_for_capture(&captured).await;
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            crate::provider::MessageContent::Text(t) => {
                assert!(t.contains("What's in this image?"));
                assert!(
                    t.contains("[image 1 description: a photo of a cat]"),
                    "expected the injected image description, got: {t}"
                );
            }
            crate::provider::MessageContent::Parts(_) => {
                panic!("non-multimodal provider must receive a text-only message")
            }
        }

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn steer_with_image_injects_user_message_with_image_block() {
        // REGRESSION (steered images dropped, 2027-01-07): a steer carrying
        // an image must land as a user message WITH the image block — the
        // same multimodal handling as a normal prompt. Previously
        // Suggestion carried only text and the injection site pushed
        // Message::user_text, so the model never saw the image.
        let caps = Capabilities {
            multimodal: true,
            ..Capabilities::openai()
        };
        let (captured, cmd_tx, mut fanin_rx, handle) = spawn_capturing_task(caps, None);
        cmd_tx
            .send(AgentCommand::Suggestion(crate::runtime::SteerPayload {
                text: "look at this screenshot".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            }))
            .await
            .unwrap();

        // The steer lands: SuggestionInjected must carry the images so the
        // transcript's steer entry can render them.
        let mut saw_injected_with_images = false;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            let ev = tokio::time::timeout(
                std::time::Duration::from_millis(2000),
                fanin_rx.recv(),
            )
            .await;
            match ev {
                Ok(Some((
                    _,
                    AgentEvent::SuggestionInjected { text, images },
                ))) => {
                    assert_eq!(text, "look at this screenshot");
                    assert_eq!(
                        images,
                        vec!["data:image/png;base64,iVBORw0KGgo=".to_string()],
                        "SuggestionInjected must carry the steer's images"
                    );
                    saw_injected_with_images = true;
                }
                Ok(Some((_, AgentEvent::Finished { .. }))) => break,
                _ => break,
            }
        }
        assert!(
            saw_injected_with_images,
            "SuggestionInjected must fire with the images"
        );

        // The follow-up turn's request carries the injected steer as a user
        // message with an image block (multimodal path).
        let msgs = wait_for_capture(&captured).await;
        let carries_image = msgs.iter().any(|m| {
            matches!(
                &m.content,
                MessageContent::Parts(parts) if parts.iter().any(|p| {
                    matches!(
                        p,
                        crate::provider::ContentPart::ImageUrl { image_url }
                            if image_url.url == "data:image/png;base64,iVBORw0KGgo="
                    )
                })
            )
        });
        assert!(
            carries_image,
            "the injected steer must carry its image as an image block: {msgs:?}"
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn non_multimodal_with_vision_describes_steered_image() {
        // REGRESSION (steered images dropped, 2027-01-07): the vision
        // fallback must fire for a STEERED image on a non-multimodal model —
        // VisionDescribe/VisionDescribed events, then the description folded
        // into the injected steer's text (exactly like a normal prompt's
        // images). Previously the steer pipeline was text-only and the image
        // vanished.
        let mut caps = Capabilities::openai();
        caps.multimodal = false;
        let vision = std::sync::Arc::new(CannedDescriber {
            description: "a photo of a cat".into(),
        });

        let (captured, cmd_tx, mut fanin_rx, handle) = spawn_capturing_task(caps, Some(vision));
        cmd_tx
            .send(AgentCommand::Suggestion(crate::runtime::SteerPayload {
                text: "what is in this screenshot?".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            }))
            .await
            .unwrap();

        // The vision round-trip must be announced: VisionDescribe then
        // VisionDescribed with the canned description.
        let mut saw_describe = None;
        let mut saw_described = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while saw_described.is_none() && std::time::Instant::now() < deadline {
            let ev = tokio::time::timeout(
                std::time::Duration::from_millis(2000),
                fanin_rx.recv(),
            )
            .await;
            let (_, event) = ev
                .expect("timed out waiting for events")
                .expect("event channel closed");
            match event {
                AgentEvent::VisionDescribe {
                    index,
                    total,
                    query,
                } => saw_describe = Some((index, total, query)),
                AgentEvent::VisionDescribed {
                    index,
                    total,
                    success,
                    description,
                } => saw_described = Some((index, total, success, description)),
                _ => {}
            }
        }
        let (d_index, d_total, _d_query) =
            saw_describe.expect("VisionDescribe must precede VisionDescribed");
        assert_eq!(d_index, 1, "1-based image index");
        assert_eq!(d_total, 1, "one attachment");
        let (index, total, success, description) =
            saw_described.expect("VisionDescribed must fire for a steered image");
        assert_eq!(index, 1);
        assert_eq!(total, 1);
        assert!(success, "canned describer must succeed");
        assert_eq!(description, "a photo of a cat");

        // The injected steer must be text-only with the description folded
        // in (the non-multimodal provider must not receive an image block).
        let messages = wait_for_capture(&captured).await;
        let folded = messages.iter().any(|m| match &m.content {
            crate::provider::MessageContent::Text(t) => {
                t.contains("what is in this screenshot?")
                    && t.contains("[image 1 description: a photo of a cat]")
            }
            crate::provider::MessageContent::Parts(_) => false,
        });
        assert!(
            folded,
            "the injected steer must fold the vision description into its text: {messages:?}"
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn non_multimodal_vision_failure_degrades_gracefully() {
        // When the vision model errors, the image is replaced by a
        // "description unavailable" note and the turn still proceeds (the
        // provider still receives a text-only message, not an error abort).
        let mut caps = Capabilities::openai();
        caps.multimodal = false;
        let (captured, cmd_tx, mut fanin_rx, handle) =
            spawn_capturing_task(caps, Some(std::sync::Arc::new(FailingDescriber)));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "look at this".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            })
            .await
            .unwrap();

        // The failed round-trip must still be PAIRED: VisionDescribe, then a
        // VisionDescribed with success=false carrying the error text — the
        // transcript card shows the failure instead of hanging open.
        let mut saw_described = None;
        while saw_described.is_none() {
            let (_, event) =
                tokio::time::timeout(std::time::Duration::from_secs(20), fanin_rx.recv())
                    .await
                    .expect("timed out waiting for events")
                    .expect("event channel closed");
            if let AgentEvent::VisionDescribed {
                index,
                total,
                success,
                description,
            } = event
            {
                saw_described = Some((index, total, success, description));
            }
        }
        let (index, total, success, description) = saw_described.unwrap();
        assert_eq!(index, 1);
        assert_eq!(total, 1);
        assert!(!success, "failing describer must mark the event failed");
        assert!(description.contains("vision is down"), "got: {description}");

        let messages = wait_for_capture(&captured).await;
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            crate::provider::MessageContent::Text(t) => {
                assert!(t.contains("look at this"));
                assert!(
                    t.contains("[image 1: description unavailable:"),
                    "expected a graceful degradation note, got: {t}"
                );
                assert!(t.contains("vision is down"));
            }
            crate::provider::MessageContent::Parts(_) => {
                panic!("non-multimodal provider must receive a text-only message")
            }
        }

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn multimodal_provider_sends_image_blocks_directly() {
        // A multimodal provider with a vision client configured still takes
        // the multipart path (image blocks sent as-is) — the vision fallback
        // is only for text-only providers.
        let mut caps = Capabilities::openai();
        caps.multimodal = true;
        let (captured, cmd_tx, _fanin_rx, handle) = spawn_capturing_task(caps, None);
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "describe".into(),
                images: vec!["data:image/png;base64,iVBORw0KGgo=".into()],
            })
            .await
            .unwrap();

        let messages = wait_for_capture(&captured).await;
        assert_eq!(messages.len(), 1);
        match &messages[0].content {
            crate::provider::MessageContent::Parts(parts) => {
                assert!(
                    parts
                        .iter()
                        .any(|p| matches!(p, crate::provider::ContentPart::ImageUrl { .. })),
                    "multimodal provider should receive an image_url part"
                );
            }
            crate::provider::MessageContent::Text(_) => {
                panic!("multimodal provider should receive a multipart message")
            }
        }

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    /// A provider that always fails, capturing the full message list each
    /// request saw (so tests can inspect what the agent actually sent).
    struct AlwaysFailProvider {
        caps: Capabilities,
        /// One snapshot of the `messages` slice per `complete()` call.
        captured: std::sync::Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
    }

    #[async_trait]
    impl LlmClient for AlwaysFailProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-fail"
        }
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.captured.lock().unwrap().push(messages.to_vec());
            Err(crate::error::Error::Provider("provider exploded".into()))
        }
    }

    /// A provider whose `complete` fails the first `fail_times` calls with a
    /// generic retryable error, then returns a minimal stop stream; captures
    /// every `record_backoff_ms` call (the parked retry sleeps — request-level
    /// from `complete_with_retry` and turn-level from `run_turn_attempt`).
    struct FlakyTurnProvider {
        caps: Capabilities,
        calls: std::sync::Arc<std::sync::atomic::AtomicU32>,
        fail_times: u32,
        backoffs: std::sync::Arc<std::sync::Mutex<Vec<u32>>>,
    }

    #[async_trait]
    impl LlmClient for FlakyTurnProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-flaky-turn"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < self.fail_times {
                return Err(crate::error::Error::Provider("flaky turn failure".into()));
            }
            let stream = futures::stream::iter(vec![LlmEvent::Finish {
                reason: crate::provider::FinishReason::Stop,
            }]);
            Ok(Box::pin(stream))
        }

        fn record_backoff_ms(&self, ms: u32) {
            self.backoffs.lock().unwrap().push(ms);
        }
    }

    /// Regression (trace-graph honesty): the turn-level retry sleeps (the
    /// waits in `run_turn_attempt` between whole-turn attempts) used to be
    /// attributed to NO trace record — an invisible gap between per-attempt
    /// columns. Now each sleep is parked on the provider via
    /// `record_backoff_ms` right after it elapses, so the NEXT turn attempt's
    /// first record carries it as `backoff_ms`. With fail_times=3: turn 1
    /// exhausts `complete_with_retry` (2 request-level parks), fails, sleeps
    /// once at the turn level (1 turn-level park), and turn 2 succeeds on
    /// call 4. Run under paused time (`start_paused`) so the jittered backoff
    /// completes instantly.
    #[tokio::test(start_paused = true)]
    async fn run_turn_attempt_attributes_backoff_to_next_attempt() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let backoffs = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(FlakyTurnProvider {
                    caps: Capabilities::openai(),
                    calls: calls.clone(),
                    fail_times: 3,
                    backoffs: backoffs.clone(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();

        // Drain events until turn 2 succeeds and the turn finishes.
        loop {
            let Some((_id, event)) = fanin_rx.recv().await else {
                panic!("fanin channel closed before the turn finished")
            };
            if matches!(event, AgentEvent::Finished { .. }) {
                break;
            }
        }
        handle.abort();

        // Parks: [request attempt 1 (500–1000), request attempt 2
        // (1000–2000), turn attempt 1 (500–1000)] — the third park is the
        // regression: the turn-level sleep used to be attributed to nothing.
        let parked = backoffs.lock().unwrap().clone();
        assert_eq!(parked.len(), 3, "one park per sleep: {parked:?}");
        assert!(
            (500..=1000).contains(&parked[0]),
            "request attempt-1 backoff out of band: {}",
            parked[0]
        );
        assert!(
            (1000..=2000).contains(&parked[1]),
            "request attempt-2 backoff out of band: {}",
            parked[1]
        );
        assert!(
            (500..=1000).contains(&parked[2]),
            "turn attempt-1 backoff out of band: {}",
            parked[2]
        );
        assert!(
            calls.load(std::sync::atomic::Ordering::SeqCst) >= 4,
            "turn 2 must reach the provider after the turn-level retry"
        );
    }

    /// Regression: a terminal provider failure must be recorded in-context as
    /// a harness-note user message, so the NEXT turn shows the model WHY its
    /// last request failed (enabling self-correction). Run under paused time
    /// (`start_paused`) so the 1s/2s retry backoff completes instantly.
    #[tokio::test(start_paused = true)]
    async fn terminal_provider_failure_is_pushed_in_context() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));
        registry.register(Box::new(CompleteStepTool::new(workflow.clone())));

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(AlwaysFailProvider {
                    caps: Capabilities::openai(),
                    captured: captured.clone(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Turn 1: the provider always fails → after the retry budget the
        // terminal Error (retrying: false) arrives and the note is pushed.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "hello".into(),
                images: vec![],
            })
            .await
            .unwrap();
        loop {
            let Some((_id, event)) = fanin_rx.recv().await else {
                panic!("fanin channel closed before the terminal error")
            };
            if matches!(
                event,
                AgentEvent::Error {
                    retrying: false,
                    ..
                }
            ) {
                break;
            }
        }

        // Turn 2: drive a second turn. Its requests must include the harness
        // note recording turn 1's failure. Wait for the second terminal error
        // so all of turn 2's provider calls are captured.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "what happened?".into(),
                images: vec![],
            })
            .await
            .unwrap();
        loop {
            let Some((_id, event)) = fanin_rx.recv().await else {
                panic!("fanin channel closed before the second terminal error")
            };
            if matches!(
                event,
                AgentEvent::Error {
                    retrying: false,
                    ..
                }
            ) {
                break;
            }
        }

        let snaps = captured.lock().unwrap();
        assert!(
            snaps.len() >= 2,
            "expected multiple provider calls, got {}",
            snaps.len()
        );
        // Turn 1's first request predates the failure — no note yet.
        let first_has_note = snaps[0]
            .iter()
            .any(|m| m.role == Role::User && m.content.as_text().contains("[harness note]"));
        assert!(
            !first_has_note,
            "turn 1's first request unexpectedly contained a harness note"
        );
        // Turn 2's requests include the in-context note with the error detail.
        let last = snaps.last().unwrap();
        let note = last
            .iter()
            .find(|m| m.role == Role::User && m.content.as_text().contains("[harness note]"));
        let note = note.expect("turn 2 request did not include the in-context harness note");
        assert!(
            note.content.as_text().contains("provider exploded"),
            "harness note lacked the provider error detail: {}",
            note.content.as_text()
        );

        cmd_tx.send(AgentCommand::Cancel).await.unwrap();
        let _ = handle.await;
    }

    /// Regression: a failed `/compact` (the summarization LLM call errors)
    /// must surface an `AgentEvent::Error` to the UI instead of being silently
    /// swallowed (the old `.unwrap_or_else(|_| …)` hid it entirely — the user
    /// saw nothing happen). The message list is preserved unchanged.
    #[tokio::test]
    async fn compact_failure_emits_error_event_and_preserves_messages() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));

        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(AlwaysFailProvider {
                    caps: Capabilities::openai(),
                    captured,
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let mut task = AgentTask::new(1, "test".into(), agent_loop);
        // Pre-populate enough messages that compaction actually runs
        // (system + > keep_recent(6)+1 turns).
        task.messages.push(Message::system("system prompt"));
        for i in 0..10 {
            task.messages.push(Message::user_text(format!("message {i}")));
        }
        let original_len = task.messages.len();

        let outcome = task.compact_context(&fanin_tx, &mut cmd_rx).await;

        // The failure is reported as Failed (not an interrupt/cancel, not a
        // success) so callers don't resume with a false "compacted" note.
        assert!(matches!(outcome, CompactOutcome::Failed));
        // Messages preserved unchanged — no partial summary applied.
        assert_eq!(task.messages.len(), original_len);
        assert_eq!(task.messages[0].content.as_text(), "system prompt");
        assert_eq!(task.messages[1].content.as_text(), "message 0");
        // A retrying Error event carrying the failure detail must have been
        // emitted (previously nothing was emitted at all).
        let mut saw_error = false;
        while let Ok((_id, event)) = fanin_rx.try_recv() {
            if let AgentEvent::Error { error, retrying } = event {
                assert!(retrying, "compaction failure should be a transient note");
                assert!(
                    error.contains("context compaction failed"),
                    "error should name the compaction failure: {error}"
                );
                saw_error = true;
            }
        }
        assert!(
            saw_error,
            "a failed compaction must emit an Error event (not be swallowed)"
        );
    }

    /// A successful `/compact` (provider returns a summary) must emit a
    /// `Compacted` confirmation event with a real before → after reduction.
    #[tokio::test]
    async fn compact_success_emits_compacted_event_with_reduction() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));

        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(MockProvider {
                    caps: Capabilities::openai(),
                }),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let mut task = AgentTask::new(1, "test".into(), agent_loop);
        // System prompt + many large turns so the post-compaction count is
        // genuinely smaller than the pre-compaction count.
        task.messages.push(Message::system("system prompt"));
        let big = "word ".repeat(500);
        for _ in 0..12 {
            task.messages.push(Message::user_text(big.clone()));
        }

        let outcome = task.compact_context(&fanin_tx, &mut cmd_rx).await;
        assert!(matches!(outcome, CompactOutcome::Completed));

        // A Compacted event with a real reduction must have been emitted.
        let mut compacted: Option<(u32, u32)> = None;
        while let Ok((_id, event)) = fanin_rx.try_recv() {
            if let AgentEvent::Compacted { before, after } = event {
                compacted = Some((before, after));
            }
        }
        let (before, after) =
            compacted.expect("a successful compaction must emit a Compacted event");
        assert!(
            after < before,
            "compaction should reduce tokens: before={before} after={after}"
        );
    }

    /// Provider for the mid-turn-compact resume regression test: the first
    /// request streams one delta then hangs (so `/compact` lands mid-stream),
    /// the second (the compaction summarization call) returns a canned
    /// summary, and the third (the resumed follow-up turn) completes
    /// normally. Records the request count so the test can assert execution
    /// continued after compaction.
    struct CompactResumeProvider {
        calls: std::sync::atomic::AtomicUsize,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for CompactResumeProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-compact-resume"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                // First turn: emit a delta, then park so /compact arrives
                // mid-stream.
                let stream = futures::stream::once(async {
                    LlmEvent::TextDelta {
                        text: "working…".into(),
                    }
                })
                .chain(futures::stream::pending::<LlmEvent>());
                return Ok(Box::pin(stream));
            }
            let text = if n == 1 {
                "compressed state"
            } else {
                "resumed answer"
            };
            let events = vec![
                LlmEvent::TextDelta { text: text.into() },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Regression (2026-08-22): a `/compact` arriving mid-turn aborted the
    /// turn, compacted, and then left the agent IDLE — the interrupted work
    /// never resumed ("when you compact mid-flight, it works but it stops
    /// execution"). The agent must compact and then CONTINUE: a follow-up
    /// turn runs on the compacted conversation.
    #[tokio::test]
    async fn mid_turn_compact_resumes_execution_on_compacted_context() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));

        let provider = Arc::new(CompactResumeProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
            caps: Capabilities::openai(),
        });
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: provider.clone(),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let mut task = AgentTask::new(1, "test".into(), agent_loop);
        // Pre-populate enough messages that compaction actually runs
        // (system + > keep_recent(6)+1 turns).
        task.messages.push(Message::system("system prompt"));
        for i in 0..10 {
            task.messages.push(Message::user_text(format!("message {i}")));
        }
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        // Kick off the work, then compact mid-turn (the first provider call
        // streams one delta then hangs, so the command lands mid-stream).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cmd_tx.send(AgentCommand::Compact).await.unwrap();

        // The agent must serve THREE requests: the aborted turn, the
        // summarization call, and the resumed follow-up turn — and emit
        // Finished twice (aborted turn + resumed turn). Pre-fix the agent
        // went idle after compaction: no third request, no second Finished.
        let mut finished = 0u32;
        let wait = async {
            while let Some((_id, event)) = fanin_rx.recv().await {
                if let AgentEvent::Finished { .. } = event {
                    finished += 1;
                }
                if finished >= 2 && provider.calls.load(std::sync::atomic::Ordering::SeqCst) >= 3 {
                    break;
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .expect("the agent must resume after a mid-turn compact (no second Finished / third request)");
        assert_eq!(
            provider.calls.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "expected turn + summarization + resumed turn = 3 requests"
        );

        // Cleanup: close the command channel so the run loop exits.
        drop(cmd_tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("agent task should exit once the command channel closes")
            .unwrap();
    }

    /// Provider for the failed-mid-turn-compact test: the first request
    /// streams one delta then hangs (so `/compact` lands mid-stream); every
    /// later request (the compaction summarization call) FAILS. Records the
    /// request count so the test can assert NO follow-up turn runs after a
    /// failed compaction.
    struct CompactFailProvider {
        calls: std::sync::atomic::AtomicUsize,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for CompactFailProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-compact-fail"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            use futures::StreamExt;
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                // First turn: emit a delta, then park so /compact arrives
                // mid-stream.
                let stream = futures::stream::once(async {
                    LlmEvent::TextDelta {
                        text: "working…".into(),
                    }
                })
                .chain(futures::stream::pending::<LlmEvent>());
                return Ok(Box::pin(stream));
            }
            Err(crate::error::Error::Provider("summarization boom".into()))
        }
    }

    /// Regression (review finding, 2026-08-22): a mid-turn `/compact` whose
    /// summarization call FAILS must not push the false "[harness note]
    /// Context was compacted mid-task…" message nor resume execution on the
    /// uncompacted conversation — the agent reports the error and goes idle.
    #[tokio::test]
    async fn mid_turn_compact_failure_goes_idle_without_resume() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));

        let provider = Arc::new(CompactFailProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
            caps: Capabilities::openai(),
        });
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: provider.clone(),
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let mut task = AgentTask::new(1, "test".into(), agent_loop);
        // Pre-populate enough messages that compaction actually runs.
        task.messages.push(Message::system("system prompt"));
        for i in 0..10 {
            task.messages.push(Message::user_text(format!("message {i}")));
        }
        let handle = tokio::spawn(task.run(cmd_rx, fanin_tx));

        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cmd_tx.send(AgentCommand::Compact).await.unwrap();

        // Phase 1: the aborted turn emits Finished; the summarization call
        // fails (request #2) with a surfaced error. Break on the ERROR event
        // itself (it implies request #2 happened) — breaking on the raw call
        // count races the event's delivery.
        let mut finished = 0u32;
        let mut saw_compact_error = false;
        let wait = async {
            while let Some((_id, event)) = fanin_rx.recv().await {
                match event {
                    AgentEvent::Finished { .. } => finished += 1,
                    AgentEvent::Error { error, .. }
                        if error.contains("context compaction failed") =>
                    {
                        saw_compact_error = true;
                    }
                    _ => {}
                }
                if finished >= 1 && saw_compact_error {
                    break;
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), wait)
            .await
            .expect("turn abort + failed summarization should happen");
        // Phase 2: grace window — a buggy resume would fire a third request.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        while let Ok((_id, event)) = fanin_rx.try_recv() {
            if let AgentEvent::Finished { .. } = event {
                finished += 1;
            }
        }
        assert_eq!(
            provider.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a failed compaction must not resume the turn (no third request)"
        );
        assert_eq!(finished, 1, "only the aborted turn emits Finished");
        assert!(saw_compact_error, "the compaction failure must be surfaced");

        drop(cmd_tx);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("agent task should exit once the command channel closes")
            .unwrap();
    }

    /// Review finding (2026-08-22): an interrupted compaction must pair the
    /// CompactStarted announcement with an end note — never a dangling
    /// "Compacting context…" — and report Stopped (not Completed/Failed).
    #[tokio::test]
    async fn compact_interrupted_emits_end_note_and_reports_stopped() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let mut registry = ToolRegistry::new();
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        registry.register(Box::new(FileReadTool::new((*sandbox).clone())));
        registry.register(Box::new(CreatePlanTool::new(workflow.clone())));

        // CompactResumeProvider's first call hangs, so the pre-queued
        // Interrupt is the only ready select! branch — deterministic.
        let provider = Arc::new(CompactResumeProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
            caps: Capabilities::openai(),
        });
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: provider,
                tools: Arc::new(registry),
                workflow: workflow,
                sandbox: sandbox,
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));

        let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let mut task = AgentTask::new(1, "test".into(), agent_loop);
        // Pre-populate enough messages that compaction actually runs.
        task.messages.push(Message::system("system prompt"));
        for i in 0..10 {
            task.messages.push(Message::user_text(format!("message {i}")));
        }

        // Queue the Interrupt BEFORE the summarization call starts.
        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
        let outcome = task.compact_context(&fanin_tx, &mut cmd_rx).await;
        assert!(matches!(
            outcome,
            CompactOutcome::Stopped(crate::agent::StopReason::Interrupt)
        ));

        let mut saw_started = false;
        let mut saw_interrupt_note = false;
        let mut saw_compacted = false;
        while let Ok((_id, event)) = fanin_rx.try_recv() {
            match event {
                AgentEvent::CompactStarted => saw_started = true,
                AgentEvent::Error { error, .. } if error.contains("Compaction interrupted") => {
                    saw_interrupt_note = true;
                }
                AgentEvent::Compacted { .. } => saw_compacted = true,
                _ => {}
            }
        }
        assert!(saw_started, "compact_context announces the start");
        assert!(
            saw_interrupt_note,
            "the interrupted compaction must emit an end note"
        );
        assert!(!saw_compacted, "no compaction happened — no Compacted");
    }

    /// A mock provider that counts `complete` calls via a shared atomic so
    /// external test code can observe request counts while instances live
    /// behind `Arc<dyn LlmClient>` trait objects.
    struct CountingMockProvider {
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for CountingMockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-counting"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let events = vec![
                LlmEvent::TextDelta {
                    text: "Done!".into(),
                },
                LlmEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn prompt_resets_auto_continue_budget_after_exhaustion() {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        // Put the workflow into Executing state so auto-continue drives.
        workflow
            .lock()
            .await
            .create_plan("test", "goal", "ctx", vec!["step".into()])
            .unwrap();
        // Sibling dual-handle idiom: an external `calls` Arc the test reads,
        // cloned inline into the config's provider field (binding a concrete
        // Arc then Arc::clone-ing into the field hits E0308 here).
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(CountingMockProvider {
                    caps: Capabilities::openai(),
                    calls: Arc::clone(&calls),
                }),
                tools: Arc::new(ToolRegistry::new()),
                workflow: workflow,
                sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        ));
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // PHASE 1 — a fresh Prompt resets the streak; the agent auto-continues
        // MAX_AUTO_CONTINUE times, so the provider is called MAX+1 times then
        // parks. Drain the fan-in channel every iteration so the spawned task
        // never blocks on a full channel.
        let cap = MAX_AUTO_CONTINUE as usize + 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("auto-continue did not exhaust budget within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // PHASE 2 — the agent is parked; no further calls arrive.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            cap,
            "agent must park after exhausting the auto-continue budget"
        );
        // PHASE 3 — a fresh Prompt resets the streak and resumes driving.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "fresh task".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap + 1 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("fresh prompt did not resume driving within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn suggestion_resets_auto_continue_budget_after_exhaustion() {
        // Regression (2027-01-07, observed live): a turn parked in the closing
        // sequence after the round-2 reviewer had finished — no descendants
        // running, workflow Reviewing — and only a manual "c" resumed it.
        // The between-turn Suggestion (the child-completion notification,
        // the closing sequence's ONLY resume mechanism) did not reset the
        // auto-continue streak (only the Prompt arm did), so with the budget
        // exhausted the Suggestion-driven turn ended, the None-arm gate
        // failed on `streak < MAX`, and the agent parked. A Suggestion is
        // external progress — a user steer or a finished descendant — not
        // mere guidance, so it must refresh the budget like a Prompt.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        // The closing-sequence state: an Implementation plan whose only
        // step is complete → Reviewing (workflow-level index 0-based).
        workflow
            .lock()
            .await
            .create_plan_with_kind(
                "test",
                "goal",
                "ctx",
                vec!["step".into()],
                crate::workflow::PlanKind::Implementation,
                None,
            )
            .unwrap();
        workflow.lock().await.complete_step(0).unwrap();
        assert_eq!(
            workflow.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing,
            "setup: the root Implementation plan must be in Reviewing"
        );
        // Sibling dual-handle idiom (see the Prompt twin above).
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(AgentLoop::new(
            AgentLoopConfig {
                provider: Arc::new(CountingMockProvider {
                    caps: Capabilities::openai(),
                    calls: Arc::clone(&calls),
                }),
                tools: Arc::new(ToolRegistry::new()),
                workflow,
                sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                safety_mode: SafetyMode::Autonomous,
                context_manager: ContextManager::new(128_000, 0.5),
                memory: None,
                vision: None,
            },
            crate::project::Constitution::default(),
        )
        .with_agent_id(1));
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "run the closing sequence".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // PHASE 1 — the Prompt drives the auto-continue chain to the
        // plateau: MAX+1 provider calls, then the budget-exhaustion park.
        let cap = MAX_AUTO_CONTINUE as usize + 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("auto-continue did not exhaust budget within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // PHASE 2 — the agent is parked with the budget exhausted; no
        // further calls arrive.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            cap,
            "agent must park after exhausting the auto-continue budget"
        );
        // PHASE 3 — the reviewer-finish Suggestion (the closing sequence's
        // resume) arrives. It must reset the streak: the Suggestion-driven
        // turn runs (call cap+1) and, when that turn ends, the None-arm
        // auto-continues again (call cap+2). Pre-fix the Suggestion turn
        // parks on `streak < MAX` and cap+2 never arrives.
        cmd_tx
            .send(AgentCommand::Suggestion(
                "reviewer finished — continue the closing sequence".into(),
            ))
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap + 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "the Suggestion resume did not refresh the auto-continue \
                     budget within 10s (the closing sequence parks until the \
                     user types \"c\")"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn unattended_planning_auto_continues() {
        // Regression (backlog a6a7727a, live 2027-01-07): a premature turn
        // end mid-Planning exploration parked the session — the None-arm
        // gate covered Executing|Reviewing only — and in run-all mode
        // nothing resumed it, halting the whole run. A run-all dispatched
        // prompt embeds the unattended preamble (UNATTENDED_PROMPT_PREFIX),
        // so the runtime knows no user is watching: Planning then expects
        // progress and a premature end auto-continues like Executing.
        let dir = tempdir().unwrap();
        // Fresh workflow — no plan created → Planning, the exploration
        // state before create_plan.
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        assert_eq!(
            workflow.lock().await.state(),
            crate::workflow::WorkflowState::Planning,
            "setup: a fresh workflow must be in Planning"
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        // The run-all dispatched prompt: the unattended preamble prefix
        // (run_all_prompt builds the real dispatch the same way).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: format!(
                    "{UNATTENDED_PROMPT_PREFIX} You are running unattended. \
                     Item: do the work."
                ),
                images: vec![],
            })
            .await
            .unwrap();
        // The first turn ends (Finish::Stop); the None-arm must
        // auto-continue (call 2) instead of parking. Pre-fix: 1 call,
        // Parked{NoWorkExpected, Planning} — the run-all run stalls until
        // a manual "c".
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "unattended Planning turn parked instead of auto-continuing \
                     (the run-all run stalls until a manual \"c\")"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn interactive_planning_parks_with_no_work_expected() {
        // The interactive contract (the reason Planning is NOT covered
        // blindly): a deliberate turn end in Planning is usually a
        // question-wait for the user — auto-continuing would self-answer
        // it. A plain (non-run-all) prompt must leave the agent parking
        // with the NoWorkExpected evidence event, workflow in Planning.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        // A plain interactive prompt — no unattended preamble prefix.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "Which approach, A or B?".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut parked: Option<(ParkReason, String, bool, u32)> = None;
        loop {
            while let Ok((_id, event)) = fanin_rx.try_recv() {
                if let AgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                } = event
                {
                    parked = Some((
                        reason,
                        workflow_state,
                        descendants_running,
                        auto_continue_streak,
                    ));
                }
            }
            if parked.is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("no Parked event within 10s after the Planning turn");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (reason, workflow_state, descendants_running, streak) = parked.unwrap();
        assert_eq!(reason, ParkReason::NoWorkExpected);
        assert_eq!(workflow_state, "Planning");
        assert!(!descendants_running);
        assert_eq!(
            streak, 0,
            "no auto-continues may run in interactive Planning"
        );
        // The park is stable — no synthetic turn follows the question.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "interactive Planning must park after one turn (question-wait)"
        );
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn plain_prompt_clears_unattended_mode() {
        // The re-derivation contract: `unattended` is assigned on EVERY
        // Prompt — a run-all dispatched prompt sets it (Planning
        // auto-continues), and a later plain user prompt (the user taking
        // over after a halt or intervention) CLEARS it, so the session
        // parks in Planning again instead of running on unattended.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        // Phase 1: the run-all dispatched prompt — Planning
        // auto-continues (the chain runs toward the plateau).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: format!(
                    "{UNATTENDED_PROMPT_PREFIX} You are running unattended. \
                     Item: do the work."
                ),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("unattended Planning chain did not start within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Phase 2: let the chain run to its plateau — unattended
        // Planning expects progress, so the bound is the streak: it
        // parks with BudgetExhausted at cap calls.
        let cap = MAX_AUTO_CONTINUE as usize + 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut parked: Option<(ParkReason, String, bool, u32)> = None;
        loop {
            while let Ok((_id, event)) = fanin_rx.try_recv() {
                if let AgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                } = event
                {
                    parked = Some((
                        reason,
                        workflow_state,
                        descendants_running,
                        auto_continue_streak,
                    ));
                }
            }
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap
                && matches!(parked, Some((ParkReason::BudgetExhausted, _, _, _)))
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("unattended Planning chain did not reach its plateau park within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Phase 3: the user takes over with a plain prompt. The agent is
        // IDLE now (the chain parked), so the Prompt arm processes it —
        // and MUST clear unattended: the turn runs once and parks with
        // NoWorkExpected (interactive Planning semantics restored). A
        // prompt sent mid-chain would instead be folded into the running
        // turn as a steer, bypassing the Prompt arm — hence the wait for
        // the plateau park first.
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "Which approach, A or B?".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, event)) = fanin_rx.try_recv() {
                if let AgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                } = event
                {
                    // Keep the LAST park: the chain's BudgetExhausted
                    // park arrived first; the plain prompt's
                    // NoWorkExpected park is the one under test.
                    parked = Some((
                        reason,
                        workflow_state,
                        descendants_running,
                        auto_continue_streak,
                    ));
                }
            }
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap + 1
                && matches!(parked, Some((ParkReason::NoWorkExpected, _, _, _)))
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("plain prompt did not run and park within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (reason, workflow_state, _descendants_running, streak) = parked.unwrap();
        assert_eq!(reason, ParkReason::NoWorkExpected);
        assert_eq!(workflow_state, "Planning");
        assert_eq!(
            streak, 0,
            "the plain prompt reset the streak, and no auto-continue followed"
        );
        // Stable: exactly cap+1 calls (the plateau chain + the one
        // plain-prompt turn) — nothing runs on after the clear.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            cap + 1,
            "after the plain prompt clears unattended, Planning parks (no further turns)"
        );
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    /// A stream that pends on a gate before yielding its first event —
    /// while it pends, the agent's cmd_rx is the ONLY ready select!
    /// branch, so a prompt landed in the inbox is deterministically
    /// buffered before any stream event is processed (round-2 HIGH: an
    /// immediately-ready stream races the cmd branch ~12.5% per run).
    struct GatedStream {
        entered: Arc<tokio::sync::Notify>,
        gate: Option<tokio::sync::oneshot::Receiver<()>>,
        events: std::vec::IntoIter<LlmEvent>,
    }

    impl futures::Stream for GatedStream {
        type Item = LlmEvent;

        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            let Self { entered, gate, events } = &mut *self;
            if let Some(rx) = gate.as_mut() {
                // Fires on every gated poll — Notify stores one permit,
                // so this is idempotent for the test's single wait.
                entered.notify_one();
                if std::future::Future::poll(std::pin::Pin::new(rx), cx).is_pending() {
                    return std::task::Poll::Pending;
                }
                *gate = None;
            }
            std::task::Poll::Ready(events.next())
        }
    }

    /// A mock provider whose `complete` returns a [`GatedStream`] — the
    /// test controls when the summarization's first event becomes ready,
    /// making the cmd-buffering window structurally deterministic.
    struct GatedMockProvider {
        caps: Capabilities,
        stream_entered: Arc<tokio::sync::Notify>,
        stream_gate: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl LlmClient for GatedMockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-gated"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            let gate = self.stream_gate.lock().unwrap().take();
            Ok(Box::pin(GatedStream {
                entered: Arc::clone(&self.stream_entered),
                gate,
                events: vec![
                    LlmEvent::TextDelta {
                        text: "summary".into(),
                    },
                    LlmEvent::Finish {
                        reason: FinishReason::Stop,
                    },
                ]
                .into_iter(),
            }))
        }
    }

    // current_thread flavor: the yield_now below relies on the
    // single-threaded scheduler ordering (the compaction task runs
    // between the prompt send and the stream release).
    #[tokio::test(flavor = "current_thread")]
    async fn buffered_prompt_during_compaction_rederives_unattended() {
        // LOW 2 (review 2026-09-07-unattended-planning-auto-continue): a
        // Prompt buffered during /compact summarization bypasses the
        // run() Prompt arm — it must still re-derive `unattended` (and
        // reset the streak), or a user takeover during a mid-turn
        // compaction of a run-all session leaves unattended=true and a
        // later Planning question-wait gets self-answered.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let stream_entered = Arc::new(tokio::sync::Notify::new());
        let (stream_gate_tx, stream_gate_rx) = tokio::sync::oneshot::channel::<()>();
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(GatedMockProvider {
                        caps: Capabilities::openai(),
                        stream_entered: Arc::clone(&stream_entered),
                        stream_gate: std::sync::Mutex::new(Some(stream_gate_rx)),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let mut task = AgentTask::new(1, "test".into(), agent_loop);
        // Simulate a run-all session mid-exploration: unattended set (as
        // the Prompt arm derived it) with a partially-spent budget, and
        // enough conversation that the compaction actually summarizes.
        task.unattended = true;
        task.auto_continue_streak = 5;
        for i in 0..10 {
            task.messages
                .push(Message::user_text(format!("old message {i}")));
        }
        // Run the compaction on a spawned task. Its summarization
        // select! loop polls the (gated, Pending) mock stream — the test
        // waits for the stream's first poll, then lands the takeover
        // prompt in the inbox while the stream is STILL gated: cmd_rx is
        // then the only ready branch, so the loop deterministically
        // buffers the prompt before any stream event is processed.
        let handle = tokio::spawn(async move {
            let outcome = task.compact_context(&fanin_tx, &mut cmd_rx).await;
            (task, outcome)
        });
        stream_entered.notified().await;
        // The user takes over with a plain prompt — it lands in the
        // inbox while the mock stream is still gated (never passing
        // through the run() Prompt arm).
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "Which approach, A or B?".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // Current-thread runtime: yield so the compaction task runs and
        // takes the ready cmd branch (buffering the prompt) while the
        // stream is still gated — only then release the stream.
        tokio::task::yield_now().await;
        let _ = stream_gate_tx.send(());
        let (task, outcome) = handle.await.unwrap();
        assert!(
            matches!(outcome, CompactOutcome::Completed),
            "the mock summarization must complete"
        );
        // The buffered plain prompt MUST have applied the same
        // re-derivations the run() Prompt arm applies: unattended cleared
        // (interactive Planning question-waits park again) and the streak
        // reset (real user input = fresh budget).
        assert!(
            !task.unattended,
            "a plain user prompt buffered during compaction must clear unattended"
        );
        assert_eq!(
            task.auto_continue_streak, 0,
            "a plain user prompt buffered during compaction must reset the streak"
        );
        // Drain the fanin channel so nothing blocks.
        while let Ok((_id, _event)) = fanin_rx.try_recv() {}
        drop(cmd_tx);
    }

    #[tokio::test]
    async fn interrupted_turn_emits_parked_event() {
        // The mid-Executing stop (2027-01-07 live): the user pressed Stop
        // during a turn — the turn parks by design (never auto-resume), but
        // the UI could not distinguish it from a hang. The Interrupt arm
        // must emit Parked{Interrupted} with the pre-stall evidence so the
        // watchdog ring captures it and the UI can show "interrupted",
        // not "hung".
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan("test", "goal", "ctx", vec!["step".into()])
            .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // The user presses Stop — queued while the first turn runs; the
        // turn's select! or the post-turn drain folds it, and either way
        // the Interrupt arm parks the agent with the evidence event.
        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut parked: Option<(ParkReason, String, bool, u32)> = None;
        loop {
            while let Ok((_id, event)) = fanin_rx.try_recv() {
                if let AgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                } = event
                {
                    parked = Some((
                        reason,
                        workflow_state,
                        descendants_running,
                        auto_continue_streak,
                    ));
                }
            }
            if parked.is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("no Parked event within 10s after the interrupt");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (reason, workflow_state, descendants_running, streak) = parked.unwrap();
        assert_eq!(reason, ParkReason::Interrupted);
        assert_eq!(workflow_state, "Executing");
        assert!(!descendants_running);
        assert_eq!(streak, 0, "no auto-continues ran before the interrupt");
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn budget_exhaustion_emits_parked_event() {
        // The closing-sequence park (2027-01-07 live): with the
        // auto-continue budget exhausted while the workflow still expects
        // progress, the agent parks needing manual input — the Parked
        // event must carry the pre-stall evidence (reason, workflow
        // state, descendants, streak) so the watchdog ring can diagnose
        // the stall without reproduction.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan_with_kind(
                "test",
                "goal",
                "ctx",
                vec!["step".into()],
                crate::workflow::PlanKind::Implementation,
                None,
            )
            .unwrap();
        workflow.lock().await.complete_step(0).unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "run the closing sequence".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // The Prompt drives the auto-continue chain to the plateau
        // (MAX+1 calls); the budget-exhaustion park must emit its
        // evidence event.
        let cap = MAX_AUTO_CONTINUE as usize + 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut parked: Option<(ParkReason, String, bool, u32)> = None;
        loop {
            while let Ok((_id, event)) = fanin_rx.try_recv() {
                if let AgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                } = event
                {
                    parked = Some((
                        reason,
                        workflow_state,
                        descendants_running,
                        auto_continue_streak,
                    ));
                }
            }
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= cap && parked.is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("budget not exhausted (or no Parked event) within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (reason, workflow_state, descendants_running, streak) = parked.unwrap();
        assert_eq!(reason, ParkReason::BudgetExhausted);
        assert_eq!(workflow_state, "Reviewing");
        assert!(!descendants_running);
        assert_eq!(streak, MAX_AUTO_CONTINUE);
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn descendant_park_emits_waiting_for_descendants() {
        // The designed spawn→end-turn wait: while tool-spawned descendants
        // run, the parent parks (the child-completion Suggestion resumes
        // it) — the Parked event must carry WaitingForDescendants with the
        // tracker state so the ring shows the wait was deliberate, not a
        // stall.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan("test", "goal", "ctx", vec!["step".into()])
            .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1)
            .with_descendant_tracker(Arc::new(FlagDescendantTracker(Arc::clone(&running)))),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // The first turn runs; with a running descendant the gate parks
        // after it — assert the Parked evidence event.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut parked: Option<(ParkReason, String, bool, u32)> = None;
        loop {
            while let Ok((_id, event)) = fanin_rx.try_recv() {
                if let AgentEvent::Parked {
                    reason,
                    workflow_state,
                    descendants_running,
                    auto_continue_streak,
                } = event
                {
                    parked = Some((
                        reason,
                        workflow_state,
                        descendants_running,
                        auto_continue_streak,
                    ));
                }
            }
            if parked.is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("no Parked event within 10s while descendants run");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (reason, workflow_state, descendants_running, streak) = parked.unwrap();
        assert_eq!(reason, ParkReason::WaitingForDescendants);
        assert_eq!(workflow_state, "Executing");
        assert!(
            descendants_running,
            "the tracker state must ride the event as evidence"
        );
        assert_eq!(streak, 0);
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    /// A mock descendant tracker whose "running descendants" answer can be
    /// flipped at runtime (an external `Arc<AtomicBool>`), so a test can
    /// simulate a tool-spawned background agent finishing mid-flight.
    struct FlagDescendantTracker(std::sync::Arc<std::sync::atomic::AtomicBool>);

    #[async_trait]
    impl crate::runtime::DescendantTracker for FlagDescendantTracker {
        async fn has_running_descendants(&self, _agent_id: u64) -> bool {
            self.0.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[tokio::test]
    async fn auto_resume_parks_while_descendants_running() {
        // Regression: a parent agent whose turn ends while its tool-spawned
        // descendants (reviewers etc.) are still running must PARK, not burn
        // auto-continue turns — the state machine refuses every workflow
        // transition while subagents run, so a synthetic "[harness note]
        // continue from where you left off." turn can make no plan progress.
        // The child-completion Suggestion is the designed resume.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan("test", "goal", "ctx", vec!["step".into()])
            .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1)
            .with_descendant_tracker(Arc::new(FlagDescendantTracker(Arc::clone(&running)))),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // The first turn runs; with a running descendant the auto-continue
        // gate must park AFTER it — no second provider call arrives.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 1 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("first turn did not run within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "agent must park while descendants are running (auto-continue must not synthesize turns)"
        );
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn auto_resume_resumes_after_descendants_finish() {
        // Companion guard: parking is the DESCENDANT gate, not streak/budget
        // exhaustion — once the last child finishes (flag flips false), a
        // fresh prompt resumes auto-continue driving (a second provider call
        // follows the first).
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan("test", "goal", "ctx", vec!["step".into()])
            .unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1)
            .with_descendant_tracker(Arc::new(FlagDescendantTracker(Arc::clone(&running)))),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "do the work".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 1 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("first turn did not run within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "must park while descendant runs"
        );
        // The spawned child finishes; auto-continue driving must resume.
        running.store(false, std::sync::atomic::Ordering::Relaxed);
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "fresh task".into(),
                images: vec![],
            })
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("auto-continue did not resume after the descendant finished within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn auto_resume_fires_when_workflow_is_reviewing() {
        // Regression (2026-12-31, observed live twice): the closing sequence
        // (review → fix → commit → finish) runs in Reviewing, but the
        // auto-continue None-arm gated only on is_workflow_executing() — a
        // turn that ended mid-closing-sequence in Reviewing PARKED until the
        // user typed "c" (live: the agent stalled after a garbled tool result
        // and stayed parked; the user had to nudge it back by hand). The gate
        // now covers Reviewing via workflow_expects_progress(): a turn that
        // ends there with no running descendants self-resumes with the
        // harness note, still bounded by MAX_AUTO_CONTINUE and the
        // descendant park.
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan_with_kind(
                "test",
                "goal",
                "ctx",
                vec!["step".into()],
                crate::workflow::PlanKind::Implementation,
                None,
            )
            .unwrap();
        // Completing the root Implementation plan's only step → Reviewing
        // (Workflow::complete_step: Implementation/BugFixing → Reviewing;
        // the workflow-level index is 0-based — the tool converts 1→0).
        workflow.lock().await.complete_step(0).unwrap();
        assert_eq!(
            workflow.lock().await.state(),
            crate::workflow::WorkflowState::Reviewing,
            "setup: the root Implementation plan must be in Reviewing"
        );
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(1),
        );
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(1, "test".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "run the closing sequence".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // The first turn runs; in Reviewing the auto-continue gate must fire
        // — a second provider call follows (the synthetic harness-note turn).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 2 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!(
                    "auto-continue did not fire in Reviewing within 10s (agent parked; \
                     the closing sequence stalls until the user types \"c\")"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[tokio::test]
    async fn auto_resume_does_not_fire_for_subagents() {
        // Regression (High 1 of the 2026-12-31 auto-continue-reviewing review,
        // observed live): historically, tool-spawned subagents shared the main
        // plans dir and load_latest() derived their workflow state from the
        // main plan on disk (factory build_inner → Workflow::load_latest:
        // unchecked steps → Executing, complete + unreviewed → Reviewing), so
        // a closing-sequence reviewer's own loop saw "progress expected" and
        // the auto-continue None-arm — which had no is_subagent guard — fired
        // up to MAX_AUTO_CONTINUE synthetic "[harness note] continue from
        // where you left off." turns on the finished reviewer: token burn,
        // the reviewer could mutate its own deliverable
        // (write_review_report), and the parent's descendant-park held until
        // the chain exhausted (blocking the dispatch state-transition gate).
        // Subagents are single-task: a normal turn end must PARK them — the
        // child-completion Suggestion resumes the parent instead. Since
        // 2026-01-03 the spawn path stamps WorkflowState::Subagent (never a
        // derived lifecycle state), so workflow_expects_progress is already
        // false for real subagents; this test hand-builds a Reviewing-state
        // subagent loop to exercise the is_subagent guard as pure
        // belt-and-braces (a future spawn path that forgets the stamp).
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        workflow
            .lock()
            .await
            .create_plan_with_kind(
                "test",
                "goal",
                "ctx",
                vec!["step".into()],
                crate::workflow::PlanKind::Implementation,
                None,
            )
            .unwrap();
        // The loaded-main-plan state a closing-sequence reviewer sees:
        // complete + unreviewed → Reviewing (workflow-level index 0-based).
        workflow.lock().await.complete_step(0).unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(CountingMockProvider {
                        caps: Capabilities::openai(),
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox: Arc::new(Sandbox::new(dir.path()).unwrap()),
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_agent_id(2),
        );
        // The spawner stamps every parented loop (spawn.rs set_is_subagent).
        agent_loop.set_is_subagent(true);
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let task = AgentTask::new(2, "reviewer".into(), agent_loop);
        let mut handle = tokio::spawn(task.run(cmd_rx, fanin_tx));
        cmd_tx
            .send(AgentCommand::Prompt {
                text: "review the change".into(),
                images: vec![],
            })
            .await
            .unwrap();
        // Wait for the single task turn to run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            while let Ok((_id, _event)) = fanin_rx.try_recv() {}
            if calls.load(std::sync::atomic::Ordering::SeqCst) >= 1 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("the subagent's first turn never started within 10s");
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Settle window: on the buggy code the auto-continue fires a second
        // provider call right after the turn ends; on the fixed code the
        // subagent parks and the count stays at 1.
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        while let Ok((_id, _event)) = fanin_rx.try_recv() {}
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a subagent must park after its single task turn — no synthetic \
             self-continue turns (the child-completion Suggestion resumes the \
             parent instead)"
        );
        drop(cmd_tx);
        loop {
            tokio::select! {
                res = &mut handle => {
                    let _ = res;
                    break;
                }
                _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {
                    while let Ok((_id, _event)) = fanin_rx.try_recv() {}
                }
            }
        }
    }

    #[test]
    fn consolidation_dedup_guard_prevents_overlap() {
        // Simulates the exact AtomicBool swap logic used in
        // spawn_consolidation: the first call proceeds (swap returns false),
        // the second is dedup'd (swap returns true), and after the spawned
        // task clears the flag, the next call proceeds again.
        let flag = std::sync::atomic::AtomicBool::new(false);

        // First call — not in flight, proceeds.
        assert!(!flag.swap(true, std::sync::atomic::Ordering::SeqCst));

        // Second call — already in flight, dedup'd (returns true = was set).
        assert!(flag.swap(true, std::sync::atomic::Ordering::SeqCst));

        // Spawned task finishes, clears the flag.
        flag.store(false, std::sync::atomic::Ordering::SeqCst);

        // Third call — flag was cleared, proceeds again.
        assert!(!flag.swap(true, std::sync::atomic::Ordering::SeqCst));
    }

    // --- Failure triage at the provider site (plan 02deea7c) -------------

    /// A provider that always returns a `Provider` error, with a call counter
    /// — the retry-ladder / classifier-skip assertion hook.
    struct ErrorProvider {
        caps: Capabilities,
        message: &'static str,
        calls: Arc<std::sync::atomic::AtomicU32>,
    }

    #[async_trait]
    impl LlmClient for ErrorProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-error"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<crate::provider::ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(crate::error::Error::Provider(self.message.into()))
        }
    }

    /// A classifier with one canned answer — the gate's decision source.
    struct CannedClassifier {
        label: &'static str,
        confidence: f64,
    }

    #[async_trait]
    impl Classifier for CannedClassifier {
        async fn classify(&self, _state: &str, _question: &Question) -> Option<Answer> {
            Some(Answer::Choice {
                label: self.label.to_string(),
                confidence: self.confidence,
                probabilities: std::collections::BTreeMap::new(),
            })
        }
    }

    /// A failure-triage gate over a canned classifier, logging to `log` — a
    /// tempdir file, never the real `~/.mnemo` training log.
    fn triage_gate(
        label: &'static str,
        confidence: f64,
        enabled: bool,
        log: std::path::PathBuf,
    ) -> crate::agent::failure_triage::FailureTriageHandle {
        let classifier: Arc<dyn Classifier> = Arc::new(CannedClassifier { label, confidence });
        crate::agent::failure_triage::FailureTriageHandle::new(
            Arc::new(std::sync::RwLock::new(Some(classifier))),
            Arc::new(std::sync::atomic::AtomicBool::new(enabled)),
        )
        .with_log_path(log)
    }

    /// Run ONE turn attempt against an always-erroring provider and return
    /// (provider calls, emitted events, resolved training rows, conversation).
    async fn run_provider_failure(
        message: &'static str,
        triage: crate::agent::failure_triage::FailureTriageHandle,
        log: std::path::PathBuf,
    ) -> (
        u32,
        Vec<AgentEvent>,
        Vec<crate::agent::failure_triage::FailureLogRow>,
        Vec<Message>,
    ) {
        let dir = tempdir().unwrap();
        let workflow = Arc::new(tokio::sync::Mutex::new(Workflow::new(
            dir.path().join("plans"),
        )));
        let sandbox = Arc::new(Sandbox::new(dir.path()).unwrap());
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let agent_loop = Arc::new(
            AgentLoop::new(
                AgentLoopConfig {
                    provider: Arc::new(ErrorProvider {
                        caps: Capabilities::openai(),
                        message,
                        calls: Arc::clone(&calls),
                    }),
                    tools: Arc::new(ToolRegistry::new()),
                    workflow,
                    sandbox,
                    safety_mode: SafetyMode::Autonomous,
                    context_manager: ContextManager::new(128_000, 0.5),
                    memory: None,
                    vision: None,
                },
                crate::project::Constitution::default(),
            )
            .with_failure_triage(triage),
        );

        let (fanin_tx, mut fanin_rx) = mpsc::channel(64);
        let (_cmd_tx, mut cmd_rx) = mpsc::channel(8);
        let mut task = AgentTask::new(1, "triage-test".into(), agent_loop);
        // Drive ONE turn attempt directly: `AgentTask::run`'s auto-continue
        // loop re-runs a failed turn (MAX_AUTO_CONTINUE rounds), which would
        // multiply every call/event count this harness asserts on.
        let outcome = task.run_turn_attempt(&fanin_tx, &mut cmd_rx).await;
        assert!(
            outcome.is_none(),
            "the provider always fails — the attempt must end in final failure"
        );
        let mut events = Vec::new();
        while let Ok((_id, event)) = fanin_rx.try_recv() {
            events.push(event);
        }
        let count = calls.load(std::sync::atomic::Ordering::SeqCst);
        let rows = crate::agent::failure_triage::read_rows(&log);
        (count, events, rows, task.messages.clone())
    }

    /// The terminal provider-failure harness note from the conversation — the
    /// in-context qualifier (`retries exhausted` / `classified …`) that the
    /// terminal EVENT text deliberately does not always carry.
    fn terminal_note(messages: &[Message]) -> String {
        messages
            .iter()
            .rev()
            .find(|m| m.content.as_text().contains("failed terminally"))
            .map(|m| m.content.as_text().to_string())
            .unwrap_or_default()
    }

    /// The text of the terminal (non-retrying) Error in `events`.
    fn terminal_error(events: &[AgentEvent]) -> String {
        events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Error {
                    error,
                    retrying: false,
                } => Some(error.clone()),
                _ => None,
            })
            .expect("a terminal Error event must be surfaced")
    }

    #[tokio::test]
    async fn provider_site_needs_user_classification_skips_the_retry_ladder() {
        // Acceptance (plan 02deea7c, tier 3): a confident `needs_user`
        // classification of a provider error that the EXISTING heuristics
        // treat as retryable skips the useless backoff ladder — one provider
        // call, zero retrying notes, and a terminal message that names the
        // class (never "retries exhausted"). The live motivation: a quota
        // error burns ~3s of jittered sleeps on a condition only the user can
        // clear.
        let dir = tempdir().unwrap();
        let log = dir.path().join("triage.jsonl");
        let (calls, events, rows, messages) = run_provider_failure(
            "quota exhausted: the account has no remaining credits for this period",
            triage_gate("needs_user", 0.9, true, log.clone()),
            log,
        )
        .await;

        assert_eq!(calls, 1, "needs-user must skip the ladder (1 call, not 3)");
        let retrying = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Error { retrying: true, .. }))
            .count();
        assert_eq!(retrying, 0, "a skipped ladder emits no retrying notes");
        let terminal = terminal_error(&events);
        assert!(
            terminal.contains("classified needs_user"),
            "the terminal message must name the classification; got: {terminal}"
        );
        let note = terminal_note(&messages);
        assert!(
            note.contains("classified needs_user") && !note.contains("retries exhausted"),
            "the in-context note must name the classification and never claim an \
             exhausted ladder; got: {note}"
        );
        assert_eq!(rows.len(), 1, "the skip logs one resolved row");
        assert_eq!(rows[0].site, "provider_turn");
        assert_eq!(rows[0].class, "needs_user");
        assert_eq!(rows[0].action, "skip_retry");
        assert_eq!(rows[0].disposition.as_deref(), Some("escalated"));
        assert_eq!(rows[0].tool, None);
    }

    #[tokio::test]
    async fn provider_site_transient_classification_keeps_the_ladder() {
        // The classifier only ADDS: a `transient` reading keeps today's retry
        // ladders byte-for-byte — the inner request ladder (3 attempts per
        // turn attempt) AND the turn-level ladder (3 attempts) — and the one
        // logged row resolves `retry_failed` when the turn finally gives up.
        let dir = tempdir().unwrap();
        let log = dir.path().join("triage.jsonl");
        let (calls, events, rows, messages) = run_provider_failure(
            "HTTP 502 Bad Gateway: upstream connection reset",
            triage_gate("transient", 0.95, true, log.clone()),
            log,
        )
        .await;

        assert_eq!(
            calls, 9,
            "a transient reading keeps BOTH pre-classifier ladders \
             (3 inner request attempts × 3 turn attempts)"
        );
        let retrying = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Error { retrying: true, .. }))
            .count();
        assert_eq!(
            retrying, 8,
            "six inner request notes + two turn-level notes — the pre-classifier count"
        );
        let terminal = terminal_error(&events);
        assert!(
            terminal.contains("HTTP 502 Bad Gateway"),
            "the ladder path surfaces the raw provider error; got: {terminal}"
        );
        let note = terminal_note(&messages);
        assert!(
            note.contains("retries exhausted") && !note.contains("classified"),
            "the ladder path keeps its exhausted-ladder note and adds no \
             classification; got: {note}"
        );
        assert_eq!(rows.len(), 1, "the classified failure logs exactly once");
        assert_eq!(rows[0].action, "ladder");
        assert_eq!(rows[0].class, "transient");
        assert_eq!(rows[0].disposition.as_deref(), Some("retry_failed"));
    }

    #[tokio::test]
    async fn provider_site_disabled_triage_keeps_the_ladder_unchanged() {
        // Acceptance (byte-identical while the flag is off): the ladder runs
        // exactly as before, nothing is classified, nothing is logged.
        let dir = tempdir().unwrap();
        let log = dir.path().join("triage.jsonl");
        let (calls, events, rows, messages) = run_provider_failure(
            "HTTP 502 Bad Gateway: upstream connection reset",
            triage_gate("transient", 0.95, false, log.clone()),
            log.clone(),
        )
        .await;

        assert_eq!(
            calls, 9,
            "a disabled gate must not touch either ladder (3 inner × 3 turn attempts)"
        );
        let retrying = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::Error { retrying: true, .. }))
            .count();
        assert_eq!(retrying, 8);
        let note = terminal_note(&messages);
        assert!(
            note.contains("provider error, retries exhausted"),
            "the wording stays pre-classifier; got: {note}"
        );
        assert!(rows.is_empty());
        assert!(!log.exists(), "a disabled gate must not write a training row");
    }
}
