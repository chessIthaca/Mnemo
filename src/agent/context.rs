// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Context management — token counting + summarization at a fill-rate threshold.
//!
//! As the agent works, the conversation grows. Without management, a real task
//! blows past the context window within a handful of turns. The strategy is a
//! hybrid sliding window + summarization: when the conversation hits the
//! threshold, summarize the oldest turns into a single system message, keeping
//! recent turns + the system prompt verbatim.
//!
//! ## Fitting the request (and the result) into the window
//!
//! Compaction has to work on exactly the histories that break it, so every
//! bound below is enforced by construction rather than hoped for:
//!
//! - **Linear measurement.** Token counts are BPE-exact up to
//!   [`TOKEN_MEASURE_CHUNK_CHARS`]; longer strings are measured chunk-wise.
//!   tiktoken's byte-pair merge is quadratic in the length of a single unbroken
//!   run (a base64 dump, a minified line, a repeated character), so measuring
//!   one as a single piece costs minutes — on the runaway tool output this
//!   module exists to survive. The chunked sum over-counts in practice (a chunk
//!   split forfeits the merges across it), which is the safe direction for a
//!   budget; [`SUMMARY_BUDGET_SLACK_TOKENS`] absorbs the residual rounding and
//!   the mechanical fallback backstops anything that slips through.
//! - **Budgeted summarization REQUEST.** [`summary_prompt_budget`] derives the
//!   summarizer's budget from its OWN window (the `[models.summarize]` slot may
//!   be a smaller model), and [`budgeted_cut_index`] marches the cut back —
//!   keeping more recent messages verbatim, losslessly — until the serialized
//!   region fits. A region that is over budget only because ONE message is a
//!   monster is truncated per-message with a marker instead, because marching
//!   there would keep the monster verbatim in the tail and re-wedge the next
//!   turn: it would also stay in the conversation rather than leave it.
//! - **Sendable RESULT.** The compacted conversation must itself fit the TURN
//!   model's window ([`sendable_budget`], [`enforce_sendable`]) — otherwise the
//!   session reports "Compacted" and then breaks on the next request. Levers,
//!   in order: aggressive per-result caps that keep their re-run pointers, then
//!   dropping the oldest tail messages — never the system message, never the
//!   summary, never the newest message, and never splitting a tool batch.
//! - **Mechanical fallback.** A provider size rejection on the summarization
//!   request itself (the provider counts more than the harness can see) is
//!   answered with [`mechanical_compaction`]: the region becomes a note that
//!   says what happened and how to re-fetch, instead of failing the compaction
//!   and leaving the session wedged until `/new`. Non-size errors still surface.
//! - **Per-result caps.** [`cap_tool_result_text`] bounds a result as it ENTERS
//!   the conversation ([`TOOL_RESULT_MAX_CHARS`]) — the only lever that acts
//!   before the damage — and [`compact_old_tool_results`] cuts any intact
//!   oversize result back to that cap even inside the hysteresis keep window.
//! - **Image blocks are priced.** Token accounting counts image payloads by
//!   length ([`content_text_and_image_bytes`]) instead of dropping them behind
//!   `as_text()`, so an image-heavy context trips the threshold it always
//!   should have — the ctx readout's numbers move for vision turns as a result.
//!
//! ## Compaction prompt design
//!
//! The summarization prompt (see [`ContextManager::build_summary_prompt`]) uses
//! a structured, section-based format that outperforms vague "summarize this"
//! prose. The design principles:
//!
//! 1. **Handoff-style numbered headings** — the model fills in eight numbered
//!    Markdown sections (1. Objective, 2. Key Context, 3. Decisions Made,
//!    4. Work Completed, 5. Current State & Open Loops, 6. Constraints &
//!    Rules, 7. Critical References, 8. Immediate Next Step) rather than
//!    free-form prose. The handoff structure packages the conversation as a
//!    compressed context a fresh agent could resume from without asking basic
//!    questions — higher information density, and nothing important is lost.
//!
//! 2. **Preserve identifiers verbatim** — file paths, function/type names,
//!    error strings, and API endpoints must be copied exactly as they appeared
//!    in the conversation, not paraphrased (the Critical References section
//!    mandates this). A paraphrased path (`src/agent/turn` instead of
//!    `src/agent/turn.rs`) breaks recall.
//!
//! 3. **Aggressive compression** — verbose tool outputs are compressed to their
//!    key results (a `file_read` of 200 lines → "read turn.rs (1151 lines):
//!    the turn driver; streaming select! at line 437"). Repeated reads of the
//!    same file collapse to the latest. Exploratory dead-ends (searches that
//!    found nothing, reads that weren't useful) are omitted entirely.
//!
//! 4. **Running-summary update** — when `messages[1]` is already a summary
//!    (starts with `## Conversation summary`), the prompt includes it and asks
//!    the model to UPDATE it rather than re-summarize from scratch. This is
//!    less lossy (the model builds on the existing summary) and more efficient
//!    (less work). The updated summary is tagged `(updated)` so the next
//!    compaction detects it.
//!
//! 5. **Token budget guidance** — the prompt tells the model to be concise but
//!    complete, aiming for a summary that captures the essential state without
//!    exceeding a rough budget. The model is told the recent messages are kept
//!    verbatim, so it should focus on the older context that will be dropped.

use crate::error::Result;
use crate::provider::{ContentPart, LlmClient, LlmEvent, Message, MessageContent, Role};

/// A context manager that counts tokens and triggers summarization.
///
/// `Clone` so an [`AgentLoopFactory`](crate::agent::factory::AgentLoopFactory)
/// can clone it into each per-agent build — the fields are cheap.
#[derive(Clone)]
pub struct ContextManager {
    max_tokens: usize,
    /// The configured fill-rate fraction (e.g. 0.3) — stored so
    /// [`Self::effective_summarize_at`] can delegate to the shared
    /// [`Self::effective_summarize_threshold`] helper (backlog ffd4bac3:
    /// the run-all between-items gate computes the SAME threshold through
    /// it, proxy-ceiling cap included).
    fill_rate: f64,
    summarize_at: usize,
    /// Whether to use aggressive compaction when the conversation exceeds the
    /// hard ceiling. When `true`, the existing compaction in `run_turn` uses
    /// `keep_recent=3` instead of the normal `6` when the pre-compaction token
    /// count exceeds [`hard_ceiling`](Self::hard_ceiling) — preventing fatal
    /// `ContextWindowExceededError` on long sessions.
    preflight_compact: bool,
    /// Tokens reserved for output when checking the hard ceiling.
    /// Compaction turns aggressive once `token_count > max_tokens -
    /// headroom`, reserving this much room for the model's reply.
    compact_headroom_tokens: usize,
    /// Total input tokens above which LiteLLM-class proxies drop
    /// whole-conversation prefix caching (hit rate collapses from ~99% to
    /// ~4%). When set, the effective summarize trigger caps at
    /// `ceiling - PROXY_CACHE_PRESSURE_MARGIN_TOKENS` so long sessions
    /// summarize before crossing the cliff. `None` disables the guard.
    proxy_cache_ceiling: Option<usize>,
}

impl ContextManager {
    /// Create a new context manager.
    ///
    /// - `max_tokens`: the model's context window (from capabilities).
    /// - `fill_rate`: the fraction at which to trigger summarization (e.g. 0.3).
    ///
    /// Preflight defaults to enabled with a 32 000-token headroom. Use
    /// [`with_preflight`](Self::with_preflight) to override.
    pub fn new(max_tokens: usize, fill_rate: f64) -> Self {
        let summarize_at = (max_tokens as f64 * fill_rate) as usize;
        Self {
            max_tokens,
            fill_rate,
            summarize_at,
            preflight_compact: true,
            compact_headroom_tokens: 32_000,
            proxy_cache_ceiling: None,
        }
    }

    /// Builder: set the preflight-compaction behavior. Returns `self` for
    /// chaining in the factory.
    pub fn with_preflight(mut self, enabled: bool, headroom: usize) -> Self {
        self.preflight_compact = enabled;
        self.compact_headroom_tokens = headroom;
        self
    }

    /// Builder: set the proxy cache ceiling (the ~340K-token cliff above which
    /// LiteLLM-class proxies drop whole-conversation prefix caching). When
    /// set, the effective summarize trigger caps at
    /// `ceiling - PROXY_CACHE_PRESSURE_MARGIN_TOKENS`. `None` disables the
    /// guard. Callers pass a value validated by the config layer (>= 65_536).
    pub fn with_proxy_cache_ceiling(mut self, ceiling: Option<usize>) -> Self {
        self.proxy_cache_ceiling = ceiling;
        self
    }

    /// The token threshold at which summarization triggers — the raw
    /// fill-rate product, NOT capped by the proxy cache ceiling. The check
    /// sites use [`effective_summarize_at`](Self::effective_summarize_at).
    pub fn summarize_at(&self) -> usize {
        self.summarize_at
    }

    /// The effective summarize trigger: `min(summarize_at, ceiling -
    /// PROXY_CACHE_PRESSURE_MARGIN_TOKENS)` when a proxy cache ceiling is
    /// set, otherwise the raw fill-rate product. Both summarize check sites
    /// (preflight and the turn loop) consult this so compaction fires before
    /// the request crosses the proxy cliff.
    pub fn effective_summarize_at(&self) -> usize {
        Self::effective_summarize_threshold(
            self.max_tokens,
            self.fill_rate,
            self.proxy_cache_ceiling,
        )
    }

    /// The effective summarize trigger for a `(max_tokens, fill_rate,
    /// ceiling)` triple — the shared core of
    /// [`Self::effective_summarize_at`], so off-manager callers compute
    /// the SAME threshold as the fill-rate path (proxy-ceiling cap
    /// included) instead of duplicating the formula. The run-all
    /// between-items auto-compact gate (backlog ffd4bac3) reads the
    /// user's `summarize_at_fill_rate` dial through this.
    pub fn effective_summarize_threshold(
        max_tokens: usize,
        fill_rate: f64,
        ceiling: Option<usize>,
    ) -> usize {
        let summarize_at = (max_tokens as f64 * fill_rate) as usize;
        match ceiling {
            None => summarize_at,
            Some(ceiling) => summarize_at.min(
                ceiling.saturating_sub(crate::provider::PROXY_CACHE_PRESSURE_MARGIN_TOKENS),
            ),
        }
    }

    /// The proxy cache ceiling (cliff guard), if configured.
    pub fn proxy_cache_ceiling(&self) -> Option<usize> {
        self.proxy_cache_ceiling
    }

    /// The max context window (in tokens).
    pub fn max_tokens(&self) -> usize {
        self.max_tokens
    }

    /// Whether the hard-ceiling pre-flight guard is enabled.
    pub fn preflight_compact(&self) -> bool {
        self.preflight_compact
    }

    /// Tokens reserved for output when checking the hard ceiling.
    pub fn compact_headroom_tokens(&self) -> usize {
        self.compact_headroom_tokens
    }

    /// The token count above which an already-triggered compaction turns
    /// aggressive (`keep_recent=3` instead of `6`). Compaction itself is still
    /// gated by the fill-rate threshold (`summarize_at`); the hard ceiling only
    /// selects the more aggressive `keep_recent` once the token count also
    /// exceeds `max_tokens - headroom`. Equals `max_tokens - headroom`.
    pub fn hard_ceiling(&self) -> usize {
        self.max_tokens.saturating_sub(self.compact_headroom_tokens)
    }

    /// Estimate the token count for a list of messages.
    /// Uses tiktoken-rs for OpenAI models; falls back to a char-based estimate.
    pub fn count_tokens(messages: &[Message]) -> usize {
        ContextManager::count_tokens_and_breakdown(messages).0
    }

    /// Count tokens broken down by message role (system/user/assistant/tool),
    /// for the context popup. Iterates messages once, using the same
    /// tiktoken/char-fallback as [`count_tokens`](Self::count_tokens).
    pub fn count_tokens_by_role(messages: &[Message]) -> crate::runtime::ContextBreakdown {
        ContextManager::count_tokens_and_breakdown(messages).1
    }

    /// Count the total tokens and the per-role breakdown in a single pass over
    /// the messages.
    ///
    /// With the BPE tokenizer available, each message contributes a fixed
    /// 4-token overhead plus the BPE length of its content and tool-call
    /// names/arguments, so the total is exactly the sum of the role buckets.
    /// Without it (tiktoken failed to load — practically unreachable, the rank
    /// table is embedded), each message falls back to ~4 chars/token and the
    /// total is the sum of the per-message floors.
    pub fn count_tokens_and_breakdown(
        messages: &[Message],
    ) -> (usize, crate::runtime::ContextBreakdown) {
        let bpe = try_tiktoken_bpe();
        let mut breakdown = crate::runtime::ContextBreakdown::default();
        for msg in messages {
            let count = count_message_tokens(bpe, msg);
            add_to_role(&mut breakdown, msg.role, count);
        }
        let total = breakdown.system as usize
            + breakdown.user as usize
            + breakdown.assistant as usize
            + breakdown.tool as usize;
        (total, breakdown)
    }

    /// Whether the conversation should be summarized.
    pub fn should_summarize(&self, messages: &[Message]) -> bool {
        let count = Self::count_tokens(messages);
        count >= self.effective_summarize_at()
    }

    /// Summarize the oldest messages into a single system message.
    ///
    /// Keeps the system prompt (first message) + the most recent `keep_recent`
    /// messages verbatim; the middle is replaced with a summary. The kept tail
    /// is cut at a tool-call boundary (see [`summary_cut_index`]), so it can
    /// be FEWER than `keep_recent` messages — and when the conversation ends
    /// in a tool-result run longer than `keep_recent`, the tail is empty and a
    /// synthetic user continuation is appended so the result never consists
    /// only of system messages.
    ///
    /// `sendable_budget` is the TURN model's window ([`sendable_budget`]): the
    /// compacted result is bounded by it, so compaction is total — what comes
    /// back is sendable, not merely smaller.
    pub async fn summarize(
        &self,
        messages: &[Message],
        keep_recent: usize,
        sendable_budget: usize,
        provider: &dyn LlmClient,
    ) -> Result<Vec<Message>> {
        if messages.len() <= keep_recent + 1 {
            // Not enough to summarize.
            return Ok(messages.to_vec());
        }

        let mut cut = summary_cut_index(messages, keep_recent);
        // Rule 4: if the open tool loop spans the entire conversation (nothing
        // to summarize before it), skip compaction — never trim inside an open
        // loop. Compaction "did not run" (spec acceptance test #3).
        if cut <= 1 {
            return Ok(messages.to_vec());
        }
        // Budget the summarization request against the summarizer's OWN
        // window: a history that dwarfs the window must not produce a
        // summarization prompt that dwarfs it too (the provider rejects that
        // request with an HTTP 400 and compaction fails exactly when it is
        // needed most). Marching the cut backward keeps more recent messages
        // verbatim in the kept tail — no data loss, the region simply
        // shrinks until the request fits.
        let budget = summary_prompt_budget(provider.capabilities());
        cut = budgeted_cut_index(messages, cut, budget);
        let system = &messages[0];
        let to_summarize = &messages[1..cut];
        let recent = &messages[cut..];

        // Build the structured summarization prompt (detects an existing
        // summary in messages[1] for a running-update path). The budget is
        // enforced per-message (markers) when the region alone is too big.
        let summary_prompt = build_summary_prompt(messages, to_summarize, budget);

        let summary_messages = vec![Message::user_text(summary_prompt)];

        // Request the summary from the LLM. A size rejection HERE means the
        // budgeted request was still refused — the provider counts more than
        // this harness can see (its own chat template, tool schemas, a smaller
        // window than advertised) — so fall back to the mechanical compaction
        // rather than wedging the session until `/new`.
        let stream = match provider.complete(&summary_messages, &[], None).await {
            Ok(stream) => stream,
            Err(e) if e.is_context_overflow() => {
                return Ok(mechanical_compaction(messages, cut, sendable_budget));
            }
            Err(e) => return Err(e),
        };
        use futures::StreamExt;
        let mut summary_text = String::new();
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            match event {
                LlmEvent::TextDelta { text } => summary_text.push_str(&text),
                LlmEvent::Error { .. } => break,
                _ => {}
            }
        }

        // Build the new message list: system + summary + recent.
        let summary_message = Message::system(format!("## Conversation summary\n\n{summary_text}"));

        let mut result = Vec::with_capacity(2 + recent.len());
        result.push(system.clone());
        result.push(summary_message);
        result.extend(recent.iter().cloned());
        ensure_sendable_tail(&mut result, recent);
        enforce_sendable(&mut result, sendable_budget);
        Ok(result)
    }

    /// Summarize the oldest messages, honoring interrupts during the
    /// summarization LLM call.
    ///
    /// This is like [`summarize`](Self::summarize) but the stream loop also
    /// polls `cmd_rx` via `select!`. If an `Interrupt` or `Cancel` arrives
    /// mid-summarization, the summary is abandoned and the original messages
    /// are returned unchanged (no data loss) — the caller's main loop then
    /// handles the command. Non-interrupt commands (`Suggestion`, `Prompt`)
    /// are buffered and returned to the caller for re-injection, matching the
    /// behavior of the main streaming loop.
    ///
    /// Returns `(messages, buffered, stop_reason)`. `stop_reason` is `Some`
    /// when an `Interrupt` or `Cancel` aborted the summary (so the caller can
    /// end the turn / terminate the agent); `None` on normal completion.
    pub async fn summarize_with_interrupt(
        &self,
        messages: &[Message],
        keep_recent: usize,
        sendable_budget: usize,
        provider: &dyn LlmClient,
        cmd_rx: &mut tokio::sync::mpsc::Receiver<crate::runtime::AgentCommand>,
    ) -> Result<(
        Vec<Message>,
        Vec<crate::runtime::AgentCommand>,
        Option<crate::agent::StopReason>,
        Option<SummarizerUsage>,
    )> {
        use crate::runtime::AgentCommand;
        use futures::StreamExt;

        if messages.len() <= keep_recent + 1 {
            // Not enough to summarize.
            return Ok((messages.to_vec(), Vec::new(), None, None));
        }

        let mut cut = summary_cut_index(messages, keep_recent);
        // Rule 4: if the open tool loop spans the entire conversation (nothing
        // to summarize before it), skip compaction — never trim inside an open
        // loop. Compaction "did not run" (spec acceptance test #3).
        if cut <= 1 {
            return Ok((messages.to_vec(), Vec::new(), None, None));
        }
        // Budget the summarization request against the summarizer's OWN
        // window — see [`ContextManager::summarize`]. Marching the cut
        // backward keeps more recent messages verbatim: no data loss, the
        // region shrinks.
        let budget = summary_prompt_budget(provider.capabilities());
        cut = budgeted_cut_index(messages, cut, budget);
        let system = &messages[0];
        let to_summarize = &messages[1..cut];
        let recent = &messages[cut..];

        // Build the structured summarization prompt (detects an existing
        // summary in messages[1] for a running-update path). The budget is
        // enforced per-message (markers) when the region alone is too big.
        let summary_prompt = build_summary_prompt(messages, to_summarize, budget);

        let summary_messages = vec![Message::user_text(summary_prompt)];

        // Request the summary from the LLM. See [`ContextManager::summarize`]
        // for why a size rejection falls back instead of failing.
        let stream = match provider.complete(&summary_messages, &[], None).await {
            Ok(stream) => stream,
            Err(e) if e.is_context_overflow() => {
                return Ok((
                    mechanical_compaction(messages, cut, sendable_budget),
                    Vec::new(),
                    None,
                    None,
                ));
            }
            Err(e) => return Err(e),
        };
        let mut summary_text = String::new();
        tokio::pin!(stream);
        let mut buffered: Vec<AgentCommand> = Vec::new();
        let mut stop_reason: Option<crate::agent::StopReason> = None;
        // R21: the summarizer's own usage — captured so the turn loop can
        // record it as a purpose='summarize' request_stats row.
        let mut usage: Option<SummarizerUsage> = None;
        loop {
            tokio::select! {
                // Stream events from the summarization LLM.
                event_opt = stream.next() => {
                    let Some(event) = event_opt else { break; };
                    match event {
                        LlmEvent::TextDelta { text } => summary_text.push_str(&text),
                        // R21: capture the summarizer's own usage so the
                        // turn loop can record it as a purpose='summarize'
                        // request_stats row (the mega-prompt was previously
                        // invisible to the aggregates).
                        LlmEvent::Usage {
                            prompt_tokens,
                            completion_tokens,
                            reasoning_tokens,
                            cached_tokens,
                            ttft_ms,
                            generation_ms,
                        } => {
                            usage = Some(SummarizerUsage {
                                prompt_tokens,
                                completion_tokens,
                                reasoning_tokens,
                                cached_tokens,
                                ttft_ms,
                                generation_ms,
                            });
                        }
                        LlmEvent::Error { error } => {
                            // R21 (round-1 review LOW 5): a mid-stream
                            // summarizer error must not silently replace the
                            // conversation with a truncated summary — surface
                            // it as an Err so the caller keeps the original
                            // messages and records a purpose='summarize'
                            // error row. A SIZE rejection is the exception: it
                            // is exactly the failure the mechanical fallback
                            // exists for, and the stream is dead anyway.
                            let error = crate::error::Error::Provider(error);
                            if error.is_context_overflow() {
                                return Ok((
                                    mechanical_compaction(messages, cut, sendable_budget),
                                    Vec::new(),
                                    None,
                                    None,
                                ));
                            }
                            return Err(error);
                        }
                        _ => {}
                    }
                }
                // Honor interrupts/cancels during the summarization call.
                cmd_opt = cmd_rx.recv() => {
                    match cmd_opt {
                        Some(AgentCommand::Interrupt) => {
                            // Abandon the summary — return the original
                            // messages unchanged so no context is lost. The
                            // caller's main loop ends the turn (agent stays
                            // alive).
                            stop_reason = Some(crate::agent::StopReason::Interrupt);
                            break;
                        }
                        Some(AgentCommand::Cancel) => {
                            // Abandon the summary — the caller's main loop
                            // terminates the agent (Exited).
                            stop_reason = Some(crate::agent::StopReason::Cancel);
                            break;
                        }
                        Some(other) => {
                            // Non-interrupt commands are buffered for the
                            // caller to re-inject (matches the main loop).
                            buffered.push(other);
                        }
                        None => { break; }
                    }
                }
            }
        }

        if let Some(reason) = stop_reason {
            // Return the original messages + any buffered commands + the stop
            // reason so the caller ends the turn / terminates the agent. The
            // captured usage (if any) rides along: the request was billed
            // even though the summary was abandoned (round-1 review LOW 5).
            return Ok((messages.to_vec(), buffered, Some(reason), usage));
        }

        // Build the new message list: system + summary + recent.
        let summary_message = Message::system(format!("## Conversation summary\n\n{summary_text}"));

        let mut result = Vec::with_capacity(2 + recent.len());
        result.push(system.clone());
        result.push(summary_message);
        result.extend(recent.iter().cloned());
        ensure_sendable_tail(&mut result, recent);
        enforce_sendable(&mut result, sendable_budget);
        Ok((result, buffered, None, usage))
    }
}

