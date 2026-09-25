// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The turn driver — `AgentLoop::run_turn`.
//!
//! One `run_turn` call drives a full conversation turn: count tokens → maybe
//! summarize → auto-recall memories → build the system prompt → stream the
//! completion → dispatch tool calls (via [`super::dispatch`]) → feed results
//! back → repeat until the model stops calling tools or the retry cap trips.
//!
//! `run_turn` itself is a thin orchestrator over the phase methods below
//! (quality review HIGH 1, 2027-01-07): `handle_pending_swap` (pre-swap
//! summarization), `resolve_iteration_provider`, `maybe_compact`,
//! `auto_recall`, `install_system_messages`, `request_stream`,
//! `consume_stream`, `record_interrupted_output`, `handle_bad_json`,
//! `execute_tool_batch` (plus `emit_workflow_state` and the free
//! `synthesize_not_run_results`). The loop-carried counters, signals, and
//! per-turn caches live in `TurnState` and thread through the phases as
//! `&mut`. The methods stay in this file so they can read the loop's
//! private state; all fields they touch are `pub(crate)` within the agent
//! module (see [`super::loop_impl`]).

use std::sync::Arc;

use tokio::sync::mpsc;

use super::context::{self, TokenAccounting};
use super::failure_triage;
use super::loop_impl::{AgentLoop, TurnOutcome};
use super::prompt;
use super::StopReason;
use super::MAX_BAD_JSON_RETRIES;
use super::MAX_RETRIES;
use super::REVIEW_REPORT_MAX_FAILURES;
use crate::memory::{Memory, MemoryTier};
use crate::provider::{
    DeltaAccumulator, FinishReason, LlmClient, LlmEvent, Message, MessageContent, Role, ToolCall,
    ToolSchema,
};
use crate::runtime::{AgentCommand, AgentEvent, AgentId, PhaseKind};
use crate::tool::agent::git::resolve_git_subcommand;
use crate::tool::ToolResult;
use crate::workflow::WorkflowState;

/// The per-row payload for [`AgentLoop::record_stats_row`] (R21): the token,
/// timing, and tag fields of a request_stats row. The identity fields (id,
/// session_id, model, endpoint, created_at) are stamped by the recorder from
/// the provider snapshot + the turn's authoritative session id, so callers
/// only carry what varies.
pub(crate) struct StatsRow {
    /// Input tokens (the provider's report, or our estimate on an error row).
    pub prompt_tokens: u32,
    /// Output tokens (0 on an error row — no usage was reported).
    pub completion_tokens: u32,
    /// Reasoning tokens (a subset of `completion_tokens`).
    pub reasoning_tokens: u32,
    /// Cached prompt tokens: `Some` when the provider reported usage (0 = a
    /// real miss), `None` on an error row (never reported — not a miss).
    pub cached_tokens: Option<u32>,
    /// Time-to-first-token (ms), when captured.
    pub ttft_ms: Option<u32>,
    /// Generation time (ms), when captured.
    pub generation_ms: Option<u32>,
    /// Outcome tag: `None` for a normal successful request, `Some("error")`
    /// when the request died before a usage report.
    pub outcome: Option<String>,
    /// Purpose tag: `None` for main-loop requests, `Some("summarize")` for
    /// compaction's own call.
    pub purpose: Option<String>,
}

/// Fill-rate threshold for the pre-swap summarization: summarize with the
/// OLD provider when the conversation exceeds this fraction of the NEW
/// (smaller) context window. Tied to the ContextManager fill-rate policy
/// (the same concept `effective_summarize_at` applies), but tighter — a
/// provider swap is a one-shot window shrink, and the regular trigger only
/// re-fires on the next turn, too late for the smaller window.
const SWAP_SUMMARIZE_FILL: f64 = 0.8;

/// Keep-recent message count for the pre-swap summarization — the same
/// keep-recent count as the regular auto-compaction path's normal tier (the
/// preflight hard-ceiling tier uses 3).
const KEEP_RECENT_ON_SWAP: usize = 6;

/// Hard ceiling on auto-compactions per turn, across every reset.
///
/// The stuck ladder aborts at 5 CONSECUTIVE failed attempts and re-arms
/// whenever a compaction lands under the threshold — which, now that compaction
/// is total (the sendable post-condition always fits the result), is every
/// cycle of a runaway tool loop. This total keeps a turn's summarizer burn
/// finite regardless: 10 leaves room for a long legitimate turn crossing the
/// fill rate several times, while stopping a loop that is not making progress.
pub(crate) const MAX_COMPACTIONS_PER_TURN: u32 = 10;

/// How many recent messages the repeat-failure circuit breaker scans for a
/// prior identical failure of the same tool. Bounded so the scan stays
/// cheap; compaction replaces old tool-result text with markers anyway,
/// so matches decay naturally at the compaction boundary.
pub(crate) const REPEAT_FAILURE_SCAN_WINDOW: usize = 50;

/// Cap on the parameters JSON rendered inside a corrective message
/// (repeat-failure circuit breaker). Larger schemas degrade to a
/// summary (property names + required list) so the correction stays
/// compact.
pub(crate) const CORRECTION_SCHEMA_CAP: usize = 1536;

/// Prefix marking a harness-attributed corrective message (repeat-failure
/// circuit breaker). Shared by the correction builder and the auto-recall
/// query lookup so the correction never hijacks the recall query.
pub(crate) const HARNESS_CORRECTION_PREFIX: &str = "[harness tool-call correction";

/// Loop-carried state for one [`AgentLoop::run_turn`] call: the counters,
/// signals, and per-turn caches that survive across loop iterations. The
/// phase methods take `&mut TurnState` so this state threads through them
/// without long parameter lists.
pub(crate) struct TurnState {
    /// Consecutive tool-error INTERACTIONS (batches) counter. A batch with
    /// any non-denial failure counts once (same-time identical failing
    /// calls are one error event — the model sees every error result and
    /// gets a chance to repair); a batch with no failure but at least one
    /// success resets it to 0. When it reaches MAX_RETRIES the turn is
    /// aborted so a stuck model can't loop forever.
    tool_error_count: u32,
    /// Consecutive bad-JSON counter — LLM-produced malformed/truncated
    /// tool-call arguments. The model can recover by emitting valid JSON,
    /// so this uses the higher MAX_BAD_JSON_RETRIES cap (not MAX_RETRIES).
    /// Reset as soon as the model produces valid tool calls.
    bad_json_count: u32,
    /// Consecutive `write_review_report` failures (backlog 5b46674d).
    /// Unlike `tool_error_count`, this is NOT reset by other tools'
    /// successes — a reviewer interleaving failed report writes with
    /// successful reads is still stuck on its single output channel. When
    /// it reaches REVIEW_REPORT_MAX_FAILURES the turn aborts with a
    /// distinct final error naming the review report. Resets only on a
    /// successful `write_review_report`.
    review_report_failures: u32,
    /// Harness auto-retries performed this turn — the per-turn budget
    /// [`failure_triage::MAX_AUTO_RETRIES_PER_TURN`] for tier-1 retries
    /// (confident `transient` read-only tool calls re-run by the harness
    /// without a model roundtrip). Reset each turn.
    auto_retries_this_turn: u32,
    /// Classified failures on the guidance route (tier 2) awaiting their
    /// disposition. A later failure-free successful batch is the retry
    /// landing — they resolve `retry_succeeded`; the MAX_RETRIES abort
    /// resolves them `cap_reached`. Leftover rows at turn end are dropped:
    /// the outcome was never observed, and an unobserved disposition must
    /// not be guessed into the training log.
    pending_triage_rows: Vec<failure_triage::PendingFailureRow>,
    /// The last classified failure class this turn — the MAX_RETRIES abort
    /// message names it when present (absent = byte-identical message).
    last_triage_class: Option<failure_triage::FailureClass>,
    /// Once the user answers DenyAll on any approval this turn, remaining
    /// tool calls auto-deny without prompting (Quality M1). Reset each turn.
    deny_all_latched: bool,
    /// The unified stop signal: why the turn should end early. Set by the
    /// streaming select! (Interrupt/Cancel/Steer during streaming), by
    /// summarize_with_interrupt (Interrupt/Cancel during summarization), by
    /// the between-tool-call safe point, and by execute_tool_call via
    /// `stop_signal` (Interrupt/Cancel during approval/ask_user/tool
    /// execution — merged in the tool batch). When set, the turn ends at
    /// the next break point.
    stop_reason: Option<StopReason>,
    /// Whether this turn already announced an auto-compaction in the
    /// transcript (CompactStarted + its paired end). While the conversation
    /// stays above the threshold, auto-compaction re-fires on EVERY loop
    /// iteration — announcing each time would spam the transcript with
    /// start/end note pairs (review finding 2026-08-22). The inflight-bar
    /// Phase::Compacting indicator is transient and still fires per
    /// iteration; only the transcript entries are gated.
    compact_announced: bool,
    /// Per-turn auto-compaction attempt counter (2027-01-07 re-emission
    /// incident): a turn whose context stays over the threshold re-compacts
    /// every iteration — each cycle burns a summarizer LLM call and feeds the
    /// model a nearly-identical context, driving byte-identical re-emission
    /// of the same tool call. The counter escalates keep_recent (attempts 3-4
    /// → 1) and aborts the turn at attempt 5. Reset whenever a compaction
    /// brings the count back under the threshold (the legitimate long-turn
    /// path crosses the fill rate repeatedly).
    compact_attempts: u32,
    /// Per-turn TOTAL auto-compaction counter (2027-01-23 runaway-tool-output
    /// report follow-up): `compact_attempts` re-arms whenever a compaction
    /// lands the count back under the threshold, and compaction became TOTAL in
    /// this change — the sendable post-condition always fits the result. A
    /// runaway tool loop therefore re-arms the attempt budget every cycle and
    /// would re-compact without bound (measured: 12 summarizer calls in one
    /// turn, with the attempt-5 abort never firing at all). This counter never
    /// resets within a turn, so the burn stays finite whatever the reset does.
    compact_total: u32,
    /// Incremental token accounting (exact): the first request-loop
    /// iteration pays one BPE pass; later iterations only encode the
    /// messages appended since the previous count (tool results, the
    /// assistant turn) plus a head swap when the system head's content
    /// changed.
    token_accounting: TokenAccounting,
    /// Per-turn auto-recall cache (Perf H2/M5): keyed by the text of the
    /// latest *user* message. If the user message text is unchanged since
    /// the previous recall within this turn, reuse the prior scored results
    /// without re-embedding or rescanning. Invalidated on summarize (the
    /// messages prefix changed) OR on any store write: the version check
    /// (`last_recall_store_version` vs the store's mutation counter) makes
    /// a same-turn `memory_write` visible to the next recall. The recall
    /// query is just the latest User-role message text, which tool-result
    /// appends can never change.
    last_recalled_user_query: Option<String>,
    /// The scored-results half of the per-turn auto-recall cache.
    last_recall_results: Option<Vec<crate::memory::ScoredMemory>>,
    /// The store version the cached recall ran at (the store's mutation
    /// counter) — a mismatch with the live version invalidates the cache
    /// even when the query text is unchanged.
    last_recall_store_version: u64,
}

impl TurnState {
    /// A fresh per-turn state: all counters zeroed, no stop signal, no
    /// compaction announced, empty caches. The single construction site
    /// is [`AgentLoop::run_turn`]; tests also construct it directly to
    /// drive [`AgentLoop::maybe_compact`] in isolation.
    pub(crate) fn fresh() -> Self {
        Self {
            tool_error_count: 0,
            bad_json_count: 0,
            review_report_failures: 0,
            auto_retries_this_turn: 0,
            pending_triage_rows: Vec::new(),
            last_triage_class: None,
            deny_all_latched: false,
            stop_reason: None,
            compact_announced: false,
            compact_attempts: 0,
            compact_total: 0,
            token_accounting: TokenAccounting::new(),
            last_recalled_user_query: None,
            last_recall_results: None,
            last_recall_store_version: 0,
        }
    }
}

/// The accumulated result of one provider stream, returned by
/// [`AgentLoop::consume_stream`].
struct StreamOutcome {
    /// The delta accumulator (tool calls, raw turn, response id, meta).
    acc: DeltaAccumulator,
    /// The streamed answer text.
    text: String,
    /// The streamed reasoning/thinking text (DeepSeek thinking mode).
    reasoning_text: String,
    /// The provider-reported finish reason.
    finish_reason: FinishReason,
    /// Whether an `LlmEvent::Error` arrived mid-stream.
    had_error: bool,
    /// The error string from a mid-stream `LlmEvent::Error`, if any.
    stream_error: Option<String>,
}

impl AgentLoop {
    /// Run a single turn: send messages to the provider, stream the response,
    /// execute tool calls, and feed results back. Emits events to the fan-in channel.
    ///
    /// `messages` is the full conversation so far (including the new user prompt).
    /// `session_id` tags working-memory tool events with their owning session so
    /// that consolidation can find them later (pass `None` when memory isn't in
    /// use). Returns the updated messages + the turn outcome.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the provider stream fails *before* producing any
    /// useful output (no text, no tool-call deltas) — e.g. a mid-stream
    /// connection reset (`os error 10054` / `WSAECONNRESET`) or unexpected EOF.
    /// This lets the caller ([`AgentTask::run_turn_with_retry`]) retry the
    /// whole turn with backoff, the same recovery path used for
    /// connection-establishment failures. A mid-stream error that arrives
    /// *after* partial output is swallowed in favor of the usable content and
    /// the turn returns `Ok`.
    pub async fn run_turn(
        &self,
        messages: &mut Vec<Message>,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        session_id: Option<&str>,
    ) -> crate::error::Result<TurnOutcome> {
        // Pending provider swap (deferred so the OLD provider can summarize
        // first when the new window is smaller) — handled in its own method;
        // Some(outcome) means an interrupt during the pre-swap summarization
        // ends the turn here.
        if let Some(outcome) = self
            .handle_pending_swap(messages, fanin_tx, agent_id, session_id, cmd_rx)
            .await
        {
            return Ok(outcome);
        }

        // Default provider + context manager snapshots, taken once per turn.
        // Both are swappable at runtime (the model picker), and a single
        // provider request must talk to one provider throughout — a swap
        // mid-request takes effect on the next request. These defaults are
        // the fallback when no per-context override resolves (see the
        // per-iteration resolution at the top of the loop below).
        let default_provider: Arc<dyn LlmClient> = self
            .provider
            .read()
            .expect("provider lock poisoned")
            .clone();
        let default_context_manager = self
            .context_manager
            .read()
            .expect("context_manager lock poisoned")
            .clone();

        let _ = fanin_tx.send((agent_id, AgentEvent::Started)).await;

        // Defensive recovery: repair any empty assistant messages already in
        // the persistent history. A prior null turn (or a crash mid-turn) can
        // leave an assistant message with empty content and no tool calls,
        // which providers reject ("messages[N] is an assistant message with no
        // content and no tool calls"). Without this, a session already wedged
        // by the bug stays wedged on every subsequent "continue" until restart
        // — exactly the symptom this fixes. Runs once per turn; cheap (linear
        // scan, no allocation unless a repair is needed).
        repair_empty_assistant_messages(messages);

        // Loop-carried turn state — the counters, signals, and per-turn
        // caches that survive across loop iterations (see `TurnState` for
        // the field docs). The phase methods thread it as `&mut`.
        let mut state = TurnState::fresh();

        loop {
            // The request cycle begins: everything from here until the POST
            // goes out is the "sending" phase (model resolution, prompt
            // build, summarization, auto-recall, the provider request
            // including internal retries). Drives the inflight bar's live
            // status.
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Phase {
                        phase: PhaseKind::Sending,
                    },
                ))
                .await;

            // Prep-phase anchor: everything from here to the POST is local
            // prompt prep (model resolution, token accounting, summarization,
            // auto-recall, prompt build). Parked on the provider right before
            // the request and stamped onto the trace record as `prep_ms`.
            let prep_started = std::time::Instant::now();

            // The provider + context manager for THIS iteration's request
            // (re-resolved every iteration — see resolve_iteration_provider).
            let (provider, context_manager) = self
                .resolve_iteration_provider(
                    &default_provider,
                    &default_context_manager,
                    fanin_tx,
                    agent_id,
                )
                .await;
            // The workflow tool filter, read at the TOP of the iteration: the
            // token accounting must see the tools-schema overhead before its
            // first consumer this iteration (the update() below feeds the ctx
            // readout, the summarize trigger, and the preflight hard-ceiling
            // check). Everything downstream reads this same value (the
            // hidden-groups index, the schemas sent with the request).
            //
            // schema_filter() (not allowed_tools()) — the SCHEMA array is the
            // plan-frozen ADVERTISEMENT: while a plan is active it is
            // byte-stable for the plan's whole lifetime (Executing ∪ finish,
            // or the research surface), because the array rides at the head
            // of the request body and any byte change resets the provider
            // prefix cache (perf review L4: a full re-bill of the
            // conversation, ~40-75s of server-side prefill at late-plan
            // context sizes). Dispatch re-checks the per-state
            // allowed_tools() (dispatch.rs), so out-of-state calls against
            // the frozen array (complete_step/create_plan during Reviewing,
            // finish during Executing) are still rejected — advertisement is
            // frozen, enforcement is not.
            let tool_filter = {
                let wf = self.workflow.lock().await;
                wf.schema_filter()
            };
            // Build the tool schemas (filtered by workflow state).
            let mut tool_schemas = self.tools.schemas(provider.capabilities(), &tool_filter);
            if !self.plan_mutations_allowed() {
                // Phase 4: do not advertise plan-mutation tools (or finish —
                // closing out the review is main-agent-only) to sub-agents.
                tool_schemas.retain(|s| {
                    !matches!(
                        s.name.as_str(),
                        "create_plan" | "update_plan" | "complete_step" | "abandon_plan" | "finish"
                    )
                });
            }
            // The provider bills the tool-schema array in usage.prompt_tokens
            // (name + description + parameters JSON per tool); a message-only
            // count understates the request by the whole schema block (tens
            // of thousands of tokens on a tool-heavy harness). Set it BEFORE
            // the accounting update below so EVERY consumer this iteration
            // sees it — the top-of-loop ContextUsage emission, the summarize
            // trigger, the preflight hard-ceiling check (review finding
            // 2026-12-23: the old set-after-update ordering meant every read
            // saw 0 — the value landed after the reads and each loop-back's
            // reset() zeroed it again, so the schema block never reached a
            // live emission or trigger).
            state.token_accounting
                .set_tools_tokens(crate::provider::estimate_tools_tokens(&tool_schemas) as u32);

