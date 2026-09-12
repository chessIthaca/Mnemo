// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The spawned stream task for the OpenAI-compatible client: the parse loop
//! that pumps raw SSE bytes into [`LlmEvent`]s.
//!
//! [`OpenAiClient::spawn_stream`] captures the per-request state into
//! [`StreamTask`] and spawns [`pump_sse_stream`] on a tokio task. The loop
//! owns the full stream lifecycle: per-chunk read timeout, trace mirroring
//! (response bytes, live usage overlay, terminal events), the think-tag
//! filter, the client-side stream guard, the repetition guard, and the
//! fallback Finish when the stream ends without one.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::provider::sse_util::{
    error_chain, finish_reason_label, truncate_raw_stream, SseOutcome,
};
use crate::provider::stream::{
    bound_repetition_buffer, detect_repetition, REPETITION_BUFFER_CAP, REPETITION_THRESHOLD,
    REPETITION_WINDOW,
};
use crate::provider::trace::{LlmRequestLog, LlmUsage};
use crate::provider::{LlmEvent, StallTracker};

use super::OpenAiClient;
use super::guard::apply_stream_guard;
use super::sse::{parse_responses_sse_buffer, parse_sse_buffer, ThinkTagFilter};
/// Everything the spawned stream task needs, captured from the request
/// phases and moved into the tokio task.
pub(super) struct StreamTask {
    /// Sending half of the event channel bridged to the caller.
    pub(super) tx: mpsc::Sender<LlmEvent>,
    /// The (log, record id) pair the task mirrors events into.
    pub(super) trace_ctx: Option<(Arc<LlmRequestLog>, u64)>,
    /// The response status (for error attribution).
    pub(super) status: reqwest::StatusCode,
    /// The response's `content-encoding` header ("" when absent).
    pub(super) content_encoding: String,
    /// The response's `transfer-encoding` header ("" when absent).
    pub(super) transfer_encoding: String,
    /// The response's `content-type` header ("" when absent).
    pub(super) content_type: String,
    /// When the POST response headers arrived — the connect-bucket end
    /// anchor (connect_ms = record_created → request_start).
    pub(super) request_start: std::time::Instant,
    /// When the trace record was created — the connect-bucket start anchor.
    pub(super) record_created: std::time::Instant,
    /// When the POST was sent — the ttft anchor (perf review L1: headers
    /// arrive with the first chunk for SSE providers, so ttft is measured
    /// from the send, not the headers).
    pub(super) post_sent: std::time::Instant,
    /// Whether inline think-tag extraction is enabled (Local providers).
    pub(super) think_tags_enabled: bool,
    /// Whether to parse the Responses API event format.
    pub(super) use_responses_api: bool,
    /// The stream guard's stop-boundary list (config `stop` +
    /// `stop_boundary_strings`, de-duplicated).
    pub(super) stop_boundaries: Vec<String>,
}