/// The summarizer stream's usage report (R21): forwarded to the turn loop so
/// compaction's own mega-prompt (up to ~300K tokens, guaranteed 0% cache)
/// lands in request_stats tagged purpose = 'summarize' — it was previously
/// invisible (the method consumed its own stream and ignored the Usage
/// event).
#[derive(Debug, Clone, Copy)]
pub struct SummarizerUsage {
    /// Input tokens billed for the summarization prompt.
    pub prompt_tokens: u32,
    /// Output tokens of the summary itself.
    pub completion_tokens: u32,
    /// Reasoning tokens (a subset of `completion_tokens`).
    pub reasoning_tokens: u32,
    /// Cached prompt tokens as reported (0 = a real reported miss).
    pub cached_tokens: u32,
    /// Time-to-first-token (ms), when the provider reported timing.
    pub ttft_ms: Option<u32>,
    /// Generation time (ms), when the provider reported timing.
    pub generation_ms: Option<u32>,
}

/// Compute the cut index for summarization — the boundary between the
/// summarized middle and the verbatim-kept tail.
///
/// The naive cut at `len - keep_recent` can land in the middle of a tool-call
/// batch (an assistant message carrying `tool_calls` followed by its tool
/// results). When the kept tail then STARTS with tool results whose matching
/// assistant tool call was summarized away, providers reject the next request
/// ("messages[N] is a tool message whose tool_call_id … has no matching
/// assistant tool call") and every retry re-fails identically — wedging the
/// session (user report 2026-08-22). Advance the cut past any leading tool
/// messages so the kept tail starts at a clean boundary; the skipped tool
/// results fold into the summarized region, so their content survives in the
/// summary. An assistant message WITH tool_calls is a valid cut point: its
/// results immediately follow inside the kept tail.
///
/// **Rule 4 (open tool loop):** a tool-use loop is one atomic unit from the
/// first assistant message after a user turn through the final text answer.
/// If the conversation ends in an OPEN loop (assistant tool_calls + tool
/// results with no final text answer), the cut MUST NOT land inside it —
/// retreat to the start of the open loop (the last user message) so the entire
/// in-progress loop is preserved verbatim. Reasoning from closed turns may be
/// dropped; reasoning from the open turn MUST NOT be.
///
/// Precondition: `messages.len() > keep_recent` (both callers guard on
/// `messages.len() <= keep_recent + 1` before calling).
fn summary_cut_index(messages: &[Message], keep_recent: usize) -> usize {
    let mut cut = messages.len() - keep_recent;
    // Advance past orphan tool results (closed-loop boundary fix).
    while cut < messages.len() && messages[cut].role == Role::Tool {
        cut += 1;
    }
    // Rule 4: never trim inside an open tool loop. If the conversation ends in
    // an open loop, retreat the cut to the loop's start so the entire
    // in-progress loop survives verbatim.
    cut = retreat_past_open_tool_loop(messages, cut);
    cut
}

/// If `cut` lands inside an open (incomplete) tool-use loop, retreat it to the
/// start of that loop (Rule 4).
///
/// An "open tool loop" is the trailing run of assistant(tool_calls) + tool
/// messages after the last user message, when there is no final text answer
/// (no text-only assistant message) closing it. Scanning backward from the end:
/// - a `User` message is the loop's start (a tool-use loop begins at the first
///   assistant message after a user turn) — retreat to it;
/// - a text-only `Assistant` message is a completed turn (final answer) — the
///   loop is closed, no retreat needed;
/// - `Tool` / `Assistant`-with-tool_calls messages are inside the loop — keep
///   scanning.
///
/// If no user message is found (e.g. a conversation that is only system +
/// orphan tool results), there is no spec-defined tool-use loop and the
/// existing forward-advance logic already handled it — no retreat.
fn retreat_past_open_tool_loop(messages: &[Message], cut: usize) -> usize {
    let mut loop_start: Option<usize> = None;
    for i in (1..messages.len()).rev() {
        let m = &messages[i];
        if m.role == Role::User {
            loop_start = Some(i);
            break;
        }
        if m.role == Role::Assistant && m.tool_calls.is_empty() {
            // A text-only assistant message closes the loop — no open loop.
            break;
        }
    }
    match loop_start {
        // Retreat only if the cut landed inside the open loop. Clamp to ≥ 1 so
        // the system message at [0] is never in the summarized region.
        Some(start) if cut > start => start.max(1),
        _ => cut,
    }
}

/// The marker appended to a tool result that has been compacted by
/// [`compact_old_tool_results`]. Checking for its presence makes the function
/// idempotent — a second pass skips messages already truncated.
const COMPACTED_MARKER: &str = "[… truncated for context efficiency]";

/// Aggressive per-result cap applied by [`enforce_sendable`] when the compacted
/// conversation is still over the turn model's window: small enough that a
/// monster-heavy history comes back under it, large enough to keep ordinary
/// results useful. Every capped result carries a re-run pointer
/// ([`rerun_pointer`]), so what the cap drops is recoverable by re-issuing the
/// call.
const AGGRESSIVE_TOOL_RESULT_CHARS: usize = 2_000;

/// The mechanical fallback's note — what replaces the summarized region when
/// the SUMMARIZER itself rejected the request as too large, so no model summary
/// exists to put there. It says plainly what happened and how to recover the
/// detail that was dropped. `pub(crate)` so the turn loop can recognize a
/// mechanical compaction and record the downgrade (it carries no Usage and
/// would otherwise look like ordinary compaction).
pub(crate) const MECHANICAL_SUMMARY_NOTE: &str = "[harness note] The conversation was compacted MECHANICALLY: the \
     summarizer rejected the request as too large, so the older turns could not be \
     summarized. They have been dropped, and every tool result was truncated to its \
     head with a re-run pointer. Re-run read_files/search for anything you still need.";

/// Whether a compacted conversation carries [`MECHANICAL_SUMMARY_NOTE`] — i.e.
/// the summarizer rejected the request for size and [`mechanical_compaction`]
/// substituted a note for a model summary. The turn loop uses this to record
/// the downgrade: the compaction SUCCEEDED, but a chronically undersized
/// `[models.summarize]` slot would otherwise be invisible — no Usage, no error
/// row, and a summary that reads like a normal one in the transcript.
pub(crate) fn is_mechanical_compaction(result: &[Message]) -> bool {
    result
        .iter()
        .any(|m| m.content.as_text().contains(MECHANICAL_SUMMARY_NOTE))
}

/// Build the one-line re-read/re-run pointer appended after [`COMPACTED_MARKER`]
/// when a tool result is truncated (D2): the originating tool call's arguments,
/// condensed so the agent can re-issue the call cheaply instead of re-searching
/// for content that left the context. The arguments are read from the preceding
/// assistant message's `tool_calls` (matched by `tool_call_id`) — the
/// `ToolResult` envelope's structured `data` is dropped when the result
/// becomes a message, so the arguments are the only creation-time metadata
/// that survives into history. Returns `None` for tools with no cheap
/// re-read source (and for unparseable arguments).
fn rerun_pointer(name: &str, arguments: &str) -> Option<String> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    match name {
        "read_files" | "file_read" => {
            // Single `path` (+ optional `start_line`/`max_lines`) and/or a
            // `files[]` batch of the same shape — the canonical parse lives
            // in `read_files_paths` (src/agent/dispatch.rs). `file_read`
            // takes the identical single-path args.
            let mut entries: Vec<(String, Option<u64>, Option<u64>)> = Vec::new();
            if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
                entries.push((
                    p.to_string(),
                    args.get("start_line").and_then(|v| v.as_u64()),
                    args.get("max_lines").and_then(|v| v.as_u64()),
                ));
            }
            if let Some(files) = args.get("files").and_then(|v| v.as_array()) {
                for f in files {
                    if let Some(p) = f.get("path").and_then(|v| v.as_str()) {
                        entries.push((
                            p.to_string(),
                            f.get("start_line").and_then(|v| v.as_u64()),
                            f.get("max_lines").and_then(|v| v.as_u64()),
                        ));
                    }
                }
            }
            if entries.is_empty() {
                return None;
            }
            let specs: Vec<String> = entries
                .into_iter()
                .map(|(p, start, max)| {
                    // Normalize like the tool does (read_files.rs treats
                    // start_line 0 as 1); saturating arithmetic keeps
                    // degenerate args from underflowing in debug builds.
                    let start = start.unwrap_or(1).max(1);
                    let p = one_line(&p, 160);
                    match max.filter(|m| *m > 0) {
                        Some(m) => {
                            format!("{p}:{start}-{}", start.saturating_add(m).saturating_sub(1))
                        }
                        None => p,
                    }
                })
                .collect();
            const MAX_SPECS: usize = 3;
            let mut line = format!("[re-read: {name} ");
            line.push_str(&specs[..MAX_SPECS.min(specs.len())].join(", "));
            if specs.len() > MAX_SPECS {
                line.push_str(&format!(" (+{} more)", specs.len() - MAX_SPECS));
            }
            line.push(']');
            Some(line)
        }
        "search" | "search_read" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str())?;
            // Keep the pointer one line: collapse control chars and cap the
            // pattern so a huge regex cannot dominate the marker.
            let pattern = one_line(pattern, 80);
            let glob = args
                .get("glob")
                .and_then(|v| v.as_str())
                .map(|g| one_line(g, 80));
            // A literal search re-run as a regex typically matches nothing —
            // keep the flag so the re-run preserves semantics.
            let literal = args.get("literal").and_then(|v| v.as_bool()).unwrap_or(false);
            let mut line = match glob {
                Some(g) => format!("[re-run: {name} pattern=\"{pattern}\" glob=\"{g}\""),
                None => format!("[re-run: {name} pattern=\"{pattern}\""),
            };
            if literal {
                line.push_str(" literal=true");
            }
            line.push(']');
            Some(line)
        }
        _ => None,
    }
}

/// Collapse control chars (a newline would break the one-line pointer) and
/// cap the length so a huge pattern/glob/path cannot bloat the marker.
fn one_line(s: &str, cap: usize) -> String {
    let mut out: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if out.chars().count() > cap {
        out = out.chars().take(cap).collect();
        out.push('…');
    }
    out
}

/// Longest tool-result text that enters the conversation at all (ingestion
/// cap): ~25K tokens, several times any sane single result, but far below what
/// a base64 image dump, a whole-file read, or a recursive grep can produce.
///
/// A result arrives in the same instant it becomes prompt content, so by the
/// time compaction could trim it the request has already been rejected — this
/// cap is the only lever that acts BEFORE the damage (see
/// [`cap_tool_result_text`]).
pub const TOOL_RESULT_MAX_CHARS: usize = 100_000;

/// Bound an individual tool result at INGESTION time.
///
/// Text at or under [`TOOL_RESULT_MAX_CHARS`] is returned unchanged —
/// byte-for-byte, so prefix caching and the classifiers that match substrings
/// (`is_user_denial_tool_output`, the retry logic) see exactly what the tool
/// produced. Longer text keeps its head plus a trailer that names the cap, the
/// dropped count, and the way to get the content back.
pub(crate) fn cap_tool_result_text(text: &str) -> String {
    let total = text.chars().count();
    if total <= TOOL_RESULT_MAX_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(TOOL_RESULT_MAX_CHARS).collect();
    format!(
        "{head}\n[tool output truncated at {TOOL_RESULT_MAX_CHARS} chars: {} of {total} chars dropped \
         — re-run the tool to see the full output]",
        total - TOOL_RESULT_MAX_CHARS
    )
}

/// The truncation trailer: [`COMPACTED_MARKER`] plus the re-read/re-run pointer
/// ([`rerun_pointer`]) when the originating tool call has one.
fn truncation_trailer(pointer: Option<&str>) -> String {
    match pointer {
        Some(p) => format!("{COMPACTED_MARKER}\n{p}"),
        None => COMPACTED_MARKER.to_string(),
    }
}

/// Truncate ONE tool result to `keep_chars` plus the truncation trailer,
/// replacing image-bearing multipart content with plain text. Returns whether
/// anything was truncated.
///
/// Shared by both compaction levers in [`compact_old_tool_results`]: the
/// hysteresis pass (cut results that fell out of the keep window back to
/// `summary_chars`) and the oversize pass (cut a runaway result back to
/// [`TOOL_RESULT_MAX_CHARS`] wherever it sits). The re-read pointer is resolved
/// from the originating tool call BEFORE the mutable borrow, and idempotency is
/// enforced on the marker, so repeat passes are no-ops.
fn truncate_tool_result_at(messages: &mut [Message], idx: usize, keep_chars: usize) -> bool {
    let (tool_call_id, name) = {
        let m = &messages[idx];
        (m.tool_call_id.clone(), m.name.clone())
    };
    let pointer = tool_call_id.as_deref().and_then(|id| {
        let arguments = messages.iter().find_map(|m| {
            m.tool_calls
                .iter()
                .find(|tc| tc.id == id)
                .map(|tc| tc.arguments.clone())
        });
        arguments.and_then(|a| name.as_deref().and_then(|n| rerun_pointer(n, &a)))
    });
    let marker = truncation_trailer(pointer.as_deref());
    let msg = &mut messages[idx];
    let head_source: String = match &msg.content {
        // Only truncate if the content is meaningfully longer than the
        // summary (avoid truncating a 300-char result to 500 chars).
        MessageContent::Text(s) => {
            if s.contains(COMPACTED_MARKER) || s.chars().count() <= keep_chars + 100 {
                return false;
            }
            s.clone()
        }
        MessageContent::Parts(parts) => {
            // A multipart result holds an image payload: shrink it by dropping
            // the payload and keeping whatever text came with it. Previously
            // these were skipped outright, so an image result could never
            // shrink — no matter how many of them the history held. The payload
            // IS the size, so this path never takes the `keep_chars + 100`
            // shortcut above.
            if !parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. }))
            {
                return false;
            }
            let text: String = parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect();
            if text.contains(COMPACTED_MARKER) {
                return false;
            }
            text
        }
    };
    let head: String = head_source.chars().take(keep_chars).collect();
    msg.content = MessageContent::text(format!("{head}…\n{marker}"));
    true
}

/// Truncate old tool-result messages to a compact summary using a hysteresis
/// window that preserves prefix caching across consecutive tool batches.
///
/// # Hysteresis semantics and prefix caching
///
/// Prefix caching requires consecutive requests to share a byte-identical prefix.
/// Truncating on every batch (sliding window) mutates the message that just fell
/// out of the keep window in place, breaking the provider's longest-common-prefix
/// cache on every request and dropping cache hits to ~46% (measured traces.jsonl
/// id 15, glm-5.3-gcp, 2027-01-04).
///
/// To prevent this, compaction uses a hysteresis high-water mark over the
/// TRUNCATABLE population — the results [`tool_result_is_truncatable`] can
/// actually shrink: while that count is `<= keep_high`, this function is a
/// strict no-op that mutates nothing, allowing consecutive requests to hit
/// ~95% cache. Once it exceeds `keep_high`, it cuts all but the newest `keep`
/// truncatable results back to `summary_chars` plus [`COMPACTED_MARKER`] in a
/// single pass.
///
/// Measuring the truncatable population rather than the intact one is load-
/// bearing: a result at or below `summary_chars + 100` is REFUSED by
/// [`truncate_tool_result_at`] and left unmarked, so it stays intact forever.
/// Counting those held the gate open on EVERY request in a real session (60+
/// short results against `keep_high = 20`), and the sliding window then rewrote
/// one already-sent result per turn — measured 70-78% cache where the same
/// session reaches 99% whenever the prefix stays byte-stable (round-6 analysis,
/// 2027-01-11, `.coding/analysis/cache-hit-6-report.md`).
///
/// Returns the number of tool results newly truncated in this pass (0 on no-op).
/// The caller should only reset token accounting when the return value is > 0.
///
/// Tool results the model has already acted on (read file contents, search
/// outputs, git diffs) are replaced with their first `summary_chars`
/// characters plus a truncation marker. The `tool_call_id` and `name` fields
/// are preserved so the conversation structure stays valid (N tool calls → N
/// tool results). A multipart result holding an image block is compacted to
/// its text plus the marker: the payload (a base64 `data:` URL) is the size, so
/// it is dropped while the result ages out.
///
/// Two rules run in this order, because prefix caching must survive both:
///
/// 1. The hysteresis pass above — cut results back to `summary_chars` once the
///    TRUNCATABLE population passes the high-water mark. Byte-identical while
///    under it.
/// 2. The OVERSIZE pass — cut any intact Text result longer than
///    [`TOOL_RESULT_MAX_CHARS`] back to that cap and no further, EVEN when the
///    hysteresis gate was a no-op and EVEN inside the keep window. Only
///    monsters qualify, so ordinary results stay byte-identical and the cache
///    still holds; a monster that stayed intact is precisely what wedges the
///    context, and it is new content for the cache either way. Running second
///    is deliberate: the aggressive pass (`keep = 0`) has already cut
///    everything there, so this only reaches what that pass SPARED.
///
/// # Re-read pointers (D2)
///
/// For tools with a cheap re-read source (`read_files`, `file_read`,
/// `search`, `search_read`), the truncation marker is followed by a one-line
/// pointer built from the originating tool call's arguments — file + line
/// range for reads, pattern + glob (+ `literal` flag) for searches — e.g.
/// `[re-read: read_files src/foo.rs:10-59]`, so the agent can re-read
/// cheaply instead of re-searching. The arguments come from the preceding
/// assistant message's `tool_calls` (matched by `tool_call_id`); see
/// [`rerun_pointer`]. Results whose id has no matching tool call (or whose
/// tool has no pointer format) get the plain marker. The pointer rides after
/// [`COMPACTED_MARKER`] and never contains it, so idempotency is unaffected.
pub fn compact_old_tool_results(
    messages: &mut [Message],
    keep: usize,
    keep_high: usize,
    summary_chars: usize,
) -> usize {
    let effective_high = keep_high.max(keep);
    // Collect the indices of all intact (not yet compacted) tool results. A
    // multipart result counts as intact only while it still holds an image
    // part: compaction rewrites it to plain text, and the marker then keeps it
    // out of this list on later passes (idempotency).
    let intact_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.role == Role::Tool
                && match &m.content {
                    MessageContent::Text(s) => !s.contains(COMPACTED_MARKER),
                    MessageContent::Parts(parts) => parts
                        .iter()
                        .any(|p| matches!(p, ContentPart::ImageUrl { .. })),
                }
        })
        .map(|(i, _)| i)
        .collect();

    // Hysteresis window: no-op while the intact window is below or at the
    // high-water mark. This keeps history byte-identical across batches so
    // the provider's longest-common-prefix cache holds.
    //
    // The gate counts the TRUNCATABLE population, not the intact one: a short
    // result is refused by `truncate_tool_result_at` without being marked, so
    // it stays intact forever. Counting those held the gate permanently open
    // (60+ short results in a real session) and the sliding window then
    // rewrote an already-sent result on every request.
    let truncatable_indices: Vec<usize> = intact_indices
        .iter()
        .copied()
        .filter(|&i| tool_result_is_truncatable(&messages[i], summary_chars))
        .collect();

    let mut truncated_count = 0;
    if truncatable_indices.len() > effective_high {
        // Cut back to `keep` truncatable tool results in one pass.
        let to_compact = &truncatable_indices[..truncatable_indices.len() - keep];
        for &idx in to_compact {
            if truncate_tool_result_at(messages, idx, summary_chars) {
                truncated_count += 1;
            }
        }
    }

    // Oversize pass: runs whatever the gate above decided, because a single
    // runaway result is what wedges a context. Scans again rather than tracking
    // during the pass — the intact list is short by construction (a handful of
    // results), and this keeps the two rules independent.
    let oversize: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.role == Role::Tool
                && match &m.content {
                    MessageContent::Text(s) => {
                        !s.contains(COMPACTED_MARKER) && s.chars().count() > TOOL_RESULT_MAX_CHARS
                    }
                    // Images inside the keep window are the aggressive pass's
                    // job (`enforce_sendable`), not the hysteresis path's.
                    MessageContent::Parts(_) => false,
                }
        })
        .map(|(i, _)| i)
        .collect();
    for idx in oversize {
        if truncate_tool_result_at(messages, idx, TOOL_RESULT_MAX_CHARS) {
            truncated_count += 1;
        }
    }

    truncated_count
}