            // Exact incremental token count + per-role breakdown in one pass
            // (see `TokenAccounting`). Summarization resets the accounting
            // so a full pass runs after the summarizer rewrites the
            // conversation (the schema overhead survives the reset — see
            // `TokenAccounting::reset`).
            let (token_count, mut breakdown, _) = state.token_accounting.update(messages);

            // Record the live count for the 429-fallback viability check
            // (try_429_fallback): the request built from this count is the
            // one that can 429. Basis: messages + tools overhead — the
            // per-request head/tail scaffolding is installed after this
            // point and is absorbed by the viability headroom (the
            // alternate's preflight is the backstop). Relaxed is sufficient
            // — the store happens before the request and the load after
            // the error, sequenced by the same task's await chain.
            self.live_token_count
                .store(token_count, std::sync::atomic::Ordering::Relaxed);

            // Check for context summarization. The summarization LLM call is
            // interruptible: an Interrupt/Cancel arriving mid-summarization
            // abandons the summary and returns the original messages. Any
            // non-interrupt commands buffered during the call are re-injected
            // as system messages below.
            // The effective trigger respects the proxy cache ceiling (when
            // set) so compaction fires before the request crosses the ~340K
            // cliff where proxies drop whole-conversation prefix caching.
            if token_count >= context_manager.effective_summarize_at() {
                if let Some(outcome) = self
                    .maybe_compact(
                        &mut state,
                        messages,
                        &provider,
                        &context_manager,
                        fanin_tx,
                        agent_id,
                        session_id,
                        cmd_rx,
                        token_count,
                        &mut breakdown,
                    )
                    .await
                {
                    return Ok(outcome);
                }
            } else {
                // No summarization — reuse the single count computed above.
                let used = token_count as u32;
                let max = context_manager.max_tokens() as u32;
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::ContextUsage {
                            used,
                            max,
                            breakdown,
                        },
                    ))
                    .await;
            }

            // Auto-recall: fetch relevant memories based on the latest user
            // message and inject them into the system prompt (per-turn
            // cache, non-bumping peek) — see auto_recall.
            let memory_context = self
                .auto_recall(&mut state, messages, fanin_tx, agent_id)
                .await;

            // Build + install the system prompt (stable head + volatile
            // tail + session primer + hidden tool groups) — see
            // install_system_messages. It pushes the tail + CONTEXT_FOOTER as
            // the last two messages (user- or system-role per vendor), popped
            // right after the request below.
            self.install_system_messages(
                messages,
                &provider,
                memory_context.as_deref(),
                &tool_filter,
            )
            .await;
            // The provider request (connection establishment + first byte)
            // is interruptible — see request_stream. The future borrows
            // `messages` immutably, so the pops below run after it returns.
            let mut buffered_steers: Vec<crate::runtime::SteerPayload> = Vec::new();
            let stream_result = self
                .request_stream(
                    messages,
                    &provider,
                    &tool_schemas,
                    fanin_tx,
                    agent_id,
                    session_id,
                    cmd_rx,
                    &mut state,
                    &mut buffered_steers,
                    prep_started,
                )
                .await;
            messages.pop(); // CONTEXT_FOOTER (the stable last message)
            messages.pop(); // volatile tail
            // If a hard signal arrived during the provider request, end the
            // turn now — no stream started, so there's no partial output to
            // keep. Emit Finished + carry the stop reason. InterruptWithSteers
            // (Stop with commands queued) and CompactWithSteers (`/compact`
            // with commands queued) are hard stops too — the carried steers
            // run as the follow-up turn via run_turn_with_retry. Without this
            // early return, Compact/Clear fall through to `stream_result?`
            // (an Err) and get swallowed by the retry loop, losing the
            // command entirely.
            if matches!(
                state.stop_reason,
                Some(StopReason::Interrupt)
                    | Some(StopReason::InterruptWithSteers(_))
                    | Some(StopReason::Cancel)
                    | Some(StopReason::Compact)
                    | Some(StopReason::CompactWithSteers(_))
                    | Some(StopReason::Clear)
            ) {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Finished {
                            reason: FinishReason::Stop,
                        },
                    ))
                    .await;
                return Ok(TurnOutcome {
                    finish_reason: FinishReason::Stop,
                    text: String::new(),
                    tool_calls_made: 0,
                    stop_reason: state.stop_reason.take(),
                });
            }
            // Apply buffered steers (from the provider-request phase) to
            // stop_reason so the post-streaming check catches them after the
            // stream produces output — mirroring the mid-stream soft-stop.
            // ALL buffered steers carry over in arrival order (not just the
            // first) — a typed command is never dropped.
            if state.stop_reason.is_none() && !buffered_steers.is_empty() {
                state.stop_reason = Some(StopReason::Steer(buffered_steers));
            }
            let (stream, estimated_prompt_tokens) = stream_result?;

            // Consume the provider stream (deltas, tool-call assembly, usage
            // stats, command folds) — see consume_stream.
            let StreamOutcome {
                mut acc,
                text,
                reasoning_text,
                finish_reason,
                had_error,
                mut stream_error,
            } = self
                .consume_stream(
                    stream,
                    &provider,
                    &context_manager,
                    &mut state,
                    fanin_tx,
                    agent_id,
                    cmd_rx,
                    session_id,
                    estimated_prompt_tokens,
                    &breakdown,
                )
                .await;

            // The reasoning/thinking text captured this turn, packaged for the
            // assistant Message. `None` when the model produced none (or the
            // provider isn't a reasoning model) so the field stays omitted on
            // serialize; `Some` only when real reasoning text was streamed.
            let assistant_reasoning = if reasoning_text.is_empty() {
                None
            } else {
                Some(reasoning_text.clone())
            };

            // A hard stop during streaming ends the turn here, keeping
            // partial output — see record_interrupted_output.
            if let Some(outcome) = self
                .record_interrupted_output(
                    &mut state,
                    messages,
                    &mut acc,
                    &text,
                    &assistant_reasoning,
                    &provider,
                    fanin_tx,
                    agent_id,
                )
                .await
            {
                return Ok(outcome);
            }

            if had_error
                && text.is_empty()
                && !acc.saw_tool_calls()
                && state.stop_reason.is_none()
            {
                // The stream errored before producing any useful output (e.g.
                // a connection reset / unexpected EOF mid-stream). Return an
                // `Err` so the outer `run_turn_with_retry` layer retries the
                // whole turn with backoff — the same recovery path used for
                // connection-establishment failures. We deliberately do NOT
                // emit an `AgentEvent::Error` here: the outer layer owns the
                // retry messaging (`retrying: true` / final `retrying: false`).
                //
                // `state.stop_reason.is_none()` guard: a pending steer must not be
                // lost — the `Err` path carries no TurnOutcome, so retrying
                // would silently drop the user's message. Fall through
                // instead: the non-fatal note below surfaces the error and
                // the turn ends with the steer driving a fresh follow-up
                // turn.
                return Err(crate::error::Error::Provider(stream_error.unwrap_or_else(
                    || "stream ended with an error before any output".into(),
                )));
            }

            // A mid-stream error that arrived AFTER partial output (text or
            // tool-call deltas) is not retried — the partial content is usable
            // and re-requesting would duplicate it / re-execute side-effecting
            // tool calls. But the truncation must be visible to the user, so
            // emit a non-fatal note (`retrying: true` keeps the agent running
            // in the UI) and then fall through to process the partial output.
            if had_error {
                if let Some(err) = stream_error.take() {
                    let _ = fanin_tx
                        .send((
                            agent_id,
                            AgentEvent::Error {
                                error: err,
                                retrying: true,
                            },
                        ))
                        .await;
                }
            }

            // If no tool calls, the turn is done.
            let msg_meta = acc.take_message_provider_meta();
            // Take the verbatim raw assistant turn (Rule 1) before finalize()
            // consumes the accumulator — the request builder echoes this
            // unchanged instead of reconstructing from view fields.
            let mut raw = acc.take_raw();
            let mut response_id = acc.take_response_id();
            let tool_calls = acc.finalize();
            // The generation-vs-harness discrimination tap (backlog e8b39d72
            // H1): stamp the delivered tool-call args — VERBATIM, before any
            // normalization and before the bad-JSON filter below — onto this
            // response's trace record. When a future incident corrupts an
            // edit, comparing this record's raw_tool_calls against the
            // executed/normalized args separates model-emission decay from
            // harness-layer mutation.
            provider.record_raw_tool_calls(
                tool_calls
                    .iter()
                    .map(|tc| (tc.id.clone(), tc.name.clone(), tc.arguments.clone()))
                    .collect(),
            );
            // Check for malformed tool-call arguments. This can happen when:
            // - The model hits the token limit (finish_reason: Length) mid-argument
            // - The gateway truncates the stream
            // - The model emits invalid JSON (e.g. unescaped newlines)
            // In all cases, feed an error back so the model retries rather than
            // sending broken JSON to the gateway on the next turn.
            let has_bad_json = tool_calls
                .iter()
                .any(|tc| serde_json::from_str::<serde_json::Value>(&tc.arguments).is_err());
            if has_bad_json {
                if let Some(outcome) = self
                    .handle_bad_json(
                        &mut state,
                        &tool_calls,
                        messages,
                        &finish_reason,
                        &text,
                        &assistant_reasoning,
                        &msg_meta,
                        &mut response_id,
                        &provider,
                        fanin_tx,
                        agent_id,
                    )
                    .await
                {
                    return Ok(outcome);
                }
                continue; // retry the loop
            }
            // The model produced valid tool-call arguments — reset the
            // bad-JSON retry counter (a recovery within the turn).
            state.bad_json_count = 0;
            if tool_calls.is_empty() {
                // Add the assistant message to the conversation. A "null turn"
                // (model returned Finish with no text and no tool calls — the
                // "agent just stops" moment) would push an EMPTY assistant
                // message here. Several providers reject an empty assistant
                // turn ("messages[N] is an assistant message with no content
                // and no tool calls"), which `validate_request_messages`
                // catches as a local error. That error is unrecoverable: the
                // empty message stays in history, so every retry re-sends it
                // and re-fails identically — wedging the session until restart.
                // Substitute a minimal non-empty placeholder so the assistant
                // turn is recorded (keeping the conversation shape valid) while
                // never producing an empty-content message.
                let assistant_text = if text.trim().is_empty() {
                    "(no output)".to_string()
                } else {
                    text.clone()
                };
                messages.push(Message {
                    reasoning_content: assistant_reasoning.clone(),
                    provider_meta: msg_meta,
                    // H1: clear raw — the null-turn raw has no content key
                    // (merge_scalar skips null/empty); echoing it would send
                    // an empty assistant message that 400s. The "(no output)"
                    // placeholder is in the structured content field.
                    raw: None,
                response_id: response_id.take(),
                    origin_provider: Some(provider.kind().as_str().to_string()),
                    origin_model: Some(provider.model().to_string()),
                    ..Message::assistant_text(assistant_text)
                });
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Finished {
                            reason: finish_reason.clone(),
                        },
                    ))
                    .await;
                return Ok(TurnOutcome {
                    finish_reason,
                    text,
                    tool_calls_made: 0,
                    // A bare Steer can be pending here (it falls through the
                    // hard-stop gate above so the batch drains; with no tool
                    // calls the turn ends here). Hardcode `None` and the
                    // steer — the user's message — is silently dropped.
                    stop_reason: state.stop_reason.take(),
                });
            }

            // Cross-iteration tool-loop defense: a response that follows a
            // tool error is a repair attempt, owned by MAX_RETRIES (the same
            // error across three error INTERACTIONS — a same-time batch of
            // failing calls is one interaction, backlog 7f72d3d7). The former
            // turn-level repetition guard (three byte-identical consecutive
            // responses) was removed 2027-01-24: after the 7f72d3d7 fix its
            // only reachable domain was identical responses following
            // SUCCESSFUL batches, which aborted legitimate re-reads (the
            // 2027-01-24 12:41 incident). Within-response loops stay owned by
            // the in-stream R10 guard (detect_repetition, provider/stream).

            // Record the assistant's tool-call message, run the tool batch
            // (safe points, execution, result feedback, workflow events),
            // then record the tools-phase duration and compact old tool
            // results — see execute_tool_batch.
            self.execute_tool_batch(
                &tool_calls,
                &text,
                &assistant_reasoning,
                msg_meta,
                &mut raw,
                &mut response_id,
                messages,
                fanin_tx,
                agent_id,
                cmd_rx,
                session_id,
                &provider,
                &mut state,
            )
            .await;

            // A stop signal arrived during a tool call (or its approval wait).
            // End the turn at this break point (the batch just completed),
            // keeping the assistant text + tool results, and carry the stop
            // reason back via TurnOutcome: Cancel terminates the agent,
            // Interrupt returns to idle, Steer runs a follow-up turn.
            if state.stop_reason.is_some() {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Finished {
                            reason: FinishReason::Stop,
                        },
                    ))
                    .await;
                return Ok(TurnOutcome {
                    finish_reason: FinishReason::Stop,
                    text,
                    tool_calls_made: 0,
                    stop_reason: state.stop_reason.take(),
                });
            }

            // Reviewer report-writing abort (backlog 5b46674d): a reviewer
            // that has failed write_review_report
            // REVIEW_REPORT_MAX_FAILURES times in a row (not reset by other
            // tools' successes) is stuck on its single output channel —
            // abort with a DISTINCT final error naming the review report, so
            // the child resolves as failed and the failed-reviewer protocol
            // (no report) fires for the parent. Checked BEFORE the generic
            // MAX_RETRIES abort so the distinct message wins when both trip
            // (three consecutive report failures with nothing in between).
            if state.review_report_failures >= REVIEW_REPORT_MAX_FAILURES {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Error {
                            error: format!(
                                "Aborting turn: write_review_report failed \
                                 {REVIEW_REPORT_MAX_FAILURES} times in a row — the reviewer \
                                 could not complete its review report (a partial report may \
                                 exist from earlier chunks)."
                            ),
                            retrying: false,
                        },
                    ))
                    .await;
                return Ok(TurnOutcome {
                    finish_reason: FinishReason::Stop,
                    text,
                    tool_calls_made: 0,
                    stop_reason: None,
                });
            }

            // If the model has hit the consecutive-error cap, surface a final
            // error and stop the turn rather than looping indefinitely.
            //
            // Terminal exclusivity: emit **only** final Error (no trailing
            // Finished). The provider-exhaustion path already does this. A
            // paired Finished used to make the IPC forwarder resolve Run-All
            // twice (rollback on Error, then commit-success on Finished). The
            // forwarder also latches against that, but the emission contract
            // is: one terminal outcome per turn failure.
            if state.tool_error_count >= MAX_RETRIES {
                // Failure triage: the turn aborts with classified failures
                // whose retry never landed — resolve them as `cap_reached`,
                // and name the last classified class in the abort message
                // (absent when triage is off => byte-identical message).
                for pending in state.pending_triage_rows.drain(..) {
                    failure_triage::resolve_failure(
                        pending,
                        failure_triage::TriageDisposition::CapReached,
                    );
                }
                let classified = match state.last_triage_class {
                    Some(class) => {
                        format!(" Last classified failure: {}.", class.label())
                    }
                    None => String::new(),
                };
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Error {
                            error: format!(
                                "Aborting turn: {MAX_RETRIES} consecutive tool errors \
                                 (the model may be stuck).{classified}"
                            ),
                            retrying: false,
                        },
                    ))
                    .await;
                return Ok(TurnOutcome {
                    finish_reason: FinishReason::Stop,
                    text,
                    tool_calls_made: 0,
                    stop_reason: None,
                });
            }

            // Loop again — the model will see the tool results and continue.
        }
    }

    /// Handle a pending provider swap: the model picker deferred it so the
    /// OLD provider can summarize first when the conversation is too large
    /// for the new, smaller window. Returns `Some(TurnOutcome)` when an
    /// interrupt during the pre-swap summarization ends the turn early;
    /// `None` to continue the turn (the swap is completed either way).
    async fn handle_pending_swap(
        &self,
        messages: &mut Vec<Message>,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        session_id: Option<&str>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
    ) -> Option<TurnOutcome> {
        // Pending provider swap: if the user switched to a model with a smaller
        // context window, the swap was deferred so we can summarize using the
        // OLD provider first. Take it here (before the provider snapshot) so
        // the snapshot reflects the new provider after the swap completes.
        if let Some(pending) = self.take_pending_swap() {
            let old_provider: Arc<dyn LlmClient> = self
                .provider
                .read()
                .expect("provider lock poisoned")
                .clone();
            let old_context_manager = self
                .context_manager
                .read()
                .expect("context_manager lock poisoned")
                .clone();
            let new_max = pending.provider.capabilities().max_context;
            let token_count = context::ContextManager::count_tokens(messages);
            if token_count > (new_max as f64 * SWAP_SUMMARIZE_FILL) as usize {
                // Conversation is too large for the new (smaller) window —
                // summarize using the OLD provider before completing the swap.
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Phase {
                            phase: PhaseKind::Compacting,
                        },
                    ))
                    .await;
                let _ = fanin_tx.send((agent_id, AgentEvent::CompactStarted)).await;
                // [models.summarize] routes the summary to the dedicated slot
                // when set; unset rides the old provider (today's behavior).
                let summary_provider = self.summarize_provider(&old_provider);
                let summarize_result = old_context_manager
                    .summarize_with_interrupt(
                        messages,
                        KEEP_RECENT_ON_SWAP,
                        // The result must fit the window it is being swapped
                        // TO — the new model is what will receive it.
                        context::sendable_budget(pending.provider.capabilities()),
                        summary_provider.as_ref(),
                        cmd_rx,
                    )
                    .await;
                let (summarized, mut buffered, stop_reason, summarize_usage) = match summarize_result
                {
                    Ok(result) => result,
                    Err(e) => {
                        // Summarization failed — keep the original messages and
                        // complete the swap anyway (the next turn's regular
                        // summarization will handle the oversized context).
                        let _ = fanin_tx
                            .send((
                                agent_id,
                                AgentEvent::Error {
                                    error: format!("model-switch summarization failed: {e}"),
                                    retrying: true,
                                },
                            ))
                            .await;
                        // R21 (round-1 review LOW 5): the summarizer's own
                        // failure is invisible to the main loop's Usage arm —
                        // record it tagged purpose='summarize' so the
                        // mega-prompt's cost shows even when it fails. The
                        // estimate uses the full conversation (the summary
                        // prompt embeds it serialized).
                        self.record_stats_row(
                            session_id,
                            summary_provider.as_ref(),
                            StatsRow {
                                prompt_tokens: crate::provider::estimate_prompt_tokens(
                                    messages,
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
                        (messages.clone(), Vec::new(), None, None)
                    }
                };
                // R21: compaction's own mega-prompt is invisible to the main
                // loop's Usage arm — record it tagged purpose='summarize' so
                // its cost (up to ~300K tokens, guaranteed 0% cache) shows in
                // the aggregates.
                if let Some(usage) = summarize_usage {
                    self.record_stats_row(
                        session_id,
                        summary_provider.as_ref(),
                        StatsRow {
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
                } else if stop_reason.is_some() {
                    // D1 (round-1 review LOW 4): the summarizer's stream was
                    // dropped mid-flight by a user interrupt — the provider
                    // billed the tokens generated so far. Record a cancelled
                    // summarize row (mirroring the turn-loop arm) so the most
                    // expensive aborted-request class stays countable. The
                    // estimate uses the full conversation (the summary prompt
                    // embeds it serialized).
                    self.record_stats_row(
                        session_id,
                        summary_provider.as_ref(),
                        StatsRow {
                            prompt_tokens: crate::provider::estimate_prompt_tokens(
                                messages,
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
                *messages = summarized;
                // Drop any steers the user dismissed (the "x" on a pending
                // steer) before re-injecting buffered commands as system
                // messages (mirrors the main-loop summarization path).
                crate::agent::drop_cancelled_steers(&mut buffered);
                // Re-inject buffered commands as system messages (mirrors the
                // main-loop summarization path).
                for cmd in buffered {
                    match cmd {
                        AgentCommand::Suggestion(payload) => {
                            // Text-only steer → guidance message (unchanged
                            // mid-work semantics; user-role on
                            // `tail_as_user_messages` vendors so no trailing
                            // system block can reach DeepSeek/Ollama — see
                            // push_suggestion_message). An image-bearing
                            // steer → user message with image blocks (image
                            // content belongs in user messages; the same
                            // multimodal / vision-fallback handling as a
                            // normal prompt).
                            let crate::runtime::SteerPayload { text, images } = payload;
                            // Keyed to the SWAP TARGET, not the provider that
                            // ran the summary: the requests that follow are
                            // served by the new vendor (the snapshot is
                            // refreshed once the swap completes), and a
                            // user-role suggestion is acceptable to every
                            // vendor even if the swap ends up interrupted.
                            if images.is_empty() {
                                Self::push_suggestion_message(messages, &pending.provider, &text);
                            } else {
                                let content = self
                                    .build_user_content(agent_id, fanin_tx, text, &images)
                                    .await;
                                messages.push(Message {
                                    content,
                                    ..Message::user_text("")
                                });
                            }
                        }
                        other => {
                            eprintln!(
                                "unexpected command buffered during \
                                 model-switch summarization: {other:?}"
                            );
                        }
                    }
                }
                if let Some(stop) = stop_reason {
                    // Interrupt/Cancel during summarization — emit a paired
                    // end event for the CompactStarted above (mirrors the main
                    // summarization path's interrupt note), complete the swap
                    // anyway (the user chose this model), then return early.
                    let _ = fanin_tx
                        .send((
                            agent_id,
                            AgentEvent::Error {
                                error: "Compaction interrupted — original \
                                    conversation kept, model switch completed."
                                    .to_string(),
                                retrying: false,
                            },
                        ))
                        .await;
                    self.set_provider(pending.provider.clone(), pending.context_manager.clone());
                    *self
                        .explicit_provider
                        .write()
                        .expect("explicit_provider lock poisoned") =
                        Some((pending.provider, pending.context_manager));
                    return Some(TurnOutcome {
                        finish_reason: FinishReason::Stop,
                        text: String::new(),
                        tool_calls_made: 0,
                        stop_reason: Some(stop),
                    });
                }
            }
            // Complete the deferred swap.
            self.set_provider(pending.provider.clone(), pending.context_manager.clone());
            *self
                .explicit_provider
                .write()
                .expect("explicit_provider lock poisoned") =
                Some((pending.provider, pending.context_manager));
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Error {
                        error: format!(
                            "Switched to '{}' (summarized for smaller context)",
                            pending.model
                        ),
                        retrying: false,
                    },
                ))
                .await;
        }
        None
    }

    /// Emit workflow events for a successful workflow-tool call so the UI
    /// updates in real time, plus the plan-completion episodic memory write
    /// when the state just reached Complete. Extracted from run_turn
    /// (quality review HIGH 1); behavior unchanged.
    async fn emit_workflow_state(
        &self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        tc: &ToolCall,
        result: &ToolResult,
        session_id: Option<&str>,
    ) {
        // If this was a workflow tool that succeeded, emit the
        // corresponding workflow events so the UI updates in real time
        // (plan created, step completed, state transitioned). `finish`
        // is included so the Reviewing → Complete transition emits a
        // WorkflowStateChanged event (it previously did not, leaving
        // the UI stale until the next turn).
        //
        // GATED ON result.success (review finding 1, 2026-08-20): a
        // FAILED workflow call (rejected by the state gate, bad
        // arguments, hallucinated tool) never changed the workflow —
        // emitting its unchanged state here is wrong twice over: it
        // lies to the UI and it counts as false "loop ran" evidence
        // for the backlog plan-loop gate (which would mark a
        // resting-Complete no-op turn as Done).
        if result.success
            && (tc.name == "create_plan"
                || tc.name == "update_plan"
                || tc.name == "complete_step"
                || tc.name == "abandon_plan"
                || tc.name == "finish"
                || tc.name == "skill_start"
                || tc.name == "skill_end"
                || tc.name == "abandon_skill")
        {
            let (wf_state, step_index, top_plan_id, plan_title, plan_goal) = {
                let wf = self.workflow.lock().await;
                let state = wf.state();
                let top_plan_id = wf.top_plan_id().map(|s| s.to_string());
                let plan = wf.plan();
                let plan_title = plan.map(|p| p.title.clone());
                let plan_goal = plan.map(|p| p.goal.clone());
                let step_index = if tc.name == "complete_step" {
                    serde_json::from_str::<serde_json::Value>(&tc.arguments)
                        .ok()
                        .and_then(|v| match v.get("step_index") {
                            // The step number is 1-indexed (1 = the
                            // first step); a model occasionally quotes
                            // it as a string — accept both so the
                            // event also fires on the numeric-string
                            // path the complete_step tool accepts.
                            Some(serde_json::Value::Number(n)) => n.as_u64(),
                            Some(serde_json::Value::String(s)) => {
                                s.trim().parse::<u64>().ok()
                            }
                            _ => None,
                        })
                        .map(|i| i as u32)
                } else {
                    None
                };
                (state, step_index, top_plan_id, plan_title, plan_goal)
            };
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::WorkflowStateChanged {
                        state: wf_state,
                        top_plan_id: top_plan_id.clone(),
                    },
                ))
                .await;
            if let Some(idx) = step_index {
                let _ = fanin_tx
                    .send((agent_id, AgentEvent::StepCompleted { step_index: idx }))
                    .await;
            }

            // Quick memory at Complete: when a workflow tool just
            // transitioned the state to Complete (the `finish` tool for
            // implementation plans, or `complete_step` on a research
            // root plan), capture a concise episodic memory NOW so the
            // finished work survives a crash and is immediately
            // recallable for the next plan — instead of waiting for
            // shutdown consolidation (which never runs if the session
            // crashes). Fire-and-forget: memory capture must never
            // block or break the turn. Only fires when a memory store
            // + session id are present.
            //
            // Dedup: the completed root plan is RETAINED on the stack
            // (not popped), and a skill that starts+ends in Complete
            // (e.g. merge_to_main) re-enters Complete for the same
            // plan. Guard on the root plan id so each plan is captured
            // at most once per session.
            if wf_state == WorkflowState::Complete && result.success {
                let already_captured = top_plan_id
                    .as_deref()
                    .map(|id| self.captured_complete_plan_id().as_deref() == Some(id))
                    .unwrap_or(false);
                if !already_captured {
                    if let Some(plan_id) = &top_plan_id {
                        self.set_captured_complete_plan_id(plan_id.clone());
                    }
                    if let Some(store) = &self.memory {
                        if let Some(sid) = session_id {
                            let title = plan_title.clone().unwrap_or_default();
                            let goal = plan_goal.clone().unwrap_or_default();
                            let now_epoch = {
                                use std::time::{SystemTime, UNIX_EPOCH};
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .map(|d| d.as_secs() as i64)
                                    .unwrap_or(0)
                            };
                            let mut memory = Memory::new(
                                MemoryTier::Episodic,
                                if title.is_empty() {
                                    "Completed plan".to_string()
                                } else {
                                    format!("Completed plan: {title}")
                                },
                                if goal.is_empty() {
                                    "Plan reached Complete.".to_string()
                                } else {
                                    format!(
                                        "Plan '{title}' reached Complete. Goal: {goal}."
                                    )
                                },
                                now_epoch,
                            );
                            memory.source_session_ids = vec![sid.to_string()];
                            let store = Arc::clone(store);
                            tokio::spawn(async move {
                                // Fire-and-forget stays (memory capture must
                                // never block or break the turn), but a failed
                                // durable write must not vanish silently —
                                // log it (quality review LOW 6).
                                if let Err(e) = store.write(memory).await {
                                    eprintln!(
                                        "mnemo: failed to persist plan-completion memory: {e}"
                                    );
                                }
                            });
                        }
                    }
                }
            }
        }
    }

    /// Execute a batch of tool calls requested by the assistant: record the
    /// assistant's tool-call message, flip the inflight bar to "running
    /// tools", then run each call with a between-call safe point (steers /
    /// interrupts / cancels folded; hard stops synthesize "not run" results
    /// for the remaining calls), feed each result back as a tool message +
    /// event, emit workflow events, and finally record the tools-phase
    /// duration and compact old tool results. Extracted from run_turn
    /// (quality review HIGH 1); behavior unchanged.
    async fn execute_tool_batch(
        &self,
        tool_calls: &[ToolCall],
        text: &str,
        assistant_reasoning: &Option<String>,
        msg_meta: Option<serde_json::Map<String, serde_json::Value>>,
        raw: &mut Option<serde_json::Value>,
        response_id: &mut Option<String>,
        messages: &mut Vec<Message>,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        session_id: Option<&str>,
        provider: &Arc<dyn LlmClient>,
        state: &mut TurnState,
    ) {
        // A stop signal set by a tool call (Interrupt/Cancel during approval or
        // ask_user, or — once step 4 lands — during tool execution). When set,
        // the turn ends at the next break point (after the current tool call
        // completes). The caller (run_turn_with_retry) reads this via
        // TurnOutcome.stop_reason and acts: Cancel terminates the agent,
        // Interrupt returns to idle, Steer runs a follow-up turn.
        let mut stop_signal: Option<StopReason> = None;
        // Repeat-failure circuit breaker (tool-call robustness): queued by
        // the per-call loop below when a failed tool result repeats a
        // PRIOR identical failure of the same tool; pushed after the
        // batch. (tool name, failed tool-result content).
        let mut pending_correction: Option<(String, String)> = None;
        // Per-interaction error accounting (backlog 7f72d3d7): the batch is
        // ONE interaction for the MAX_RETRIES cap — a batch of identical
        // failing calls is one error event (the model sees every error
        // result and gets a chance to repair), not N consecutive errors.
        let mut batch_had_error = false;
        let mut batch_had_success = false;
        // Failure triage (Laya, opt-in; backlog 1a4049c1): classify the
        // batch's first non-denial failure ONCE. The class steers the harness
        // auto-retry below (tier 1), the guidance appended to the fed-back
        // failure (tier 2), and the turn-level abort message (which names
        // it). No gate / flag off / no usable answer / a denial => stays
        // `None` and every branch below keeps its pre-classifier behavior
        // byte-for-byte.
        let mut batch_triage: Option<failure_triage::FailureTriage> = None;
        // One classification per failed batch => at most one training-log
        // row for it (either the tier-1 retry row or the tier-2 guidance
        // row), never both.
        let mut batch_triage_logged = false;
        // Add the assistant message with tool calls.
        messages.push(Message {
            reasoning_content: assistant_reasoning.clone(),
            provider_meta: msg_meta,
            raw: raw.take(),
            response_id: response_id.take(),
            origin_provider: Some(provider.kind().as_str().to_string()),
            origin_model: Some(provider.model().to_string()),
            ..Message::assistant(text.to_string(), tool_calls.to_vec())
        });

        // Execute each tool call. `deny_all_latched` (set when the user
        // answers DenyAll) skips remaining calls without re-prompting
        // (Quality M1) — including later tool batches in this turn.
        // The stream ended and the model asked for tools — flip the
        // inflight bar to "running tools" (emitted only when there IS a
        // batch to run; the no-tool-calls path ends the turn here).
        // Also anchor the tools-phase measurement for the trace: the
        // duration is attributed to the request whose response produced
        // these tool calls via `record_tools_phase_ms` after the batch.
        let tools_start = std::time::Instant::now();
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::Phase {
                    phase: PhaseKind::RunningTools,
                },
            ))
            .await;
        for (i, tc) in tool_calls.iter().enumerate() {
            // Between-tool-call safe point: drain any commands that arrived
            // during the gap between the previous tool call completing and
            // this one starting. This is where steers, interrupts, and
            // cancels that arrived mid-batch (but not during an approval or
            // stream) are caught — unifying all three as "find a safe
            // stopping point." An Interrupt/Cancel here stops the turn
            // immediately (synthesizing "not run" results for the remaining
            // calls so messages stays consistent: N tool calls → N tool
            // results). A Steer sets stop_reason but does NOT break — the
            // batch completes (current behavior; interrupting mid-batch
            // would leave partial side effects).
            while let Ok(cmd) = cmd_rx.try_recv() {
                // Single accumulation rule for every command kind (steers
                // accumulate; Interrupt/Compact preserve queued steers;
                // Cancel/Clear override) — delegated to `fold` so the rule
                // is written once and can't drift (review Low 2).
                StopReason::fold(&mut state.stop_reason, cmd);
            }
            if matches!(
                state.stop_reason,
                Some(StopReason::Interrupt)
                    | Some(StopReason::InterruptWithSteers(_))
                    | Some(StopReason::Cancel)
                    | Some(StopReason::Compact)
                    | Some(StopReason::CompactWithSteers(_))
                    | Some(StopReason::Clear)
            ) {
                // Synthesize "not run" results for the remaining tool calls
                // (including this one — it hasn't started) so the
                // conversation has N tool results for N tool calls.
                synthesize_not_run_results(messages, fanin_tx, agent_id, &tool_calls[i..])
                    .await;
                break;
            }

            let (mut result, mut buffered) = self
                .execute_tool_call(
                    tc,
                    fanin_tx,
                    agent_id,
                    cmd_rx,
                    &mut state.deny_all_latched,
                    &mut stop_signal,
                )
                .await;

            // Failure triage (Laya, opt-in; backlog 1a4049c1): classify the
            // batch's first non-denial failure ONCE. The class steers the
            // harness auto-retry below (tier 1), the guidance appended to
            // the fed-back failure (tier 2), and the turn-level abort message
            // (which names it). No gate / flag off / no usable answer / a
            // denial => `batch_triage` stays `None` and every branch below
            // keeps its pre-classifier behavior byte-for-byte.
            if !result.success
                && !is_user_denial_tool_output(&result.output)
                && batch_triage.is_none()
                && stop_signal.is_none()
            {
                if let Some(gate) = &self.failure_triage {
                    let triage = gate.triage(&result.output).await;
                    if let failure_triage::FailureTriage::Classified { class, .. } = &triage {
                        state.last_triage_class = Some(*class);
                    }
                    batch_triage = Some(triage);
                }
            }

            // Tier 1 — harness auto-retry, NO model roundtrip: a confident
            // `transient` classification of a READ-ONLY tool call is re-run
            // by the harness itself with the identical arguments (read-only
            // tools are approval-free, so the re-run cannot double-apply a
            // side effect), bounded to one retry per call and
            // [`failure_triage::MAX_AUTO_RETRIES_PER_TURN`] per turn. A
            // swallowed success feeds the model only the successful result
            // plus a one-line note and never counts as a tool error; a failed
            // retry falls through to the normal feed-back below.
            let mut auto_retried = false;
            if !result.success && !batch_triage_logged && stop_signal.is_none() {
                if let (
                    Some(gate),
                    Some(failure_triage::FailureTriage::Classified { class, confidence }),
                ) = (&self.failure_triage, &batch_triage)
                {
                    let (class, confidence) = (*class, *confidence);
                    if failure_triage::should_auto_retry(
                        class,
                        &tc.name,
                        0,
                        state.auto_retries_this_turn,
                    ) {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            failure_triage::AUTO_RETRY_BACKOFF_MS,
                        ))
                        .await;
                        let mut retry_stop: Option<StopReason> = None;
                        let (retry_result, retry_buffered) = self
                            .execute_tool_call(
                                tc,
                                fanin_tx,
                                agent_id,
                                cmd_rx,
                                &mut state.deny_all_latched,
                                &mut retry_stop,
                            )
                            .await;
                        buffered.extend(retry_buffered);
                        if retry_stop.is_some() {
                            // The retry is this call's final word — merge its
                            // stop signal exactly like the first attempt's.
                            stop_signal = retry_stop;
                        }
                        state.auto_retries_this_turn += 1;
                        batch_triage_logged = true;
                        let pending = gate.log_failure(
                            failure_triage::FailureSite::ToolBatch,
                            Some(tc.name.as_str()),
                            &result.output,
                            class,
                            confidence,
                            failure_triage::TriageAction::AutoRetry,
                        );
                        if retry_result.success {
                            failure_triage::resolve_failure(
                                pending,
                                failure_triage::TriageDisposition::RetrySucceeded,
                            );
                            auto_retried = true;
                        } else {
                            failure_triage::resolve_failure(
                                pending,
                                failure_triage::TriageDisposition::RetryFailed,
                            );
                        }
                        result = retry_result;
                    } else {
                        // Tier 2 — the model still decides, but with the
                        // classification named: queue the training-log row
                        // (its disposition resolves when a later failure-free
                        // batch succeeds, or as `cap_reached` at the abort).
                        state
                            .pending_triage_rows
                            .push(gate.log_failure(
                                failure_triage::FailureSite::ToolBatch,
                                Some(tc.name.as_str()),
                                &result.output,
                                class,
                                confidence,
                                failure_triage::TriageAction::Guidance,
                            ));
                        batch_triage_logged = true;
                    }
                }
            }

            // Merge the per-tool-call stop signal (Interrupt/Cancel from
            // approval or ask_user) into the turn-level stop_reason.
            // Interrupt/Cancel stops the turn after this tool's result is
            // fed back — but steers are never silently dropped: an
            // Interrupt arriving with steers already folded during this
            // batch PRESERVES them (route through `fold` →
            // InterruptWithSteers), and a steer folded AFTER the signal
            // (the re-injection loop below) promotes Interrupt →
            // InterruptWithSteers / Compact → CompactWithSteers. A Steer
            // (from buffered commands) with no hard signal sets
            // stop_reason but lets the batch complete.
            let mut hard_stop = false;
            if let Some(signal) = stop_signal.take() {
                hard_stop = matches!(
                    signal,
                    StopReason::Interrupt
                        | StopReason::InterruptWithSteers(_)
                        | StopReason::Cancel
                );
                match signal {
                    StopReason::Interrupt => {
                        // fold preserves any steer already folded this batch.
                        StopReason::fold(&mut state.stop_reason, AgentCommand::Interrupt);
                    }
                    other => state.stop_reason = Some(other),
                }
            }

            // Re-inject any commands buffered while the tool executed
            // (approval/ask_user waits, or the grace-window drain). A
            // steer during approval triggers a soft-stop (like a
            // mid-stream steer): end the turn at this break point (the
            // tool call just completed) and carry the steer as a pending
            // user message for the next turn. The loop runs EVEN under
            // hard_stop: `fold` promotes a steer folded into
            // Interrupt/Compact to the steer-carrying variant, so a steer
            // buffered before the interrupt still drives a follow-up turn
            // instead of being silently dropped (2026-12-30 review
            // finding 1, plan edfff8d9), and a CancelSuggestion still
            // removes a matching steer the user dismissed. Cancel/Clear
            // keep dropping (documented intent). Unexpected commands are
            // logged.
            for cmd in buffered {
                match cmd {
                    cmd @ (AgentCommand::Suggestion(_)
                    | AgentCommand::Prompt { .. }
                    | AgentCommand::CancelSuggestion(_)) => {
                        // Accumulate re-injected steers (never dropped);
                        // a CancelSuggestion folds to drop a matching
                        // steer the user dismissed (the "x").
                        StopReason::fold(&mut state.stop_reason, cmd);
                    }
                    other => {
                        eprintln!("unexpected command buffered during approval: {other:?}");
                    }
                }
            }

            // Per-interaction error accounting (backlog 7f72d3d7): track
            // batch-level outcomes here; tool_error_count is applied ONCE
            // after the loop. User denials / DenyAll skips are deliberate
            // safety choices, not a stuck model (Quality H1 / Phase 1
            // review) — they neither count nor reset the counter.
            if result.success {
                batch_had_success = true;
            } else if !is_user_denial_tool_output(&result.output) {
                batch_had_error = true;
            }

            // Reviewer report-writing retry accounting (backlog 5b46674d):
            // a DEDICATED consecutive-failure counter for write_review_report
            // — NOT reset by other tools' successes (unlike
            // tool_error_count) — so a reviewer that keeps failing verdict
            // validation fails loudly instead of exhausting its turns and
            // finishing silently report-less.
            if tc.name == "write_review_report" {
                if result.success {
                    state.review_report_failures = 0;
                } else {
                    state.review_report_failures += 1;
                }
            }

            // Capture the report path when a reviewer subagent writes its
            // report, so the event forwarder can include it in the
            // completion notification to the parent agent (instead of a
            // generic "read its report" message that forces the parent to
            // search for the file — unreliable with multiple concurrent
            // reviewers). The path is the `.with_data` payload from
            // write_review_report.
            if tc.name == "write_review_report" && result.success {
                if let Some(path) = result
                    .data
                    .as_ref()
                    .and_then(|d| d.get("path"))
                    .and_then(|v| v.as_str())
                {
                    self.set_last_review_report(path.to_string());
                }
            }

            // Silently record the tool event as working memory (no
            // approval, no event — just background capture for recall).
            // Only DURABLE actions are recorded: read-only tools
            // (file_read, search, browser_*, describe_image, …) produce
            // noise that crowds out distilled facts in recall, so they are
            // skipped. The full output of every tool already lives in the
            // conversation history; working memory only needs the durable
            // signal for consolidation to distill later.
            if let Some(store) = &self.memory {
                let parsed_args = serde_json::from_str::<serde_json::Value>(&tc.arguments)
                    .unwrap_or(serde_json::Value::Null);
                if is_durable_tool(&tc.name, &parsed_args) {
                    let error_text = if !result.success {
                        Some(result.output.as_str())
                    } else {
                        None
                    };
                    // Fire-and-forget stays (the capture must never block
                    // or break the turn), but a failed durable write must
                    // not vanish silently — log it (quality review LOW 6).
                    if let Err(e) = store
                        .record_tool_event(
                            session_id,
                            &tc.name,
                            parsed_args,
                            &result.output,
                            error_text,
                        )
                        .await
                    {
                        eprintln!("mnemo: failed to record tool event: {e}");
                    }
                }
            }

            // Feed the result back as a tool message. FAILED results are
            // wrapped in a structured `[tool error]` marker so the model
            // can reliably parse the error text and re-issue the call
            // with corrected arguments — the raw output alone reads as a
            // generic result. The error text itself is preserved VERBATIM
            // inside the marker (no rewriting), so the user-denial /
            // interrupt classifiers (is_user_denial_tool_output) keep
            // matching by substring and MAX_RETRIES stays blind to
            // deliberate denials.
            //
            // INGESTION CAP: the output is bounded here, before it ever
            // becomes prompt content — a runaway result (base64 dump, whole
            // file, recursive grep) is otherwise already in flight by the time
            // compaction could trim it. Under the cap the text passes through
            // byte-for-byte, so the substring classifiers are unaffected (a
            // denial is orders of magnitude shorter than the cap).
            let capped = context::cap_tool_result_text(&result.output);
            let tool_content = if result.success {
                if auto_retried {
                    // Tier-1 transparency: the model sees the SUCCESS (the
                    // swallowed failure would only cost a repair roundtrip),
                    // with a one-line note so it knows the result took an
                    // extra beat and a transient blip was in play.
                    format!(
                        "{capped}\n[harness note] (auto-retried once after a transient failure)"
                    )
                } else {
                    capped
                }
            } else {
                format!("[tool error] {capped}")
            };
            // The model-facing content: a classified failure carries its
            // guidance note (tier 2). The repeat-failure breaker below keeps
            // comparing the RAW marker (`tool_content`), so triage never
            // disturbs repeat detection.
            let feed_content = match (&batch_triage, result.success) {
                (Some(failure_triage::FailureTriage::Classified { class, .. }), false)
                    if !is_user_denial_tool_output(&result.output) =>
                {
                    format!(
                        "{tool_content}\n[harness triage] {}",
                        failure_triage::guidance_note(*class)
                    )
                }
                _ => tool_content.clone(),
            };

            // Repeat-failure circuit breaker (tool-call robustness): when
            // this failure's (tool, error text) matches a PRIOR identical
            // failure in recent history, the model is pattern-matching off
            // its own previous malformed call instead of the schema
            // (live incident: 15 consecutive `git` commit calls missing
            // `message`, re-emitted verbatim while the error repeated
            // identically; only a fresh corrective user turn broke the
            // loop). Queue ONE corrective message — pushed after the
            // batch below — carrying the tool's actual schema. The scan
            // runs BEFORE this result is pushed, so a match is always
            // against a prior failure (a same-batch duplicate counts).
            // User denials / interrupts are deliberate safety choices,
            // not a stuck model — excluded, mirroring the MAX_RETRIES
            // rule at the top of this loop.
            if !result.success
                && !is_user_denial_tool_output(&result.output)
                && pending_correction.is_none()
                && repeated_tool_failure(messages, &tc.name, &tool_content)
            {
                pending_correction = Some((tc.name.clone(), tool_content.clone()));
            }

            let tool_message = Message::tool_result(tc.id.clone(), tc.name.clone(), feed_content);
            messages.push(tool_message);

            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::ToolResult {
                        tool_call_id: tc.id.clone(),
                        result: result.clone(),
                    },
                ))
                .await;

            // Deliberately NO per-turn recall-cache invalidation here: the
            // auto-recall query is only the latest User-role message text
            // (see the recall block above), and tool results are appended
            // as Tool-role messages — they can never change the query. A
            // former ">4 KiB output invalidates" branch here re-ran an
            // identical recall (re-embed + rescan) and re-emitted a
            // duplicate MemoryRecalled transcript entry after every large
            // tool output, e.g. file reads (defect removed 2026-08-20).

            // Workflow events + plan-completion memory for a successful
            // workflow-tool call (see emit_workflow_state).
            self.emit_workflow_state(fanin_tx, agent_id, tc, &result, session_id)
                .await;

            // Hard stop (Interrupt/Cancel from approval or ask_user):
            // the current tool's result is fed back, but the remaining calls
            // in this batch must NOT run. Synthesize "not run" results for
            // them so messages stays consistent (N tool calls → N tool
            // results), then break so the post-loop stop_reason check ends
            // the turn.
            if hard_stop {
                synthesize_not_run_results(
                    messages,
                    fanin_tx,
                    agent_id,
                    &tool_calls[i + 1..],
                )
                .await;
                break;
            }
        }

        // Apply the per-interaction error accounting ONCE per batch
        // (backlog 7f72d3d7): any non-denial failure makes the batch ONE
        // error interaction (+1 — same-time identical failing calls must
        // not abort the turn in a single interaction, and the model gets a
        // repair chance between interactions); a batch with no failure but
        // at least one success resets the consecutive-error counter; a
        // denial-only batch leaves it unchanged.
        if batch_had_error {
            state.tool_error_count += 1;
        } else if batch_had_success {
            state.tool_error_count = 0;
            // Failure triage: a failure-free successful batch is the model's
            // retry landing — the turn's pending classified failures (the
            // guidance route) are now observed as `retry_succeeded`.
            for pending in state.pending_triage_rows.drain(..) {
                failure_triage::resolve_failure(
                    pending,
                    failure_triage::TriageDisposition::RetrySucceeded,
                );
            }
        }

        // Repeat-failure circuit breaker: push the queued corrective
        // message AFTER the batch — every tool result precedes it, so the
        // Anthropic tool_result-immediately-after-tool_use ordering
        // holds (the request builder coalesces adjacent tool messages
        // only; a user text message after them is a separate turn). One
        // correction per batch (the first repeat-failure wins); a model
        // that keeps failing identically re-fires next batch. The message
        // is harness-attributed so the transcript never masquerades as
        // human input, and it is a plain user-role tail message:
        // cache-stable (the stable head is untouched) and compaction
        // treats it like any user turn.
        if let Some((tool_name, failed_content)) = pending_correction.take() {
            if let Some(tool) = self.tools.get(&tool_name) {
                // Render the SAME schema the request carries (plan
                // 21118961): on a strict endpoint the advertised form is
                // the normalized one, and a correction quoting the raw
                // registry schema would disagree with what actually
                // constrains decoding.
                let mut schema = tool.schema();
                crate::provider::strict::apply(
                    &mut schema,
                    provider.capabilities().supports_strict_schema,
                );
                let correction = tool_call_correction(&tool_name, &failed_content, &schema);
                messages.push(Message::user_text(correction));
                // Context-only by design (user report 2027-01-24): the
                // correction rides the provider-facing messages — good in
                // the LLM context, NOT in the output. No event is
                // emitted: an Error here rendered a red "error: tool-call
                // correction: …" box in the chat transcript, an
                // activity-log row, and extended the UI doom streak
                // (reduceError counts every error event), none of which
                // the user wants.
            }
        }

        // The tools phase (stream end → batch finished) is complete:
        // record its duration on the newest trace record (the request
        // whose response produced these tool calls).
        provider.record_tools_phase_ms(tools_start.elapsed().as_millis() as u32);

        // Compact old tool results with hysteresis: no-op while the intact
        // window is <= keep_high (20), then cut back to keep (10) in one
        // pass. This keeps history byte-identical across batches so prefix
        // cache holds (~95% hit on stable requests), while keeping context
        // bounded under the provider's limit.
        let truncated = crate::agent::context::compact_old_tool_results(messages, 10, 20, 500);
        if truncated > 0 {
            state.token_accounting.reset();
        }
    }

    /// Resolve the provider + context manager for THIS iteration's request
    /// (re-run every loop iteration so a mid-turn workflow change — a
    /// `skill_start`/`skill_end` call, or a state transition from any
    /// workflow tool — switches to the configured per-context model on
    /// the next request within the same turn). Falls back to the turn-start
    /// default snapshot when no override resolves, and emits ModelChanged
    /// when the effective model flips. Extracted from run_turn (quality
    /// review HIGH 1); behavior unchanged.
    async fn resolve_iteration_provider(
        &self,
        default_provider: &Arc<dyn LlmClient>,
        default_context_manager: &context::ContextManager,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
    ) -> (Arc<dyn LlmClient>, context::ContextManager) {
        // Per-context model resolution — re-run EVERY iteration so a
        // mid-turn workflow change (skill_start/skill_end, or a state
        // transition from any workflow tool) takes effect on the next
        // provider request within this same turn. When the resolver
        // returns a model, build the throwaway provider + context
        // manager for it; otherwise fall back to the default snapshot.
        //
        // Track the effective model across resolutions and emit
        // ModelChanged when it flips: the per-turn override path updated
        // `resolved_model` internally but never told the UI, so a
        // mid-turn skill model switch (e.g. merge_to_main → grok) left
        // the toolbar/tab label stale — only set_model/save_endpoints
        // emitted the event (user report 2026-04-20).
        let prev_resolved = self.resolved_model();
        let prev_effort = self.resolved_effort();
        let (provider, context_manager) = {
            let wf = self.workflow.lock().await;
            let skill_name = wf.active_skill().map(|s| s.name.as_str());
            let plan_kind = wf.active_plan_kind();
            match self.resolve_turn_provider(wf.state(), skill_name, plan_kind) {
                Some((p, cm)) => (p, cm),
                None => (Arc::clone(default_provider), default_context_manager.clone()),
            }
        };
        // The wf guard is dropped above — safe to await the fan-in send.
        let now_resolved = self.resolved_model();
        let now_effort = self.resolved_effort();
        // Emit when the effective MODEL flips, or when only the EFFORT
        // changes (the same model id can serve two contexts with different
        // reasoning_effort overrides — the UI must not keep the stale
        // effort, backlog 51dab4da).
        if now_resolved != prev_resolved || now_effort != prev_effort {
            // None = the default provider's model (the turn-start
            // snapshot — the same model list_agents would report).
            let effective =
                now_resolved.unwrap_or_else(|| default_provider.model().to_string());
            // The endpoint serving that model: the local `provider` Arc is
            // exactly the client this iteration uses (the override when
            // one resolved, else the default snapshot). Empty names (test
            // mocks) become None — the frontend then falls back to its
            // endpoint-list resolution, the pre-wire behavior.
            let serving_endpoint = {
                let n = provider.provider_name().to_string();
                (!n.is_empty()).then_some(n)
            };
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::ModelChanged {
                        model: effective,
                        provider: serving_endpoint,
                        reasoning_effort: now_effort,
                    },
                ))
                .await;
        }

        (provider, context_manager)
    }

    /// Auto-compaction for one request-loop iteration — the caller has
    /// already checked the trigger (`token_count >=
    /// context_manager.effective_summarize_at()`). Announces the
    /// compaction (once per turn), runs the interruptible summarization
    /// with the preflight hard-ceiling keep-recent tier, re-injects
    /// buffered commands, resets + recomputes the token accounting, emits
    /// ContextUsage + Compacted, and invalidates the per-turn recall cache.
    /// Returns `Some(TurnOutcome)` when an interrupt during the
    /// summarization ends the turn early; `None` to continue. Extracted
    /// from run_turn (quality review HIGH 1); behavior unchanged.
    pub(crate) async fn maybe_compact(
        &self,
        state: &mut TurnState,
        messages: &mut Vec<Message>,
        provider: &Arc<dyn LlmClient>,
        context_manager: &context::ContextManager,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        session_id: Option<&str>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        token_count: usize,
        breakdown: &mut crate::runtime::ContextBreakdown,
    ) -> Option<TurnOutcome> {
        // Auto-compaction runs a whole extra LLM call inside the
        // "sending" window — surface it as its own live phase so the
        // inflight bar doesn't sit on "sending…" for the duration.
        // `Sending` is re-emitted right after the call.
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::Phase {
                    phase: PhaseKind::Compacting,
                },
            ))
            .await;
        // Transcript announcement too — paired with the Compacted (or
        // Error) event below so auto-compaction has the same start/end
        // visibility as manual /compact. Once per turn
        // (`compact_announced`) — a turn that stays over the threshold
        // re-compacts every iteration and would otherwise spam
        // start/end pairs.
        let announce = !state.compact_announced;
        state.compact_announced = true;
        if announce {
            let _ = fanin_tx.send((agent_id, AgentEvent::CompactStarted)).await;
        }
        // Per-turn attempt budget (2027-01-07 re-emission incident): a turn
        // that stays over the threshold re-compacts every iteration (see
        // compact_announced above) — unbounded, each cycle burning a
        // summarizer call and feeding the model a nearly-identical context
        // (which drives byte-identical re-emission of the same tool call).
        // Bound it: escalate keep_recent after 2 stuck attempts, abort the
        // turn at the 5th with a clear error. The counter resets whenever a
        // compaction brings the count back under the threshold (the
        // legitimate long-turn path crosses the fill rate repeatedly).
        state.compact_attempts += 1;
        state.compact_total += 1;
        if state.compact_attempts >= 5 {
            // Terminal failure — Error only, no trailing Finished (the
            // MAX_RETRIES terminal-exclusivity contract: one terminal
            // outcome per turn failure; the forwarder's Error arm flips
            // the running state and cleans up).
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Error {
                        error: format!(
                            "context stuck over threshold after {} compaction attempts — \
                             aborting turn",
                            state.compact_attempts
                        ),
                        retrying: false,
                    },
                ))
                .await;
            return Some(TurnOutcome {
                finish_reason: FinishReason::Stop,
                text: String::new(),
                tool_calls_made: 0,
                stop_reason: None,
            });
        }
        if state.compact_total >= MAX_COMPACTIONS_PER_TURN {
            // Absolute per-turn ceiling, independent of the reset below: a
            // compaction that lands the count back under the threshold re-arms
            // the attempt budget, and compaction became TOTAL with the sendable
            // post-condition — so a runaway tool loop re-arms on every cycle
            // and the attempt-5 abort above never fires (measured on that
            // fixture: 12 summarizer calls in one turn, each preceded by a
            // compaction that reported success). The reset stays, because a
            // legitimate long turn DOES cross the fill rate repeatedly while
            // making progress; this ceiling is what keeps the burn finite when
            // the turn is not making progress at all. One terminal outcome per
            // turn failure (Error only, no trailing Finished).
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Error {
                        error: format!(
                            "context compacted {} times in one turn — aborting to avoid an \
                             unbounded re-compaction loop",
                            state.compact_total
                        ),
                        retrying: false,
                    },
                ))
                .await;
            return Some(TurnOutcome {
                finish_reason: FinishReason::Stop,
                text: String::new(),
                tool_calls_made: 0,
                stop_reason: None,
            });
        }
        let compact_started = std::time::Instant::now();
        // Pre-flight hard-ceiling guard (auto-continuation L4): when
        // the pre-compaction token count exceeds the hard ceiling
        // (max_tokens − headroom), use aggressive keep_recent=3 instead
        // of the normal 6. This prevents fatal ContextWindowExceededError
        // on long sessions where the soft compaction at summarize_at
        // (50% fill) ran but didn't shrink enough — the conversation
        // sailed past the model's actual context window with no second
        // guard. The headroom (default 32K) reserves space for what
        // the conversation-token count can't see at trigger time —
        // the per-iteration volatile tail + stable footer (appended
        // after this check, popped after the request; the tools-schema
        // array IS included — set from the iteration-top schemas) —
        // plus the model's output.
        let keep_recent = if state.compact_attempts >= 3 {
            // Stuck escalation: the normal tail (6, or 3 over the hard
            // ceiling) keeps the context over the threshold — keep only the
            // single most recent message so the next request fits.
            1
        } else if context_manager.preflight_compact()
            && token_count > context_manager.hard_ceiling()
        {
            3
        } else {
            6
        };
        // [models.summarize] routes the summary to the dedicated slot when
        // set; unset rides the turn's provider (today's behavior). The
        // compact_ms stamp below stays on the turn's provider either way —
        // the metric attaches to the turn's trace, wherever the summary ran.
        let summary_provider = self.summarize_provider(provider);
        let summarize_result = context_manager
            .summarize_with_interrupt(
                messages,
                keep_recent,
                // The compacted result is what the TURN provider sends next, so
                // its window is the one that has to fit — not the summarizer's.
                context::sendable_budget(provider.capabilities()),
                summary_provider.as_ref(),
                cmd_rx,
            )
            .await;
        // Park the compaction time on the provider — the request's
        // trace record doesn't exist yet; the next record this client
        // creates (the POST below) gets stamped with it as
        // `compact_ms`. Recorded even when the call failed: the time
        // was spent either way.
        provider.record_compact_ms(compact_started.elapsed().as_millis() as u32);
        // Back to the normal sending window for the rest of the prep.
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::Phase {
                    phase: PhaseKind::Sending,
                },
            ))
            .await;
        let mut compact_failed = false;
        let (summarized, mut buffered, summarize_stop, summarize_usage) = match summarize_result {
            Ok(result) => result,
            Err(e) => {
                compact_failed = true;
                // R21 (round-1 review LOW 5): the summarizer's own failure is
                // invisible to the main loop's Usage arm — record it tagged
                // purpose='summarize' so the mega-prompt's cost shows even
                // when it fails. The estimate uses the full conversation (the
                // summary prompt embeds it serialized).
                self.record_stats_row(
                    session_id,
                    summary_provider.as_ref(),
                    StatsRow {
                        prompt_tokens: crate::provider::estimate_prompt_tokens(
                            messages,
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
                // Surface the failure instead of silently swallowing it
                // (the old `.unwrap_or_else` hid it). The turn
                // continues — `retrying: true` marks it transient.
                // Note: `summarize_stop` is None here, so the
                // summarization block falls through even though no
                // compaction happened (the messages are unchanged).
                // Once per turn (`announce`): one failure note is
                // enough — a wedged-over-threshold turn would
                // otherwise repeat it on every loop iteration.
                if announce {
                    let _ = fanin_tx
                        .send((
                            agent_id,
                            AgentEvent::Error {
                                error: format!(
                                    "context summarization failed (auto-compact skipped): {e}"
                                ),
                                retrying: true,
                            },
                        ))
                        .await;
                }
                // Keep the original messages and continue the turn.
                (messages.clone(), Vec::new(), None, None)
            }
        };
        // R21 (round-1 review LOW 2): a MECHANICAL compaction means the
        // summarizer rejected the request for SIZE, so the "summary" is a
        // harness note and there is no Usage to record — compaction succeeded
        // (that is the fallback's whole point) but the downgrade is otherwise
        // invisible: a chronically undersized [models.summarize] slot would
        // look like ordinary compaction in the transcript and in .coding/logs.
        // Record it as a summarize-purpose failure row (the request DID fail)
        // plus a once-per-turn note explaining why the summary reads like one.
        if !compact_failed && context::is_mechanical_compaction(&summarized) {
            self.record_stats_row(
                session_id,
                summary_provider.as_ref(),
                StatsRow {
                    prompt_tokens: crate::provider::estimate_prompt_tokens(messages, &[]) as u32,
                    completion_tokens: 0,
                    reasoning_tokens: 0,
                    cached_tokens: None,
                    ttft_ms: None,
                    generation_ms: None,
                    outcome: Some("error".into()),
                    purpose: Some("summarize".into()),
                },
            );
            if announce {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Error {
                            error: "the summarizer rejected the request for size — the context was \
                                    compacted MECHANICALLY (the summary is a harness note, not a \
                                    model summary); check the [models.summarize] window"
                                .to_string(),
                            retrying: true,
                        },
                    ))
                    .await;
            }
        }
        // R21: compaction's own mega-prompt is invisible to the main loop's
        // Usage arm — record it tagged purpose='summarize' so its cost (up to
        // ~300K tokens, guaranteed 0% cache) shows in the aggregates.
        if let Some(usage) = summarize_usage {
            self.record_stats_row(
                session_id,
                summary_provider.as_ref(),
                StatsRow {
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
        } else if summarize_stop.is_some() {
            // D1 (round-1 review LOW 4): the summarizer's stream was dropped
            // mid-flight by a user interrupt — the provider billed the tokens
            // generated so far. Record a cancelled summarize row (mirroring
            // the turn-loop arm) so the most expensive aborted-request class
            // stays countable. The estimate uses the full conversation (the
            // summary prompt embeds it serialized).
            self.record_stats_row(
                session_id,
                summary_provider.as_ref(),
                StatsRow {
                    prompt_tokens: crate::provider::estimate_prompt_tokens(
                        messages,
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
        *messages = summarized;
        // Drop any steers the user dismissed (the "x" on a pending
        // steer) before re-injecting buffered commands.
        crate::agent::drop_cancelled_steers(&mut buffered);
        for cmd in buffered {
            match cmd {
                crate::runtime::AgentCommand::Suggestion(payload) => {
                    // Notify the UI the steer landed, then inject it: a
                    // text-only steer as a system message (unchanged mid-work
                    // guidance semantics), an image-bearing steer as a user
                    // message with image blocks (image content belongs in
                    // user messages; the same multimodal / vision-fallback
                    // handling as a normal prompt).
                    let crate::runtime::SteerPayload { text, images } = payload;
                    let _ = fanin_tx
                        .send((
                            agent_id,
                            AgentEvent::SuggestionInjected {
                                text: text.clone(),
                                images: images.clone(),
                            },
                        ))
                        .await;
                    if images.is_empty() {
                        Self::push_suggestion_message(messages, &provider, &text);
                    } else {
                        let content = self
                            .build_user_content(agent_id, fanin_tx, text, &images)
                            .await;
                        messages.push(Message {
                            content,
                            ..Message::user_text("")
                        });
                    }
                }
                other => {
                    eprintln!(
                        "unexpected command buffered during summarization: {other:?}"
                    );
                }
            }
        }
        // Summarization (and any re-injected suggestions) changed the
        // messages, so the pre-summary accounting is stale — reset and
        // recompute once for an accurate ContextUsage event.
        state.token_accounting.reset();
        let (used, bd, _) = state.token_accounting.update(messages);
        let used = used as u32;
        let max = context_manager.max_tokens() as u32;
        *breakdown = bd;
        // A compaction that brought the count back under the threshold is the
        // legitimate path (a long turn legitimately crossing the fill rate
        // repeatedly) — reset the attempt budget. Still over: the kept-verbatim
        // tail dominates and the next iteration re-compacts (the counter climbs
        // toward the abort above). The per-turn TOTAL ceiling is what keeps
        // this reset from becoming an unbounded loop now that compaction always
        // succeeds.
        if (used as usize) < context_manager.effective_summarize_at() {
            state.compact_attempts = 0;
        }
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::ContextUsage {
                    used,
                    max,
                    breakdown: *breakdown,
                },
            ))
            .await;
        // Pair the CompactStarted announcement with its end on the
        // completed path — even without a reduction (the frontend
        // shows a "nothing to compact" note). Once per turn
        // (`announce`); skipped on failure (the Error event above
        // already paired with it); the interrupted path gets its own
        // end note below.
        if summarize_stop.is_none() && !compact_failed && announce {
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Compacted {
                        before: token_count as u32,
                        after: used,
                    },
                ))
                .await;
        }
        // Invalidate per-turn recall cache on summarize (messages prefix changed).
        state.last_recalled_user_query = None;
        state.last_recall_results = None;
        state.last_recall_store_version = 0;
        // If summarization was interrupted/cancelled, end the turn now —
        // the summary was abandoned (original messages preserved), so
        // there's no partial output to keep. Pair the CompactStarted
        // announcement with an end note first (never a dangling
        // "Compacting context…"), then emit Finished + carry the stop
        // reason so run_turn_with_retry acts on it.
        if let Some(reason) = summarize_stop {
            if announce {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Error {
                            error: "Compaction interrupted — original conversation kept."
                                .into(),
                            retrying: true,
                        },
                    ))
                    .await;
            }
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Finished {
                        reason: FinishReason::Stop,
                    },
                ))
                .await;
            return Some(TurnOutcome {
                finish_reason: FinishReason::Stop,
                text: String::new(),
                tool_calls_made: 0,
                stop_reason: Some(reason),
            });
        }
        None
    }

    /// Auto-recall: fetch relevant memories based on the latest user
    /// message and return them as the system-prompt memory context. Uses
    /// the non-bumping peek path (being surfaced is not evidence of
    /// usefulness, so passive recall must not refresh recency) and the
    /// per-turn cache (see TurnState). Extracted from run_turn (quality
    /// review HIGH 1); behavior unchanged.
    async fn auto_recall(
        &self,
        state: &mut TurnState,
        messages: &[Message],
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
    ) -> Option<String> {
        // Auto-recall: fetch relevant memories based on the latest user
        // message and inject them into the system prompt. Uses the
        // non-bumping peek path: being surfaced is not evidence of
        // usefulness, so passive recall must not refresh a memory's
        // recency (self-reinforcement loop, 2026-08-20).
        //
        // Per-turn cache (Perf H2/M5): if the latest user message text is
        // unchanged since the last recall in this turn, reuse the prior
        // scored results without re-embedding or rescanning the memory
        // store. The cache is scoped to the turn loop and is invalidated
        // only on summarization (the summarizer can rewrite the latest
        // user message). Tool-result appends never touch the query — it
        // reads User-role messages only, and harness-attributed corrections
        // are skipped in the lookup below — so tool results must NOT
        // invalidate:
        // doing so re-ran an identical recall and emitted duplicate
        // MemoryRecalled lines after every >4 KiB tool output (defect
        // fixed 2026-08-20).
        if let Some(store) = &self.memory {
            let query = last_user_query(messages);
            if let Some(q_text) = query {
                let q: &str = &q_text;
                let filter = crate::memory::MemoryFilter::new().limit(5);
                // Track whether THIS iteration hit the store (a FRESH
                // recall) or reused the per-turn cache — the
                // MemoryRecalled event fires only for fresh recalls
                // (review finding 1, 2026-04-20: emitting on cache reuse
                // re-sent the identical event every loop iteration).
                let mut fresh_recall = false;
                // Read the store version BEFORE the recall and stamp THAT:
                // with bump-after-visibility (the store bumps at the end of
                // each mutating op), any write invisible to this recall's
                // snapshot necessarily bumps above the stamped version, so
                // the next same-query recall misses the cache and re-runs
                // (review LOW-3 — stamping the post-recall version could pin
                // a write that landed mid-recall).
                let version_before = store.version();
                let results: Vec<crate::memory::ScoredMemory> =
                    if state.last_recalled_user_query.as_deref() == Some(q)
                        && state.last_recall_store_version == store.version()
                    {
                        if let Some(cached) = &state.last_recall_results {
                            cached.clone()
                        } else {
                            match store.recall_peek(q, &filter).await {
                                Ok(r) => {
                                    fresh_recall = true;
                                    state.last_recalled_user_query = Some(q.to_string());
                                    state.last_recall_results = Some(r.clone());
                                    state.last_recall_store_version = version_before;
                                    r
                                }
                                _ => Vec::new(),
                            }
                        }
                    } else {
                        match store.recall_peek(q, &filter).await {
                            Ok(r) => {
                                fresh_recall = true;
                                state.last_recalled_user_query = Some(q.to_string());
                                state.last_recall_results = Some(r.clone());
                                state.last_recall_store_version = version_before;
                                r
                            }
                            _ => Vec::new(),
                        }
                    };
                if !results.is_empty() {
                    // Visibility of memory access (user request
                    // 2026-04-20): tell the UI a FRESH auto-recall just
                    // injected memories into the prompt. Cache-reuse
                    // iterations and empty results emit nothing.
                    if fresh_recall {
                        let _ = fanin_tx
                            .send((
                                agent_id,
                                AgentEvent::MemoryRecalled {
                                    hits: results
                                        .iter()
                                        .map(|sm| crate::runtime::channels::RecallHit {
                                            tier: sm.memory.tier.as_str().to_string(),
                                            title: sm.memory.title.clone(),
                                        })
                                        .collect(),
                                },
                            ))
                            .await;
                    }
                    Some(prompt::format_recall_context(&results))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Whether this provider's request must end with USER-role messages instead
    /// of system ones: Local-kind (Ollama accepts a system message only in
    /// first position) and `tail_as_user_messages` vendors (DeepSeek — its
    /// models echo trailing system blocks instead of answering the user;
    /// sentinel-mirror 5cf5469c + the 2027-01-11 exit-note loop). Those vendors
    /// get the volatile tail + CONTEXT_FOOTER as trailing user messages, which
    /// keeps the request ending with a user message while leaving messages[0]
    /// byte-stable — the prefix a prompt cache keys on.
    pub(crate) fn tail_is_user_role(provider: &Arc<dyn LlmClient>) -> bool {
        matches!(
            provider.kind(),
            crate::provider::ProviderKind::Local
        ) || crate::provider::policy::ProviderPolicy::for_kind_and_model(
            provider.kind(),
            provider.model(),
        )
        .tail_as_user_messages
    }

    /// Push a text-only user suggestion as mid-work guidance. The role follows
    /// [`Self::tail_is_user_role`]: on those vendors a trailing system block is
    /// exactly the echo trigger the tail placement avoids (and Ollama rejects it
    /// in that position), so the same text rides a user message instead —
    /// identical mid-work guidance semantics, no trailing system block anywhere
    /// in the request (closes the review 2026-09-11 L2 residual).
    pub(crate) fn push_suggestion_message(
        messages: &mut Vec<Message>,
        provider: &Arc<dyn LlmClient>,
        text: &str,
    ) {
        if Self::tail_is_user_role(provider) {
            messages.push(Message::user_text(format!("User suggestion: {text}")));
        } else {
            messages.push(Message::system(format!("User suggestion: {text}")));
        }
    }

    /// Build and install the system prompt for this iteration: the stable head
    /// (preamble + constitution + session primer + hidden tool-group index) into
    /// messages[0], and the volatile tail (workflow state + recalled memories)
    /// + the byte-stable CONTEXT_FOOTER as the last two messages, popped right
    /// after the request returns. Their ROLE follows
    /// [`Self::tail_is_user_role`]: user for DeepSeek-vendor/Local, system
    /// otherwise — the same two messages either way.
    ///
    /// Neither placement touches messages[0], which is what a provider's prompt
    /// cache keys on: folding the tail into the head (the previous strategy for
    /// DeepSeek/Local) made every `complete_step` progress bump rewrite the
    /// leading message, so the cache died at the head and each check-off
    /// re-billed the whole history (measured 2026-09-14: 5.1-9.0% hit,
    /// 115-120K tokens re-read — .coding/analysis/cache-hit-6-aggregates.txt).
    /// Extracted from run_turn (quality review HIGH 1).
    async fn install_system_messages(
        &self,
        messages: &mut Vec<Message>,
        provider: &Arc<dyn LlmClient>,
        memory_context: Option<&str>,
        tool_filter: &crate::tool::ToolFilter,
    ) {
        // Build the system prompt head + volatile tail. (The workflow
        // tool filter was already read at the top of the iteration —
        // the token accounting consumes it first.)
        // The stable head is cached and rebuilt only when the constitution
        // changes (L4) — re-reads agent.md on mtime change internally.
        let (stable_head, volatile_tail) = {
            let wf = self.workflow.lock().await;
            let stable_head = self.constitution.stable_head();
            let mut volatile_tail = prompt::build_volatile_tail(
                provider.capabilities(),
                &wf,
                memory_context.as_deref(),
            );
            // Eager Complete → Planning nudge: when the agent is resting
            // in Complete state and the user's latest message looks like a
            // code-change task (contains a work verb like fix/add/
            // implement/refactor), append a directive telling the agent to
            // plan instead of answering freeform. The nudge is a
            // suggestion — false positives are harmless (the agent can
            // still answer a question); false negatives fall back to the
            // enhanced STATE_COMPLETE prompt guidance.
            let latest_user_msg = messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| m.content.as_text());
            prompt::append_plan_nudge(
                &mut volatile_tail,
                wf.state(),
                latest_user_msg.as_deref(),
            );
            (stable_head, volatile_tail)
        };

        // Session-start primer: fetch the top strongest semantic+
        // procedural memories ONCE per session and cache them, so the
        // agent starts with standing project knowledge instead of
        // re-discovering it each turn. The primer is appended to the
        // stable head (messages[0]) — it is byte-stable for the whole
        // session, so it does NOT invalidate the provider's prefix cache
        // across turns (unlike the volatile-tail recalled memories).
        //
        // The cache is three-state so the fetch is NOT retried every turn
        // when there are no distilled memories yet or the fetch errored:
        //   None          → never fetched (the first turn fetches)
        //   Some(None)    → fetched, no primer (do not refetch)
        //   Some(Some(s)) → fetched, with primer string s
        let mut head_content = stable_head;
        // Progressive disclosure index: one line per tool group that is
        // NOT in the tools array yet. The browser family is ten schemas
        // (~900 tokens) a typical coding turn never calls; this costs
        // ~60 tokens instead and tells the model exactly how to get them.
        //
        // Placed in the stable head deliberately: it changes only when a
        // group is loaded, which is the same moment the tools array grows
        // — so the one prompt-cache invalidation is paid once per group
        // per session, not per turn.
        let hidden_groups = self.tools.hidden_groups_for(tool_filter);
        if !hidden_groups.is_empty() {
            head_content.push_str(
                "
AVAILABLE TOOL GROUPS — not in your tool list yet. Call                      load_tools(group) to add one, then use its tools:
",
            );
            for (name, summary) in hidden_groups {
                head_content.push_str(&format!(
                    "- {name}: {summary}
"
                ));
            }
        }
        match self.session_primer() {
            Some(Some(primer)) => {
                head_content.push_str("\n");
                head_content.push_str(&primer);
                head_content.push_str("\n");
            }
            Some(None) => {
                // Fetched already, no primer — append nothing, do not refetch.
            }
            None if self.memory.is_some() => {
                let store = self.memory.as_ref().unwrap();
                match store
                    .strongest(&[MemoryTier::Semantic, MemoryTier::Procedural], 8)
                    .await
                {
                    Ok(memories) if !memories.is_empty() => {
                        let primer = prompt::format_primer(&memories);
                        if !primer.trim().is_empty() {
                            head_content.push_str("\n");
                            head_content.push_str(&primer);
                            head_content.push_str("\n");
                        }
                        self.set_session_primer(Some(Some(primer)));
                    }
                    _ => {
                        // No distilled memories yet (or the fetch failed) —
                        // cache Some(None) so we don't retry the fetch every turn.
                        self.set_session_primer(Some(None));
                    }
                }
            }
            None => {
                // No memory store wired — nothing to fetch, nothing to cache.
            }
        }

        // Which ROLE the trailing messages carry (see tail_is_user_role):
        // user for Local (Ollama accepts a system message only in first
        // position) and DeepSeek-vendor (its models echo trailing system
        // blocks instead of answering the user — sentinel-mirror 5cf5469c +
        // the 2027-01-11 exit-note loop). Both classes used to fold the tail
        // into the leading system message instead, which sacrificed that
        // vendor's prefix cache: every `complete_step` progress bump rewrote
        // messages[0] and the provider re-read the whole conversation
        // (measured 2026-09-14: 5.1-9.0% hit, 115-120K tokens per check-off).
        let tail_as_user = Self::tail_is_user_role(provider);

        // Prepend/replace the system message with the STABLE HEAD only
        // (preamble + constitution + session primer). The volatile
        // workflow/memories tail is appended after the conversation history
        // below on EVERY vendor, so the provider's prompt-cache prefix
        // survives complete_step progress bumps and new turns. Nothing
        // vendor-specific belongs in here: this message is the cached prefix.
        if messages.is_empty() || messages[0].role != Role::System {
            messages.insert(0, Message::system(head_content));
        } else {
            messages[0].content = MessageContent::text(head_content);
        }

        // Append the volatile tail (workflow state + recalled memories) AFTER
        // the conversation history, where the provider's prefix cache is
        // immune — so its per-step/per-turn changes never invalidate the
        // cached stable head. Then append the byte-stable CONTEXT_FOOTER as
        // the FINAL message: the provider reuses the request prefix only when
        // the last message is byte-identical to the previous request's
        // (empirical cache law, see .coding/analysis/cache-hit-2-report.md),
        // so a constant last message makes tail changes (complete_step
        // progress, new turns) cost only the tail + footer to re-process
        // instead of the whole cached context.
        //
        // Role per vendor: user for `tail_as_user` (DeepSeek echoes trailing
        // system blocks instead of answering; Ollama accepts a system message
        // only in first position) — the request still ends with a user message
        // the way a normal client turn does — and system for everyone else.
        //
        // Both are transient: pushed here and popped immediately after the
        // request returns, so they never pollute the persistent
        // conversation. The pops run before the `?` so they always execute
        // even on error; this is safe because `complete_with_retry`
        // serializes the messages into the request body and the returned
        // stream borrows `&self` (the provider), not the messages slice.
        if tail_as_user {
            messages.push(Message::user_text(volatile_tail));
            messages.push(Message::user_text(prompt::CONTEXT_FOOTER));
        } else {
            messages.push(Message::system(volatile_tail));
            messages.push(Message::system(prompt::CONTEXT_FOOTER));
        }
    }

    /// Build + fire-and-forget-record one request_stats row (R21). The
    /// model + serving endpoint come from the per-turn provider snapshot
    /// (model()/provider_name() are &str; an empty provider name — unnamed
    /// test mocks, the effective_provider_name empty-skip convention —
    /// becomes None, grouping with pre-endpoint rows). `session_id` is the
    /// turn's authoritative id (the parameter, not loop state — they can
    /// diverge if a caller passes an id without set_session_id). Never
    /// blocks or breaks the turn; a failed durable write is logged, not
    /// swallowed (quality review LOW 6).
    pub(crate) fn record_stats_row(
        &self,
        session_id: Option<&str>,
        provider: &dyn LlmClient,
        row: StatsRow,
    ) {
        let model = provider.model().to_string();
        // The serving endpoint's `endpoints.toml` name. Empty (unnamed test
        // mocks — the effective_provider_name empty-skip convention) becomes
        // None, grouping with pre-endpoint rows.
        let endpoint = {
            let name = provider.provider_name();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        };
        let now_epoch = {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        };
        let stats = crate::memory::RequestStats {
            id: uuid::Uuid::new_v4().to_string(),
            // The turn's authoritative session_id (the parameter) rather
            // than re-reading loop state — they can diverge if a caller
            // passes an id without set_session_id.
            session_id: session_id.map(|s| s.to_string()),
            model,
            endpoint,
            prompt_tokens: row.prompt_tokens,
            completion_tokens: row.completion_tokens,
            reasoning_tokens: row.reasoning_tokens,
            cached_tokens: row.cached_tokens,
            ttft_ms: row.ttft_ms,
            generation_ms: row.generation_ms,
            created_at: now_epoch,
            outcome: row.outcome,
            purpose: row.purpose,
        };
        if let Some(store) = &self.memory {
            // Fire-and-forget — stats recording must never block or break
            // the turn. A failed durable write must not vanish silently —
            // log it (quality review LOW 6).
            let store = Arc::clone(store);
            tokio::spawn(async move {
                if let Err(e) = store.record_request_stats(&stats).await {
                    eprintln!("mnemo: failed to record request stats: {e}");
                }
            });
        }
    }

    /// Issue the provider request (connection establishment + first byte),
    /// interruptibly: an Interrupt/Cancel/Compact/Clear arriving during
    /// this window is caught by a select! that polls cmd_rx (reqwest is
    /// drop-safe); steers are buffered into `buffered_steers` and applied
    /// by the caller after the hard-stop check. Also parks the prep
    /// window on the provider (prep_ms), emits Phase::Waiting, and strips
    /// cross-vendor reasoning. Returns the provider stream plus our
    /// scaffolding-inclusive prompt-token estimate (R21: the error rows'
    /// prompt basis — the caller pops the scaffolding before consume_stream
    /// runs, so the estimate must be computed here). Extracted from
    /// run_turn (quality review HIGH 1); behavior unchanged.
    async fn request_stream<'a>(
        &self,
        messages: &mut Vec<Message>,
        provider: &'a Arc<dyn LlmClient>,
        tool_schemas: &[crate::provider::ToolSchema],
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        session_id: Option<&str>,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        state: &mut TurnState,
        buffered_steers: &mut Vec<crate::runtime::SteerPayload>,
        prep_started: std::time::Instant,
    ) -> Result<
        (futures::stream::BoxStream<'a, LlmEvent>, u32),
        crate::error::Error,
    > {
        // The provider request (connection establishment + first byte) is
        // interruptible: an Interrupt/Cancel arriving during this window is
        // caught by a select! that polls cmd_rx. reqwest is drop-safe (the
        // connection is dropped), so dropping the pinned future on an
        // interrupt is safe. A Steer is buffered locally and re-applied to
        // stop_reason after the request completes (so the turn ends at the
        // next break point — the stream hasn't started yet).
        //
        // The future borrows `messages` immutably, so it's scoped in a
        // block — dropped before the `messages.pop()` calls below.
        // Park the local prep window (loop top → here, including any
        // auto-compaction above) on the provider; the request's trace
        // record gets stamped with it as `prep_ms`. Deliberately excludes
        // the POST + retries (that's `connect_ms`).
        provider.record_prep_ms(prep_started.elapsed().as_millis() as u32);
        // We have handed off to the network — TCP/TLS connect, POST upload,
        // time-to-first-token, AND any retry backoff sleeps are all
        // "waiting" (the user sees "waiting for response…"), not "sending"
        // (which is local prep only, already measured as prep_ms above).
        // Emitted HERE — before complete_with_retry — so a 30s connect
        // timeout or a 1s/2s backoff sleep shows as "waiting", not a
        // multi-second "sending" stall (user report 2026-12-04).
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::Phase {
                    phase: PhaseKind::Waiting,
                },
            ))
            .await;
        // Rule 5: strip reasoning from cross-vendor assistant turns before
        // the request builder echoes raw. Same-vendor turns are left
        // unchanged (the backend handles compatibility); cross-vendor turns
        // have reasoning removed and reasoning_stripped set.
        crate::provider::strip_cross_vendor_reasoning(
            messages,
            provider.kind(),
            provider.model(),
        );
        // R21: our scaffolding-inclusive prompt-token estimate — the error
        // rows' prompt basis (the provider never reports usage on a failed
        // request). Computed here because the caller pops the scaffolding
        // before consume_stream runs. Accepted cost (round-1 review LOW 3):
        // estimate_prompt_tokens walks the full history (~1-3ms + one String
        // alloc per message on very long conversations) on EVERY request,
        // success or failure — the L3 serialized-prefix cache does not cover
        // this path. The lazy alternative (a closure consumed only by the
        // Error arm) is impossible: the closure would hold a shared borrow of
        // `messages` across the caller's scaffolding pops, which need a
        // unique borrow.
        let estimated_prompt_tokens =
            crate::provider::estimate_prompt_tokens(messages, tool_schemas) as u32;
        let stream_result = {
            let complete_fut = self.complete_with_retry(
                &provider,
                messages,
                &tool_schemas,
                fanin_tx,
                agent_id,
                session_id,
            );
            tokio::pin!(complete_fut);
            loop {
                tokio::select! {
                    result = &mut complete_fut => break result,
                    cmd = cmd_rx.recv() => match cmd {
                        Some(AgentCommand::Interrupt) => {
                            // Preserve any steers buffered so far across
                            // the interrupt — the user pressed Stop with
                            // commands queued; those commands must run
                            // immediately after the stop, not be dropped.
                            let steers = std::mem::take(buffered_steers);
                            state.stop_reason = Some(if steers.is_empty() {
                                StopReason::Interrupt
                            } else {
                                StopReason::InterruptWithSteers(steers)
                            });
                            break Err(crate::error::Error::Provider(
                                "interrupted during provider request".into(),
                            ));
                        }
                        Some(AgentCommand::Cancel) => {
                            state.stop_reason = Some(StopReason::Cancel);
                            break Err(crate::error::Error::Provider(
                                "cancelled during provider request".into(),
                            ));
                        }
                        Some(AgentCommand::Compact) => {
                            // Preserve any steers buffered so far across
                            // the compaction (same dropped-command bug
                            // class as Stop) — they run on the compacted
                            // conversation afterwards.
                            let steers = std::mem::take(buffered_steers);
                            state.stop_reason = Some(if steers.is_empty() {
                                StopReason::Compact
                            } else {
                                StopReason::CompactWithSteers(steers)
                            });
                            break Err(crate::error::Error::Provider(
                                "compact requested during provider request".into(),
                            ));
                        }
                        Some(AgentCommand::Clear) => {
                            state.stop_reason = Some(StopReason::Clear);
                            break Err(crate::error::Error::Provider(
                                "clear requested during provider request".into(),
                            ));
                        }
                        Some(AgentCommand::Suggestion(p)) => {
                            buffered_steers.push(p);
                        }
                        Some(AgentCommand::Prompt { text, images }) => {
                            // A Prompt folded during the provider request
                            // rides the steer pipeline — its images ride
                            // along too.
                            buffered_steers.push(crate::runtime::SteerPayload {
                                text,
                                images,
                            });
                        }
                        Some(AgentCommand::CancelSuggestion(s)) => {
                            // The "x" on a pending steer: drop a matching
                            // buffered steer so it isn't injected after the
                            // provider request resolves. Text-match (the
                            // documented limitation).
                            buffered_steers.retain(|p| p.text != s);
                        }
                        None => {
                            break Err(crate::error::Error::Provider(
                                "command channel closed during provider request".into(),
                            ));
                        }
                    }
                }
            }
        };
        stream_result.map(|stream| (stream, estimated_prompt_tokens))
    }

    /// Consume the provider stream: accumulate text/reasoning deltas and
    /// tool calls, forward view events to the fan-in channel, record
    /// request stats + re-anchor the context-usage bar on Usage events,
    /// and fold commands arriving mid-stream into the stop reason (a
    /// hard stop cuts the stream short; steers accumulate). Returns the
    /// accumulated outcome. Extracted from run_turn (quality review
    /// HIGH 1); behavior unchanged.
    async fn consume_stream(
        &self,
        stream: futures::stream::BoxStream<'_, LlmEvent>,
        provider: &Arc<dyn LlmClient>,
        context_manager: &context::ContextManager,
        state: &mut TurnState,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        session_id: Option<&str>,
        estimated_prompt_tokens: u32,
        breakdown: &crate::runtime::ContextBreakdown,
    ) -> StreamOutcome {
        let mut acc = DeltaAccumulator::new();
        let mut text = String::new();
        // The model's reasoning/thinking text this turn (DeepSeek
        // thinking mode). Accumulated alongside `text` so the stored
        // assistant Message can carry it — DeepSeek requires it echoed
        // back on the next request or it rejects with HTTP 400.
        let mut reasoning_text = String::new();
        let mut finish_reason = FinishReason::Stop;
        let mut had_error = false;
        // R21: whether this stream already recorded a success row — a
        // Usage-then-Error stream (the connection dies after the usage chunk)
        // must not produce BOTH a success row and an error row for one
        // request (round-1 review LOW 6).
        let mut usage_recorded = false;
        // The error string from a mid-stream `LlmEvent::Error` (e.g. a
        // connection reset / unexpected EOF). Held rather than emitted
        // immediately so the no-partial-output path below can return it as
        // an `Err` — which lets the outer `run_turn_with_retry` layer retry
        // the whole turn with backoff (the same machinery that handles
        // connection-establishment failures). When partial output *was*
        // received, the error is swallowed in favor of the usable content.
        let mut stream_error: Option<String> = None;
        // The stream phase tracks WHICH kind of delta is arriving:
        // reasoning deltas flip the inflight bar from "waiting" to
        // "reasoning", answer/tool-call deltas to "answering" (the
        // streaming phase). Emitted on change — an interleaved
        // thinking+answering stream (Anthropic) stays truthful.
        let mut stream_phase: Option<PhaseKind> = None;
        // A mid-stream stall (connection open, nothing arriving)
        // deliberately does NOT flip the phase back to Waiting: the
        // trace graphs count stall_ms as part of the generate window
        // (decision f2b62d85 — stall is byte-silence inside
        // generation), so the live status bar matches and keeps
        // showing "reasoning…"/"answering…" through the silence.
        // Stall stays visible post-hoc as the red hatch overlay in
        // the trace graphs (StallTracker, 2 s threshold,
        // provider/mod.rs).

        tokio::pin!(stream);
        loop {
            tokio::select! {
                // Stream events from the LLM.
                event_opt = futures::StreamExt::next(&mut stream) => {
                    let Some(event) = event_opt else { break; };
                    // Feed the event to the accumulator first (before the match
                    // moves any fields out of it).
                    acc.feed(&event);
                    match event {
                        LlmEvent::TextDelta { text: t } => {
                            if stream_phase != Some(PhaseKind::Streaming) {
                                stream_phase = Some(PhaseKind::Streaming);
                                let _ = fanin_tx
                                    .send((
                                        agent_id,
                                        AgentEvent::Phase {
                                            phase: PhaseKind::Streaming,
                                        },
                                    ))
                                    .await;
                            }
                            text.push_str(&t);
                            let _ = fanin_tx
                                .send((agent_id, AgentEvent::TextDelta(t)))
                                .await;
                        }
                        LlmEvent::ReasoningDelta { text: t } => {
                            if stream_phase != Some(PhaseKind::Reasoning) {
                                stream_phase = Some(PhaseKind::Reasoning);
                                let _ = fanin_tx
                                    .send((
                                        agent_id,
                                        AgentEvent::Phase {
                                            phase: PhaseKind::Reasoning,
                                        },
                                    ))
                                    .await;
                            }
                            reasoning_text.push_str(&t);
                            let _ = fanin_tx
                                .send((agent_id, AgentEvent::ReasoningDelta(t)))
                                .await;
                        }
                        LlmEvent::ToolCallStart { index, id, name } => {
                            if stream_phase != Some(PhaseKind::Streaming) {
                                stream_phase = Some(PhaseKind::Streaming);
                                let _ = fanin_tx
                                    .send((
                                        agent_id,
                                        AgentEvent::Phase {
                                            phase: PhaseKind::Streaming,
                                        },
                                    ))
                                    .await;
                            }
                            let _ = fanin_tx
                                .send((
                                    agent_id,
                                    AgentEvent::ToolCallStart { index, id, name },
                                ))
                                .await;
                        }
                        LlmEvent::ToolCallArgumentDelta { index, fragment } => {
                            if stream_phase != Some(PhaseKind::Streaming) {
                                stream_phase = Some(PhaseKind::Streaming);
                                let _ = fanin_tx
                                    .send((
                                        agent_id,
                                        AgentEvent::Phase {
                                            phase: PhaseKind::Streaming,
                                        },
                                    ))
                                    .await;
                            }
                            let _ = fanin_tx
                                .send((
                                    agent_id,
                                    AgentEvent::ToolCallArgDelta { index, fragment },
                                ))
                                .await;
                        }
                        LlmEvent::Finish { reason } => {
                            finish_reason = reason;
                        }
                        LlmEvent::Usage {
                            prompt_tokens,
                            completion_tokens,
                            reasoning_tokens,
                            cached_tokens,
                            ttft_ms,
                            generation_ms,
                        } => {
                            // Record request stats to the memory store
                            // (.coding/memory.db). The provider's reported
                            // cached_tokens is trusted as-is: 0 means
                            // nothing was cached. The old min(prev,curr)
                            // heuristic masked real cache misses (e.g. a
                            // proxy's cache-size limit).
                            let effective_cached = cached_tokens;
                            self.record_stats_row(
                                session_id,
                                provider.as_ref(),
                                StatsRow {
                                    prompt_tokens,
                                    completion_tokens,
                                    reasoning_tokens,
                                    cached_tokens: Some(effective_cached),
                                    ttft_ms,
                                    generation_ms,
                                    outcome: None,
                                    purpose: None,
                                },
                            );
                            usage_recorded = true;
                            let _ = fanin_tx
                                .send((agent_id, AgentEvent::Usage {
                                    prompt_tokens,
                                    completion_tokens,
                                    reasoning_tokens,
                                    cached_tokens: effective_cached,
                                    ttft_ms,
                                    generation_ms,
                                }))
                                .await;
                            // Re-anchor the ctx bar to the provider's own
                            // count — the exact wire basis (system +
                            // tools + full history as billed). The
                            // top-of-loop emission is the local estimate;
                            // this lands right after each response so the
                            // bar converges to the provider-exact number
                            // (regression
                            // `context_usage_reanchors_to_provider_prompt_tokens`,
                            // backlog da4fc87d). 0 is a bogus/unknown
                            // count — keep the local estimate instead of
                            // collapsing the bar.
                            if prompt_tokens > 0 {
                                let _ = fanin_tx
                                    .send((agent_id, AgentEvent::ContextUsage {
                                        used: prompt_tokens,
                                        max: context_manager.max_tokens() as u32,
                                        breakdown: breakdown.clone(),
                                    }))
                                    .await;
                            }
                        }
                        LlmEvent::Error { error } => {
                            // R21: a mid-stream failure re-sent the full
                            // prompt — record it once per stream (the arm
                            // can fire repeatedly if the provider emits
                            // multiple Error events), and never when a
                            // success row already landed for this stream
                            // (Usage-then-Error would double-count the
                            // request — round-1 review LOW 6). Same shape
                            // as the complete_with_retry error rows: our
                            // estimated prompt tokens, cached_tokens NULL
                            // (the provider never reported usage), no
                            // timing.
                            if !had_error && !usage_recorded {
                                self.record_stats_row(
                                    session_id,
                                    provider.as_ref(),
                                    StatsRow {
                                        prompt_tokens: estimated_prompt_tokens,
                                        completion_tokens: 0,
                                        reasoning_tokens: 0,
                                        cached_tokens: None,
                                        ttft_ms: None,
                                        generation_ms: None,
                                        outcome: Some("error".into()),
                                        purpose: None,
                                    },
                                );
                            }
                            had_error = true;
                            stream_error = Some(error);
                        }
                        LlmEvent::ProviderMeta { .. } => {
                            // Already fed to the accumulator via acc.feed
                            // above — the accumulator stores the metadata
                            // for later round-trip into the request body.
                            // No turn-loop action needed here.
                        }
                        LlmEvent::RawAssistantDelta { .. } => {
                            // Already fed to the accumulator via acc.feed
                            // above — the raw delta is the verbatim source
                            // of truth for replay (Rule 1). No turn-loop
                            // action; the UI reads the derived view events.
                        }
                        LlmEvent::ResponseId { .. } => {
                            // Already fed to the accumulator via acc.feed
                            // above — the server-assigned response id is
                            // captured for the next request's
                            // previous_response_id (Rule 3 stateful path).
                        }
                    }
                }
                // Check for interrupts/suggestions while streaming.
                cmd_opt = cmd_rx.recv() => {
                    match cmd_opt {
                        Some(cmd @ (AgentCommand::Interrupt
                            | AgentCommand::Cancel
                            | AgentCommand::Compact
                            | AgentCommand::Clear)) => {
                            // A hard stop signal. `fold` preserves any
                            // buffered steers across an Interrupt (it
                            // becomes InterruptWithSteers so the queued
                            // commands still run after the stop), then we
                            // break the stream.
                            StopReason::fold(&mut state.stop_reason, cmd);
                            // D1: the stream is about to be dropped
                            // mid-flight (the pump's next send fails and the
                            // provider stamps the record cancelled). The
                            // provider billed the tokens generated so far —
                            // record a cancelled stats row so aborted
                            // requests stay countable in the latency/token
                            // aggregates (cached_tokens NULL — no usage was
                            // reported). Skip when a success or error row
                            // already landed for this stream (Usage-then-cancel
                            // or Error-then-interrupt — the Error arm does not
                            // break the loop, so a queued hard-stop can win the
                            // next select iteration and must not double-record
                            // the request as cancelled).
                            if !usage_recorded && !had_error {
                                self.record_stats_row(
                                    session_id,
                                    provider.as_ref(),
                                    StatsRow {
                                        prompt_tokens: estimated_prompt_tokens,
                                        completion_tokens: 0,
                                        reasoning_tokens: 0,
                                        cached_tokens: None,
                                        ttft_ms: None,
                                        generation_ms: None,
                                        outcome: Some("cancelled".into()),
                                        purpose: None,
                                    },
                                );
                            }
                            break;
                        }
                        Some(cmd) => {
                            // A steer or new prompt mid-stream: soft-stop
                            // signal. `fold` ACCUMULATES it into the
                            // pending-steer list (never dropped) — the
                            // post-loop block ends the turn at this break
                            // point and every queued command drives the
                            // follow-up turn in order.
                            StopReason::fold(&mut state.stop_reason, cmd);
                        }
                        None => { break; }
                    }
                }
            }
        }

        StreamOutcome {
            acc,
            text,
            reasoning_text,
            finish_reason,
            had_error,
            stream_error,
        }
    }

    /// Record a hard-stopped partial turn: when a hard stop (Interrupt,
    /// Cancel, Compact, Clear) arrived during streaming, keep the partial
    /// output — push the assistant record (tool calls sanitized when
    /// the stream was cut mid-JSON), synthesize "not run" results for
    /// the announced calls, emit Finished, and return the outcome.
    /// Returns `None` when no hard stop is pending (the turn continues
    /// normally). Extracted from run_turn (quality review HIGH 1);
    /// behavior unchanged.
    async fn record_interrupted_output(
        &self,
        state: &mut TurnState,
        messages: &mut Vec<Message>,
        acc: &mut DeltaAccumulator,
        text: &str,
        assistant_reasoning: &Option<String>,
        provider: &Arc<dyn LlmClient>,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
    ) -> Option<TurnOutcome> {
        // A HARD stop signal arrived during streaming (Interrupt, Cancel,
        // Compact, Clear — with or without carried steers). End the turn
        // at this break point (the current stream chunk completed — we're
        // past the select! loop), keeping partial output. The stop_reason
        // is carried back via TurnOutcome so AgentTask acts on it: Cancel
        // terminates the agent (Exited), Interrupt returns to idle.
        //
        // A bare Steer is deliberately NOT handled here: it falls through
        // to the tool loop below so the batch the model just emitted
        // DRAINS (executes with real results) and the steer drives the
        // follow-up turn afterwards — the same outcome as a steer arriving
        // a moment later, after stream-end (the between-call safe point
        // only breaks for hard stops). Ending the turn here for a steer
        // made execution nondeterministic: sub-second timing decided
        // whether the batch ran at all (2026-12-30 bug: interrupted turns
        // silently dropped in-flight tool calls).
        let hard_stop = state.stop_reason
            .as_ref()
            .is_some_and(StopReason::is_hard_stop);
        if hard_stop {
            // Use trim() (not just is_empty()) so whitespace-only partial
            // output (a leading space/newline is common) is NOT pushed as a
            // whitespace-only assistant message — validate_request_messages
            // rejects those on `s.trim().is_empty()`, the same class of bug
            // the null-turn fix targets. Mirrors the root-cause fix below.
            //
            // 2026-12-30 bug (interrupted turns silently drop in-flight
            // tool calls): the cancelled calls must be recorded in HISTORY,
            // not just the UI — the model's next turn must see what it
            // announced and that it did not run (it cannot re-issue what
            // its context never recorded), and a raw echo carrying
            // tool_calls without matching results can even 400 on the next
            // request. So the assistant record below carries the
            // accumulated tool_calls — args sanitized when the stream was
            // cut mid-JSON (same rule as the bad-JSON path: echoing the
            // verbatim raw would resend the malformed arguments the
            // sanitization just fixed) — and one "interrupted: not run"
            // tool_result message per call id follows it, the same
            // structure the tool loop's own stop paths produce.
            let msg_meta = acc.take_message_provider_meta();
            let raw = acc.take_raw();
            let response_id = acc.take_response_id();
            let orphaned = std::mem::take(acc).finalize();
            let any_bad_args = orphaned
                .iter()
                .any(|tc| serde_json::from_str::<serde_json::Value>(&tc.arguments).is_err());
            let sanitized_calls: Vec<ToolCall> = orphaned
                .iter()
                .map(|tc| {
                    let args =
                        if serde_json::from_str::<serde_json::Value>(&tc.arguments).is_ok() {
                            tc.arguments.clone()
                        } else {
                            "{}".to_string()
                        };
                    ToolCall {
                        provider_meta: tc.provider_meta.clone(),
                        ..ToolCall::new(tc.id.clone(), tc.name.clone(), args)
                    }
                })
                .collect();
            if !text.trim().is_empty() || !sanitized_calls.is_empty() {
                // Whitespace-only text is never recorded (see trim() note
                // above); empty text alongside tool_calls is valid.
                let record_text = if text.trim().is_empty() {
                    String::new()
                } else {
                    text.to_string()
                };
                messages.push(Message {
                    reasoning_content: assistant_reasoning.clone(),
                    provider_meta: msg_meta,
                    raw: if any_bad_args { None } else { raw },
                    response_id,
                    origin_provider: Some(provider.kind().as_str().to_string()),
                    origin_model: Some(provider.model().to_string()),
                    ..Message::assistant(record_text, sanitized_calls)
                });
                for tc in &orphaned {
                    messages.push(Message::tool_result(
                        tc.id.clone(),
                        tc.name.clone(),
                        "interrupted: not run (turn stopped)",
                    ));
                }
            }
            // Backlog 63cbc20f: a tool call the model already announced
            // (ToolCallStart was forwarded mid-stream) was cut by this stop
            // and never runs — without a result its card spun "running"
            // forever. Give the UI a terminal synthetic result, mirroring
            // the tool loop's own not-run synthesis below.
            for tc in &orphaned {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::ToolResult {
                            tool_call_id: tc.id.clone(),
                            result: ToolResult::error("interrupted: not run (turn stopped)"),
                        },
                    ))
                    .await;
            }
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Finished {
                        reason: FinishReason::Stop,
                    },
                ))
                .await;
            return Some(TurnOutcome {
                finish_reason: FinishReason::Stop,
                text: text.to_string(),
                tool_calls_made: 0,
                stop_reason: state.stop_reason.take(),
            });
        }

        None
    }

    /// Handle malformed tool-call arguments (bad JSON): count the strike
    /// against MAX_BAD_JSON_RETRIES, and either abort the turn (Some
    /// outcome) or sanitize + re-inject the assistant message with
    /// per-call error results so the model retries with valid JSON
    /// (None — the caller continues the request loop). A pending
    /// steer short-circuits the retry (Some outcome) so the user's
    /// message is not delayed behind a retry. Extracted from run_turn
    /// (quality review HIGH 1); behavior unchanged.
    async fn handle_bad_json(
        &self,
        state: &mut TurnState,
        tool_calls: &[ToolCall],
        messages: &mut Vec<Message>,
        finish_reason: &FinishReason,
        text: &str,
        assistant_reasoning: &Option<String>,
        msg_meta: &Option<serde_json::Map<String, serde_json::Value>>,
        response_id: &mut Option<String>,
        provider: &Arc<dyn LlmClient>,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
    ) -> Option<TurnOutcome> {
        // Bad JSON is an LLM output issue the model can recover from by
        // emitting valid JSON — it does NOT count toward MAX_RETRIES
        // (tool-execution failures). It uses the higher
        // MAX_BAD_JSON_RETRIES cap so three strikes doesn't stop a
        // model that can self-correct.
        state.bad_json_count += 1;
        // Failure triage (Laya, opt-in; plan 02deea7c): a confident
        // `permanent` reading of the malformed-arguments failure aborts the
        // repair loop EARLY — the model roundtrip IS the bad-JSON retry, so
        // no harness auto-retry exists here; the classification only decides
        // whether more repair attempts are worth their roundtrips. Every
        // other class, and any fallback, keeps today's ladder byte-identical.
        // One row per strike, resolved immediately with `escalated` (the
        // abort skips any further repair attempt — off-policy for the
        // trainer, same as the provider skip).
        if let Some(gate) = &self.failure_triage {
            let error_text = match tool_calls.first() {
                Some(tc) => format!(
                    "tool-call arguments failed to parse as JSON (finish reason \
                     {finish_reason:?}): {}",
                    tc.arguments
                ),
                None => format!(
                    "tool-call arguments failed to parse as JSON (finish reason \
                     {finish_reason:?})"
                ),
            };
            if let failure_triage::FailureTriage::Classified {
                class: failure_triage::FailureClass::Permanent,
                confidence,
            } = gate.triage(&error_text).await
            {
                let pending = gate.log_failure(
                    failure_triage::FailureSite::BadJsonRepair,
                    tool_calls.first().map(|tc| tc.name.as_str()),
                    &error_text,
                    failure_triage::FailureClass::Permanent,
                    confidence,
                    failure_triage::TriageAction::AbortEarly,
                );
                failure_triage::resolve_failure(
                    pending,
                    failure_triage::TriageDisposition::Escalated,
                );
                // UI-only (backlog 63cbc20f): end every announced card
                // before the final error — same discipline as the cap arm
                // below.
                for tc in tool_calls {
                    let _ = fanin_tx
                        .send((
                            agent_id,
                            AgentEvent::ToolResult {
                                tool_call_id: tc.id.clone(),
                                result: ToolResult::error(
                                    "arguments malformed or truncated — not run",
                                ),
                            },
                        ))
                        .await;
                }
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::Error {
                            error: format!(
                                "Aborted after {} malformed-arguments failures — \
                                 classified permanent: repair attempts cannot help \
                                 ({}).",
                                state.bad_json_count,
                                failure_triage::skip_hint(
                                    failure_triage::FailureClass::Permanent
                                )
                            ),
                            retrying: false,
                        },
                    ))
                    .await;
                return Some(TurnOutcome {
                    finish_reason: FinishReason::Stop,
                    text: text.to_string(),
                    tool_calls_made: 0,
                    stop_reason: state.stop_reason.take(),
                });
            }
        }
        if state.bad_json_count >= MAX_BAD_JSON_RETRIES {
            // UI-only (backlog 63cbc20f): end every announced card
            // before the final error — the whole batch is discarded
            // from history here, so the cards must be closed by
            // event. Counts toward the frontend doom streak by
            // design (review F3 — see the retry arm's note).
            for tc in tool_calls {
                let _ = fanin_tx
                    .send((
                        agent_id,
                        AgentEvent::ToolResult {
                            tool_call_id: tc.id.clone(),
                            result: ToolResult::error(
                                "arguments malformed or truncated — not run",
                            ),
                        },
                    ))
                    .await;
            }
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::Error {
                        error: format!(
                            "Aborted: tool-call arguments kept failing to parse as JSON \
                         after {MAX_BAD_JSON_RETRIES} retries."
                        ),
                        retrying: false,
                    },
                ))
                .await;
            return Some(TurnOutcome {
                finish_reason: FinishReason::Stop,
                text: text.to_string(),
                tool_calls_made: 0,
                // A bare Steer can be pending here (it falls through
                // the hard-stop gate above). Hardcode `None` and the
                // steer — the user's message — is silently dropped.
                stop_reason: state.stop_reason.take(),
            });
        }
        let reason = if *finish_reason == FinishReason::Length {
            "Tool call was truncated (hit the token limit). Retrying."
        } else {
            "Tool call had malformed JSON arguments. Retrying."
        };
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::Error {
                    error: reason.into(),
                    retrying: true,
                },
            ))
            .await;
        // Re-inject the assistant message so the model sees what it
        // tried, but SANITIZE the arguments: replace any malformed
        // JSON string with "{}". Without this, the broken arguments
        // are serialized into the next request body and the gateway
        // rejects it (400 "Unterminated string") when it parses the
        // `arguments` field. The tool error messages below tell the
        // model to retry with valid JSON.
        let sanitized_calls: Vec<ToolCall> = tool_calls
            .iter()
            .map(|tc| {
                let args =
                    if serde_json::from_str::<serde_json::Value>(&tc.arguments).is_ok() {
                        tc.arguments.clone()
                    } else {
                        "{}".to_string()
                    };
                ToolCall {
                    provider_meta: tc.provider_meta.clone(),
                    ..ToolCall::new(tc.id.clone(), tc.name.clone(), args)
                }
            })
            .collect();
        messages.push(Message {
            reasoning_content: assistant_reasoning.clone(),
            provider_meta: msg_meta.clone(),
            // H2: clear raw — the sanitized tool_calls are in the
            // structured fields; echoing the verbatim raw would resend
            // the malformed arguments the sanitization just fixed.
            raw: None,
        response_id: response_id.take(),
            origin_provider: Some(provider.kind().as_str().to_string()),
            origin_model: Some(provider.model().to_string()),
            ..Message::assistant(text, sanitized_calls)
        });
        for tc in tool_calls {
            // Strategy-changing guidance on a REPEAT (backlog 38040f12):
            // a bad-JSON failure is usually an EMISSION problem, and the
            // generic "re-read the schema" advice below does not address
            // it — so on a repeat of the same (tool, error) signature the
            // model is steered toward a DIFFERENT FORMULATION instead of
            // being told the same thing again. Live incident (plan
            // 263a9e31): a backslash-dense `search` pattern failed to
            // emit FIVE times in a row; every retry carried identical
            // guidance, so the loop could not self-break. The scan runs
            // BEFORE this result is pushed (same ordering rule as the
            // tool-execution circuit breaker), so a match is always
            // against a PRIOR failure.
            let failed_content = bad_json_retry_message();
            let guidance = if repeated_tool_failure(messages, &tc.name, &failed_content) {
                repeated_bad_json_correction(&tc.name, &failed_content)
            } else {
                failed_content
            };
            messages.push(Message::tool_result(
                tc.id.clone(),
                tc.name.clone(),
                guidance,
            ));
        }
        // UI-only (backlog 63cbc20f): end the announced card — the
        // turn keeps going (this is a retry, not a stop), so no later
        // terminal event would rescue it otherwise. History already
        // carries the model-facing error pushed above. These
        // synthetic results intentionally COUNT toward the frontend
        // doom streak (a failed call the model must fix; the paired
        // retrying error event already does) and are invisible to
        // the backend's MAX_RETRIES exclusion (review F3, 2026-09-18).
        for tc in tool_calls {
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::ToolResult {
                        tool_call_id: tc.id.clone(),
                        result: ToolResult::error(
                            "arguments malformed or truncated — not run; retrying",
                        ),
                    },
                ))
                .await;
        }
        if state.stop_reason.is_some() {
            // A steer is pending — do NOT start another stream: the
            // user's message must not wait behind a retry the user
            // never asked for. The re-injected assistant message +
            // per-call error results above are already in history, so
            // the steer-driven follow-up turn sees them and the model
            // retries there.
            let _ = fanin_tx
                .send((agent_id, AgentEvent::Finished {
                    reason: FinishReason::Stop,
                }))
                .await;
            return Some(TurnOutcome {
                finish_reason: FinishReason::Stop,
                text: text.to_string(),
                tool_calls_made: 0,
                stop_reason: state.stop_reason.take(),
            });
        }

        None
    }
}