/// The parse-loop stage of the pipeline: pump the raw SSE byte stream into
/// [`LlmEvent`]s on the channel.
///
/// Runs on its own tokio task (spawned by [`OpenAiClient::spawn_stream`]).
/// Owns the full stream lifecycle: per-chunk read timeout, trace mirroring
/// (response bytes, live usage overlay, terminal events), the think-tag
/// filter, the client-side stream guard, the repetition guard, and the
/// fallback Finish when the stream ends without one.
pub(super) async fn pump_sse_stream(response: reqwest::Response, task: StreamTask) {
    let StreamTask {
        tx,
        trace_ctx,
        status,
        content_encoding,
        transfer_encoding,
        content_type,
        request_start,
        record_created,
        post_sent,
        think_tags_enabled,
        use_responses_api,
        stop_boundaries: stream_stop_boundaries,
    } = task;
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    // First-chunk timestamp (set when the first bytes arrive). Used for
    // TTFT (post_sent→first_chunk — perf review L1) and generation time
    // (first_chunk→usage event, i.e. the final chunk).
    let mut first_chunk: Option<std::time::Instant> = None;
    // Reasoning-window tracking for `reasoning_ms`: whether any
    // ReasoningDelta streamed, and the first chunk that carried an
    // answer/tool delta (the reasoning→answer boundary).
    let mut saw_reasoning = false;
    let mut first_content_chunk: Option<std::time::Instant> = None;
    // Byte-level stall tracking for `stall_ms`: gaps between chunk
    // arrivals longer than StallTracker::THRESHOLD are waiting, not
    // generation — split out of `generation_ms` in the graph.
    let mut stall_tracker = StallTracker::default();
    // Per-chunk read timeout: if no bytes arrive for this long, the
    // connection is dead (not just slow — a reasoning model's thinking
    // phase still sends keepalive chunks). This bounds how long we'll
    // wait on a stalled connection without imposing a total lifetime on
    // the stream (which would kill long-but-active generations).
    let read_timeout = OpenAiClient::READ_TIMEOUT;
    // Whether a Finish was already mirrored into the trace log. The
    // fallback Finish at stream end must NOT overwrite the real
    // finish reason (tool_calls/length/...) logged by the event arm.
    let mut finish_logged = false;
    // Reroutes inline `<think>…</think>` content blocks (local Ollama
    // / LM Studio style) to ReasoningDelta so the thinking is
    // displayed instead of leaking raw tags into the answer.
    let mut think_filter = ThinkTagFilter::new();
    // R10: accumulated response text for the repetition guard. Only
    // TextDelta content is appended; when ANY repeating unit of
    // ≤ 200 bytes spans the last 600 tail bytes, the stream is aborted.
    let mut response_text = String::new();
    // Live usage overlay (backlog b5503915): chars/4 estimates of the
    // deltas streamed so far, pushed onto the trace record at most
    // every 500ms (throttled in the Ok(bytes) arm) so the Trace tab
    // shows live token progress during a long reasoning phase — the
    // only stretch where nothing else in the record changes. The
    // authoritative set_usage at stream end overwrites these.
    // `final_usage_seen` stops the overlay dead once the real usage
    // landed: a non-conforming server that sends usage without a
    // prior finish_reason chunk, then more bytes, must not have its
    // estimates clobber the real numbers.
    let mut est_reasoning_chars: usize = 0;
    let mut est_completion_chars: usize = 0;
    let mut last_usage_push: Option<std::time::Instant> = None;
    let mut final_usage_seen = false;

    loop {
        // Wrap the next-chunk read in a timeout. Elapsed → dead
        // connection; emit a clear error and stop.
        let chunk_result = match tokio::time::timeout(read_timeout, stream.next()).await {
            Ok(Some(result)) => result,
            Ok(None) => break, // stream ended normally
            Err(_elapsed) => {
                let partial = if buffer.is_empty() {
                    String::from("(no data received before the timeout)")
                } else {
                    truncate_raw_stream(&buffer, 2000)
                };
                let error = format!(
                    "stream stalled — no data for {read_timeout:?} \
                     (HTTP {status}). The connection appears dead.\n\
                     content-encoding: {content_encoding}\n\
                     transfer-encoding: {transfer_encoding}\n\
                     content-type: {content_type}\n\
                     partial stream data received so far:\n{partial}"
                );
                if let Some((log, id)) = &trace_ctx {
                    // Attribute the stall to the right bucket: if no
                    // chunk ever arrived, the entire window is TTFT
                    // (waiting); if data was flowing, the stall is
                    // mid-generation.
                    if first_chunk.is_none() {
                        // Headers arrived but no chunk ever followed:
                        // connect covers record→headers, ttft the full
                        // POST-send wait (upload + prefill + silence).
                        log.set_connect_ms(
                            *id,
                            request_start
                                .duration_since(record_created)
                                .as_millis() as u32,
                        );
                        log.set_ttft_ms(*id, post_sent.elapsed().as_millis() as u32);
                    } else {
                        // The trailing silence (last chunk →
                        // timeout) is a stall too — finalize the
                        // tracker so stall_ms covers it.
                        stall_tracker.finalize(std::time::Instant::now());
                        log.set_stall_ms(*id, stall_tracker.ms());
                        if let Some(fc) = first_chunk {
                            log.set_generation_ms(*id, fc.elapsed().as_millis() as u32);
                        }
                    }
                    log.fail(*id, status.as_u16(), &error);
                }
                let _ = tx.send(LlmEvent::Error { error }).await;
                return;
            }
        };
        match chunk_result {
            Ok(bytes) => {
                // Stamp the first-chunk timestamp for usage timing.
                let now = std::time::Instant::now();
                if first_chunk.is_none() {
                    first_chunk = Some(now);
                }
                stall_tracker.on_chunk(now);
                let chunk_text = String::from_utf8_lossy(&bytes);
                if let Some((log, id)) = &trace_ctx {
                    log.append_response(*id, &chunk_text);
                    // Live usage overlay (backlog b5503915): push the
                    // chars/4 estimate at most every 500ms — during a
                    // long reasoning phase this is the only thing that
                    // moves in the record until the stream finishes.
                    // The authoritative set_usage (the Usage event
                    // arm below) overwrites it at stream end.
                    if !final_usage_seen
                        && last_usage_push.map_or(true, |t| {
                            now.duration_since(t) >= std::time::Duration::from_millis(500)
                        })
                    {
                        last_usage_push = Some(now);
                        log.update_streaming_usage(
                            *id,
                            (est_completion_chars / 4) as u32,
                            (est_reasoning_chars / 4) as u32,
                        );
                    }
                }
                buffer.push_str(&chunk_text);
                // Process complete SSE lines. The line-splitting +
                // parsing is factored into `parse_sse_buffer` so it's
                // unit-testable without a live HTTP stream; it uses
                // `String::drain()` to drop processed bytes in-place
                // (no O(n²) buffer re-copy on multi-line chunks).
                for outcome in if use_responses_api {
                    parse_responses_sse_buffer(&mut buffer)
                } else {
                    parse_sse_buffer(&mut buffer)
                } {
                    match outcome {
                        SseOutcome::Event(event) => {
                            // Inline `<think>…</think>` blocks arrive
                            // inside `content` (local Ollama / LM
                            // Studio style) — reroute them through the
                            // tag filter to ReasoningDelta. The filter
                            // can split one delta into two events (a
                            // close tag mid-fragment).
                            let events = match event {
                                LlmEvent::TextDelta { text } if think_tags_enabled => {
                                    think_filter.feed(&text)
                                }
                                other => vec![other],
                            };
                            // Client-side stream guard (defense-in-depth): terminate turns early
                            // if raw boundary tokens (<|user|>, <|assistant|>, <|observation|>,
                            // <|endoftext|>, or configured stops) appear in stream deltas. GLM
                            // boundary tokens are single tokenizer units that decode into complete
                            // delimiter strings; truncating at the boundary and ending the stream
                            // prevents runaway hallucinated turns when proxies strip stops.
                            let (guarded_events, guard_cut) = apply_stream_guard(
                                events,
                                &stream_stop_boundaries,
                                est_completion_chars,
                            );
                            let stream_stopped = guard_cut.is_some();
                            for event in guarded_events {
                                // Track the reasoning→answer transition
                                // for reasoning_ms (first reasoning
                                // delta → first answer/tool delta).
                                match &event {
                                    LlmEvent::ReasoningDelta { text } => {
                                        saw_reasoning = true;
                                        est_reasoning_chars += text.chars().count();
                                    }
                                    LlmEvent::TextDelta { text } => {
                                        est_completion_chars += text.chars().count();
                                        if first_content_chunk.is_none() {
                                            first_content_chunk = Some(now);
                                        }
                                    }
                                    LlmEvent::ToolCallStart { .. }
                                    | LlmEvent::ToolCallArgumentDelta { .. } => {
                                        if first_content_chunk.is_none() {
                                            first_content_chunk = Some(now);
                                        }
                                    }
                                    _ => {}
                                }
                                // Enrich the Usage event with timing from
                                // the stream loop (parse_sse_chunk leaves
                                // ttft_ms/generation_ms None — only the
                                // loop owns the timestamps).
                                let event = match event {
                                    LlmEvent::Usage {
                                        prompt_tokens,
                                        completion_tokens,
                                        reasoning_tokens,
                                        cached_tokens,
                                        ..
                                    } => {
                                        // TTFT = POST-send → first chunk
                                        // (perf review L1: headers arrive
                                        // WITH the first chunk for SSE
                                        // providers, so the old
                                        // headers→first-chunk window
                                        // measured ~0); generation = first
                                        // → last chunk. The last chunk is
                                        // the one being processed (this is
                                        // the Ok(bytes) arm), so the
                                        // generation span is first → now.
                                        let ttft_ms = first_chunk.map(|fc| {
                                            fc.duration_since(post_sent).as_millis()
                                                as u32
                                        });
                                        let generation_ms = first_chunk.map(|fc| {
                                            now.duration_since(fc).as_millis() as u32
                                        });
                                        // connect = record created → stream open
                                        // (the response headers arrived; includes
                                        // the full round trip + the reasoning_effort
                                        // fallback retry when one fired).
                                        let connect_ms = request_start
                                            .duration_since(record_created)
                                            .as_millis()
                                            as u32;
                                        // reasoning = first chunk → first
                                        // answer/tool delta (or → the final
                                        // chunk when the model thought but
                                        // never answered).
                                        let reasoning_ms = if saw_reasoning {
                                            first_chunk.map(|fc| {
                                                let end =
                                                    first_content_chunk.unwrap_or(now);
                                                end.duration_since(fc).as_millis() as u32
                                            })
                                        } else {
                                            None
                                        };
                                        // The authoritative usage landed — the
                                        // streaming overlay must never fire again.
                                        final_usage_seen = true;
                                        if let Some((log, id)) = &trace_ctx {
                                            log.set_usage(
                                                *id,
                                                LlmUsage {
                                                    prompt: prompt_tokens,
                                                    completion: completion_tokens,
                                                    reasoning: reasoning_tokens,
                                                    cached: cached_tokens,
                                                },
                                                ttft_ms,
                                                generation_ms,
                                                reasoning_ms,
                                                Some(connect_ms),
                                            );
                                            // Split the byte-silence gaps
                                            // out of generation so the
                                            // graph shows actual
                                            // streaming vs. waiting.
                                            log.set_stall_ms(*id, stall_tracker.ms());
                                        }
                                        LlmEvent::Usage {
                                            prompt_tokens,
                                            completion_tokens,
                                            reasoning_tokens,
                                            cached_tokens,
                                            ttft_ms,
                                            generation_ms,
                                        }
                                    }
                                    other => {
                                        // Mirror terminal events into the
                                        // trace log (finish reason / error).
                                        match &other {
                                            LlmEvent::Finish { reason } => {
                                                if let Some((log, id)) = &trace_ctx {
                                                    log.finish(
                                                        *id,
                                                        &finish_reason_label(reason),
                                                    );
                                                    finish_logged = true;
                                                }
                                            }
                                            LlmEvent::Error { error } => {
                                                if let Some((log, id)) = &trace_ctx {
                                                    if let Some(fc) = first_chunk {
                                                        log.set_generation_ms(
                                                            *id,
                                                            fc.elapsed().as_millis() as u32,
                                                        );
                                                        log.set_stall_ms(
                                                            *id,
                                                            stall_tracker.ms(),
                                                        );
                                                    } else {
                                                        // No chunk: connect =
                                                        // record→headers, ttft =
                                                        // POST-send→error.
                                                        log.set_connect_ms(
                                                            *id,
                                                            request_start
                                                                .duration_since(
                                                                    record_created,
                                                                )
                                                                .as_millis() as u32,
                                                        );
                                                        log.set_ttft_ms(
                                                            *id,
                                                            post_sent
                                                                .elapsed()
                                                                .as_millis()
                                                                as u32,
                                                        );
                                                    }
                                                    log.fail(*id, status.as_u16(), error);
                                                }
                                            }
                                            _ => {}
                                        }
                                        other
                                    }
                                };
                                // R10: accumulate TextDelta content for
                                // the repetition guard. The borrow from
                                // &event ends at the block close, so the
                                // subsequent tx.send(event) move is free.
                                let is_text_delta;
                                if let LlmEvent::TextDelta { text } = &event {
                                    response_text.push_str(text);
                                    is_text_delta = true;
                                } else {
                                    is_text_delta = false;
                                }
                                if tx.send(event).await.is_err() {
                                    // Receiver dropped — a USER interrupt
                                    // (Interrupt/Cancel/Compact/Clear), not a
                                    // provider failure (D1). Stamp the
                                    // terminal-cancelled marker: no error text
                                    // (so provider-errors.jsonl never grows for
                                    // a user cancel), the healthy http_status
                                    // preserved, and a genuine stream error that
                                    // landed just before the drop is not masked.
                                    if let Some((log, id)) = &trace_ctx {
                                        if let Some(fc) = first_chunk {
                                            log.set_generation_ms(
                                                *id,
                                                fc.elapsed().as_millis() as u32,
                                            );
                                        } else {
                                            // No chunk: connect = record→headers,
                                            // ttft = POST-send→cancel.
                                            log.set_connect_ms(
                                                *id,
                                                request_start
                                                    .duration_since(record_created)
                                                    .as_millis() as u32,
                                            );
                                            log.set_ttft_ms(
                                                *id,
                                                post_sent.elapsed().as_millis() as u32,
                                            );
                                        }
                                        log.cancelled(*id, status.as_u16());
                                    }
                                    return;
                                }
                                // R10: abort stuck loops. When ANY repeating
                                // unit of ≤ 200 bytes spans the last 600
                                // tail bytes, the model is stuck — emit an
                                // error and stop to prevent token waste.
                                if is_text_delta
                                    && detect_repetition(
                                        &response_text,
                                        REPETITION_WINDOW,
                                        REPETITION_THRESHOLD,
                                    )
                                {
                                    let msg = "repetition detected — \
                                        stream aborted to prevent token waste";
                                    if let Some((log, id)) = &trace_ctx {
                                        if let Some(fc) = first_chunk {
                                            log.set_generation_ms(
                                                *id,
                                                fc.elapsed().as_millis() as u32,
                                            );
                                            log.set_stall_ms(
                                                *id,
                                                stall_tracker.ms(),
                                            );
                                        } else {
                                            // No chunk: connect = record→headers,
                                            // ttft = POST-send→error.
                                            log.set_connect_ms(
                                                *id,
                                                request_start
                                                    .duration_since(record_created)
                                                    .as_millis() as u32,
                                            );
                                            log.set_ttft_ms(
                                                *id,
                                                post_sent.elapsed().as_millis() as u32,
                                            );
                                        }
                                        log.fail(*id, status.as_u16(), msg);
                                    }
                                    let _ = tx
                                        .send(LlmEvent::Error {
                                            error: msg.to_string(),
                                        })
                                        .await;
                                    return;
                                }
                                // L5: bound the accumulation buffer.
                                // detect_repetition only inspects the
                                // last `needed` bytes (the tail), so
                                // once the buffer exceeds the cap,
                                // drop everything before the last
                                // `needed` bytes (char-boundary
                                // aligned). Semantically safe because
                                // the guard is purely suffix-based —
                                // the truncated tail is exactly what
                                // the next check examines.
                                if is_text_delta {
                                    response_text = bound_repetition_buffer(
                                        response_text,
                                        REPETITION_WINDOW * REPETITION_THRESHOLD,
                                        REPETITION_BUFFER_CAP,
                                    );
                                }
                            }
                            if stream_stopped {
                                if let Some(details) = &guard_cut {
                                    eprintln!(
                                        "stream guard: cut turn — boundary={:?} byte_idx={} delta_len={} chars_before={} prefix_empty={} active_boundaries={:?}",
                                        details.boundary,
                                        details.byte_idx,
                                        details.delta_len,
                                        details.chars_before,
                                        details.prefix_empty,
                                        stream_stop_boundaries
                                    );
                                }
                                if let Some((log, id)) = &trace_ctx {
                                    if let Some(fc) = first_chunk {
                                        log.set_generation_ms(
                                            *id,
                                            fc.elapsed().as_millis() as u32,
                                        );
                                    } else {
                                        // No chunk: connect = record→headers,
                                        // ttft = POST-send→error.
                                        log.set_connect_ms(
                                            *id,
                                            request_start
                                                .duration_since(record_created)
                                                .as_millis() as u32,
                                        );
                                        log.set_ttft_ms(
                                            *id,
                                            post_sent.elapsed().as_millis() as u32,
                                        );
                                    }
                                    if let Some(details) = &guard_cut {
                                        log.set_guard_cut(*id, details.clone());
                                    }
                                    log.finish(*id, "stop");
                                }
                                let _ = tx
                                    .send(LlmEvent::Finish {
                                        reason: crate::provider::FinishReason::Stop,
                                    })
                                    .await;
                                return;
                            }
                        }
                        SseOutcome::ParseError(error) => {
                            if let Some((log, id)) = &trace_ctx {
                                if let Some(fc) = first_chunk {
                                    log.set_generation_ms(
                                        *id,
                                        fc.elapsed().as_millis() as u32,
                                    );
                                } else {
                                    // No chunk: connect = record→headers,
                                    // ttft = POST-send→error.
                                    log.set_connect_ms(
                                        *id,
                                        request_start
                                            .duration_since(record_created)
                                            .as_millis() as u32,
                                    );
                                    log.set_ttft_ms(
                                        *id,
                                        post_sent.elapsed().as_millis() as u32,
                                    );
                                }
                                log.fail(*id, status.as_u16(), &error);
                            }
                            let _ = tx.send(LlmEvent::Error { error }).await;
                        }
                    }
                }
            }
            Err(e) => {
                // reqwest's body-decode error ("error decoding
                // response body") is a wrapper — the actual reason
                // (connection reset, unexpected EOF mid-chunk, hyper
                // protocol error, TLS error) is in the cause chain.
                // Walk the whole chain so the failure is debuggable.
                let chain = error_chain(&e);
                let partial = if buffer.is_empty() {
                    String::from("(no data received before the error)")
                } else {
                    truncate_raw_stream(&buffer, 2000)
                };
                let error = format!(
                    "stream error (HTTP {status}): {e}\n\
                     cause chain: {chain}\n\
                     content-encoding: {content_encoding}\n\
                     transfer-encoding: {transfer_encoding}\n\
                     content-type: {content_type}\n\
                     partial stream data received so far:\n{partial}"
                );
                if let Some((log, id)) = &trace_ctx {
                    if let Some(fc) = first_chunk {
                        log.set_generation_ms(*id, fc.elapsed().as_millis() as u32);
                    } else {
                        // No chunk: connect = record→headers, ttft = POST-send→error.
                        log.set_connect_ms(
                            *id,
                            request_start
                                .duration_since(record_created)
                                .as_millis() as u32,
                        );
                        log.set_ttft_ms(*id, post_sent.elapsed().as_millis() as u32);
                    }
                    log.fail(*id, status.as_u16(), &error);
                }
                let _ = tx.send(LlmEvent::Error { error }).await;
                return;
            }
        }
    }
    // Flush anything held by the think-tag filter (an unclosed think
    // block flushes as reasoning — the model thought but never
    // answered). Best-effort: the receiver may already be gone.
    for event in think_filter.finish() {
        let _ = tx.send(event).await;
    }
    // If the stream ended without a Finish event, emit one — and only
    // mirror the fallback into the trace log when no real Finish was
    // already logged (the fallback must not overwrite tool_calls /
    // length / content_filter with "stop").
    if !finish_logged {
        if let Some((log, id)) = &trace_ctx {
            log.finish(*id, "stop");
        }
    }
    let _ = tx
        .send(LlmEvent::Finish {
            reason: crate::provider::FinishReason::Stop,
        })
        .await;
}