/// Whether [`truncate_tool_result_at`] would actually shrink this message.
///
/// Mirrors that function's refusal rules exactly: a text result at or below
/// `keep_chars + 100` is refused AND left unmarked, and a multipart result
/// without an image part is refused — while one that carries an image is always
/// truncatable, because the payload (a base64 `data:` URL) IS the size. A
/// refused result therefore stays in the intact population forever, which is
/// why the hysteresis gate measures the TRUNCATABLE population instead
/// (round-6 cache-hit analysis, 2027-01-11: counting unmarkable short results
/// held the gate open on every request, so the sliding keep window rewrote one
/// already-sent tool result per turn and broke the provider's prefix cache
/// every time — 70-78% measured against 99% when the prefix stays stable).
fn tool_result_is_truncatable(msg: &Message, keep_chars: usize) -> bool {
    match &msg.content {
        MessageContent::Text(s) => {
            !s.contains(COMPACTED_MARKER) && s.chars().count() > keep_chars + 100
        }
        MessageContent::Parts(parts) => {
            if !parts
                .iter()
                .any(|p| matches!(p, ContentPart::ImageUrl { .. }))
            {
                return false;
            }
            let text: String = parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect();
            !text.contains(COMPACTED_MARKER)
        }
    }
}

/// Append a synthetic user continuation when the boundary-aligned kept tail
/// came out empty — the conversation ended in a run of tool messages longer
/// than `keep_recent`, so the cut advanced to the very end and the compacted
/// result would contain ONLY system messages (the system prompt + the
/// summary). Strict providers (e.g. Anthropic requires ≥1 non-system message)
/// reject such a request — wedging the very path meant to recover an
/// over-full context. The continuation keeps the compacted history sendable
/// and tells the model to resume from the summary.
fn ensure_sendable_tail(result: &mut Vec<Message>, recent: &[Message]) {
    if recent.is_empty() {
        result.push(Message::user_text(
            "[harness note] The conversation was compacted — continue from \
             the summary above.",
        ));
    }
}

/// The last-resort compaction: replace the summarized region with a mechanical
/// note when the SUMMARIZER rejected the request for size.
///
/// Reached when the budgeted request ([`build_summary_prompt`]) was still
/// refused, so there is no model summary to insert. Without this the very path
/// meant to recover an over-full context fails and the session stays wedged
/// until `/new` — the reported symptom.
///
/// No summarization happens here: the region becomes a system note that says so
/// plainly ([`MECHANICAL_SUMMARY_NOTE`]) and tells the agent how to re-fetch
/// what it needs. The recent tail is kept verbatim, every tool result is capped
/// aggressively (each keeping its re-run pointer), and the result is bounded by
/// `sendable_budget` — the fallback must not hand back a conversation that the
/// next request would reject for the same reason.
fn mechanical_compaction(messages: &[Message], cut: usize, sendable_budget: usize) -> Vec<Message> {
    let recent = &messages[cut..];
    let mut result = Vec::with_capacity(2 + recent.len());
    result.push(messages[0].clone());
    result.push(Message::system(format!(
        "## Conversation summary\n\n{MECHANICAL_SUMMARY_NOTE}"
    )));
    result.extend(recent.iter().cloned());
    ensure_sendable_tail(&mut result, recent);
    compact_old_tool_results(&mut result, 0, 0, AGGRESSIVE_TOOL_RESULT_CHARS);
    enforce_sendable(&mut result, sendable_budget);
    result
}

/// The compaction result's budget, from the TURN model's advertised window.
///
/// The request and the result are budgeted SEPARATELY and deliberately: the
/// summarizer may be a cheaper model with a smaller window than the turn model
/// ([`summary_prompt_budget`]), so using the summarizer's window here would
/// shrink a conversation the turn model could still hold — while using the turn
/// model's window for the request would overflow the summarizer.
pub fn sendable_budget(caps: &crate::provider::Capabilities) -> usize {
    context_budget(caps.max_context, caps.max_output_tokens)
}

/// The "compaction is total" post-condition: the compacted conversation must
/// ITSELF be sendable — otherwise the wedge survives a "successful" compaction.
///
/// Budgeting the summarization REQUEST is not enough. The cut march keeps more
/// recent messages verbatim (no data loss), so on a 7.2M-token history the
/// compacted result is still millions of tokens and the next request is
/// rejected exactly as before — the user compacts, sees "Compacted (7.2M →
/// 6.1M)", and the session breaks again on the following turn. This bounds the
/// OUTPUT, in escalating order:
///
/// 1. Under budget — untouched (the common case: nothing to do).
/// 2. Cap every tool result at [`AGGRESSIVE_TOOL_RESULT_CHARS`] (hysteresis
///    disabled via `keep=0, keep_high=0`), so even results inside the normal
///    keep window are cut. Each capped result keeps its re-run pointer, so the
///    content is recoverable by re-issuing the call rather than lost.
/// 3. If still over: drop the OLDEST tail messages one batch at a time. The
///    system message and the summary are never dropped, the newest message is
///    always kept, and a drop boundary never lands on a tool result (its call
///    would leave with it and orphan it — see the loop below). When the
///    surviving tail is a single open tool batch, no legal boundary exists and
///    the batch is kept whole: system + summary + that batch is the untouchable
///    core, and totality yields to it rather than emitting a request the
///    provider would reject for a dangling tool call.
///
/// Dropped tokens are SUBTRACTED from a running total instead of re-measuring
/// the whole conversation per pass: this path exists for multi-million-token
/// histories, where a full recount per dropped batch is exactly the O(n²)
/// shape the budget work removed elsewhere. The counter is the same per-message
/// one [`ContextManager::count_tokens`] sums, so the arithmetic is exact.
fn enforce_sendable(result: &mut Vec<Message>, sendable_budget: usize) {
    let mut total = ContextManager::count_tokens(result);
    if total <= sendable_budget {
        return;
    }
    compact_old_tool_results(result, 0, 0, AGGRESSIVE_TOOL_RESULT_CHARS);
    total = ContextManager::count_tokens(result);
    if total <= sendable_budget {
        return;
    }
    let bpe = try_tiktoken_bpe();
    // Always keep system + summary + the newest message, so each iteration
    // drops at least one message and the loop terminates.
    while result.len() > 3 && total > sendable_budget {
        let mut drop_to = 3;
        // Never start the new tail with an orphaned tool result: its call sits
        // in the run being dropped, so step past the tool messages.
        while drop_to < result.len() && result[drop_to].role == Role::Tool {
            drop_to += 1;
        }
        if drop_to >= result.len() {
            // The surviving tail IS one open tool batch that ends on the newest
            // message, so NO legal boundary exists. Dropping any of it would
            // leave a tool result whose call went with the dropped run, and
            // `validate_request_messages` rejects a dangling `tool_call_id` on
            // every LATER request — a wedge auto-compaction will not repair,
            // because the context is under the threshold by then (round-1
            // review HIGH 1). Keep the whole batch verbatim: system + summary +
            // the final batch is the untouchable core, and totality yields to
            // it.
            break;
        }
        let dropped: usize = result[2..drop_to]
            .iter()
            .map(|m| count_message_tokens(bpe, m) as usize)
            .sum();
        total = total.saturating_sub(dropped);
        result.drain(2..drop_to);
    }
}

/// The shared summary-format + compression instructions, used by both the
/// fresh-summary and running-update branches of [`build_summary_prompt`].
/// Extracted so the two branches differ only in their intro + payload
/// placement (previous-summary block vs conversation-turns block), not in
/// the format spec they both carry.
///
/// The format is a handoff-style summary: a compressed context package a fresh
/// agent could resume from without asking basic questions. Eight numbered
/// Markdown headings (Objective → Immediate Next Step) cover goal, context,
/// decisions, work done, open loops, constraints, references, and the next
/// action — higher information density and less lossy than a flat section
/// list. Identifiers stay verbatim (see Critical References).
const SUMMARY_FORMAT: &str = "\n\n\
             ## Instructions\n\n\
             Package this conversation into a compressed context summary so the work can \
             continue seamlessly. Format the response in clear Markdown using exactly \
             these numbered headings:\n\n\
             1. **Objective**: One clear sentence stating what we are trying to achieve.\n\
             2. **Key Context**: Background, environment, tools, or constraints a fresh \
             agent must know to avoid basic questions.\n\
             3. **Decisions Made**: Format each as `[Decision] -> [Reason]`. Include \
             rejected alternatives if relevant.\n\
             4. **Work Completed**: What has been built, tested, or resolved so far.\n\
             5. **Current State & Open Loops**: Active components, files, modules, or \
             pending questions still unresolved.\n\
             6. **Constraints & Rules**: Specific boundaries, formatting styles, or \
             negative constraints (\"do not do X\") established.\n\
             7. **Critical References**: Exact file paths, links, names, or code snippets \
             we are relying on — copied VERBATIM (do not paraphrase paths).\n\
             8. **Immediate Next Step**: Precisely where the very next step should begin.\n\n\
             Compress aggressively: verbose tool outputs → their key results (e.g. 'read \
             turn.rs (1151 lines): the turn driver'); repeated reads of the same file → the \
             latest; exploratory dead-ends (searches that found nothing, reads that weren't \
             useful) → omit entirely. The most recent messages are kept verbatim (not \
             summarized), so focus on the older context that will be dropped. Provide only \
             the final markdown block, keeping high information density and omitting \
             conversational filler — do not lose any decision, file path, or error that \
             the agent still needs.";

/// Fresh-summary prompt head: intro + the "## Conversation turns" label that
/// precedes the conversation block. Extracted as a constant so the budget
/// arithmetic ([`summary_prompt_frame_tokens`], [`budgeted_cut_index`])
/// measures exactly what [`build_summary_prompt`] emits.
const SUMMARY_PROMPT_HEAD: &str = "Summarize the following conversation turns into a structured summary. The system \
     prompt and the most recent messages are kept verbatim (not summarized), so focus on \
     the older context that will be dropped.\n\n\
     ## Conversation turns\n\n";

/// Running-update prompt head, up to and including the previous-summary
/// block (the existing summary text is spliced after it).
const SUMMARY_PROMPT_UPDATE_HEAD: &str = "You are updating an existing conversation summary with new turns. The previous \
     summary is below, followed by the new conversation turns to incorporate.\n\n\
     ## Previous summary\n\n";

/// Running-update prompt middle: the label before the new conversation turns.
const SUMMARY_PROMPT_UPDATE_MID: &str = "\n\n## New conversation turns\n\n";

/// Running-update prompt tail: the update instruction, up to (not including)
/// the shared [`SUMMARY_FORMAT`].
const SUMMARY_PROMPT_UPDATE_TAIL: &str = "\n\nProduce an UPDATED summary that merges the previous summary with the new turns. \
     Keep the same structured format:";

/// Marker appended to a message's truncated head when the conversation region
/// alone exceeds the summarization budget.
const SUMMARY_BUDGET_TRUNCATION: &str = "\n[… truncated — exceeded the summary budget]";

/// Note appended at the end of the conversation block when parts of it were
/// truncated/omitted to fit the summarizer's window.
const SUMMARY_BUDGET_NOTE: &str = "[Note: the conversation region exceeded the summarization budget — content \
     after the first '…' marker was omitted. The kept tail (the most recent messages) is verbatim.]";

/// The separator between serialized messages in the conversation block.
const CONV_SEPARATOR: &str = "\n\n";

/// Longest string handed to the BPE encoder in ONE call.
///
/// tiktoken's byte-pair merge is quadratic in the length of a single pretoken
/// (the regex `\p{L}+` matches an unbroken run as one piece), so one long run —
/// a base64 image blob, a minified JSON line, a repeated character — costs
/// minutes: measured on this machine, 8K chars = 0.14s, 32K = 2.1s, 64K = 8.3s,
/// 400K ≈ 5 min (4x per doubling). Compaction must never stall on the runaway
/// tool output it exists to recover from, so longer strings are measured in
/// chunks of this width (see [`prompt_text_tokens`]).
const TOKEN_MEASURE_CHUNK_CHARS: usize = 2_048;

/// Headroom reserved for BPE merges across the summary prompt's junctions.
///
/// The frame ([`summary_prompt_frame_tokens`]) and the conversation block are
/// measured separately and then concatenated: a token that would have merged
/// across a junction becomes two, so the assembled prompt can measure a
/// token or two above `frame + block + separators`. 16 covers every junction
/// with room to spare, which keeps "the built prompt fits its budget" a
/// strict guarantee instead of one that lands a token over.
const SUMMARY_BUDGET_SLACK_TOKENS: usize = 16;

/// Serialize one message for the summary prompt exactly as
/// [`build_summary_prompt`] embeds it (role label + text content).
fn format_conversation_turn(m: &Message) -> String {
    format!("{}: {}", role_str(m.role), m.content.as_text())
}