/// Synthesize "interrupted: not run (turn stopped)" results for tool calls
/// that were announced but will never run (a hard stop cut the turn before
/// their execution): pushes a tool-result message per call AND emits the
/// terminal ToolResult event per call, so the conversation has N results
/// for N calls and no UI card spins "running" forever (backlog 63cbc20f).
async fn synthesize_not_run_results(
    messages: &mut Vec<Message>,
    fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
    agent_id: AgentId,
    calls: &[ToolCall],
) {
    for tc in calls {
        let not_run = "interrupted: not run (turn stopped)";
        messages.push(Message::tool_result(tc.id.clone(), tc.name.clone(), not_run));
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::ToolResult {
                    tool_call_id: tc.id.clone(),
                    result: ToolResult::error(not_run),
                },
            ))
            .await;
    }
}

/// Repair assistant messages with empty content (and no tool calls) that are
/// already in the persistent history. Several providers reject an empty
/// assistant turn ("messages[N] is an assistant message with no content and no
/// tool calls"), which `validate_request_messages` catches as a local error —
/// but that error is unrecoverable while the empty message stays in history, so
/// every retry re-sends it and re-fails identically, wedging the session until
/// restart. This substitutes a minimal non-empty placeholder so an already-
/// corrupted session recovers on the next turn. Called once at the top of
/// `run_turn`. Cheap: linear scan, allocates only when a repair is needed.
fn repair_empty_assistant_messages(messages: &mut [Message]) {
    for m in messages.iter_mut() {
        if m.role == Role::Assistant
            && m.tool_calls.is_empty()
            && match &m.content {
                MessageContent::Text(s) => s.trim().is_empty(),
                MessageContent::Parts(parts) => parts.is_empty(),
            }
        {
            m.content = MessageContent::text("(no output)");
        }
    }
}

/// Whether a tool-result error string is a user denial / interrupt rather than
/// a model/tool failure. Used so DenyAll batches and interrupt/cancel stops do
/// not trip `MAX_RETRIES`.
pub(crate) fn is_user_denial_tool_output(output: &str) -> bool {
    output.contains("user denied")
        || output.contains("interrupted while awaiting approval")
        || output.contains("cancelled while awaiting approval")
        || output.contains("approval channel closed")
        || output.contains("interrupted: not run")
}

/// Whether a failed tool result repeats a PRIOR identical failure of the
/// same tool — the signature of the self-reinforcing malformed-call loop
/// (live incident: 15 consecutive `git` commit calls missing `message`,
/// re-emitted verbatim while the error text repeated identically; the
/// loop was only broken by a fresh corrective user turn). Scans the recent
/// window of the conversation history; the caller runs this BEFORE
/// pushing the current result, so a match is always against a prior
/// failure — across turn boundaries too, since `messages` is the
/// session history. Compacted tool results no longer carry the verbatim
/// error text, so matches decay naturally at the compaction boundary.
fn repeated_tool_failure(messages: &[Message], tool_name: &str, failed_content: &str) -> bool {
    messages
        .iter()
        .rev()
        .take(REPEAT_FAILURE_SCAN_WINDOW)
        .any(|m| {
            m.role == Role::Tool
                && m.name.as_deref() == Some(tool_name)
                && match &m.content {
                    MessageContent::Text(s) => s == failed_content,
                    MessageContent::Parts(_) => false,
                }
        })
}