/// The largest index `<= i` that is a UTF-8 char boundary (clamped to
/// `s.len()`).
///
/// Chunked measurement must never split a multi-byte character: `i` is walked
/// back at most 3 bytes, so a caller chunking wider than that always makes
/// progress (see [`prompt_text_tokens`]).
fn floor_char_boundary(s: &str, i: usize) -> usize {
    let mut j = i.min(s.len());
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

/// Token count of `s`, using the context module's standard BPE (the embedded
/// tiktoken rank table) with the ~4 chars/token fallback.
///
/// Strings up to [`TOKEN_MEASURE_CHUNK_CHARS`] are measured exactly. Longer
/// ones are split into chunks of that width and the chunk counts summed, which
/// keeps the cost linear (the alternative is the quadratic blowup documented
/// on the constant) at the price of a conservative over-count: a chunk split
/// forfeits any BPE merge across it, so the parts measure HIGHER in practice —
/// greedy BPE is not globally optimal, so a contrived input could in principle
/// measure lower, which [`SUMMARY_BUDGET_SLACK_TOKENS`] absorbs and the
/// mechanical fallback backstops. Over-counting is the safe direction — every
/// caller spends this number against a budget or a fill-rate threshold, so the
/// guard trips marginally early rather than ever letting a request exceed the
/// window.
fn prompt_text_tokens(bpe: Option<&tiktoken_rs::CoreBPE>, s: &str) -> usize {
    match bpe {
        None => s.chars().count().div_ceil(4).max(1),
        Some(bpe) if s.len() <= TOKEN_MEASURE_CHUNK_CHARS => {
            bpe.encode_with_special_tokens(s).len()
        }
        Some(bpe) => {
            let mut total = 0usize;
            let mut start = 0usize;
            while start < s.len() {
                let end = floor_char_boundary(s, start + TOKEN_MEASURE_CHUNK_CHARS);
                // Progress: `start` is a char boundary and the chunk is far
                // wider than the 4-byte walk-back, so `end > start` always.
                debug_assert!(end > start, "chunked measurement must advance");
                total += bpe.encode_with_special_tokens(&s[start..end]).len();
                start = end;
            }
            total
        }
    }
}

/// Token count of `messages[start..end]` serialized into the summary prompt's
/// conversation block — role labels + content joined with [`CONV_SEPARATOR`],
/// exactly what [`build_summary_prompt`] embeds for a budget-fitting region.
fn conv_text_tokens(
    bpe: Option<&tiktoken_rs::CoreBPE>,
    messages: &[Message],
    start: usize,
    end: usize,
) -> usize {
    let mut total = 0usize;
    for (i, m) in messages[start..end].iter().enumerate() {
        if i > 0 {
            total += prompt_text_tokens(bpe, CONV_SEPARATOR);
        }
        total += prompt_text_tokens(bpe, &format_conversation_turn(m));
    }
    total
}

/// Token budget for a request/context against a model's window.
///
/// The summary prompt embeds the old conversation region, so without a
/// ceiling a conversation whose history exceeds the model's window produces
/// a summarization request that exceeds it too — every provider rejects
/// that ("stream request: HTTP 400 … the token count is longer than the
/// limit", user report 2027-01-23 with a 7.2M-token tool history) and
/// compaction fails exactly when it is needed most. The budget reserves the
/// output allowance plus a small slack for provider-side overhead, so the
/// request always fits the model's OWN window. A degenerate capabilities
/// object still gets a minimal floor so the last-resort mechanical fallback
/// has room to run.
///
/// Also used for the compaction post-condition ([`enforce_sendable`]): the
/// compacted result must fit the window it will be sent to (the TURN model —
/// which may be larger than the summarizer).
pub fn context_budget(max_context: usize, max_output_tokens: usize) -> usize {
    max_context
        .saturating_sub(max_output_tokens.max(4096).saturating_add(2048))
        .max(8192)
}

/// The summarization request's budget, from the summarizer's advertised
/// window (the summarizer may be a cheaper model with a smaller window than
/// the turn model, which is why the request and the result are budgeted
/// separately).
fn summary_prompt_budget(caps: &crate::provider::Capabilities) -> usize {
    context_budget(caps.max_context, caps.max_output_tokens)
}

/// The token counts of the summary prompt's fixed frame — everything except
/// the conversation block: the head (intro, plus the previous-summary block
/// when `messages[1]` is already a summary) and the tail ([`SUMMARY_FORMAT`],
/// or the running-update variant's update instruction + format). Mirrors
/// [`build_summary_prompt`]'s structure exactly so the budget arithmetic
/// here and the final prompt agree.
fn summary_prompt_fixed_tokens(messages: &[Message], bpe: Option<&tiktoken_rs::CoreBPE>) -> usize {
    let (head_tokens, tail_tokens) = summary_prompt_frame_tokens(messages, bpe);
    head_tokens + tail_tokens + SUMMARY_BUDGET_SLACK_TOKENS
}

/// Token budget left for the summary prompt's conversation block: `budget_tokens`
/// minus the fixed frame and the boundary slack.
///
/// [`build_summary_prompt`] (which enforces the budget) and [`budgeted_cut_index`]
/// (which decides how much history the prompt will contain) must agree on this
/// number — the march assumes what the builder enforces — so both call this
/// instead of recomputing the frame arithmetic.
fn conv_block_budget(
    messages: &[Message],
    bpe: Option<&tiktoken_rs::CoreBPE>,
    budget_tokens: usize,
) -> usize {
    budget_tokens.saturating_sub(summary_prompt_fixed_tokens(messages, bpe))
}

fn summary_prompt_frame_tokens(
    messages: &[Message],
    bpe: Option<&tiktoken_rs::CoreBPE>,
) -> (usize, usize) {
    let (head, tail) = match existing_summary_at(messages) {
        Some(summary) => (
            format!("{SUMMARY_PROMPT_UPDATE_HEAD}{summary}{SUMMARY_PROMPT_UPDATE_MID}"),
            format!("{SUMMARY_PROMPT_UPDATE_TAIL}{SUMMARY_FORMAT}"),
        ),
        None => (SUMMARY_PROMPT_HEAD.to_string(), SUMMARY_FORMAT.to_string()),
    };
    (prompt_text_tokens(bpe, &head), prompt_text_tokens(bpe, &tail))
}

/// Detect an existing summary in `messages[1]` (the slot where summaries are
/// placed): a system message starting with `## Conversation summary`.
fn existing_summary_at(messages: &[Message]) -> Option<String> {
    messages
        .get(1)
        .filter(|m| {
            m.role == Role::System && m.content.as_text().starts_with("## Conversation summary")
        })
        .map(|m| m.content.as_text())
}

/// March the summarization cut TOWARD `messages[1]` while the summarized
/// region would overflow the summarization request's token budget.
///
/// The cut is the boundary between the summarized region `messages[1..cut]`
/// and the verbatim kept tail `messages[cut..]`. Moving it backward keeps
/// MORE recent messages verbatim — no data loss, the region simply shrinks,
/// so the summary covers less old context — until the serialized region fits
/// the summarizer's own window (the summarizer may be a cheaper model than
/// the turn model). The backward march never splits a tool batch: a boundary
/// that lands on a tool result keeps marching until it sits on a non-tool
/// message, so the kept tail never starts with an orphaned tool result and
/// every tool result stays with its call.
///
/// Marching is the LOSSLESS lever, so it is only spent where it can win: when
/// a single region message already busts the region allowance, removing other
/// messages cannot fit the request — the oversized one stays, and staying it
/// also stays in the conversation (the march would move it to the verbatim
/// tail), which is precisely what re-wedges the next turn. In that case the
/// cut is returned untouched and [`budgeted_conv_text`] bounds the region by
/// truncating the oversized message with a marker, keeping the summary's
/// coverage while the runaway result leaves the history for good.
fn budgeted_cut_index(messages: &[Message], cut: usize, budget: usize) -> usize {
    let bpe = try_tiktoken_bpe();
    let conv_budget = conv_block_budget(messages, bpe, budget);
    let sep_tokens = prompt_text_tokens(bpe, CONV_SEPARATOR);
    // Normalize the incoming cut first: the march's rule (the tail never starts
    // with an orphaned tool result) has to hold for the bail-out return below
    // too, and a caller's cut can land on a tool result. Clamped at 1 — a
    // decrement to 0 would make the callers' `&messages[1..cut]` slice panic,
    // and this normalization exists precisely to defend against caller shapes
    // the module cannot prove absent.
    let mut cut = cut;
    while cut > 1 && cut < messages.len() && messages[cut].role == Role::Tool {
        cut -= 1;
    }
    // Seed the region total (and its largest turn) in ONE pass, then subtract
    // per marched step — re-serializing the region each iteration is O(n²)
    // and a 7.2M-token history would spend minutes in the tokenizer exactly
    // when the session is already wedged.
    let mut region_tokens: usize = 0;
    let mut max_turn_tokens: usize = 0;
    for (i, m) in messages[1..cut].iter().enumerate() {
        if i > 0 {
            region_tokens += sep_tokens;
        }
        let turn_tokens = prompt_text_tokens(bpe, &format_conversation_turn(m));
        max_turn_tokens = max_turn_tokens.max(turn_tokens);
        region_tokens += turn_tokens;
    }
    if max_turn_tokens > conv_budget {
        return cut;
    }
    while cut > 1 && region_tokens > conv_budget {
        let old_cut = cut;
        cut -= 1;
        // Keep the boundary on a non-tool message so the kept tail never
        // starts with an orphaned tool result.
        while cut > 1 && messages[cut].role == Role::Tool {
            cut -= 1;
        }
        // Every message leaving the region contributed its serialized form
        // plus one separator (the region's first message is `messages[1]`,
        // so anything at index >= 1 carried a separator).
        for m in &messages[cut..old_cut] {
            region_tokens = region_tokens
                .saturating_sub(prompt_text_tokens(bpe, &format_conversation_turn(m)) + sep_tokens);
        }
    }
    cut
}

/// Serialize `to_summarize` into the summary prompt's conversation block,
/// honoring a token ceiling.
///
/// When the region fits `conv_budget` the output is byte-identical to the
/// unconstrained join (the normal case — history below the model window, so
/// prompt output and provider cache behavior are unchanged). When even the
/// WHOLE region exceeds the budget (a monster tool result, or an enormous
/// number of turns), as much as fits is kept, the first message that would
/// bust the budget is truncated with [`SUMMARY_BUDGET_TRUNCATION`], and
/// everything after it is omitted with a [`SUMMARY_BUDGET_NOTE`] — the total
/// is bounded by `conv_budget` by construction.
fn budgeted_conv_text(
    bpe: Option<&tiktoken_rs::CoreBPE>,
    to_summarize: &[Message],
    conv_budget: usize,
) -> String {
    if conv_text_tokens(bpe, to_summarize, 0, to_summarize.len()) <= conv_budget {
        return to_summarize
            .iter()
            .map(format_conversation_turn)
            .collect::<Vec<_>>()
            .join(CONV_SEPARATOR);
    }

    let note_tokens = prompt_text_tokens(bpe, SUMMARY_BUDGET_NOTE);
    let trunc_tokens = prompt_text_tokens(bpe, SUMMARY_BUDGET_TRUNCATION);
    let sep_tokens = prompt_text_tokens(bpe, CONV_SEPARATOR);
    // Reserve the note up front so the closing marker always fits — plus the
    // separator that precedes it, which is only spent when `out` is non-empty
    // (reserving it unconditionally is conservative by exactly one separator).
    let usable = conv_budget.saturating_sub(note_tokens + sep_tokens);
    let mut out = String::new();
    let mut used = 0usize;
    let mut truncated = false;
    for m in to_summarize {
        let part = format_conversation_turn(m);
        let part_tokens = prompt_text_tokens(bpe, &part);
        let sep = if out.is_empty() { 0 } else { sep_tokens };
        if !truncated && used + sep + part_tokens <= usable {
            if !out.is_empty() {
                out.push_str(CONV_SEPARATOR);
            }
            out.push_str(&part);
            used += sep + part_tokens;
            continue;
        }
        // The first message that does not fit: append its head (truncated to
        // whatever budget remains) + the marker, then stop — everything after
        // it is omitted.
        truncated = true;
        let left = usable.saturating_sub(used + sep);
        if left > trunc_tokens + 16 {
            let head = truncate_to_token_budget(bpe, &part, left - trunc_tokens);
            if !out.is_empty() {
                out.push_str(CONV_SEPARATOR);
            }
            out.push_str(&head);
            out.push_str(SUMMARY_BUDGET_TRUNCATION);
        }
        break;
    }
    if truncated {
        if !out.is_empty() {
            out.push_str(CONV_SEPARATOR);
        }
        out.push_str(SUMMARY_BUDGET_NOTE);
    }
    out
}

/// Truncate `s` to a prefix whose token count is at most `budget` (empty when
/// even a tiny prefix cannot fit). Token count is the module measure — the
/// char-based start estimate overruns on multi-token characters (CJK, emoji),
/// so the candidate is measured and halved until it fits or is empty. Above
/// [`TOKEN_MEASURE_CHUNK_CHARS`] that measure is the chunked one, which errs
/// high, so the truncation lands short rather than long.
fn truncate_to_token_budget(
    bpe: Option<&tiktoken_rs::CoreBPE>,
    s: &str,
    budget: usize,
) -> String {
    let mut chars = s.chars().take(budget.saturating_mul(4)).collect::<String>();
    while prompt_text_tokens(bpe, &chars) > budget && !chars.is_empty() {
        let keep = chars.chars().count() / 2;
        chars = s.chars().take(keep).collect::<String>();
    }
    chars
}

/// Build the structured summarization prompt for the LLM, honoring a token
/// budget for the REQUEST.
///
/// Detects whether `messages[1]` is an existing summary (starts with
/// `## Conversation summary`) and, if so, includes it so the model UPDATES it
/// rather than re-summarizing from scratch (less lossy, more efficient). The
/// prompt uses the handoff-style numbered headings (1. Objective … 8.
/// Immediate Next Step — see [`SUMMARY_FORMAT`]) and instructs aggressive
/// compression of tool outputs + verbatim preservation of identifiers.
///
/// The budget guards the summarization request against a history that
/// dwarfs the summarizer's window (see [`summary_prompt_budget`]): the fixed
/// frame ([`summary_prompt_fixed_tokens`]) is reserved first, and the
/// conversation block is serialized through [`budgeted_conv_text`] so the
/// total always fits — truncated with a marker instead of failing the
/// compaction.
///
/// See the module-level docs for the full design rationale.
fn build_summary_prompt(
    messages: &[Message],
    to_summarize: &[Message],
    budget_tokens: usize,
) -> String {
    let bpe = try_tiktoken_bpe();
    let conv_budget = conv_block_budget(messages, bpe, budget_tokens);
    let conv_text = budgeted_conv_text(bpe, to_summarize, conv_budget);

    match existing_summary_at(messages) {
        Some(summary) => format!(
            "{SUMMARY_PROMPT_UPDATE_HEAD}{summary}{SUMMARY_PROMPT_UPDATE_MID}{conv_text}{SUMMARY_PROMPT_UPDATE_TAIL}{SUMMARY_FORMAT}"
        ),
        None => format!("{SUMMARY_PROMPT_HEAD}{conv_text}{SUMMARY_FORMAT}"),
    }
}

/// Incremental token accounting over a growing conversation.
///
/// The turn loop's message list only grows within a request loop (tool
/// results and assistant turns are appended; the system head at `messages[0]`
/// is rebuilt in place). A full BPE pass over the entire history every
/// iteration — the old `count_tokens` + `count_tokens_by_role` pair — is
/// therefore pure re-work: only the appended suffix and (rarely) the head
/// changed since the previous count. [`update`](Self::update) encodes just
/// those deltas, so each request-loop iteration pays O(new messages) instead
/// of O(history).
///
/// Exactness: with the BPE tokenizer available, the results are
/// byte-for-byte identical to [`ContextManager::count_tokens_and_breakdown`]
/// — both share the per-message formula ([`count_message_tokens`]) and every
/// message is counted independently, so summing deltas equals re-summing the
/// whole list. Without the tokenizer (practically unreachable — the rank
/// table is embedded) both use per-message ~4-chars/token floors, so the
/// accounting matches `count_tokens_and_breakdown` exactly there too (this is
/// a deliberate change from the historical single-division `count_tokens`
/// fallback).
///
/// # Index-stability invariant
///
/// The delta path assumes the counted prefix is *index-stable*: callers may
/// append messages and replace `messages[0]`'s content in place, but must NOT
/// insert or remove messages inside the already-counted prefix. Violations
/// (a shrink, or a list whose head is not yet a system message — the turn
/// loop inserts the system head at index 0 on fresh sessions) are detected
/// and fall back to a full pass; call [`reset`](Self::reset) after any other
/// rewrite (e.g. summarization).
#[derive(Default)]
pub struct TokenAccounting {
    /// Number of leading messages accounted for.
    counted_len: usize,
    /// Content of `messages[0]` at the last head accounting (`None` when the
    /// head wasn't a system message or nothing was counted yet). Compared
    /// against the live head to detect in-place replacement.
    head_content: Option<String>,
    /// The token count of the head message alone (for the swap math).
    head_count: u32,
    /// Per-role bucket totals.
    breakdown: crate::runtime::ContextBreakdown,
    /// Token overhead of the request's tool-schema array (name + description
    /// + parameters JSON per tool). The provider bills it in
    /// `usage.prompt_tokens` but it belongs to no message, so it is kept out
    /// of the per-role buckets and added to the total. Set by the turn loop
    /// at the TOP of each iteration, from the schemas actually sent
    /// ([`crate::provider::estimate_tools_tokens`]) — before the first
    /// [`Self::update`] read — and preserved by [`Self::reset`].
    tools_tokens: u32,
}

impl TokenAccounting {
    /// Create empty accounting state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop the message-accounting state — the next
    /// [`update`](Self::update) does a full pass. Call after anything that
    /// rewrites the conversation out from under the accounting
    /// (summarization, compaction).
    ///
    /// The tools-schema overhead is deliberately KEPT: the schema array is
    /// independent of the conversation (rewrites touch messages, never the
    /// tools), so a recount right after a rewrite — e.g. the
    /// post-summarization ContextUsage emission — must still include it. The
    /// turn loop re-sets it from the schemas actually sent at the top of
    /// every iteration anyway.
    pub fn reset(&mut self) {
        self.counted_len = 0;
        self.head_content = None;
        self.head_count = 0;
        self.breakdown = crate::runtime::ContextBreakdown::default();
    }

    /// Update the accounting to match `messages` and return the total token
    /// count, the per-role breakdown, and whether a full pass ran.
    ///
    /// A full pass runs on first use, when the list shrank (a rewrite), or
    /// when `messages[0]` is not a system message (the head-replacement
    /// detection only applies to the system head). Otherwise only the
    /// messages appended since the last update are encoded, plus the head
    /// itself when its content changed.
    pub fn update(
        &mut self,
        messages: &[Message],
    ) -> (usize, crate::runtime::ContextBreakdown, bool) {
        let bpe = try_tiktoken_bpe();
        if self.counted_len == 0
            || messages.len() < self.counted_len
            || messages.first().map(|m| m.role) != Some(Role::System)
        {
            self.full_pass(messages, bpe);
            return (self.total(), self.breakdown, true);
        }

        // Head replacement: messages[0] is rebuilt each request-loop
        // iteration; when its content changed, swap only its contribution
        // into the system bucket.
        let head = &messages[0];
        let head_text = head.content.as_text();
        if self.head_content.as_deref() != Some(head_text.as_str()) {
            let new_head_count = count_message_tokens(bpe, head);
            self.breakdown.system =
                self.breakdown.system.saturating_sub(self.head_count) + new_head_count;
            self.head_count = new_head_count;
            self.head_content = Some(head_text.to_string());
        }

        // Appended messages: encode only the new suffix.
        for msg in &messages[self.counted_len..] {
            let count = count_message_tokens(bpe, msg);
            add_to_role(&mut self.breakdown, msg.role, count);
        }
        self.counted_len = messages.len();
        (self.total(), self.breakdown, false)
    }

    /// One BPE pass over the whole list, resetting the state to match.
    fn full_pass(&mut self, messages: &[Message], bpe: Option<&tiktoken_rs::CoreBPE>) {
        let mut breakdown = crate::runtime::ContextBreakdown::default();
        for msg in messages {
            let count = count_message_tokens(bpe, msg);
            add_to_role(&mut breakdown, msg.role, count);
        }
        self.breakdown = breakdown;
        if messages.first().map(|m| m.role) == Some(Role::System) {
            let head = &messages[0];
            self.head_content = Some(head.content.as_text().to_string());
            self.head_count = count_message_tokens(bpe, head);
            self.counted_len = messages.len();
        } else {
            // No system head yet: the turn loop will INSERT the head at index
            // 0, shifting every existing index right. The counted prefix is
            // index-stable only once a system head exists, so leave the state
            // empty — the next update re-runs a full pass instead of
            // double-counting the last pre-insert message (regression test
            // `token_accounting_recovers_after_head_insertion`).
            self.head_content = None;
            self.head_count = 0;
            self.counted_len = 0;
        }
    }

    /// Record the tools-schema overhead included in the total. The turn loop
    /// calls this at the top of each iteration from the schemas actually
    /// sent — before the iteration's first [`Self::update`] read, so every
    /// consumer of the count (ctx readout, summarize trigger, preflight
    /// ceiling check) sees the block.
    pub fn set_tools_tokens(&mut self, tokens: u32) {
        self.tools_tokens = tokens;
    }

    /// The total token count: the sum of the role buckets plus the
    /// tools-schema overhead — the provider-visible basis.
    fn total(&self) -> usize {
        self.breakdown.system as usize
            + self.breakdown.user as usize
            + self.breakdown.assistant as usize
            + self.breakdown.tool as usize
            + self.tools_tokens as usize
    }
}

/// The text and image payload of a message's content, split for pricing.
///
/// [`MessageContent::as_text`] returns TEXT parts only, so pricing a message
/// through it makes image blocks invisible to token accounting: a message
/// carrying a 400KB base64 `data:` URL counted as a handful of tokens, and an
/// image-heavy context could never trip the summarize trigger, the preflight
/// ceiling, or the ctx readout. Images are what the provider bills for, so
/// this walks the content once and reports both halves — the concatenated text
/// (BPE-measured by the caller) and the image payload bytes (priced by ratio).
fn content_text_and_image_bytes(content: &MessageContent) -> (String, usize) {
    match content {
        MessageContent::Text(s) => (s.clone(), 0),
        MessageContent::Parts(parts) => {
            let mut text = String::new();
            let mut image_bytes = 0usize;
            for part in parts {
                match part {
                    ContentPart::Text { text: t } => text.push_str(t),
                    ContentPart::ImageUrl { image_url } => image_bytes += image_url.url.len(),
                }
            }
            (text, image_bytes)
        }
    }
}

/// Price an image payload at ~4 chars per token — the same ratio the no-BPE
/// text fallback uses, and the one the real tokenizer lands near for base64.
///
/// A `data:` URL carries the base64 image inline (with its own header), so the
/// URL's byte length stands in for the payload the provider is billed for;
/// remote URLs cost their few bytes plus what the client fetches, which this
/// measure cannot see and deliberately does not invent.
fn image_payload_tokens(image_bytes: usize) -> u32 {
    (image_bytes.div_ceil(4)) as u32
}

/// The token count of a single message: the shared 4-token overhead plus the
/// BPE length of its content, its `reasoning_content` echo, and tool-call
/// names/arguments; falls back to ~4 chars per token (no overhead) when the
/// BPE tokenizer is unavailable, mirroring the historical per-role fallback.
/// Every part goes through [`prompt_text_tokens`] so turn accounting and the
/// summarization budget share ONE measure (a large tool result cannot stall
/// the count — see [`TOKEN_MEASURE_CHUNK_CHARS`]). Image blocks are priced by
/// payload length ([`content_text_and_image_bytes`]) instead of vanishing
/// behind `as_text()`.
/// Reasoning text round-trips on the wire (DeepSeek thinking mode HTTP-400s
/// without the echo) and the provider counts it in `usage.prompt_tokens`, so
/// it must be counted here too (regression
/// `token_accounting_counts_reasoning_content`).
fn count_message_tokens(bpe: Option<&tiktoken_rs::CoreBPE>, msg: &Message) -> u32 {
    let (content_text, image_bytes) = content_text_and_image_bytes(&msg.content);
    let image_tokens = image_payload_tokens(image_bytes);
    match bpe {
        Some(bpe) => {
            let mut total = 4;
            total += prompt_text_tokens(Some(bpe), &content_text) as u32;
            total += image_tokens;
            if let Some(rc) = &msg.reasoning_content {
                total += prompt_text_tokens(Some(bpe), rc) as u32;
            }
            for tc in &msg.tool_calls {
                total += prompt_text_tokens(Some(bpe), &tc.name) as u32;
                total += prompt_text_tokens(Some(bpe), &tc.arguments) as u32;
            }
            total
        }
        None => {
            let mut content_chars = content_text.len() + image_bytes;
            if let Some(rc) = &msg.reasoning_content {
                content_chars += rc.len();
            }
            let tool_chars: usize = msg
                .tool_calls
                .iter()
                .map(|tc| tc.name.len() + tc.arguments.len())
                .sum();
            ((content_chars + tool_chars) / 4) as u32
        }
    }
}

/// Add a message's token count to its role's bucket.
fn add_to_role(breakdown: &mut crate::runtime::ContextBreakdown, role: Role, count: u32) {
    match role {
        Role::System => breakdown.system += count,
        Role::User => breakdown.user += count,
        Role::Assistant => breakdown.assistant += count,
        Role::Tool => breakdown.tool += count,
    }
}

/// Get the cached tiktoken BPE tokenizer (`cl100k_base`), loading it once on
/// first use. Returns `None` when tiktoken is unavailable (non-OpenAI model
/// or the tokenizer failed to load). Shared by [`count_message_tokens`] so
/// the tokenizer is decoded once.
fn try_tiktoken_bpe() -> Option<&'static tiktoken_rs::CoreBPE> {
    use tiktoken_rs::{cl100k_base, CoreBPE};
    static BPE: std::sync::OnceLock<Option<CoreBPE>> = std::sync::OnceLock::new();
    BPE.get_or_init(|| cl100k_base().ok()).as_ref()
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ImageUrl;

    /// Budget handed to compaction by tests that are NOT about the sendable
    /// post-condition: large enough that [`enforce_sendable`] is a no-op, so
    /// those fixtures keep asserting what they were written to assert (the
    /// summary prompt, the cut, the tail). Tests that target the
    /// post-condition pass a tight budget instead (see
    /// `enforce_sendable_bounds_the_compacted_result`).
    const TEST_SENDABLE_BUDGET: usize = 10_000_000;

    #[test]
    fn effective_summarize_threshold_mirrors_effective_summarize_at() {
        // Backlog ffd4bac3: the between-items gate computes its threshold
        // through the shared helper — it must agree with the manager's
        // own effective trigger for the same triple, ceiling cap included.
        for &(max, rate, ceiling) in &[
            (500_000usize, 0.5, None),
            (500_000, 0.5, Some(340_000)),
            (500_000, 0.9, Some(340_000)), // the cap binds
            (200_000, 0.3, Some(340_000)), // the product binds
            (0, 0.3, None),
        ] {
            let mut m = ContextManager::new(max, rate);
            if ceiling.is_some() {
                m = m.with_proxy_cache_ceiling(ceiling);
            }
            assert_eq!(
                ContextManager::effective_summarize_threshold(max, rate, ceiling),
                m.effective_summarize_at(),
                "max={max} rate={rate} ceiling={ceiling:?}"
            );
        }
    }

    #[test]
    fn effective_summarize_threshold_caps_at_the_proxy_ceiling() {
        // 90% of 500k = 450k, above the 340k ceiling − 32k margin → the
        // cap binds; 30% of 500k = 150k → the product binds; no ceiling →
        // the raw product.
        assert_eq!(
            ContextManager::effective_summarize_threshold(500_000, 0.9, Some(340_000)),
            340_000 - crate::provider::PROXY_CACHE_PRESSURE_MARGIN_TOKENS
        );
        assert_eq!(
            ContextManager::effective_summarize_threshold(500_000, 0.3, Some(340_000)),
            150_000
        );
        assert_eq!(
            ContextManager::effective_summarize_threshold(500_000, 0.5, None),
            250_000
        );
    }

    #[test]
    fn count_tokens_nonempty() {
        let messages = vec![Message::user_text("hello world this is a test message")];
        let count = ContextManager::count_tokens(&messages);
        assert!(count > 0);
    }

    #[test]
    fn preflight_enabled_by_default() {
        let cm = ContextManager::new(128_000, 0.5);
        assert!(cm.preflight_compact(), "preflight should be on by default");
        assert_eq!(cm.compact_headroom_tokens(), 32_000);
    }

    #[test]
    fn hard_ceiling_is_max_minus_headroom() {
        let cm = ContextManager::new(262_144, 0.3).with_preflight(true, 32_000);
        // hard_ceiling = max_tokens - headroom = 262144 - 32000 = 230144
        assert_eq!(cm.hard_ceiling(), 230_144);
    }

    #[test]
    fn with_preflight_overrides_defaults() {
        let cm = ContextManager::new(128_000, 0.5).with_preflight(false, 16_000);
        assert!(!cm.preflight_compact());
        assert_eq!(cm.compact_headroom_tokens(), 16_000);
        assert_eq!(cm.hard_ceiling(), 128_000 - 16_000);
    }

    #[test]
    fn hard_ceiling_saturates_at_zero() {
        // When headroom >= max_tokens, hard_ceiling is 0 (not underflow).
        let cm = ContextManager::new(10_000, 0.5).with_preflight(true, 20_000);
        assert_eq!(cm.hard_ceiling(), 0);
    }

    #[test]
    fn compact_truncates_old_tool_results_beyond_keep() {
        // Tool results beyond `keep` are truncated to summary_chars + marker.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::tool_result("1", "read_files", &long),
            Message::tool_result("2", "read_files", &long),
            Message::tool_result("3", "read_files", &long),
            Message::tool_result("4", "read_files", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 2, 2, 100);
        assert_eq!(truncated, 2);
        // First two (old) are truncated; last two (kept) are intact.
        assert!(messages[0].content.as_text().contains(COMPACTED_MARKER));
        assert!(messages[1].content.as_text().contains(COMPACTED_MARKER));
        assert!(!messages[2].content.as_text().contains(COMPACTED_MARKER));
        assert!(!messages[3].content.as_text().contains(COMPACTED_MARKER));
        // tool_call_id + name preserved on the truncated ones.
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("1"));
        assert_eq!(messages[0].name.as_deref(), Some("read_files"));
    }

    #[test]
    fn compact_keeps_recent_tool_results_intact() {
        // The last `keep` tool results are NOT truncated.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::tool_result("1", "search", &long),
            Message::tool_result("2", "search", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 2, 2, 100);
        assert_eq!(truncated, 0);
        assert!(!messages[0].content.as_text().contains(COMPACTED_MARKER));
        assert!(!messages[1].content.as_text().contains(COMPACTED_MARKER));
        // With exactly `keep` tool results, nothing is compacted.
        assert_eq!(messages[0].content.as_text(), long);
    }

    #[test]
    fn compact_is_idempotent() {
        // Calling twice produces the same result — the marker is detected.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::tool_result("1", "read_files", &long),
            Message::tool_result("2", "read_files", &long),
            Message::tool_result("3", "read_files", &long),
        ];
        let first = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(first, 2);
        let after_first = messages[0].content.as_text();
        let second = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(second, 0);
        let after_second = messages[0].content.as_text();
        assert_eq!(
            after_first, after_second,
            "second pass must not re-truncate"
        );
    }

    #[test]
    fn compact_skips_short_tool_results() {
        // Tool results shorter than summary_chars + 100 are not truncated.
        let short = "x".repeat(150);
        let mut messages = vec![
            Message::tool_result("1", "read_files", &short),
            Message::tool_result("2", "read_files", &short),
        ];
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(truncated, 0);
        // 150 <= 100 + 100 = 200, so not truncated.
        assert!(!messages[0].content.as_text().contains(COMPACTED_MARKER));
    }

    #[test]
    fn compact_preserves_tool_call_id_and_name() {
        // The tool_call_id and name fields are preserved after truncation.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::tool_result("call-42", "git_read", &long),
            Message::tool_result("call-43", "git_read", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 50);
        assert_eq!(truncated, 1);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("call-42"));
        assert_eq!(messages[0].name.as_deref(), Some("git_read"));
    }

    /// Regression (2027-01-23 runaway-tool-output report, L3): the ingestion
    /// cap must bound a result the instant it enters the conversation — by the
    /// time compaction could trim it, the request carrying it has already been
    /// rejected. Under the cap the text must pass through BYTE-FOR-BYTE (the
    /// substring classifiers and prefix caching depend on it); over the cap the
    /// head survives with a trailer naming the cap and the dropped count.
    #[test]
    fn cap_tool_result_text_bounds_at_ingestion() {
        // Under the cap: identical, byte for byte (including a result exactly
        // at the cap, which must NOT be touched).
        let under = "line of output\n".repeat(100);
        assert_eq!(cap_tool_result_text(&under), under);
        let exact = "x".repeat(TOOL_RESULT_MAX_CHARS);
        assert_eq!(cap_tool_result_text(&exact), exact);

        // Over the cap: head kept, trailer present, dropped count reported.
        let over = "x".repeat(TOOL_RESULT_MAX_CHARS + 37);
        let capped = cap_tool_result_text(&over);
        assert!(capped.starts_with(&"x".repeat(TOOL_RESULT_MAX_CHARS)));
        assert!(
            capped.contains("truncated at 100000 chars"),
            "the trailer must name the cap: {}",
            &capped[TOOL_RESULT_MAX_CHARS..]
        );
        assert!(
            capped.contains("37 of 100037 chars dropped"),
            "the trailer must report the dropped count: {}",
            &capped[TOOL_RESULT_MAX_CHARS..]
        );
        assert!(capped.contains("re-run the tool"));
        // Multi-byte text must be cut on a char boundary, not a byte one.
        let wide = "é".repeat(TOOL_RESULT_MAX_CHARS + 10);
        let capped_wide = cap_tool_result_text(&wide);
        assert!(
            capped_wide.starts_with(&"é".repeat(TOOL_RESULT_MAX_CHARS)),
            "the head must be whole characters"
        );
    }

    /// Regression (2027-01-23 runaway-tool-output report, L3): a single
    /// oversized result must be cut back even while the hysteresis gate is a
    /// no-op and even INSIDE the keep window — a monster left intact is what
    /// wedges the context, and waiting for it to age out is what fails. Normal
    /// results must stay byte-identical so prefix caching holds.
    #[test]
    fn compact_caps_oversized_results_inside_keep_window() {
        let monster = "x".repeat(TOOL_RESULT_MAX_CHARS + 5_000);
        let normal = "y".repeat(5_000);
        let args = "{\"path\":\"src/agent/context.rs\"}";
        let mut messages = vec![
            Message::assistant("read", vec![crate::provider::ToolCall::new("c1", "read_files", args)]),
            Message::tool_result("c1", "read_files", &normal),
            Message::assistant("read", vec![crate::provider::ToolCall::new("c2", "read_files", args)]),
            Message::tool_result("c2", "read_files", &monster),
        ];
        // keep_high = 2 → the hysteresis gate counts 2 intact results and is a
        // no-op; the oversize rule must still fire on the monster only.
        let truncated = compact_old_tool_results(&mut messages, 2, 2, 1_000);
        assert_eq!(truncated, 1, "only the monster must be cut");
        let monster_text = messages[3].content.as_text();
        assert!(
            monster_text.contains(COMPACTED_MARKER),
            "the monster must be marked"
        );
        assert!(
            monster_text.contains("re-read"),
            "the monster must keep its re-read pointer: {}",
            &monster_text[monster_text.len().saturating_sub(200)..]
        );
        assert!(
            monster_text.chars().count() < TOOL_RESULT_MAX_CHARS + 200,
            "the monster must be cut back to the cap: {} chars",
            monster_text.chars().count()
        );
        assert_eq!(
            messages[1].content.as_text(),
            normal,
            "a normal result inside the keep window must stay byte-identical"
        );
        // Idempotent: a second pass truncates nothing more.
        assert_eq!(compact_old_tool_results(&mut messages, 2, 2, 1_000), 0);
    }

    /// Regression (2027-01-23 runaway-tool-output report, L3): an image-bearing
    /// tool result used to be skipped outright by compaction
    /// (`MessageContent::Parts(_) => continue`), so image results could never
    /// shrink — however many the history held. Once such a result is old enough
    /// to compact, its payload must be dropped and replaced with the marker.
    #[test]
    fn compact_old_tool_results_replaces_image_parts_with_marker() {
        let payload = "A".repeat(20_000);
        let image_result = Message {
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "screenshot of the failing test".into(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: format!("data:image/png;base64,{payload}"),
                    },
                },
            ]),
            ..Message::tool_result("c1", "read_files", "")
        };
        let mut messages = vec![
            image_result,
            Message::tool_result("c2", "read_files", "x".repeat(5_000)),
        ];
        let before = ContextManager::count_tokens(&messages);
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 1_000);
        assert_eq!(truncated, 1, "the old image result must be compacted");
        let text = messages[0].content.as_text();
        assert!(
            text.contains(COMPACTED_MARKER),
            "the image result must be marked: {text}"
        );
        assert!(
            text.contains("screenshot of the failing test"),
            "the text that came with the image must survive"
        );
        assert!(
            !text.contains("data:image/png;base64"),
            "the base64 payload must be gone"
        );
        let after = ContextManager::count_tokens(&messages);
        assert!(
            after < before,
            "dropping the payload must shrink the history: {before} → {after}"
        );
        // Pairing survives, and a second pass is a no-op.
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("c1"));
        assert_eq!(compact_old_tool_results(&mut messages, 1, 1, 1_000), 0);
    }

    #[test]
    fn compact_below_high_water_mark_mutates_nothing() {
        // Build a conversation with system, user, and 14 fat tool results.
        let mut messages = vec![
            Message::text(Role::System, "system head"),
            Message::text(Role::User, "user task"),
        ];
        for i in 0..14 {
            let id = format!("call_{i}");
            let content = format!("result {i}: {}", "x".repeat(2000));
            messages.push(Message::tool_result(id, "read_files", content));
        }
        let before = messages.clone();

        let truncated = compact_old_tool_results(&mut messages, 10, 20, 500);
        assert_eq!(truncated, 0);

        assert_eq!(messages.len(), before.len());
        for (i, (m, b)) in messages.iter().zip(&before).enumerate() {
            assert_eq!(m.role, b.role, "message {i} role changed");
            assert_eq!(
                m.content.as_text(),
                b.content.as_text(),
                "message {i} content was mutated below the high-water mark"
            );
        }
    }

    #[test]
    fn short_results_do_not_hold_the_gate_open() {
        // Round-6 cache-hit analysis (2027-01-11): `truncate_tool_result_at`
        // refuses a result of <= summary_chars + 100 AND does not mark it, so
        // short results never leave the intact population. Counting them kept
        // the gate permanently open, and the sliding keep window then rewrote
        // one already-sent tool result on EVERY request — breaking the
        // provider prefix cache each time (70-78% hit where the same session
        // reaches 99% whenever the prefix stays stable).
        let mut messages = vec![
            Message::text(Role::System, "system head"),
            Message::text(Role::User, "user task"),
        ];
        // Two truncatable results FIRST, so the pre-fix window (keep=10 of the
        // intact population) reaches them.
        for i in 0..2 {
            messages.push(Message::tool_result(
                format!("long_{i}"),
                "read_files",
                format!("big result {i}: {}", "x".repeat(5_000)),
            ));
        }
        // 24 short results: unmarkable by construction, permanently intact.
        for i in 0..24 {
            messages.push(Message::tool_result(
                format!("short_{i}"),
                "read_files",
                format!("small result {i}: {}", "s".repeat(200)),
            ));
        }
        let before = messages.clone();

        let truncated = compact_old_tool_results(&mut messages, 10, 20, 500);

        assert_eq!(
            truncated, 0,
            "2 truncatable results must not open a keep_high=20 gate: the gate \
             has to measure what it can actually shrink, not the intact count"
        );
        for (i, (m, b)) in messages.iter().zip(&before).enumerate() {
            assert_eq!(
                m.content.as_text(),
                b.content.as_text(),
                "message {i} was rewritten although nothing was truncatable"
            );
        }
    }

    #[test]
    fn compact_fires_at_high_water_mark() {
        // 21 intact fat tool results: exceeds keep_high=20 -> cuts back to keep=10 in one pass.
        let mut messages = vec![
            Message::text(Role::System, "system head"),
            Message::text(Role::User, "user task"),
        ];
        for i in 0..21 {
            let id = format!("call_{i}");
            let content = format!("result {i}: {}", "x".repeat(2000));
            messages.push(Message::tool_result(id, "read_files", content));
        }

        // 21 intact > keep_high (20) -> cuts 21 - 10 = 11 results.
        let truncated = compact_old_tool_results(&mut messages, 10, 20, 500);
        assert_eq!(truncated, 11);

        // Tool results indices in `messages` are 2..23 (21 results).
        // First 11 (indices 2..13) are truncated.
        for i in 2..13 {
            assert!(
                messages[i].content.as_text().contains(COMPACTED_MARKER),
                "message {i} should be truncated"
            );
        }
        // Last 10 (indices 13..23) are intact.
        for i in 13..23 {
            assert!(
                !messages[i].content.as_text().contains(COMPACTED_MARKER),
                "message {i} should be intact"
            );
        }

        // A second call is an idempotent no-op because now only 10 intact results remain (<= 20).
        let second = compact_old_tool_results(&mut messages, 10, 20, 500);
        assert_eq!(second, 0);
    }

    #[test]
    fn compact_appends_reread_pointer_for_read_files() {
        // D2: a truncated read_files result carries a one-line pointer naming
        // the file + line range, built from the originating tool call's
        // arguments in the preceding assistant message.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::assistant(
                "reading",
                vec![crate::provider::ToolCall::new(
                    "1",
                    "read_files",
                    r#"{"path":"src/foo.rs","start_line":10,"max_lines":50}"#,
                )],
            ),
            Message::tool_result("1", "read_files", &long),
            Message::tool_result("2", "read_files", &long),
            Message::tool_result("3", "read_files", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(truncated, 2);
        let text = messages[1].content.as_text();
        assert!(text.contains(COMPACTED_MARKER));
        assert!(
            text.contains("[re-read: read_files src/foo.rs:10-59]"),
            "truncated read_files result must name the file + line range: {text}"
        );
        // The kept (newest) result is untouched.
        assert!(!messages[3].content.as_text().contains(COMPACTED_MARKER));
    }

    #[test]
    fn compact_appends_rerun_pointer_for_search() {
        // D2: a truncated search result carries the query (pattern + glob).
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::assistant(
                "searching",
                vec![crate::provider::ToolCall::new(
                    "s1",
                    "search",
                    r#"{"pattern":"compact_old_tool_results","glob":"**/*.rs"}"#,
                )],
            ),
            Message::tool_result("s1", "search", &long),
            Message::tool_result("s2", "search", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(truncated, 1);
        let text = messages[1].content.as_text();
        assert!(text.contains(COMPACTED_MARKER));
        assert!(
            text.contains("[re-run: search pattern=\"compact_old_tool_results\" glob=\"**/*.rs\"]"),
            "truncated search result must name the query: {text}"
        );
    }

    #[test]
    fn compact_pointer_keeps_idempotency() {
        // D2: the pointer rides after the marker and never contains it, so a
        // second pass is still a byte-identical no-op.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::assistant(
                "reading",
                vec![crate::provider::ToolCall::new(
                    "1",
                    "read_files",
                    r#"{"path":"src/foo.rs","start_line":10,"max_lines":50}"#,
                )],
            ),
            Message::tool_result("1", "read_files", &long),
            Message::tool_result("2", "read_files", &long),
        ];
        let first = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(first, 1);
        let after_first = messages[1].content.as_text();
        assert!(after_first.contains("[re-read: read_files src/foo.rs:10-59]"));
        let second = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(second, 0);
        assert_eq!(
            messages[1].content.as_text(),
            after_first,
            "second pass must not re-truncate or re-append the pointer"
        );
    }

    #[test]
    fn compact_no_pointer_without_matching_tool_call() {
        // D2: a result whose id has no matching assistant tool call, or a
        // tool with no pointer format, gets the plain marker — no pointer.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::assistant(
                "git",
                vec![crate::provider::ToolCall::new("g1", "git_read", r#"{"op":"diff"}"#)],
            ),
            Message::tool_result("orphan", "read_files", &long),
            Message::tool_result("g1", "git_read", &long),
            Message::tool_result("3", "read_files", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(truncated, 2);
        // Orphan id: no matching tool call -> plain marker.
        let orphan = messages[1].content.as_text();
        assert!(orphan.contains(COMPACTED_MARKER));
        assert!(!orphan.contains("[re-read:"), "orphan id must not gain a pointer");
        // git_read has a matching tool call but no pointer format -> plain marker.
        let git = messages[2].content.as_text();
        assert!(git.contains(COMPACTED_MARKER));
        assert!(!git.contains("[re-run:"), "git_read has no pointer format");
    }

    #[test]
    fn compact_appends_reread_pointer_for_file_read() {
        // D2 LOW 2: file_read takes the same {path, start_line, max_lines}
        // args and gets the same pointer, prefixed with its own tool name.
        let long = "x".repeat(800);
        let mut messages = vec![
            Message::assistant(
                "reading",
                vec![crate::provider::ToolCall::new(
                    "1",
                    "file_read",
                    r#"{"path":"src/bar.rs","start_line":5,"max_lines":20}"#,
                )],
            ),
            Message::tool_result("1", "file_read", &long),
            Message::tool_result("2", "file_read", &long),
        ];
        let truncated = compact_old_tool_results(&mut messages, 1, 1, 100);
        assert_eq!(truncated, 1);
        let text = messages[1].content.as_text();
        assert!(text.contains(COMPACTED_MARKER));
        assert!(
            text.contains("[re-read: file_read src/bar.rs:5-24]"),
            "truncated file_read result must name the tool + file + line range: {text}"
        );
    }

    #[test]
    fn rerun_pointer_normalizes_degenerate_read_args() {
        // D2 LOW 1: start_line 0 / max_lines 0 must not underflow (debug
        // panic) and must normalize like the tool does (start_line 0 -> 1).
        let p = rerun_pointer("read_files", r#"{"path":"a.rs","start_line":0,"max_lines":0}"#);
        assert_eq!(p.as_deref(), Some("[re-read: read_files a.rs]"));
        let p = rerun_pointer("read_files", r#"{"path":"a.rs","start_line":0,"max_lines":50}"#);
        assert_eq!(p.as_deref(), Some("[re-read: read_files a.rs:1-50]"));
        // Huge values saturate instead of overflowing.
        let p = rerun_pointer(
            "read_files",
            r#"{"path":"a.rs","start_line":10,"max_lines":18446744073709551615}"#,
        );
        assert_eq!(
            p.as_deref(),
            Some("[re-read: read_files a.rs:10-18446744073709551614]")
        );
    }

    #[test]
    fn rerun_pointer_covers_batch_form_and_literal_flag() {
        // D2: the files[] batch form (3-spec cap + "(+N more)") and the
        // search literal flag (a re-run must not reinterpret the pattern
        // as a regex).
        let args = r#"{"files":[
            {"path":"a.rs","start_line":1,"max_lines":10},
            {"path":"b.rs"},
            {"path":"c.rs","start_line":5,"max_lines":5},
            {"path":"d.rs"}]}"#;
        let p = rerun_pointer("read_files", args);
        assert_eq!(
            p.as_deref(),
            Some("[re-read: read_files a.rs:1-10, b.rs, c.rs:5-9 (+1 more)]")
        );
        let p = rerun_pointer("search", r#"{"pattern":"$5.00","literal":true}"#);
        assert_eq!(
            p.as_deref(),
            Some("[re-run: search pattern=\"$5.00\" literal=true]")
        );
    }

    /// Regression (2026-08-22, BPE re-tokenization fix): incremental updates
    /// must stay byte-for-byte identical to a fresh full recount as the
    /// conversation grows the way the turn loop grows it — head replaced in
    /// place, then tool results + assistant turns appended.
    #[test]
    fn token_accounting_matches_full_recount_with_appends() {
        let mut messages = vec![
            Message::text(Role::System, "system head v1"),
            Message::text(Role::User, "hello"),
            Message::text(Role::Assistant, "working on it"),
        ];
        let mut acc = TokenAccounting::new();
        let (total, breakdown, full) = acc.update(&messages);
        assert!(full, "first update is a full pass");
        assert_eq!(total, ContextManager::count_tokens(&messages));
        assert_eq!(breakdown, ContextManager::count_tokens_by_role(&messages));

        // The turn loop rebuilds the head in place, then appends a tool result
        // and an assistant turn (with tool calls + reasoning — reasoning is
        // not tokenized, mirroring the full recount).
        messages[0].content = MessageContent::text("system head v2 (longer)");
        messages.push(Message::tool_result("call_1", "search", "a sizeable tool result body ".repeat(50)));
        messages.push(Message {
            reasoning_content: Some("some reasoning text".into()),
            ..Message::assistant(
                "found it",
                vec![crate::provider::ToolCall::new(
                    "call_2",
                    "file_read",
                    r#"{"path":"src/lib.rs"}"#,
                )],
            )
        });
        let (total, breakdown, full) = acc.update(&messages);
        assert!(!full, "append-only update must not be a full pass");
        assert_eq!(total, ContextManager::count_tokens(&messages));
        assert_eq!(breakdown, ContextManager::count_tokens_by_role(&messages));
    }

    /// Regression (2026-08-22, BPE re-tokenization fix): an unchanged list
    /// must reuse the cached accounting — no re-encoding of the history.
    #[test]
    fn token_accounting_skips_rescan_when_unchanged() {
        let messages = vec![Message::text(Role::System, "head"), Message::text(Role::User, "hi")];
        let mut acc = TokenAccounting::new();
        let (t1, b1, f1) = acc.update(&messages);
        assert!(f1);
        let (t2, b2, f2) = acc.update(&messages);
        assert!(!f2, "an unchanged list must reuse the cached accounting");
        assert_eq!((t1, b1), (t2, b2));
    }

    /// A list that shrank (a conversation rewrite) must trigger a full pass —
    /// deltas from a stale prefix would be wrong.
    #[test]
    fn token_accounting_full_passes_after_rewrite() {
        let mut messages = vec![
            Message::text(Role::System, "head"),
            Message::text(Role::User, "hi"),
            Message::text(Role::Assistant, "ok"),
        ];
        let mut acc = TokenAccounting::new();
        acc.update(&messages);
        messages.truncate(1); // a rewrite that shortened the list
        let (total, breakdown, full) = acc.update(&messages);
        assert!(full, "a shortened list must trigger a full pass");
        assert_eq!(total, ContextManager::count_tokens(&messages));
        assert_eq!(breakdown, ContextManager::count_tokens_by_role(&messages));
    }

    /// A head swap must only re-tokenize the head — the untouched history's
    /// buckets survive unchanged.
    #[test]
    fn token_accounting_head_swap_only_retokens_the_head() {
        let mut messages = vec![
            Message::text(Role::System, "head v1"),
            Message::text(Role::User, "hi"),
        ];
        let mut acc = TokenAccounting::new();
        acc.update(&messages);
        let user_before = acc.breakdown.user;
        messages[0].content = MessageContent::text("head v2 with more words");
        let (total, breakdown, full) = acc.update(&messages);
        assert!(!full);
        assert_eq!(
            breakdown.user, user_before,
            "unchanged history must not be re-tokenized"
        );
        assert_eq!(total, ContextManager::count_tokens(&messages));
        assert_eq!(breakdown, ContextManager::count_tokens_by_role(&messages));
    }

    /// Regression (2026-08-22, review H1): fresh sessions start user-first,
    /// then the turn loop INSERTs the system head at index 0 (turn.rs), which
    /// shifts every existing index right. The delta path must fall back to a
    /// full pass across that transition — trusting the old counted prefix
    /// would double-count the last pre-insert message on every later update.
    #[test]
    fn token_accounting_recovers_after_head_insertion() {
        let mut messages = vec![
            Message::text(Role::User, "hi"),
            Message::text(Role::Assistant, "ok"),
        ];
        let mut acc = TokenAccounting::new();
        acc.update(&messages);
        // The turn loop's head-prepend branch.
        messages.insert(0, Message::text(Role::System, "head"));
        messages.push(Message::text(Role::Tool, "tool result"));
        let (total, breakdown, _) = acc.update(&messages);
        assert_eq!(total, ContextManager::count_tokens(&messages));
        assert_eq!(breakdown, ContextManager::count_tokens_by_role(&messages));
    }

    /// Regression (2026-12-23, backlog da4fc87d): stored assistant turns echo
    /// `reasoning_content` (DeepSeek thinking mode HTTP-400s without it) and
    /// the provider counts that text in `usage.prompt_tokens` — but the
    /// accounting skipped it, so the ctx bar read tens of thousands of tokens
    /// low on reasoning-heavy sessions. The per-message count must include
    /// `reasoning_content` alongside the content text.
    #[test]
    fn token_accounting_counts_reasoning_content() {
        let reasoning = "step by step reasoning text ".repeat(50);
        let mut with = Message::assistant_text("ok");
        with.reasoning_content = Some(reasoning.clone());
        let without = Message::assistant_text("ok");
        let messages = vec![Message::system("head"), with];
        let baseline = vec![Message::system("head"), without];

        let mut acc = TokenAccounting::new();
        let (total, breakdown, _) = acc.update(&messages);
        let mut base_acc = TokenAccounting::new();
        let (base_total, base_breakdown, _) = base_acc.update(&baseline);

        // Any sane tokenizer spends well over len/6 tokens on ASCII prose
        // (cl100k gives ~5.6 chars/token here, the chars/4 fallback gives 4);
        // the old code added 0, so this pins the reasoning text into the
        // count under either counting path.
        let min_reasoning_tokens = (reasoning.len() / 6) as u32;
        assert!(
            total >= base_total + min_reasoning_tokens as usize,
            "reasoning_content must be counted: total={total}, base={base_total}, \
             reasoning_chars={}",
            reasoning.len()
        );
        assert!(
            breakdown.assistant >= base_breakdown.assistant + min_reasoning_tokens,
            "the assistant bucket must carry the reasoning tokens too: {} vs base {}",
            breakdown.assistant, base_breakdown.assistant
        );
    }

    /// Regression (2027-01-23 runaway-tool-output report, L1): an image block
    /// is billed by the provider but was invisible to token accounting —
    /// [`MessageContent::as_text`] drops non-text parts, so a message carrying
    /// a 400KB base64 `data:` URL counted as a handful of tokens. Image-heavy
    /// contexts (screenshots, vision turns) therefore never tripped the
    /// summarize trigger or the preflight ceiling until the provider itself
    /// rejected the request. Every part must be priced, on both the BPE and
    /// the chars/4 fallback path, and the breakdown must stay consistent with
    /// the total.
    #[test]
    fn image_part_is_priced_in_token_accounting() {
        // ~400KB of base64 → ~100K tokens at 4 chars/token.
        let payload = "A".repeat(400_000);
        let msg = Message {
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "look at this".into(),
                },
                ContentPart::ImageUrl {
                    image_url: ImageUrl {
                        url: format!("data:image/png;base64,{payload}"),
                    },
                },
            ]),
            ..Message::user_text("")
        };
        let payload_price = payload.len().div_ceil(4) as u32;

        // The BPE path must price the payload, not just the text part, and it
        // must price it on the CONSERVATIVE 4 chars/token law: base64 is
        // high-entropy (mixed case, digits, `+/`), so it needs MORE tokens per
        // char than prose — pricing it lower would undercount exactly the
        // image-heavy context this guards. The window keeps the price from
        // drifting into pure inflation.
        let counted = count_message_tokens(try_tiktoken_bpe(), &msg);
        assert!(
            counted >= payload_price,
            "a 400KB base64 image part must be priced at >= {payload_price}: {counted} tokens"
        );
        assert!(
            counted <= payload_price + 64,
            "the image price must not inflate past the payload: {counted} vs {payload_price}"
        );
        // The no-BPE fallback prices it too (chars/4, no overhead).
        let fallback = count_message_tokens(None, &msg);
        assert!(
            fallback >= payload_price,
            "the chars/4 fallback must price image payloads: {fallback}"
        );

        // The breakdown totals stay consistent with the per-message sum.
        let messages = vec![Message::system("head"), msg];
        let (total, breakdown) = ContextManager::count_tokens_and_breakdown(&messages);
        assert!(
            breakdown.user >= payload_price,
            "user bucket: {}",
            breakdown.user
        );
        assert_eq!(
            total,
            breakdown.system as usize + breakdown.user as usize,
            "breakdown must sum to the total"
        );
    }

    /// Regression (2026-12-23, backlog da4fc87d): every request carries the
    /// tool-schema array (name + description + parameters JSON per tool) and
    /// the provider counts it in `usage.prompt_tokens`, but the accounting
    /// only ever summed messages — on a tool-heavy session that's tens of
    /// thousands of tokens missing from the ctx bar. The caller-supplied
    /// schema overhead must be additive to the total while the per-role
    /// buckets stay message-only.
    #[test]
    fn token_accounting_includes_tools_overhead() {
        let messages = vec![Message::system("head"), Message::user_text("hi")];
        let mut acc = TokenAccounting::new();
        let (base, base_breakdown, _) = acc.update(&messages);

        acc.set_tools_tokens(1_234);
        let (total, breakdown, _) = acc.update(&messages);

        assert_eq!(
            total,
            base + 1_234,
            "tools overhead must be added to the total"
        );
        assert_eq!(
            breakdown.system, base_breakdown.system,
            "role buckets stay message-only"
        );
    }

    #[test]
    fn should_summarize_under_threshold() {
        let cm = ContextManager::new(100_000, 0.5);
        let messages = vec![Message::user_text("short message")];
        assert!(!cm.should_summarize(&messages));
    }

    #[test]
    fn should_summarize_over_threshold() {
        let cm = ContextManager::new(100, 0.5); // threshold = 50 tokens
                                                // Create a large message that exceeds 50 tokens.
        let big_text = "word ".repeat(200);
        let messages = vec![Message::user_text(big_text)];
        assert!(cm.should_summarize(&messages));
    }

    #[test]
    fn summarize_at_uses_fill_rate() {
        let cm = ContextManager::new(128_000, 0.5);
        assert_eq!(cm.summarize_at(), 64_000);
        let cm2 = ContextManager::new(128_000, 0.75);
        assert_eq!(cm2.summarize_at(), 96_000);
    }

    #[test]
    fn proxy_cache_ceiling_caps_effective_summarize_at() {
        // Raw trigger stays the fill-rate product; only the effective trigger
        // (used by both summarize check sites) is capped below the cliff.
        let cm = ContextManager::new(1_000_000, 0.5);
        assert_eq!(cm.summarize_at(), 500_000);
        let cm = cm.with_proxy_cache_ceiling(Some(crate::provider::PROXY_CACHE_CEILING_TOKENS));
        assert_eq!(
            cm.effective_summarize_at(),
            crate::provider::PROXY_CACHE_CEILING_TOKENS
                - crate::provider::PROXY_CACHE_PRESSURE_MARGIN_TOKENS
        );
        // should_summarize consults the effective trigger: a message pushing
        // the count past the capped trigger fires, a short one does not.
        let big = "word ".repeat(400_000); // ~2M chars → ~500K tokens (chars/4)
        assert!(cm.should_summarize(&[Message::user_text(big)]));
        assert!(!cm.should_summarize(&[Message::user_text("short message")]));
        // Small models: the fill-rate product sits below the cliff, so the
        // ceiling changes nothing.
        let small = ContextManager::new(128_000, 0.3)
            .with_proxy_cache_ceiling(Some(crate::provider::PROXY_CACHE_CEILING_TOKENS));
        assert_eq!(small.effective_summarize_at(), 38_400);
        assert_eq!(small.summarize_at(), 38_400);
        // No ceiling: raw behavior preserved everywhere.
        let uncapped = ContextManager::new(1_000_000, 0.5);
        assert_eq!(uncapped.effective_summarize_at(), 500_000);
        assert_eq!(uncapped.proxy_cache_ceiling(), None);
    }

    // --- Mock provider for summarization tests ---

    use crate::provider::{Capabilities, ProviderKind, ToolChoice, ToolSchema};
    use crate::runtime::AgentCommand;
    use async_trait::async_trait;
    use futures::stream::BoxStream;

    /// A mock provider that returns a canned stream of LlmEvents. Used to test
    /// summarization without a real LLM.
    struct SummaryMockProvider {
        events: Vec<LlmEvent>,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for SummaryMockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-summary"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            Ok(Box::pin(futures::stream::iter(self.events.clone())))
        }
    }

    /// A mock provider whose `complete` call itself FAILS — the shape of a
    /// provider that rejects the summarization request with an HTTP 400 before
    /// any stream exists (the real-world size-rejection path).
    struct FailingCompleteMock {
        error: String,
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for FailingCompleteMock {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-failing-complete"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            Err(crate::error::Error::Provider(self.error.clone()))
        }
    }

    /// A mock provider that records the messages of the last `complete` call
    /// (so a test can assert what the summarization prompt actually was) and
    /// otherwise behaves like [`SummaryMockProvider`].
    struct RecordingSummaryMock {        events: Vec<LlmEvent>,
        caps: Capabilities,
        requested: std::sync::Arc<std::sync::Mutex<Option<Vec<Message>>>>,
    }

    #[async_trait]
    impl LlmClient for RecordingSummaryMock {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-recording-summary"
        }
        async fn complete(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            *self.requested.lock().unwrap() = Some(messages.to_vec());
            Ok(Box::pin(futures::stream::iter(self.events.clone())))
        }
    }

    /// A mock provider whose stream yields one delta, then parks forever
    /// (pending) so an interrupt sent mid-stream is observed by select!.
    struct HangingMockProvider {
        caps: Capabilities,
    }

    #[async_trait]
    impl LlmClient for HangingMockProvider {
        fn capabilities(&self) -> &Capabilities {
            &self.caps
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "mock-hanging"
        }
        async fn complete(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _tool_choice: Option<ToolChoice>,
        ) -> Result<BoxStream<'_, LlmEvent>> {
            // First emit a delta, then hang forever (the interrupt must fire).
            use futures::StreamExt;
            let stream = futures::stream::once(async {
                LlmEvent::TextDelta {
                    text: "partial".into(),
                }
            })
            .chain(futures::stream::pending::<LlmEvent>());
            Ok(Box::pin(stream))
        }
    }

    fn make_messages(n: usize) -> Vec<Message> {
        let mut msgs = vec![Message::system("system prompt")];
        for i in 0..n {
            msgs.push(Message::user_text(format!("message {i}")));
        }
        msgs
    }

    #[tokio::test]
    async fn summarize_with_interrupt_produces_summary() {
        // No interrupt → the summary is produced and the system prompt +
        // recent messages are preserved.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "This is the summary.".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_messages(10);
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);

        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 2, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());
        // system + summary + 2 recent = 4 messages.
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].role, Role::System);
        assert_eq!(result[0].content.as_text(), "system prompt");
        assert!(result[1].content.as_text().contains("This is the summary."));
        // The last 2 are the recent messages.
        assert_eq!(result[2].content.as_text(), "message 8");
        assert_eq!(result[3].content.as_text(), "message 9");
    }

    #[tokio::test]
    async fn summarize_with_interrupt_aborts_on_interrupt() {
        // An Interrupt arriving mid-summarization must abandon the summary and
        // return the original messages unchanged (no data loss).
        let provider = HangingMockProvider {
            caps: Capabilities::openai(),
        };
        let messages = make_messages(10);
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);

        // Run summarization in a task so we can send the interrupt mid-stream.
        let cm_clone = ContextManager::new(128_000, 0.5);
        let messages_clone = messages.clone();
        let handle = tokio::spawn(async move {
            cm_clone
                .summarize_with_interrupt(&messages_clone, 2, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
                .await
        });

        // Give the stream a moment to start, then interrupt.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();

        let (result, buffered, stop, _) = handle.await.unwrap().unwrap();
        assert!(buffered.is_empty());
        assert_eq!(stop, Some(crate::agent::StopReason::Interrupt));
        // Original messages returned unchanged — no summary applied.
        assert_eq!(result.len(), messages.len());
        assert_eq!(result[0].content.as_text(), "system prompt");
        // No summary message was inserted.
        assert_eq!(result[1].content.as_text(), "message 0");
    }

    #[tokio::test]
    async fn summarize_with_interrupt_aborts_on_cancel() {
        // A Cancel must also abort summarization and return original messages.
        let provider = HangingMockProvider {
            caps: Capabilities::openai(),
        };
        let messages = make_messages(8);
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);

        let cm_clone = ContextManager::new(128_000, 0.5);
        let messages_clone = messages.clone();
        let handle = tokio::spawn(async move {
            cm_clone
                .summarize_with_interrupt(&messages_clone, 2, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cmd_tx.send(AgentCommand::Cancel).await.unwrap();

        let (result, buffered, stop, _) = handle.await.unwrap().unwrap();
        assert!(buffered.is_empty());
        assert_eq!(stop, Some(crate::agent::StopReason::Cancel));
        assert_eq!(result.len(), messages.len());
    }

    #[tokio::test]
    async fn summarize_with_interrupt_buffers_suggestions() {
        // A Suggestion arriving mid-summarization is buffered (not treated as
        // an interrupt) and returned to the caller for re-injection.
        let provider = HangingMockProvider {
            caps: Capabilities::openai(),
        };
        let messages = make_messages(10);
        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);

        let cm_clone = ContextManager::new(128_000, 0.5);
        let messages_clone = messages.clone();
        let handle = tokio::spawn(async move {
            cm_clone
                .summarize_with_interrupt(&messages_clone, 2, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cmd_tx
            .send(AgentCommand::Suggestion("use rust 2021".into()))
            .await
            .unwrap();
        // Now interrupt to end the hang.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cmd_tx.send(AgentCommand::Interrupt).await.unwrap();

        let (result, buffered, stop, _) = handle.await.unwrap().unwrap();
        // The suggestion was buffered; the interrupt aborted the summary.
        assert_eq!(buffered.len(), 1);
        assert!(matches!(
            &buffered[0],
            AgentCommand::Suggestion(s) if s.text == "use rust 2021"
        ));
        assert_eq!(stop, Some(crate::agent::StopReason::Interrupt));
        // Original messages returned unchanged.
        assert_eq!(result.len(), messages.len());
    }

    #[tokio::test]
    async fn summarize_with_interrupt_too_few_messages_is_noop() {
        // Not enough messages to summarize → returns original unchanged.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![],
            caps: Capabilities::openai(),
        };
        let messages = make_messages(2); // system + 2 = 3, keep_recent=2 → 3 <= 3
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);
        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 2, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());
        assert_eq!(result.len(), messages.len());
    }

    #[tokio::test]
    async fn summarize_with_interrupt_budgets_prompt_when_history_over_window() {
        // Regression (2027-01-23 user report): with a 7.2M-token tool-role
        // history, the summarization prompt itself exceeded the model window,
        // the provider rejected it (HTTP 400 "token count … longer than the
        // limit"), and compaction failed — the session stayed wedged until
        // /new. The request must be budgeted to the summarizer's OWN window:
        // the cut marches backward (more recent messages stay verbatim), so
        // the prompt that reaches the provider never exceeds the window.
        let cm = ContextManager::new(128_000, 0.5);
        let mut messages = vec![Message::system("system prompt")];
        for i in 0..5 {
            messages.push(Message::user_text(format!("question {i}")));
            messages.push(Message::assistant(
                "using tools".to_string(),
                vec![crate::provider::ToolCall::new(
                    format!("call_{i}"),
                    "read_files",
                    r#"{"path":"a.rs"}"#.to_string(),
                )],
            ));
            // ~325K chars > 81K tokens per result — a fraction of the
            // history is several times the mock's entire window.
            messages.push(Message::tool_result(
                format!("call_{i}"),
                "read_files".to_string(),
                "line of data ".repeat(25_000),
            ));
        }
        messages.push(Message::text(Role::Assistant, "final answer"));
        messages.push(Message::text(Role::User, "recent tail message"));
        let provider = RecordingSummaryMock {
            events: vec![
                LlmEvent::TextDelta {
                    text: "compact summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities {
                max_context: 16_384,
                max_output_tokens: 2_048,
                ..Capabilities::openai()
            },
            requested: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);

        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 6, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());
        assert!(
            result.len() < messages.len(),
            "compaction must have dropped the summarized region"
        );

        // The prompt that reached the provider fits the summarizer window.
        let requested = provider.requested.lock().unwrap();
        let request = requested.as_ref().expect("provider must have been called");
        assert_eq!(request.len(), 1, "summary request is a single user message");
        let prompt = request[0].content.as_text();
        let budget = summary_prompt_budget(&provider.caps);
        let tokens = prompt_text_tokens(try_tiktoken_bpe(), &prompt);
        assert!(
            tokens <= budget,
            "summary prompt must fit the summarizer window: {tokens} > {budget}"
        );

        // The kept tail survived verbatim: system + summary, then a
        // byte-identical SUFFIX of the original conversation. WHICH suffix is
        // the budget march's call — it moves whole messages into the tail to
        // fit the summarizer's window — but that the tail is a contiguous
        // suffix of unrewritten messages, every tool result still paired with
        // its call, is the guarantee.
        assert_eq!(result[0].content.as_text(), "system prompt");
        assert!(result[1].content.as_text().contains("compact summary"));
        let kept_tail: Vec<Message> = result[2..].to_vec();
        let offset = messages.len() - kept_tail.len();
        assert!(
            offset > 1 && offset < messages.len(),
            "compaction must drop the summarized region and keep a tail: kept {} of {} messages",
            kept_tail.len(),
            messages.len()
        );
        for (kept, original) in kept_tail.iter().zip(messages[offset..].iter()) {
            assert_eq!(kept.role, original.role);
            assert_eq!(
                kept.content.as_text(),
                original.content.as_text(),
                "kept tail must be byte-identical to the original"
            );
            assert_eq!(kept.tool_call_id, original.tool_call_id);
        }
        assert_tool_pairing_intact(&result);
    }

    #[tokio::test]
    async fn summarize_with_interrupt_keeps_recent_verbatim_when_region_over_budget() {
        // Many moderately-sized tool results (not one monster): the region
        // alone exceeds the summarizer window, but a shorter region fits —
        // the cut must march back exactly far enough, keeping the rest of
        // the conversation verbatim WITHOUT per-message truncation markers
        // (the budgeted region is still summarized as a whole).
        let cm = ContextManager::new(128_000, 0.5);
        let mut messages = vec![Message::system("system prompt")];
        for i in 0..40 {
            messages.push(Message::user_text(format!("question {i}")));
            messages.push(Message::assistant(
                "searching".to_string(),
                vec![crate::provider::ToolCall::new(
                    format!("call_{i}"),
                    "search",
                    format!(r#"{{"pattern":"hit {i}"}}"#),
                )],
            ));
            // ~2K chars ≈ 500 tokens each — 40 results ≈ 20K tokens, twice
            // the budget (10_240), but each pair fits comfortably.
            messages.push(Message::tool_result(
                format!("call_{i}"),
                "search".to_string(),
                format!("hit line {i}: {} data", "x".repeat(2_000)),
            ));
        }
        messages.push(Message::text(Role::Assistant, "done"));
        messages.push(Message::text(Role::User, "recent tail"));
        let provider = RecordingSummaryMock {
            events: vec![
                LlmEvent::TextDelta {
                    text: "compact summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities {
                max_context: 16_384,
                max_output_tokens: 2_048,
                ..Capabilities::openai()
            },
            requested: std::sync::Arc::new(std::sync::Mutex::new(None)),
        };
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);

        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 6, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());

        // The prompt fits the window and was NOT per-message-truncated.
        let requested = provider.requested.lock().unwrap();
        let request = requested.as_ref().expect("provider must have been called");
        let prompt = request[0].content.as_text();
        let budget = summary_prompt_budget(&provider.caps);
        assert!(
            prompt_text_tokens(try_tiktoken_bpe(), &prompt) <= budget,
            "summary prompt must fit the summarizer window"
        );
        assert!(
            !prompt.contains(SUMMARY_BUDGET_TRUNCATION),
            "the backward march must suffice — no per-message truncation"
        );
        // The summarized region starts at the oldest content …
        assert!(prompt.contains("question 0"));
        // … but the region shrank: content that ended up in the kept tail
        // (verbatim) is not re-summarized.
        assert!(
            !prompt.contains("question 39"),
            "newest content must be kept verbatim, not summarized"
        );

        // The kept tail is the original conversation's suffix, byte-identical
        // (system + summary first).
        assert_eq!(result[0].content.as_text(), "system prompt");
        assert!(result[1].content.as_text().contains("compact summary"));
        let original_tail: Vec<Message> = messages[messages.len() - (result.len() - 2)..].to_vec();
        let kept_tail: Vec<Message> = result[2..].to_vec();
        assert_eq!(kept_tail.len(), original_tail.len());
        for (kept, original) in kept_tail.iter().zip(original_tail.iter()) {
            assert_eq!(kept.role, original.role);
            assert_eq!(
                kept.content.as_text(),
                original.content.as_text(),
                "kept tail must be byte-identical to the original"
            );
            assert_eq!(kept.tool_call_id, original.tool_call_id);
        }
        assert_tool_pairing_intact(&result);
    }

    #[test]
    fn build_summary_prompt_enforces_budget_with_marker() {
        // Direct-caller backstop: a region holding one runaway message (a
        // single unbroken 200K-char run — the size class of a base64 image or
        // a minified dump) truncates per-message with a marker and always fits
        // the budget by construction. The budget is the frame plus a small
        // region allowance, so the per-message backstop is what fires.
        let messages = vec![
            Message::system("system prompt"),
            Message::user_text("what is the plan?"),
            Message::text(Role::Tool, format!("huge result {}", "x".repeat(200_000))),
        ];
        let bpe = try_tiktoken_bpe();
        let budget = summary_prompt_fixed_tokens(&messages, bpe) + 400;
        let prompt = build_summary_prompt(&messages, &messages[1..], budget);
        assert!(
            prompt.contains(SUMMARY_BUDGET_TRUNCATION),
            "over-budget region must carry the truncation marker"
        );
        assert!(
            prompt.contains("what is the plan?"),
            "the fitting part of the region must survive"
        );
        assert!(
            prompt.contains(SUMMARY_BUDGET_NOTE),
            "the omission note must tell the summarizer what was dropped"
        );
        let tokens = prompt_text_tokens(bpe, &prompt);
        assert!(
            tokens <= budget,
            "the built prompt must never exceed its budget: {tokens} > {budget}"
        );
    }

    /// Regression (2027-01-23 runaway-tool-output report): token measurement
    /// must stay LINEAR in the length of a single unbroken run. tiktoken's
    /// byte-pair merge is O(n²) in pretoken length (measured on this machine:
    /// 64K chars ≈ 8.3s, 400K ≈ 5 min), and a runaway tool result is exactly
    /// such a run — so an unbounded measure stalls compaction on the input it
    /// exists to recover from. The cap is generous (the chunked measure needs
    /// well under a second) because it guards a blowup of two orders of
    /// magnitude, not a tight budget.
    #[test]
    fn prompt_text_tokens_stays_linear_on_one_huge_piece() {
        let huge = "x".repeat(128_000);
        let start = std::time::Instant::now();
        let tokens = prompt_text_tokens(try_tiktoken_bpe(), &huge);
        let elapsed = start.elapsed();
        assert!(
            tokens >= 8_000,
            "measurement must stay in the right magnitude: {tokens} tokens for {} chars",
            huge.len()
        );
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "measuring one {}-char pretoken took {elapsed:?} — the quadratic \
             pretoken blowup is back (chunked measurement expects < 1s)",
            huge.len()
        );
    }

    #[tokio::test]
    async fn summarize_produces_summary_preserving_system_and_recent() {
        // The original summarize() (no interrupt handling) should: keep the
        // system prompt (first message), replace the middle with an LLM
        // summary, and keep the most recent `keep_recent` messages verbatim.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "This is the summary.".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_messages(10);
        let result = cm.summarize(&messages, 2, TEST_SENDABLE_BUDGET, &provider).await.unwrap();
        // system + summary + 2 recent = 4 messages.
        assert_eq!(result.len(), 4);
        // System prompt preserved verbatim.
        assert_eq!(result[0].role, Role::System);
        assert_eq!(result[0].content.as_text(), "system prompt");
        // Summary message inserted.
        assert_eq!(result[1].role, Role::System);
        assert!(result[1].content.as_text().contains("This is the summary."));
        // Recent messages preserved verbatim.
        assert_eq!(result[2].content.as_text(), "message 8");
        assert_eq!(result[3].content.as_text(), "message 9");
    }

    #[tokio::test]
    async fn summarize_too_few_messages_is_noop() {
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![],
            caps: Capabilities::openai(),
        };
        let messages = make_messages(2); // system + 2 = 3, keep_recent=2 → 3 <= 3
        let result = cm.summarize(&messages, 2, TEST_SENDABLE_BUDGET, &provider).await.unwrap();
        assert_eq!(result.len(), messages.len());
        // Unchanged.
        assert_eq!(result[0].content.as_text(), "system prompt");
    }

    #[test]
    fn summary_prompt_uses_eight_heading_handoff_format() {
        // Regression guard for the compaction prompt upgrade: the fresh-summary
        // prompt must use the 8-heading handoff structure so a fresh agent can
        // resume without basic questions. Asserts the first + last headings
        // (the sentinel markers) plus the verbatim-preservation rule.
        let messages = make_messages(6);
        let to_summarize = &messages[1..messages.len() - 2];
        let prompt = build_summary_prompt(&messages, to_summarize, usize::MAX);
        assert!(
            prompt.contains("1. **Objective**"),
            "missing Objective heading: {prompt}"
        );
        assert!(
            prompt.contains("8. **Immediate Next Step**"),
            "missing Immediate Next Step heading: {prompt}"
        );
        assert!(
            prompt.contains("Critical References"),
            "missing Critical References heading: {prompt}"
        );
        // The verbatim-preservation rule (no paraphrased paths) survives.
        assert!(prompt.contains("VERBATIM"));
    }

    #[test]
    fn summary_prompt_running_update_keeps_previous_summary() {
        // The running-update path (messages[1] is an existing `## Conversation
        // summary`) must still include the previous summary text AND the new
        // 8-heading format.
        let mut messages = make_messages(6);
        messages.insert(
            1,
            Message::system("## Conversation summary\n\nPRIOR STATE"),
        );
        let to_summarize = &messages[1..messages.len() - 2];
        let prompt = build_summary_prompt(&messages, to_summarize, usize::MAX);
        assert!(prompt.contains("## Previous summary"));
        assert!(prompt.contains("PRIOR STATE"));
        // Still carries the handoff format for the update.
        assert!(prompt.contains("8. **Immediate Next Step**"));
    }

    /// Build a conversation whose naive `len - keep_recent` cut lands ON a
    /// tool message: [system, user, assistant, assistant(tool_calls c1+c2),
    /// tool(c1), tool(c2), assistant, user] — len 8, keep_recent 3 → naive
    /// cut = 5, which is tool(c2), orphaned from its assistant call at [3].
    fn make_tool_batch_messages() -> Vec<Message> {
        let text = |role: Role, t: &str| Message::text(role, t);
        let tool_result = |id: &str, t: &str| Message::tool_result(id.to_string(), "file_read".to_string(), t);
        vec![
            text(Role::System, "system prompt"),
            text(Role::User, "q1"),
            text(Role::Assistant, "working on it"),
            Message::assistant(
                "calling tools",
                vec![
                    crate::provider::ToolCall::new("c1".to_string(), "file_read".to_string(), "{}".to_string()),
                    crate::provider::ToolCall::new("c2".to_string(), "file_read".to_string(), "{}".to_string()),
                ],
            ),
            tool_result("c1", "result 1"),
            tool_result("c2", "result 2"),
            text(Role::Assistant, "done"),
            text(Role::User, "next"),
        ]
    }

    /// Assert every tool message's tool_call_id is carried by an assistant
    /// message in the same history — the invariant providers require
    /// (`validate_request_messages` rejects orphans locally).
    fn assert_tool_pairing_intact(messages: &[Message]) {
        let known: std::collections::HashSet<&str> = messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|tc| tc.id.as_str()))
            .collect();
        for (i, m) in messages.iter().enumerate() {
            if m.role == Role::Tool {
                let id = m.tool_call_id.as_deref().unwrap_or("<none>");
                assert!(
                    known.contains(id),
                    "messages[{i}] is an orphaned tool message (tool_call_id {id:?})"
                );
            }
        }
    }

    #[tokio::test]
    async fn summarize_never_keeps_orphaned_tool_messages() {
        // Regression (2026-08-22): the naive `len - keep_recent` cut landed
        // mid tool-call batch, so the kept tail STARTED with a tool result
        // whose assistant tool call was summarized away — the next request
        // was rejected ("messages[2] is a tool message whose tool_call_id
        // '…' has no matching assistant tool call") and every retry re-failed,
        // wedging the session after compaction.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_tool_batch_messages();
        let result = cm.summarize(&messages, 3, TEST_SENDABLE_BUDGET, &provider).await.unwrap();
        assert_tool_pairing_intact(&result);
        // The cut advanced past the orphaned tool result: the kept tail is
        // the trailing assistant + user, so result = system + summary + 2.
        assert_eq!(result.len(), 4);
        assert_eq!(result[2].role, Role::Assistant);
        assert_eq!(result[2].content.as_text(), "done");
        assert_eq!(result[3].content.as_text(), "next");
    }

    #[tokio::test]
    async fn summarize_with_interrupt_never_keeps_orphaned_tool_messages() {
        // Same regression guard for the interruptible path (the one used by
        // both auto-compaction and manual /compact).
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_tool_batch_messages();
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);
        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 3, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());
        assert_tool_pairing_intact(&result);
        assert_eq!(result.len(), 4);
        assert_eq!(result[2].content.as_text(), "done");
    }

    #[test]
    fn summary_cut_index_skips_leading_tool_messages() {
        // Unit-level guard on the boundary helper itself: cut on a tool
        // message advances past the whole orphaned run; cut on an assistant
        // WITH tool_calls is already clean (its results follow in the tail).
        let messages = make_tool_batch_messages();
        assert_eq!(summary_cut_index(&messages, 3), 6); // naive 5 → tool(c2) → 6
        assert_eq!(summary_cut_index(&messages, 5), 3); // naive 3 → assistant(calls) stays
        assert_eq!(summary_cut_index(&messages, 2), 6); // already clean
    }

    /// Build a conversation ending in a tool-result run LONGER than
    /// keep_recent: [system, assistant(tool_calls c1..c8), tool(c1)..tool(c8)]
    /// — len 10, keep_recent 6 → naive cut 4 lands on tool(c4), and advancing
    /// past the tool run runs off the end (empty kept tail).
    fn make_all_tool_tail_messages() -> Vec<Message> {
        let tool_calls = (1..=8)
            .map(|i| crate::provider::ToolCall::new(format!("c{i}"), "file_read".to_string(), "{}".to_string()))
            .collect::<Vec<_>>();
        let mut msgs = vec![
            Message::system("system prompt"),
            Message::assistant("calling tools", tool_calls),
        ];
        for i in 1..=8 {
            msgs.push(Message::tool_result(format!("c{i}"), "file_read".to_string(), format!("result {i}")));
        }
        msgs
    }

    #[tokio::test]
    async fn summarize_all_tool_tail_appends_sendable_continuation() {
        // Regression (review finding, 2026-08-22): when the conversation ends
        // in a tool-result run longer than keep_recent, the boundary-aligned
        // cut empties the kept tail — the compacted result would be only
        // system messages, which strict providers (Anthropic requires ≥1
        // non-system message) reject. A synthetic user continuation keeps the
        // history sendable.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_all_tool_tail_messages();
        // The cut advances off the end.
        assert_eq!(summary_cut_index(&messages, 6), messages.len());
        let result = cm.summarize(&messages, 6, TEST_SENDABLE_BUDGET, &provider).await.unwrap();
        // system + summary + synthetic user continuation = 3 messages.
        assert_eq!(result.len(), 3);
        assert!(
            result.iter().any(|m| m.role == Role::User),
            "the compacted history must contain a non-system message"
        );
        assert!(result[2].content.as_text().contains("compacted"));
        // The tool-pairing invariant trivially holds (no tool messages kept).
        assert_tool_pairing_intact(&result);
    }

    #[tokio::test]
    async fn summarize_with_interrupt_all_tool_tail_appends_sendable_continuation() {
        // Same empty-tail guard on the interruptible path (the one used by
        // auto-compaction + manual /compact).
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_all_tool_tail_messages();
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(8);
        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 6, TEST_SENDABLE_BUDGET, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());
        assert_eq!(result.len(), 3);
        assert!(result.iter().any(|m| m.role == Role::User));
        assert_tool_pairing_intact(&result);
    }

    /// Build a conversation ending in an OPEN tool loop (no final text answer):
    /// [system, user, assistant(text), user, assistant(tool_calls c1..c4),
    ///  tool(c1)..tool(c4)] — len 12, keep_recent 6 → naive cut 6 lands inside
    /// the open loop (on tool(c2)). Rule 4 requires the cut to retreat to the
    /// loop start (the user message at index 3) so the entire in-progress loop
    /// is preserved verbatim.
    fn make_open_tool_loop_messages() -> Vec<Message> {
        let text = |role: Role, t: &str| Message::text(role, t);
        let tool_result = |id: &str, t: &str| {
            Message::tool_result(id.to_string(), "file_read".to_string(), t)
        };
        let tool_calls = (1..=4)
            .map(|i| {
                crate::provider::ToolCall::new(
                    format!("c{i}"),
                    "file_read".to_string(),
                    "{}".to_string(),
                )
            })
            .collect::<Vec<_>>();
        vec![
            text(Role::System, "system prompt"),
            text(Role::User, "first question"),
            text(Role::Assistant, "first answer"),
            text(Role::User, "second question"),
            Message::assistant("calling tools", tool_calls),
            tool_result("c1", "result 1"),
            tool_result("c2", "result 2"),
            tool_result("c3", "result 3"),
            tool_result("c4", "result 4"),
        ]
    }

    #[test]
    fn summary_cut_index_retreats_past_open_tool_loop() {
        // Rule 4: the naive cut (len - keep_recent = 9 - 6 = 3) lands on the
        // user message at index 3 — the start of the open loop. The retreat
        // keeps it (cut == loop_start, not > loop_start), so the entire open
        // loop [3..] is preserved. But with keep_recent=4, naive cut = 5
        // (tool(c2)) — inside the open loop — and the retreat moves it back
        // to the user message at index 3.
        let messages = make_open_tool_loop_messages();
        // naive cut = 9 - 4 = 5 → tool(c2), inside the open loop.
        // Retreat to loop_start = 3 (the user message).
        assert_eq!(summary_cut_index(&messages, 4), 3);
        // naive cut = 9 - 6 = 3 → user message (loop start). cut == loop_start,
        // not > loop_start, so no retreat — but the open loop is still
        // preserved (it starts at the cut).
        assert_eq!(summary_cut_index(&messages, 6), 3);
    }

    #[test]
    fn summary_cut_index_no_retreat_when_loop_closed() {
        // When the conversation ends in a completed turn (text-only assistant
        // = final answer), there is no open loop — no retreat. The forward-
        // advance still moves past the orphan tool result (its matching
        // assistant tool_calls would be summarized away).
        let text = |role: Role, t: &str| Message::text(role, t);
        let tool_result = |id: &str, t: &str| {
            Message::tool_result(id.to_string(), "file_read".to_string(), t)
        };
        let messages = vec![
            text(Role::System, "system"),
            text(Role::User, "q1"),
            Message::assistant(
                "calling",
                vec![crate::provider::ToolCall::new(
                    "c1".to_string(),
                    "file_read".to_string(),
                    "{}".to_string(),
                )],
            ),
            tool_result("c1", "result"),
            text(Role::Assistant, "final answer"), // closes the loop
        ];
        // naive cut = 5 - 2 = 3 → tool(c1), an orphan (its assistant call at
        // [2] would be summarized). Forward-advance → 4 (the text-only
        // assistant). No open loop → no retreat. cut = 4.
        assert_eq!(summary_cut_index(&messages, 2), 4);
    }

    #[tokio::test]
    async fn summarize_preserves_open_tool_loop_verbatim() {
        // Rule 4 acceptance test #3: fill context past the compaction threshold
        // mid-tool-loop. Assert compaction ran only before the loop start —
        // the entire open loop is preserved verbatim in the kept tail.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta { text: "summary".into() },
                LlmEvent::Finish { reason: crate::provider::FinishReason::Stop },
            ],
            caps: Capabilities::openai(),
        };
        let messages = make_open_tool_loop_messages();
        // keep_recent=4 → naive cut 5 (inside the loop) → retreats to 3.
        let cut = summary_cut_index(&messages, 4);
        assert_eq!(cut, 3, "cut must retreat to the open-loop start");
        let result = cm.summarize(&messages, 4, TEST_SENDABLE_BUDGET, &provider).await.unwrap();
        // system + summary + [user, assistant(tool_calls), tool(c1..c4)] = 8.
        assert_eq!(result.len(), 8);
        // The kept tail starts at the user message (loop start) — the entire
        // open tool loop is preserved verbatim.
        assert_eq!(result[2].role, Role::User);
        assert_eq!(result[2].content.as_text(), "second question");
        assert_eq!(result[3].role, Role::Assistant);
        assert_eq!(result[3].tool_calls.len(), 4);
        assert_tool_pairing_intact(&result);
    }

    #[tokio::test]
    async fn summarize_skips_when_open_loop_spans_conversation() {
        // Rule 4 degenerate case: the open tool loop spans the entire
        // conversation (system + user + assistant(tool_calls) + tools, nothing
        // before the loop to summarize). Compaction must NOT run — return the
        // original messages unchanged.
        let cm = ContextManager::new(128_000, 0.5);
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta { text: "summary".into() },
                LlmEvent::Finish { reason: crate::provider::FinishReason::Stop },
            ],
            caps: Capabilities::openai(),
        };
        let tool_calls = (1..=8)
            .map(|i| {
                crate::provider::ToolCall::new(
                    format!("c{i}"),
                    "file_read".to_string(),
                    "{}".to_string(),
                )
            })
            .collect::<Vec<_>>();
        let messages = vec![
            Message::system("system"),
            Message::user_text("question"),
            Message::assistant("calling", tool_calls),
            Message::tool_result("c1".to_string(), "file_read".to_string(), "r1"),
            Message::tool_result("c2".to_string(), "file_read".to_string(), "r2"),
            Message::tool_result("c3".to_string(), "file_read".to_string(), "r3"),
        ];
        // naive cut = 6 - 2 = 4 → tool(c3), inside the open loop.
        // Retreat to loop_start = 1 (the user message). cut = 1 ≤ 1 → skip.
        let cut = summary_cut_index(&messages, 2);
        assert_eq!(cut, 1);
        let result = cm.summarize(&messages, 2, TEST_SENDABLE_BUDGET, &provider).await.unwrap();
        // Compaction did not run — original messages returned unchanged.
        assert_eq!(result.len(), messages.len());
        assert_eq!(result[1].role, Role::User);
        assert_eq!(result[2].tool_calls.len(), 8);
    }

    /// Regression (2027-01-23 runaway-tool-output report, L2): when the
    /// summarizer rejects the request for SIZE, compaction must fall back to a
    /// mechanical compaction instead of failing — failing is what wedged the
    /// session until `/new`. Both rejection shapes are covered: the immediate
    /// `complete()` error and a mid-stream [`LlmEvent::Error`].
    #[tokio::test]
    async fn summarize_with_interrupt_falls_back_to_mechanical_compaction_on_context_overflow() {
        // The two wordings the provider stack produces: the OpenAI request
        // rejection and the DeepSeek/GLM "longer than the limit" 400.
        let overflow_errors = [
            "HTTP 400: This model's maximum context length is 16384 tokens",
            "HTTP 400: input is longer than the limit of the model",
        ];
        for error in overflow_errors {
            // (a) the request itself is refused.
            let failing = FailingCompleteMock {
                error: error.to_string(),
                caps: Capabilities::openai(),
            };
            let messages = make_messages(10);
            let cm = ContextManager::new(128_000, 0.5);
            let (_tx, mut cmd_rx) = tokio::sync::mpsc::channel(4);
            let (result, buffered, stop, usage) = cm
                .summarize_with_interrupt(
                    &messages,
                    2,
                    TEST_SENDABLE_BUDGET,
                    &failing,
                    &mut cmd_rx,
                )
                .await
                .expect("a size rejection must fall back, not fail");
            assert!(buffered.is_empty(), "no commands buffered: {error}");
            assert!(stop.is_none(), "must not report a stop: {error}");
            assert!(usage.is_none(), "a mechanical compaction calls no model");
            assert!(
                result[1].content.as_text().contains(MECHANICAL_SUMMARY_NOTE),
                "the mechanical note must explain what happened: {}",
                result[1].content.as_text()
            );
            assert_eq!(
                result.last().expect("non-empty").content.as_text(),
                messages.last().expect("non-empty").content.as_text(),
                "the recent tail must survive verbatim"
            );
            assert_tool_pairing_intact(&result);

            // (b) the rejection arrives mid-stream instead.
            let streaming = SummaryMockProvider {
                events: vec![LlmEvent::Error {
                    error: error.to_string(),
                }],
                caps: Capabilities::openai(),
            };
            let (_tx, mut cmd_rx) = tokio::sync::mpsc::channel(4);
            let (result, _buffered, _stop, _usage) = cm
                .summarize_with_interrupt(
                    &messages,
                    2,
                    TEST_SENDABLE_BUDGET,
                    &streaming,
                    &mut cmd_rx,
                )
                .await
                .expect("a mid-stream size rejection must fall back too");
            assert!(
                result[1].content.as_text().contains(MECHANICAL_SUMMARY_NOTE),
                "the mechanical note must explain what happened: {error}"
            );
        }
    }

    /// The mechanical fallback must be narrow: a rejection that is NOT about
    /// size keeps the existing behaviour (an `Err` the caller surfaces), so a
    /// bad API key is never silently turned into "your context was compacted".
    #[tokio::test]
    async fn summarize_with_interrupt_surfaces_non_size_errors() {
        let messages = make_messages(10);
        let cm = ContextManager::new(128_000, 0.5);
        for error in [
            "HTTP 401: unauthorized — invalid api key",
            "HTTP 429: rate limit exceeded",
        ] {
            let failing = FailingCompleteMock {
                error: error.to_string(),
                caps: Capabilities::openai(),
            };
            let (_tx, mut cmd_rx) = tokio::sync::mpsc::channel(4);
            let result = cm
                .summarize_with_interrupt(
                    &messages,
                    2,
                    TEST_SENDABLE_BUDGET,
                    &failing,
                    &mut cmd_rx,
                )
                .await;
            assert!(result.is_err(), "a non-size error must surface: {error}");

            let streaming = SummaryMockProvider {
                events: vec![LlmEvent::Error {
                    error: error.to_string(),
                }],
                caps: Capabilities::openai(),
            };
            let (_tx, mut cmd_rx) = tokio::sync::mpsc::channel(4);
            let result = cm
                .summarize_with_interrupt(
                    &messages,
                    2,
                    TEST_SENDABLE_BUDGET,
                    &streaming,
                    &mut cmd_rx,
                )
                .await;
            assert!(
                result.is_err(),
                "a mid-stream non-size error must surface: {error}"
            );
        }
    }

    /// Regression (2027-01-23 runaway-tool-output report, L1 acceptance): the
    /// END-TO-END post-condition. A real compaction through the provider stub
    /// must leave a conversation that is ITSELF sendable — merely "smaller" is
    /// not enough, because the very next request is the one that gets rejected.
    ///
    /// The fixture ends in an OPEN tool loop (the last assistant message is
    /// still calling its tool, with a 120K-char result), so the cut retreats to
    /// the last user turn and the whole loop — including the monster result —
    /// is kept VERBATIM. That is exactly the shape the post-condition exists
    /// for; with the cut landing after the last tool batch instead, the tail
    /// would be tiny and this test would pass even with the post-condition
    /// disabled (it did, until the fixture below was fixed).
    #[tokio::test]
    async fn compaction_result_is_sendable() {
        let payload = "x".repeat(120_000);
        let args = "{\"path\":\"src/lib.rs\"}";
        let mut messages = vec![Message::system("system prompt")];
        for i in 0..4 {
            messages.push(Message::user_text(format!("question {i}")));
            messages.push(Message::assistant(
                format!("calling tool {i}"),
                vec![crate::provider::ToolCall::new(
                    format!("c{i}"),
                    "read_files",
                    args,
                )],
            ));
            messages.push(Message::tool_result(format!("c{i}"), "read_files", &payload));
        }
        messages.push(Message::user_text("one more"));
        messages.push(Message::assistant(
            "calling tool 4",
            vec![crate::provider::ToolCall::new("c4", "read_files", args)],
        ));
        messages.push(Message::tool_result("c4", "read_files", &payload));
        let provider = SummaryMockProvider {
            events: vec![
                LlmEvent::TextDelta {
                    text: "compact summary".into(),
                },
                LlmEvent::Finish {
                    reason: crate::provider::FinishReason::Stop,
                },
            ],
            caps: Capabilities::openai(),
        };
        let cm = ContextManager::new(128_000, 0.5);
        let (_cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel(4);
        let sendable = 8_192;
        let (result, buffered, stop, _) = cm
            .summarize_with_interrupt(&messages, 2, sendable, &provider, &mut cmd_rx)
            .await
            .unwrap();
        assert!(buffered.is_empty());
        assert!(stop.is_none());
        assert!(
            result.len() < messages.len(),
            "compaction must have dropped the summarized region"
        );
        let tokens = ContextManager::count_tokens(&result);
        assert!(
            tokens <= sendable,
            "the compacted conversation must itself be sendable: {tokens} > {sendable}"
        );
        assert_tool_pairing_intact(&result);
    }

    /// The result budget must be derived from the TURN model's window and sit
    /// strictly below it (the reply has to fit too) — the summarizer may be a
    /// cheaper model with a different window, which is why request and result
    /// are budgeted separately.
    #[test]
    fn sendable_budget_sits_below_the_window() {
        let caps = Capabilities::openai();
        let budget = sendable_budget(&caps);
        assert_eq!(budget, context_budget(caps.max_context, caps.max_output_tokens));
        assert!(
            budget < caps.max_context,
            "the result budget must leave room for the reply: {budget} vs {}",
            caps.max_context
        );
        // A generous budget must not shrink an already-sendable history.
        let messages = vec![Message::system("head"), Message::user_text("hello")];
        let mut untouched = messages.clone();
        enforce_sendable(&mut untouched, budget);
        assert_eq!(untouched.len(), messages.len());
        assert_eq!(untouched[1].content.as_text(), messages[1].content.as_text());
    }

    /// Regression (2027-01-23 runaway-tool-output report): budgeting only the
    /// summarization REQUEST let the wedge survive compaction. The march keeps
    /// recent messages verbatim, so a monster-heavy history compacts to a
    /// still-over-window result — the user sees "Compacted", and the next
    /// request is rejected exactly as before. The post-condition must bound the
    /// RESULT: here by capping the runaway tool results (which keep their
    /// re-run pointer, so the content is recoverable rather than lost).
    #[tokio::test]
    async fn enforce_sendable_bounds_the_compacted_result() {
        let payload = "x".repeat(120_000);
        let mut messages = vec![Message::system("system prompt")];
        for i in 0..4 {
            messages.push(Message::assistant(
                format!("calling tool {i}"),
                vec![crate::provider::ToolCall::new(
                    format!("c{i}"),
                    "read_files",
                    format!("{{\"path\":\"src/agent/context.rs\",\"n\":{i}}}"),
                )],
            ));
            messages.push(Message::tool_result(format!("c{i}"), "read_files", &payload));
        }
        messages.push(Message::user_text("newest message"));
        let before = ContextManager::count_tokens(&messages);
        assert!(
            before > 50_000,
            "fixture must be a runaway history: {before} tokens"
        );

        // The minimum legal sendable budget ([`context_budget`] floors here),
        // far below the fixture — the same shape as the report's 7.2M-token
        // history against a 1M window.
        let budget = 8_192;
        let mut result = vec![
            messages[0].clone(),
            Message::system("## Conversation summary\n\nsummary text"),
        ];
        result.extend(messages[1..].iter().cloned());
        enforce_sendable(&mut result, budget);

        let after = ContextManager::count_tokens(&result);
        assert!(
            after <= budget,
            "compaction must be TOTAL: {after} tokens still over {budget}"
        );
        assert!(
            after < before,
            "the post-condition must shrink the history: {before} → {after}"
        );
        // The frame survives: the system head, the summary, and the newest turn.
        assert_eq!(result[0].content.as_text(), "system prompt");
        assert!(result[1].content.as_text().contains("## Conversation summary"));
        assert_eq!(
            result.last().expect("non-empty").content.as_text(),
            "newest message",
            "the newest message must always survive"
        );
        // Capping is the lossless-ish lever: the capped results are MARKED and
        // keep the re-run pointer, so the agent can re-issue the call.
        assert!(
            result
                .iter()
                .any(|m| m.content.as_text().contains(COMPACTED_MARKER)),
            "capped results must be marked"
        );
        assert!(
            result
                .iter()
                .any(|m| m.content.as_text().contains("read_files")),
            "a capped result must keep its re-run pointer"
        );
        assert_tool_pairing_intact(&result);
    }

    /// Regression (2027-01-23 review HIGH 1): the drop lever must never orphan
    /// a tool result. When the surviving tail IS a single open tool batch — the
    /// batch's call plus its parallel results, ending on the newest message —
    /// no non-tool boundary exists to start the new tail on. Dropping any of it
    /// removes the call and leaves a dangling `tool_call_id`, which
    /// `validate_request_messages` rejects on every LATER request; the context
    /// is under the threshold by then, so auto-compaction never runs again and
    /// the session is wedged for good. The batch stays whole and totality
    /// yields to the untouchable core (system + summary + final batch).
    #[test]
    fn enforce_sendable_keeps_an_open_final_tool_batch_intact() {
        let args = "{\"path\":\"src/agent/context.rs\"}";
        let monster = "x".repeat(120_000);
        let mut result = vec![
            Message::system("system prompt"),
            // The summary is big enough that the residual after the aggressive
            // cap is still over budget — caps cannot touch a summary message.
            Message::system(format!("## Conversation summary\n\n{}", "s".repeat(120_000))),
            // One open batch: a call for TWO parallel results, ending on the
            // newest message.
            Message::assistant(
                "final call",
                vec![
                    crate::provider::ToolCall::new("f1", "read_files", args),
                    crate::provider::ToolCall::new("f2", "read_files", args),
                ],
            ),
            Message::tool_result("f1", "read_files", &monster),
            Message::tool_result("f2", "read_files", &monster),
        ];
        let budget = 8_192;
        enforce_sendable(&mut result, budget);

        // The whole batch survives: the call and BOTH results.
        assert!(
            result.iter().any(|m| m.tool_calls.len() == 2),
            "the final batch's call must survive"
        );
        assert!(
            result
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("f1")),
            "the first parallel result must survive"
        );
        assert!(
            result
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("f2")),
            "the newest message must survive"
        );
        // The invariant a dangling tool_call_id would break.
        assert_tool_pairing_intact(&result);
        // Documented outcome: with no legal boundary the batch is kept whole, so
        // the result can stay over the sendable budget. That is the intended
        // trade — a request that is too large gets a clear provider error and
        // the retry path, whereas an orphaned tool result would be rejected
        // forever. The caller's stuck ladder handles the size case.
        assert!(
            ContextManager::count_tokens(&result) > budget,
            "totality yields to the untouchable core: {} should exceed {budget}",
            ContextManager::count_tokens(&result)
        );
        // Idempotent: a second pass changes nothing.
        let before = result.len();
        enforce_sendable(&mut result, budget);
        assert_eq!(result.len(), before);
        assert_tool_pairing_intact(&result);
    }

    /// Regression (2027-01-23 runaway-tool-output report): capping is not
    /// always enough — an oversized history of big TEXT turns is untouched by
    /// the tool-result cap, and only dropping the oldest tail messages can fit
    /// it. The dropped end must never take the system message, the summary, or
    /// the newest message with it, and must never orphan a tool result.
    #[tokio::test]
    async fn enforce_sendable_drops_oldest_turns_when_capping_is_not_enough() {
        let mut result = vec![
            Message::system("system prompt"),
            Message::system("## Conversation summary\n\nsummary text"),
        ];
        for i in 0..20 {
            let filler = "y".repeat(10_000);
            result.push(if i % 2 == 0 {
                Message::user_text(format!("turn {i} {filler}"))
            } else {
                Message::assistant(format!("turn {i} {filler}"), vec![])
            });
        }
        result.push(Message::user_text("newest message"));
        let fixture_len = result.len();
        let before = ContextManager::count_tokens(&result);
        assert!(before > 20_000, "fixture must be over budget: {before}");

        enforce_sendable(&mut result, 8_192);

        let after = ContextManager::count_tokens(&result);
        assert!(after <= 8_192, "must fit the sendable window: {after}");
        assert!(after < before, "must shrink: {before} → {after}");
        assert!(
            result.len() < fixture_len,
            "the oldest turns must have been dropped: kept {} of {fixture_len}",
            result.len()
        );
        assert_eq!(result[0].content.as_text(), "system prompt");
        assert!(result[1].content.as_text().contains("## Conversation summary"));
        assert_eq!(
            result.last().expect("non-empty").content.as_text(),
            "newest message",
            "the newest message must always survive"
        );
        assert_tool_pairing_intact(&result);
    }
}