/// The auto-recall query: the latest User-role message text, skipping
/// harness-attributed corrective messages (repeat-failure circuit
/// breaker) so a correction never hijacks the recall query for the
/// remainder of the turn.
pub(crate) fn last_user_query(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| {
            m.role == Role::User
                && !m.content.as_text().starts_with(HARNESS_CORRECTION_PREFIX)
        })
        .map(|m| m.content.as_text())
}

/// The per-call bad-JSON retry guidance (the FIRST failure): the model is
/// told the arguments did not parse, that an empty argument object is a
/// mistake, and to re-read the schema and re-emit the complete call. This
/// is the right advice for the dropped-required-field class (plan
/// 4fa222cc); it is deliberately NOT used on a repeat, where the problem
/// is the emission itself (see [`repeated_bad_json_correction`]).
fn bad_json_retry_message() -> String {
    "error: arguments JSON was malformed or truncated — \
     please retry with valid, complete JSON. An empty \
     argument object is a mistake (except genuine no-arg \
     tools like current_plan/backlog_list): re-read the \
     tool's schema, rewrite the COMPLETE call with every \
     required field present and non-blank — content \
     first, never emit a call to discover fields — and \
     emit the corrected call once; never resend the \
     broken call unchanged. For large file writes, split \
     the content into smaller chunks or use multiple \
     file_edit calls."
        .to_string()
}

/// Build the strategy-changing guidance for a REPEATED bad-JSON failure
/// (backlog 38040f12). The first-failure message above assumes the model
/// dropped a required field; when the SAME call fails to parse twice, the
/// likely cause is the argument BODY — a pattern or string that will not
/// survive JSON escaping. Telling the model to "re-read the schema" again
/// is useless there (it already knows the schema), which is exactly how
/// the live loop sustained five identical retries (plan 263a9e31).
///
/// So this message changes STRATEGY: it names a different formulation for
/// the specific tool when the harness knows one, and otherwise directs a
/// rebuild-from-scratch of the argument body. Harness-attributed so the
/// transcript never reads it as human input.
fn repeated_bad_json_correction(tool_name: &str, failed_content: &str) -> String {
    let remedy = match tool_name {
        // The live case: a regex with backslashes is exactly the payload
        // that breaks JSON escaping, and `literal` sidesteps the escaping
        // entirely — no backslashes needed for a plain-text query.
        "search" | "search_read" => {
            "This is usually an ESCAPING problem, not a schema problem — a \
             pattern containing backslashes or quotes is hard to emit as \
             JSON. Do NOT resend it. Instead, in priority order: (1) pass \
             the same text with `literal: true`, which needs no regex \
             escaping — e.g. pattern \"(?<\" with literal true; (2) use a \
             metacharacter-free pattern — for a literal `(?<`, write the \
             character class [(][?][<]; (3) search a single \
             distinctive substring instead of the full pattern, or narrow \
             with `glob` instead."
                .to_string()
        }
        // A large body (a file's contents) is the other classic
        // unparseable-arguments cause: the emission is truncated.
        "file_write" | "file_edit" | "multi_edit" => {
            "This is usually an EMISSION problem — the argument body is too \
             large or too escape-heavy to come through intact. Do NOT \
             resend it. Instead: write the file in smaller chunks (one \
             file_write per section, or several file_edit calls), and avoid \
             embedding large blocks of quotes/backslashes in a single \
             argument."
                .to_string()
        }
        _ => {
            "Re-reading the schema will not help — you already know it, and \
             the same call has now failed to parse twice. Change the SHAPE \
             of the call instead: build the argument object from scratch \
             (do not edit the previous one), keep string values short and \
             free of backslashes/quotes where possible, and emit only the \
             required fields plus what you actually need."
                .to_string()
        }
    };
    format!(
        "{HARNESS_CORRECTION_PREFIX} — not the user]\n\
         Your last two calls to `{tool_name}` failed with the identical error:\n\
         {failed_content}\n\
         A repeated identical failure means the earlier malformed call is \
         still steering your output — you are re-emitting the previous \
         arguments instead of reformulating. {remedy}"
    )
}

/// Build the harness-attributed corrective message for a repeated
/// identical tool failure: the error verbatim, the rewrite instruction,
/// and the tool's actual parameter schema (size-capped). This is the
/// mechanism that empirically broke the live loop — a fresh corrective
/// turn — delivered automatically on the second identical failure
/// instead of the user's attempt-15 hint.
fn tool_call_correction(tool_name: &str, failed_content: &str, schema: &ToolSchema) -> String {
    format!(
        "{HARNESS_CORRECTION_PREFIX} — not the user]\n\
         Your last two calls to `{tool_name}` failed with the identical error:\n\
         {failed_content}\n\
         A repeated identical failure means the earlier malformed call is \
         still steering your output — you are re-emitting the previous \
         arguments instead of re-reading the schema. Break the pattern: \
         re-emit the COMPLETE call with every required field present, and \
         do not resend the previous arguments unchanged.\n\
         The `{tool_name}` tool's schema:\n{}",
        render_correction_schema(schema)
    )
}

/// Render a tool's schema for a corrective message, size-capped: the
/// description (truncated) plus the full parameters JSON when it fits
/// the cap, otherwise a degraded summary (property names + required
/// list) so the correction stays compact even for large schemas.
fn render_correction_schema(schema: &ToolSchema) -> String {
    let description = crate::provider::sse_util::truncate_for_display(&schema.description, 400);
    let params = serde_json::to_string_pretty(&schema.parameters).unwrap_or_default();
    if params.len() <= CORRECTION_SCHEMA_CAP {
        format!("  description: {description}\n  parameters: {params}")
    } else {
        let props = schema
            .parameters
            .get("properties")
            .and_then(|p| p.as_object())
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let required = schema
            .parameters
            .get("required")
            .and_then(|r| r.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        format!(
            "  description: {description}\n  parameters (summary — full schema \
             too large): properties [{props}], required [{required}]"
        )
    }
}

/// Whether a tool call is "durable" — worth recording as a working-memory
/// event for later consolidation. Read-only tools (file_read, search,
/// browser_*, describe_image, memory_recall, …) are skipped: their output
/// already lives in the conversation history, and recording them floods the
/// working tier with raw snapshots that match common keywords and crowd out
/// distilled facts in recall. Only actions that *change* project state or
/// represent a durable decision are recorded, so consolidation has signal to
/// distill rather than noise to compress.
///
/// `args` is the parsed tool arguments (used to distinguish git subcommands:
/// only commit/merge/push are durable; read-only git ops like status/diff/log
/// are not).
pub(crate) fn is_durable_tool(name: &str, args: &serde_json::Value) -> bool {
    match name {
        // File mutations.
        "file_write" | "file_edit" | "file_append" | "convert_line_endings" => true,
        // Plan lifecycle (durable workflow decisions).
        "create_plan" | "update_plan" | "complete_step" | "abandon_plan" => true,
        // Skill lifecycle.
        "skill_start" | "skill_end" | "abandon_skill" => true,
        // Explicit memory writes.
        "memory_write" => true,
        // Review reports — findings are durable signal worth distilling
        // (recurring review findings should compound into procedural rules).
        "write_review_report" => true,
        // Sub-agent spawns.
        "spawn_agent" => true,
        // Git — only the history-landing subcommands (commit/merge/push).
        // Other mutating ops (checkout/stash/branch) are excluded: they don't
        // land durable changes worth distilling. The subcommand is resolved
        // forgivingly (either field), so `action: "commit"` is still recorded.
        "git" => matches!(
            resolve_git_subcommand(args).as_deref(),
            Some("commit") | Some("merge") | Some("push")
        ),
        // Everything else (file_read, read_files, search, search_read,
        // browser_*, describe_image, memory_recall, memory_consolidate,
        // ask_user, shell, git status/diff/log/branch/stash/checkout, …) is
        // read-only or noise and is NOT recorded.
        _ => false,
    }
}

#[cfg(test)]
mod workflow_event_tests {
    use super::*;

    #[test]
    fn is_durable_tool_resolves_action_field() {
        // The subcommand may arrive in the `action` field — durable recording
        // must still apply (and branch/stash action names must not record).
        assert!(is_durable_tool(
            "git",
            &serde_json::json!({"action": "commit", "message": "x"})
        ));
        assert!(is_durable_tool(
            "git",
            &serde_json::json!({"action": "merge", "branch": "feat"})
        ));
        assert!(is_durable_tool(
            "git",
            &serde_json::json!({"action": "push"})
        ));
        assert!(!is_durable_tool(
            "git",
            &serde_json::json!({"action": "status"})
        ));
        assert!(!is_durable_tool(
            "git",
            &serde_json::json!({"subcommand": "delete"})
        ));
        assert!(!is_durable_tool(
            "git",
            &serde_json::json!({"action": "stash"})
        ));
        assert!(!is_durable_tool("git", &serde_json::json!({})));
    }

    /// Regression (review finding 1, 2026-08-20): the `WorkflowStateChanged`
    /// emission must be gated on the tool call having SUCCEEDED — a failed
    /// workflow call (rejected by the state gate, bad arguments, hallucinated
    /// tool) never changed the workflow, and its unchanged-state event faked
    /// plan-loop evidence for the backlog gate (resting-Complete no-op turns
    /// were being marked Done). The turn harness is not unit-testable as
    /// structured, so this pins the emission block as a source contract. The
    /// slice starts at the block's marker comment and ends at the first event
    /// send after it, so this test's own literals cannot satisfy it (the
    /// include_str self-reference trap — see backlog_cmds.rs).
    #[test]
    fn workflow_state_changed_emission_requires_success() {
        let src = include_str!("turn.rs");
        let start = src
            .find("If this was a workflow tool that succeeded")
            .expect("emission block marker present");
        let end = src[start..]
            .find("AgentEvent::WorkflowStateChanged")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let block = &src[start..end];
        assert!(
            block.contains("result.success"),
            "the WorkflowStateChanged emission must be gated on result.success"
        );
    }
}
